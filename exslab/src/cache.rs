//! Size-class caches bound to immutable precomputed slab layouts.
//!
//! The caller serializes owner operations and supplies complete slab-aligned backing. Remote
//! producers publish through the page header without accessing cache state.

use core::ptr::NonNull;

use memory_addr::{MemoryAddr, VirtAddr};

use crate::{
    SlabLayout,
    error::{AllocError, AllocResult},
    page::{SlabPage, SlabPageHeader},
};

/// Cache with one permanently bound immutable layout and three common-header heads.
pub struct SlabCache {
    /// Slabs with locally available slots.
    partial: Option<NonNull<SlabPageHeader>>,
    /// Slabs with no locally available slots.
    full: Option<NonNull<SlabPageHeader>>,
    /// Retained empty slab, without list neighbors.
    empty: Option<NonNull<SlabPageHeader>>,
    /// Shared compile-time layout selected when the cache is constructed.
    layout: &'static SlabLayout,
}

impl SlabCache {
    /// Creates an empty cache for supported object and slab exponents.
    ///
    /// Object exponents range from 3 to 11. Slab exponents range from 12 to 18.
    pub const fn new(object_log2: usize, slab_log2: usize) -> AllocResult<Self> {
        match SlabLayout::select(object_log2, slab_log2) {
            Some(layout) => Ok(Self::with_layout(layout)),
            None => Err(AllocError::InvalidParam),
        }
    }

    /// Binds an empty cache to one validated static geometry record.
    pub(crate) const fn with_layout(layout: &'static SlabLayout) -> Self {
        Self {
            partial: None,
            full: None,
            empty: None,
            layout,
        }
    }

    /// Returns the immutable geometry bound to this cache.
    pub const fn layout(&self) -> &'static SlabLayout {
        self.layout
    }

    /// Registers backing after checking its complete slab alignment.
    ///
    /// # Safety
    ///
    /// An accepted base must identify exclusive writable backing of the bound slab size. Backing
    /// must remain live until returned or all cache operations, slots, and remote producers end.
    pub unsafe fn add_slab(&mut self, base: VirtAddr, owner_cpu: u16) -> AllocResult {
        if base.as_usize() == 0 || !base.is_aligned(self.layout.slab_size) {
            return Err(AllocError::InvalidParam);
        }
        let slab = NonNull::new(base.as_mut_ptr_of::<SlabPage>()).unwrap();
        // SAFETY: The caller supplies exclusive backing of the checked geometry and alignment.
        unsafe {
            SlabPage::init_at(slab.as_ptr(), self.layout, owner_cpu);
            Self::push_front(&mut self.partial, slab.cast());
        }
        Ok(())
    }

    /// Allocates a slot after draining pending remote releases when needed.
    ///
    /// The caller serializes all owner operations. The private layout binding keeps slab
    /// geometry consistent without selecting an operation-table entry.
    pub fn alloc_object(&mut self) -> Option<VirtAddr> {
        if let Some(slab) = self.partial {
            // SAFETY: Private state fixes geometry and the mutable borrow serializes access.
            return Some(unsafe { self.alloc_from_partial(slab) });
        }
        let mut current = self.full;
        while let Some(slab) = current {
            // SAFETY: Full-list headers remain live and links have exclusive owner access.
            current = unsafe { (*slab.as_ptr()).list_next };
            // SAFETY: Only the atomic remote head is accessed concurrently.
            if unsafe { SlabPageHeader::has_remote_frees(slab) } {
                // SAFETY: The cache owns the slab and serializes bitmap and list updates.
                unsafe {
                    Self::remove(&mut self.full, slab);
                    Self::push_front(&mut self.partial, slab);
                    return Some(self.alloc_from_partial(slab));
                }
            }
        }
        let slab = self.empty.take()?;
        // SAFETY: A retained empty slab is live and has no list neighbors.
        unsafe {
            Self::push_front(&mut self.partial, slab);
            Some(self.alloc_from_partial(slab))
        }
    }

    /// Frees a live slot and returns excess empty backing.
    ///
    /// # Safety
    ///
    /// The address must identify one live slot belonging to this cache, released exactly once
    /// by the serialized owner.
    pub unsafe fn dealloc_object(&mut self, obj_addr: VirtAddr) -> Option<VirtAddr> {
        // SAFETY: A live slot belongs to backing of this exact cache geometry.
        let slab = unsafe { SlabPage::from_object(obj_addr, self.layout) };
        let header = slab.cast::<SlabPageHeader>();
        // SAFETY: Local fields have serialized owner access, without whole-header references.
        unsafe {
            debug_assert_eq!((*header.as_ptr()).size_class, self.layout.size_class);
            let was_full = (*header.as_ptr()).local_free_count == 0;
            SlabPage::local_free(slab, self.layout, obj_addr);
            if was_full {
                Self::remove(&mut self.full, header);
                Self::push_front(&mut self.partial, header);
            }
            SlabPage::drain_remote_frees(slab, self.layout);
            if (*header.as_ptr()).local_free_count != self.layout.slot_count as u16 {
                return None;
            }
            Self::remove(&mut self.partial, header);
            if self.empty.is_none() {
                self.empty = Some(header);
                None
            } else {
                Some(VirtAddr::from_mut_ptr_of(slab.as_ptr()))
            }
        }
    }

    /// Allocates from a known partial slab and updates its list membership.
    ///
    /// # Safety
    ///
    /// The slab must belong to this cache's partial list and have its bound geometry.
    /// All owner accesses must be serialized and the backing must remain live.
    unsafe fn alloc_from_partial(&mut self, header: NonNull<SlabPageHeader>) -> VirtAddr {
        let slab = header.cast::<SlabPage>();
        // SAFETY: The caller guarantees matching geometry and serialized owner accesses.
        unsafe {
            SlabPage::drain_remote_frees(slab, self.layout);
            let object =
                SlabPage::local_alloc(slab, self.layout).expect("a partial slab has a free slot");
            if (*header.as_ptr()).local_free_count == 0 {
                Self::remove(&mut self.partial, header);
                Self::push_front(&mut self.full, header);
            }
            object
        }
    }

    /// Inserts a live unlinked slab at the front of an owner-managed list.
    ///
    /// # Safety
    ///
    /// All nodes must have live headers and valid links. The owner must have exclusive access
    /// to the links, and `slab` must not already belong to a list.
    unsafe fn push_front(
        head: &mut Option<NonNull<SlabPageHeader>>,
        slab: NonNull<SlabPageHeader>,
    ) {
        // SAFETY: Individual link fields are live and exclusively owner-managed.
        unsafe {
            (*slab.as_ptr()).list_prev = None;
            (*slab.as_ptr()).list_next = *head;
            if let Some(first) = *head {
                (*first.as_ptr()).list_prev = Some(slab);
            }
        }
        *head = Some(slab);
    }

    /// Removes one slab from an owner-managed doubly linked list.
    ///
    /// # Safety
    ///
    /// `slab` must belong to `head`, all links must identify live headers, and the owner must
    /// serialize all link updates.
    unsafe fn remove(head: &mut Option<NonNull<SlabPageHeader>>, slab: NonNull<SlabPageHeader>) {
        // SAFETY: The caller guarantees live, exclusively owner-managed link fields.
        unsafe {
            let previous = (*slab.as_ptr()).list_prev;
            let next = (*slab.as_ptr()).list_next;
            if let Some(previous) = previous {
                (*previous.as_ptr()).list_next = next;
            } else {
                *head = next;
            }
            if let Some(next) = next {
                (*next.as_ptr()).list_prev = previous;
            }
            (*slab.as_ptr()).list_prev = None;
            (*slab.as_ptr()).list_next = None;
        }
    }
}
