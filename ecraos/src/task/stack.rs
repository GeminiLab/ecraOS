//! Owned kernel task stacks.
//!
//! Defines guarded stack geometry and owns each task's vmalloc-backed usable pages.

use expt::pte::MappingFlags;
use memory_addr::{MemoryAddr, VirtAddr, VirtAddrRange};

use crate::mem::{allocs::vmalloc, vmm};

/// The default size of a kernel task stack in bytes.
///
/// The actual size is rounded up to the next page boundary and may be larger than this value.
pub const KERNEL_STACK_DEFAULT_SIZE: usize = 16 * 1024;

/// The required alignment of kernel task stacks.
pub const KERNEL_STACK_ALIGNMENT: usize = 16;

/// The page-table permissions used for mapped kernel task stack pages.
///
/// Stacks are mapped read/write without execute permission.
pub const KERNEL_STACK_MAPPING_FLAGS: MappingFlags = MappingFlags::READ.union(MappingFlags::WRITE);

/// An owned vmalloc kernel stack.
///
/// The usable pages are mapped read/write without execute permission. Vmalloc reserves one
/// unmapped guard page on each side, including the required low guard page.
pub struct KernelStack {
    /// The mapped usable stack range.
    mapped_range: VirtAddrRange,
}

impl KernelStack {
    /// Allocates a guarded kernel stack.
    ///
    /// The returned stack owns its vmalloc reservation until it is dropped.
    pub fn allocate() -> Result<Self, vmalloc::VMAllocError> {
        Self::allocate_with_size(KERNEL_STACK_DEFAULT_SIZE)
    }

    /// Allocates a guarded kernel stack with a specific size.
    ///
    /// The size is rounded up to the next page boundary and may be larger than the requested
    /// value. The returned stack owns its vmalloc reservation until it is dropped.
    pub fn allocate_with_size(size: usize) -> Result<Self, vmalloc::VMAllocError> {
        let page_count = vmm::page_count_for_bytes(size);
        let range = vmalloc::alloc_range_and_map_alloc(page_count, 1, KERNEL_STACK_MAPPING_FLAGS)?;

        Ok(Self::new_inner(range))
    }

    /// Adopts an existing vmalloc stack reservation.
    ///
    /// The caller must provide an allocated and mapped vmalloc range **as is**. The range will be
    /// owned and managed by the returned [`KernelStack`].
    pub(crate) fn adopt_existing(mapped_range: VirtAddrRange) -> Self {
        Self::new_inner(mapped_range)
    }

    fn new_inner(mapped_range: VirtAddrRange) -> Self {
        debug_assert!(mapped_range.start.is_aligned(vmm::page_size()));
        debug_assert!(mapped_range.end.is_aligned(vmm::page_size()));
        debug_assert!(mapped_range.end.is_aligned(KERNEL_STACK_ALIGNMENT));
        Self { mapped_range }
    }

    /// Returns the initial stack boundary.
    ///
    /// The semantic virtual address is aligned for both supported Rust ABIs.
    pub const fn top(&self) -> VirtAddr {
        self.mapped_range.end
    }

    /// Returns the first mapped usable stack address.
    ///
    /// This boundary identifies the owned vmalloc allocation and lies one page above the low guard.
    #[expect(unused)]
    pub const fn mapped_start(&self) -> VirtAddr {
        self.mapped_range.start
    }
}

impl Drop for KernelStack {
    /// Releases the owned vmalloc mapping and backing pages.
    ///
    /// The containing task guarantees that this destructor cannot run while the stack is current.
    /// The virtual reservation is quarantined so a stale remote TLB entry cannot alias a new task
    /// stack before cross-CPU shootdown support exists.
    fn drop(&mut self) {
        // FIXME: Release the virtual reservation after cross-CPU TLB shootdown is implemented.
        vmalloc::with_allocator(|allocator| allocator.unmap(self.mapped_range))
            .expect("vmalloc allocator unavailable while releasing kernel task stack")
            .expect("failed to release kernel task stack");
    }
}
