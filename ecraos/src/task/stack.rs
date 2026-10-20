//! Owned kernel task stacks.
//!
//! Defines guarded stack geometry and owns each task's vmalloc-backed usable pages.

use alloc::boxed::Box;
use core::{
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

use expt::pte::MappingFlags;
use memory_addr::{MemoryAddr, VirtAddr, VirtAddrRange};

use crate::{
    mem::{allocs::vmalloc, vmm},
    task::preempt,
};

/// A preallocated ownership record for deferred stack release.
///
/// Arbitrary final TaskRef drops can enqueue this node without allocating under an IRQ guard.
struct StackRelease {
    /// The usable allocation start for high-level deallocation.
    ///
    /// Vmalloc retains the full reservation and physical backing until completion.
    address: VirtAddr,
    /// The next owned node in the detached release batch.
    ///
    /// Producers write this before publishing their node with Release ordering.
    next: *mut StackRelease,
}

/// An allocation-free pending stack ownership queue.
///
/// It has no CPU inventory and one atomic head shared by producers and task-context consumers.
struct ReleaseQueue {
    /// The head of the owned intrusive node chain.
    ///
    /// Consumers atomically detach one batch before performing any blocking completion.
    head: AtomicPtr<StackRelease>,
}

impl ReleaseQueue {
    /// Creates an empty pending ownership queue.
    ///
    /// Nodes are allocated with their stack before any final-reference drop.
    const fn new() -> Self {
        Self {
            head: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// Transfers an existing release node into the pending queue.
    ///
    /// This operation neither waits nor allocates and is valid with local interrupts disabled.
    fn push(&self, node: Box<StackRelease>) {
        let node = Box::into_raw(node);
        let mut head = self.head.load(Ordering::Relaxed);
        loop {
            // SAFETY: This producer uniquely owns the unpublished node until its CAS succeeds.
            unsafe {
                (*node).next = head;
            }
            match self
                .head
                .compare_exchange_weak(head, node, Ordering::Release, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(observed) => head = observed,
            }
        }
    }

    /// Detaches one owned batch for reclamation outside all subsystem locks.
    ///
    /// Acquire observes each producer's initialized node and link. A concurrent consumer owns a
    /// separate batch, so no node can be released twice.
    fn drain(&self, mut release: impl FnMut(VirtAddr)) {
        let mut next = self.head.swap(ptr::null_mut(), Ordering::AcqRel);
        while !next.is_null() {
            // SAFETY: Atomic detachment transferred ownership of this complete chain to us.
            let node = unsafe { Box::from_raw(next) };
            next = node.next;
            release(node.address);
        }
    }
}

/// The retained stack release nodes awaiting an ordinary task-context safe point.
///
/// The virtual and physical resources remain in vmalloc until exact remote completion.
static RELEASE_QUEUE: ReleaseQueue = ReleaseQueue::new();

/// Tests whether synchronous reclamation is allowed in the current context.
///
/// An IRQ-saving or preemption guard may protect a queue, mapping or allocator lock.
fn can_release(irq_enabled: bool, depth: usize) -> bool {
    irq_enabled && depth == 0
}

/// Reclaims one detached batch with interrupts enabled and no scheduler locks held.
///
/// The preemption guard pins the consumer and causes recursive safe-point calls to defer. A failed
/// completion retains vmalloc quarantine while only the small release node is reclaimed.
pub(super) fn reap_released_stacks() {
    if !can_release(exarch::trap::local_enabled(), preempt::depth()) {
        return;
    }
    let _guard = kernel_guard::NoPreempt::new();
    RELEASE_QUEUE.drain(|address| {
        if let Err(error) = vmalloc::dealloc_range(address) {
            log::error!("stack release retained reservation at {address:#x}: {error:?}");
        }
    });
}

/// The default size of a kernel task stack in bytes.
///
/// Debug builds need more than 16 KiB for acknowledged vmalloc release, including metadata moves,
/// page-table traversal and interrupt frames. The default leaves room for that full task-context
/// path. The actual size is rounded up to the next page boundary and may be larger than this value.
pub const KERNEL_STACK_DEFAULT_SIZE: usize = 32 * 1024;

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
    /// The preallocated record available to an unsafe final-drop context.
    ///
    /// Taking this option transfers ownership and prevents automatic node destruction on enqueue.
    release: Option<Box<StackRelease>>,
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

    /// Constructs ownership over an already-mapped guarded stack.
    ///
    /// The release node is allocated before the stack can become a current execution context.
    fn new_inner(mapped_range: VirtAddrRange) -> Self {
        debug_assert!(mapped_range.start.is_aligned(vmm::page_size()));
        debug_assert!(mapped_range.end.is_aligned(vmm::page_size()));
        debug_assert!(mapped_range.end.is_aligned(KERNEL_STACK_ALIGNMENT));
        Self {
            mapped_range,
            release: Some(Box::new(StackRelease {
                address: mapped_range.start,
                next: ptr::null_mut(),
            })),
        }
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
    pub const fn mapped_start(&self) -> VirtAddr {
        self.mapped_range.start
    }
}

impl Drop for KernelStack {
    /// Releases the owned vmalloc mapping and backing pages.
    ///
    /// The containing task guarantees that this destructor cannot run while the stack is current.
    /// Remote completion precedes release of both backing and the complete guarded reservation.
    fn drop(&mut self) {
        let node = self
            .release
            .take()
            .expect("stack release ownership already transferred");
        if can_release(exarch::trap::local_enabled(), preempt::depth()) {
            let _guard = kernel_guard::NoPreempt::new();
            if let Err(error) = vmalloc::dealloc_range(self.mapped_start()) {
                log::error!(
                    "stack release retained reservation at {:#x}: {error:?}",
                    self.mapped_start()
                );
            }
        } else {
            RELEASE_QUEUE.push(node);
        }
    }
}

/// Stack-release context tests.
///
/// A final TaskRef drop can occur under an IRQ-saving guard and must defer synchronous completion.
#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, vec::Vec};
    use core::{
        ptr,
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    use std::sync::Arc;

    use memory_addr::va;

    use super::{ReleaseQueue, StackRelease, can_release};

    /// Allows reclamation only outside critical sections with interrupts enabled.
    ///
    /// This covers context handoff, arbitrary TaskRef drops and ordinary task execution.
    #[test]
    fn defers_unsafe_release_contexts() {
        assert!(can_release(true, 0));
        assert!(!can_release(false, 0));
        assert!(!can_release(true, 1));
        assert!(!can_release(false, 1));
    }

    /// Transfers each retired reservation exactly once while producers and consumers race.
    ///
    /// The real allocation-free intrusive queue is exercised without privileged stack deallocation.
    #[test]
    fn concurrent_release_nodes_are_owned_once() {
        let queue = Arc::new(ReleaseQueue::new());
        let finished = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();
        for producer in 0..4 {
            let queue = Arc::clone(&queue);
            let finished = Arc::clone(&finished);
            workers.push(std::thread::spawn(move || {
                for index in 0..64 {
                    queue.push(Box::new(StackRelease {
                        address: va!(producer * 64 + index + 1),
                        next: ptr::null_mut(),
                    }));
                }
                finished.fetch_add(1, Ordering::Release);
            }));
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut seen = Vec::new();
        while finished.load(Ordering::Acquire) != 4 {
            assert!(std::time::Instant::now() < deadline);
            queue.drain(|address| seen.push(address.as_usize()));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        queue.drain(|address| seen.push(address.as_usize()));
        seen.sort_unstable();
        assert_eq!(seen, (1..=256).collect::<Vec<_>>());
    }
}
