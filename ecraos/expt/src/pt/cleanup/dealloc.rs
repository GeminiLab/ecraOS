//! Deferred deallocation of independent table-frame allocations.
//!
//! Records retain original byte sizes. Storage grows in allocator frames without a fixed limit.

use core::{mem, ptr::NonNull, slice};

use expalloc_trait::DynPageAllocator;
use memory_addr::PhysAddr;

/// A physical allocation and its original requested byte size awaiting deallocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeallocationItem {
    /// The allocation's starting physical address.
    pub paddr: PhysAddr,
    /// The byte size originally passed to the allocator.
    pub size: usize,
}

/// A node in the linked list of deallocation items stored in a physical frame.
struct DeallocationListNode {
    /// The physical address of this storage frame.
    self_phys: PhysAddr,
    /// The number of initialized records.
    len: usize,
    /// The number of available record slots.
    cap: usize,
    /// The next storage frame.
    next: Option<NonNull<Self>>,
}

impl DeallocationListNode {
    /// Allocates and initializes one storage frame without uninitialized references.
    fn new(allocator: &DynPageAllocator) -> Option<NonNull<Self>> {
        const {
            // Ensure the header alignment is a multiple of the record alignment so that the
            // array of records following the header is properly aligned.
            assert!(mem::align_of::<Self>().is_multiple_of(mem::align_of::<DeallocationItem>()));
        };

        let frame_bytes = 1usize.checked_shl((allocator.page_size_shift)().try_into().ok()?)?;
        let cap =
            frame_bytes.checked_sub(mem::size_of::<Self>())? / mem::size_of::<DeallocationItem>();
        if cap == 0 {
            return None;
        }

        let self_phys = (allocator.alloc_frame)()?;
        let ptr = NonNull::new((allocator.phys_to_virt)(self_phys).as_mut_ptr_of::<Self>())
            .expect("allocator returned an inaccessible storage frame");
        // SAFETY: the allocator supplies aligned, writable storage for the complete node.
        unsafe {
            ptr.as_ptr().write(Self {
                self_phys,
                len: 0,
                cap,
                next: None,
            });
        }
        Some(ptr)
    }

    /// Returns the aligned start of the record array following this header.
    fn items_ptr(&self) -> *mut DeallocationItem {
        // SAFETY: the header size is a multiple of the record alignment.
        unsafe { (self as *const Self).add(1).cast_mut().cast() }
    }

    /// Returns the initialized records in this storage frame.
    fn as_slice(&self) -> &[DeallocationItem] {
        // SAFETY: the first len slots are initialized, and len never exceeds cap.
        unsafe { slice::from_raw_parts(self.items_ptr(), self.len) }
    }
}

/// A list of independent allocations awaiting deallocation stored in a linked list of frames.
///
/// This list is **just a list** and does **NOT** automatically perform deallocation on its items.
/// It's caller's ability and responsibility to interpret the list and perform deallocation on its
/// items.
///
/// Dropping or clearing the list deallocates only its storage frames.
pub struct DeallocationList {
    /// The first and last storage nodes, or neither for an empty list.
    head_and_tail: Option<(NonNull<DeallocationListNode>, NonNull<DeallocationListNode>)>,
    /// The allocator used for all storage nodes in this list.
    allocator: DynPageAllocator,
}

impl DeallocationList {
    /// Creates an empty list without allocating storage.
    pub fn new(allocator: &DynPageAllocator) -> Self {
        Self {
            head_and_tail: None,
            allocator: allocator.clone(),
        }
    }

    /// Appends an allocation, returning it unchanged if storage cannot grow.
    pub fn push(&mut self, item: DeallocationItem) -> Result<(), DeallocationItem> {
        if self.head_and_tail.is_none() {
            let node = DeallocationListNode::new(&self.allocator).ok_or(item)?;
            self.head_and_tail = Some((node, node));
        }

        // SAFETY: we have just assigned it.
        let (_, tail) = unsafe { self.head_and_tail.as_mut().unwrap_unchecked() };

        // SAFETY: nodes remain allocated until this list is drained.
        let node = unsafe { tail.as_mut() };
        if node.len == node.cap {
            let next = DeallocationListNode::new(&self.allocator).ok_or(item)?;
            node.next = Some(next);
            *tail = next;
        }

        // SAFETY: tail has an unused slot and is exclusively accessed by this list.
        let node = unsafe { tail.as_mut() };
        unsafe { node.items_ptr().add(node.len).write(item) };
        node.len += 1;
        Ok(())
    }

    /// Joins another list without allocation or deallocation.
    ///
    /// # Safety
    ///
    /// This list's allocator must be able to deallocate every storage frame transferred from
    /// `other`. Their stored virtual pointers must remain valid until deallocation. This operation
    /// does not require compatibility between the allocator and the recorded allocations.
    pub unsafe fn append(&mut self, mut other: Self) {
        if let Some((head, tail)) = other.head_and_tail.take() {
            match &mut self.head_and_tail {
                Some((_, current_tail)) => {
                    // SAFETY: both lists have disjoint, live storage nodes.
                    unsafe { current_tail.as_mut().next = Some(head) };
                    *current_tail = tail;
                }
                None => self.head_and_tail = Some((head, tail)),
            }
        }
    }

    /// Returns the records in insertion order without processing them.
    pub fn iter(&self) -> impl Iterator<Item = &DeallocationItem> {
        gen {
            let Some((mut node, _)) = self.head_and_tail else {
                return;
            };

            loop {
                // SAFETY: the iterator borrows the list, keeping its nodes alive.
                // Use an explicit type because rust-analyzer cannot infer this node reference.
                let node_ref: &DeallocationListNode = unsafe { node.as_ref() };

                for item in node_ref.as_slice() {
                    yield item;
                }

                if let Some(next) = node_ref.next {
                    node = next;
                } else {
                    break;
                }
            }
        }
    }

    /// Deallocates list storage and discards its records without deallocating recorded allocations.
    pub fn clear(&mut self) {
        self.clear_list();
    }

    /// Deallocates all recorded allocations with the specified allocator and clears the list.
    ///
    /// # Safety
    ///
    /// Each record must identify a distinct live allocation, with a matching address and requested
    /// byte size, that `allocator` can deallocate. Allocations must not overlap each other or the
    /// list's storage. All uses must have ended, including any hardware access requiring
    /// translation or page-walk synchronization. No other code may deallocate these allocations.
    pub unsafe fn deallocate_all_with(&mut self, allocator: &DynPageAllocator) {
        for item in self.iter() {
            (allocator.dealloc_frames_of_size)(item.paddr, item.size);
        }

        self.clear_list();
    }

    /// Explicitly deallocates all recorded allocations with the same allocator and clears the list.
    ///
    /// # Safety
    ///
    /// Same as [`Self::deallocate_all_with`], using this list's allocator.
    pub unsafe fn deallocate_all(&mut self) {
        unsafe { self.deallocate_all_with(&self.allocator.clone()) }
    }

    /// Clears the list and discards all its items.
    fn clear_list(&mut self) {
        let allocator = &self.allocator;
        let mut next = self.head_and_tail.take().map(|(head, _)| head);
        while let Some(ptr) = next {
            // SAFETY: this node is live and no longer linked from the list.
            let node = unsafe { ptr.as_ref() };
            next = node.next;

            let paddr = node.self_phys;
            // SAFETY: no fields are used after destroying this header.
            unsafe { ptr.as_ptr().drop_in_place() };
            (allocator.dealloc_frame)(paddr);
        }
    }
}

impl Drop for DeallocationList {
    /// Deallocates list storage without deallocating recorded allocations.
    fn drop(&mut self) {
        self.clear_list();
    }
}
