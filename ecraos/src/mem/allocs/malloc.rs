//! Kernel heap allocator integration.
//!
//! Connects the reusable slab allocator core to ecraOS per-CPU state and the
//! physical page allocator, then exposes the result through Rust's global
//! allocator interface.
//!
//! Each CPU serializes local slab operations with its interrupt-disabling lock. Slab memory regions
//! follow exslab's precomputed layouts and are aligned to their complete size. Remote frees
//! publish to the owning slab directly and are drained by its owner on later allocation or
//! deallocation. Owner lookup uses the original allocation layout without scanning memory.
//! Local cache selection and owner routing disable interrupts and preemption together.

use core::{
    alloc::{GlobalAlloc, Layout},
    ptr::NonNull,
};

use expercpu::def_percpu;
use exslab::{AllocResult, SlabAllocResult, SlabAllocator, SlabDeallocResult};
use kspin::SpinNoIrq;
use lazyinit::LazyInit;
use log::info;
use memory_addr::{VirtAddr, pa, va};

use crate::{
    mem::{self, allocs::palloc},
    mp,
};

/// The current CPU's slab allocator.
#[def_percpu]
static CURRENT_SLAB: LazyInit<PerCpuSlab> = LazyInit::new();

/// A CPU's slab allocator state.
///
/// Couples the logical owner identifier with the lock serializing local allocation operations.
struct PerCpuSlab {
    /// Logical CPU id that owns this slab allocator.
    cpu_id: u16,
    /// Locked slab allocator core.
    inner: SpinNoIrq<SlabAllocator>,
}

/// Locked allocation and slab registration operations for one CPU.
impl PerCpuSlab {
    /// Creates an empty per-CPU slab allocator wrapper.
    fn new(cpu_id: u16, page_size: usize) -> Self {
        Self {
            cpu_id,
            inner: SpinNoIrq::new(
                SlabAllocator::new(page_size).expect("unsupported slab hardware page size"),
            ),
        }
    }

    /// Allocates one object from this CPU's slab allocator.
    fn alloc(&self, layout: Layout) -> AllocResult<SlabAllocResult> {
        self.inner.lock().alloc(layout)
    }

    /// Registers mapped slab memory with this CPU's allocator.
    ///
    /// # Safety
    ///
    /// `base` must identify exclusive mapped writable frames of the requested span, aligned to
    /// that complete span. Slab memory must remain valid until returned or all slab users finish.
    unsafe fn add_slab(&self, size_class: exslab::SizeClass, base: VirtAddr) -> AllocResult {
        // SAFETY: The caller guarantees that the supplied frames are valid slab memory.
        unsafe { self.inner.lock().add_slab(size_class, base, self.cpu_id) }
    }

    /// Deallocates a valid object on the owner CPU path.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live allocation owned by this CPU, and `layout` must match its allocation.
    unsafe fn dealloc_local(&self, ptr: NonNull<u8>, layout: Layout) -> SlabDeallocResult {
        // SAFETY: The caller upholds the local deallocation contract.
        unsafe { self.inner.lock().dealloc(ptr, layout) }
    }
}

/// Runs a callback with the current CPU's slab allocator.
///
/// Pins the CPU before selecting its slot and prevents interrupt reentry while it is borrowed.
fn with_current_slab<R>(f: impl FnOnce(&PerCpuSlab) -> R) -> R {
    let _guard = kernel_guard::NoPreemptIrqSave::new();
    CURRENT_SLAB.with_current(|slot| f(&*slot))
}

/// Initializes the current CPU's heap allocator state.
pub fn init_malloc_current_cpu() {
    let cpu_id = mp::current_cpu_id();
    let slab_cpu_id = u16::try_from(cpu_id).expect("CPU id exceeds slab owner range");

    CURRENT_SLAB.with_current(|slot| {
        slot.init_once(PerCpuSlab::new(slab_cpu_id, mem::vmm::page_size()));
    });
    info!("Small object allocator initialized for CPU {}", cpu_id);
}

/// Allocates a small object from a slab.
fn alloc_small(layout: Layout) -> *mut u8 {
    let page_size = mem::vmm::page_size();

    loop {
        match with_current_slab(|slab| slab.alloc(layout)) {
            Ok(SlabAllocResult::Allocated(ptr)) => return ptr.as_ptr(),
            Ok(SlabAllocResult::NeedsSlab { size_class, pages }) => {
                let span = pages * page_size;
                let paddr = match palloc::alloc_frames(pages, span) {
                    Ok(paddr) => paddr,
                    Err(_) => return core::ptr::null_mut(),
                };
                let vaddr = va!(paddr.as_usize() + mem::vmm::direct_mapping_offset());
                // SAFETY: palloc supplies the exact span. The direct-map offset preserves its
                // alignment, and registration validates the resulting virtual base.
                let add_result =
                    with_current_slab(|slab| unsafe { slab.add_slab(size_class, vaddr) });
                if add_result.is_err() {
                    palloc::dealloc_frames(paddr, pages)
                        .expect("failed to dealloc rejected slab frames");
                    return core::ptr::null_mut();
                }
            }
            Err(_) => return core::ptr::null_mut(),
        }
    }
}

/// Allocates a page-backed large object.
fn alloc_large(layout: Layout) -> *mut u8 {
    let page_size = mem::vmm::page_size();
    let bytes = layout.size().max(layout.align());
    let pages = bytes.div_ceil(page_size);
    let align = layout.align().max(page_size);

    match palloc::alloc_frames(pages, align) {
        Ok(paddr) => va!(paddr.as_usize() + mem::vmm::direct_mapping_offset()).as_mut_ptr(),
        Err(_) => core::ptr::null_mut(),
    }
}

/// Deallocates a small object through its owning slab.
///
/// # Safety
///
/// `ptr` and `layout` must satisfy the GlobalAlloc deallocation contract for this allocator.
unsafe fn dealloc_small(ptr: *mut u8, layout: Layout) {
    let page_size = mem::vmm::page_size();
    // SAFETY: GlobalAlloc requires a non-null live allocation with its original layout.
    let nn_ptr = unsafe { NonNull::new_unchecked(ptr) };
    // SAFETY: The live object and its matching layout keep the immutable slab owner readable.
    let owner_cpu = unsafe { SlabAllocator::owner_cpu(nn_ptr, layout, page_size) }
        .expect("invalid slab allocation geometry") as usize;

    let local_result = with_current_slab(|slab| {
        if owner_cpu == slab.cpu_id as usize {
            // SAFETY: The guard pins this CPU from owner comparison through the locked release.
            Some(unsafe { slab.dealloc_local(nn_ptr, layout) })
        } else {
            None
        }
    });
    match local_result {
        Some(SlabDeallocResult::Done) => {}
        Some(SlabDeallocResult::FreeSlab { base, pages }) => {
            let paddr = pa!(base.as_usize() - mem::vmm::direct_mapping_offset());
            palloc::dealloc_frames(paddr, pages).expect("failed to dealloc frames");
        }
        None => {
            // SAFETY: The original layout and page size identify the live slot's exact geometry.
            unsafe { SlabAllocator::remote_free_object(nn_ptr, layout, page_size) }
                .expect("invalid remote slab allocation geometry");
        }
    }
}

/// Deallocates a page-backed large object.
fn dealloc_large(ptr: *mut u8, layout: Layout) {
    let page_size = mem::vmm::page_size();
    let bytes = layout.size().max(layout.align());
    let pages = bytes.div_ceil(page_size);
    let vaddr = VirtAddr::from_mut_ptr_of(ptr);
    let paddr = pa!(vaddr.as_usize() - mem::vmm::direct_mapping_offset());
    palloc::dealloc_frames(paddr, pages).expect("failed to dealloc frames");
}

/// The ecraOS global allocator.
struct EcraosGlobalAlloc;

/// Kernel global allocation behavior for the slab caches and page allocator.
unsafe impl GlobalAlloc for EcraosGlobalAlloc {
    /// Allocates an object from a slab or contiguous physical frames according to its layout.
    ///
    /// # Safety
    ///
    /// The caller must uphold the [`GlobalAlloc::alloc`] contract for the requested layout.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let effective_size = layout.size().max(layout.align());
        if effective_size <= exslab::size_class::SLAB_MAX_SIZE
            && layout.align() <= exslab::size_class::SLAB_MAX_SIZE
        {
            alloc_small(layout)
        } else {
            alloc_large(layout)
        }
    }

    /// Releases an object through the same layout-selected path used during allocation.
    ///
    /// # Safety
    ///
    /// The caller must uphold the [`GlobalAlloc::dealloc`] contract for the pointer and layout.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let effective_size = layout.size().max(layout.align());
        if effective_size <= exslab::size_class::SLAB_MAX_SIZE
            && layout.align() <= exslab::size_class::SLAB_MAX_SIZE
        {
            // SAFETY: The GlobalAlloc caller must provide a live pointer and matching layout.
            unsafe { dealloc_small(ptr, layout) }
        } else {
            dealloc_large(ptr, layout)
        }
    }
}

/// Global allocator instance registered with Rust.
#[global_allocator]
static GLOBAL_ALLOCATOR: EcraosGlobalAlloc = EcraosGlobalAlloc;
