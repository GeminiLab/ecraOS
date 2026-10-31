//! Size-class slab caches.
//!
//! A cache holds the slabs of one size class. It keeps slabs with free slots separate from full
//! ones so that allocation rarely has to search, and retains one empty slab instead of returning it
//! immediately.
//!
//! A remote free touches only the atomic list in a slab header, so it needs no access to the cache.
//! The cache drains pending remote frees when a later allocation or deallocation uses that slab.

use core::ptr::NonNull;

use memory_addr::{MemoryAddr, VirtAddr};

use crate::{
    SlabLayout,
    error::{AllocError, AllocResult},
    slab_page::{SlabPage, SlabPageHeader},
};

/// A size-class slab cache.
///
/// The cache owns a partial list, a full list, and one retained empty slab. All three hold slabs
/// registered with [`SlabCache::add_slab`]. The bound layout is fixed at construction, so every
/// slab in the cache has the same size, slot count, and slot size.
pub struct SlabCache {
    /// Slabs with at least one locally available slot.
    partial: Option<NonNull<SlabPageHeader>>,
    /// Slabs with no locally available slot, though some may have pending remote frees.
    full: Option<NonNull<SlabPageHeader>>,
    /// Retained empty slab, kept out of both lists.
    empty: Option<NonNull<SlabPageHeader>>,
    /// Shared compile-time layout selected when the cache is constructed.
    layout: &'static SlabLayout,
}

/// Slot allocation and slab memory management for [`SlabCache`].
impl SlabCache {
    /// Creates an empty size-class cache.
    ///
    /// `object_log2` and `slab_log2` are the base-2 logarithms of the slot and slab sizes in bytes.
    /// They must select a power-of-two slot size from 8 to 2048 bytes and a power-of-two slab size
    /// from 4 to 256 KiB. Other values return [`AllocError::InvalidParam`]. The resulting cache
    /// holds no slab until the caller registers one with [`Self::add_slab`].
    pub const fn new(object_log2: usize, slab_log2: usize) -> AllocResult<Self> {
        match SlabLayout::select(object_log2, slab_log2) {
            Some(layout) => Ok(Self::with_layout(layout)),
            None => Err(AllocError::InvalidParam),
        }
    }

    /// Binds an empty cache to one validated static slab layout record.
    ///
    /// Used by [`crate::SlabAllocator`], which selects the layout from its page-size policy.
    pub(crate) const fn with_layout(layout: &'static SlabLayout) -> Self {
        Self {
            partial: None,
            full: None,
            empty: None,
            layout,
        }
    }

    /// Returns the immutable slab layout bound to this cache.
    pub const fn layout(&self) -> &'static SlabLayout {
        self.layout
    }

    /// Registers slab memory with this cache.
    ///
    /// Initializes the slab so that all of its slots are available, then adds it to the partial
    /// list. Returns [`AllocError::InvalidParam`] before memory access if the base is zero or is
    /// not aligned to [`SlabLayout::slab_size`].
    ///
    /// # Safety
    ///
    /// An accepted base must identify exclusive writable slab memory of the bound slab size. Keep
    /// this memory valid until it is returned or all cache operations, slots, and remote producers
    /// end.
    pub unsafe fn add_slab(&mut self, base: VirtAddr, owner_cpu: u16) -> AllocResult {
        if base.as_usize() == 0 || !base.is_aligned(self.layout.slab_size) {
            return Err(AllocError::InvalidParam);
        }
        let slab = NonNull::new(base.as_mut_ptr_of::<SlabPage>()).unwrap();
        // SAFETY: The caller supplies exclusive slab memory of the checked size and alignment.
        unsafe {
            SlabPage::init_at(slab, self.layout, owner_cpu);
            Self::push_front(&mut self.partial, slab.cast());
        }
        Ok(())
    }

    /// Allocates one object slot.
    ///
    /// Takes a slot from the first partial slab. If there is none, a full slab with pending remote
    /// frees is moved back to the partial list, and failing that the retained empty slab is reused.
    /// Returns `None` when the cache holds no slab that can supply a slot, which is the caller's
    /// signal to register a new slab.
    ///
    /// The caller serializes all operations through this cache.
    pub fn alloc_object(&mut self) -> Option<VirtAddr> {
        if let Some(slab) = self.partial {
            // SAFETY: Private state fixes the slab layout and the mutable borrow serializes access.
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

    /// Releases an object slot to this cache.
    ///
    /// Collects pending remote frees before judging whether the slab is empty, so that a slab
    /// emptied by remote frees is not mistaken for a partial one. Returns `Some` with the slab
    /// base when the slab became empty and the cache already retained another empty slab,
    /// leaving that slab for the caller to release. Its memory spans [`SlabLayout::slab_size`]
    /// bytes. Otherwise returns `None`, possibly after retaining this slab as the empty one.
    ///
    /// # Safety
    ///
    /// The address must identify one live slot belonging to this cache, released exactly once.
    /// The caller must serialize this call with all other operations through the cache.
    pub unsafe fn dealloc_object(&mut self, obj_addr: VirtAddr) -> Option<VirtAddr> {
        // SAFETY: A live slot belongs to slab memory with this cache's exact slab layout.
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

    /// Allocates a slot from a partial slab.
    ///
    /// Moves the slab to the full list if the final locally available slot is taken.
    ///
    /// # Safety
    ///
    /// The slab must belong to this cache's partial list and have its bound slab layout.
    /// All owner accesses must be serialized and the slab memory must remain valid.
    unsafe fn alloc_from_partial(&mut self, header: NonNull<SlabPageHeader>) -> VirtAddr {
        let slab = header.cast::<SlabPage>();
        // SAFETY: The caller guarantees a matching slab layout and serialized owner accesses.
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
