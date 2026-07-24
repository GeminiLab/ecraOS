//! Kernel heap allocator integration.
//!
//! Connects the reusable slab allocator core to ecraOS per-CPU state and the
//! physical page allocator, then exposes the result through Rust's global
//! allocator interface.

use core::{
    alloc::{GlobalAlloc, Layout},
    ops::Deref,
    ptr::NonNull,
};

use expercpu::def_percpu;
use exslab::{
    AllocResult, SlabAllocResult, SlabAllocator, page::SlabPageHeader, slab::SlabDeallocResult,
};
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

/// ecraOS per-CPU wrapper around the reusable slab allocator core.
struct PerCpuSlab {
    /// Logical CPU id that owns this slab allocator.
    cpu_id: u16,
    /// Page size used by all slab pages owned by this allocator.
    page_size: usize,
    /// Locked slab allocator core.
    inner: SpinNoIrq<SlabAllocator>,
}

impl PerCpuSlab {
    /// Creates an empty per-CPU slab allocator wrapper.
    const fn new(cpu_id: u16, page_size: usize) -> Self {
        Self {
            cpu_id,
            page_size,
            inner: SpinNoIrq::new(SlabAllocator::new(page_size)),
        }
    }

    /// Allocates one object from this CPU's slab allocator.
    fn alloc(&self, layout: Layout) -> AllocResult<SlabAllocResult> {
        self.inner.lock().alloc(layout)
    }

    /// Registers a slab page owned by this CPU.
    fn add_slab(&self, size_class: exslab::SizeClass, base: VirtAddr, bytes: usize) -> AllocResult {
        self.inner
            .lock()
            .add_slab(size_class, base, bytes, self.cpu_id)
    }

    /// Deallocates an object on the owner CPU path.
    fn dealloc_local(&self, ptr: NonNull<u8>, layout: Layout) -> SlabDeallocResult {
        self.inner.lock().dealloc(ptr, layout)
    }

    /// Queues an object onto this CPU's remote-free list.
    fn dealloc_remote(&self, ptr: NonNull<u8>) {
        unsafe { SlabPageHeader::remote_free_object(ptr, self.cpu_id, self.page_size) };
    }
}

/// Runs a callback with the current CPU's slab allocator.
fn with_current_slab<R>(f: impl FnOnce(&PerCpuSlab) -> R) -> R {
    CURRENT_SLAB.with_current(|slot| f(&*slot))
}

/// Initializes the current CPU's heap allocator state.
pub fn init_malloc_current_cpu() {
    let cpu_id = crate::mp::current_cpu_id();
    let slab_cpu_id = u16::try_from(cpu_id).expect("CPU id exceeds slab owner range");

    CURRENT_SLAB.with_current(|slot| {
        slot.init_once(PerCpuSlab::new(slab_cpu_id, mem::vmm::page_size()));
    });
    info!("Small object allocator initialized for CPU {}", cpu_id);
}

/// Allocates a slab-backed small object.
fn alloc_small(layout: Layout) -> *mut u8 {
    let page_size = mem::vmm::page_size();

    loop {
        match with_current_slab(|slab| slab.alloc(layout)) {
            Ok(SlabAllocResult::Allocated(ptr)) => return ptr.as_ptr(),
            Ok(SlabAllocResult::NeedsSlab { size_class, pages }) => {
                let paddr = match palloc::alloc_frames(pages, page_size) {
                    Ok(paddr) => paddr,
                    Err(_) => return core::ptr::null_mut(),
                };
                let vaddr = va!(paddr.as_usize() + mem::vmm::direct_mapping_offset());
                if with_current_slab(|slab| slab.add_slab(size_class, vaddr, pages * page_size))
                    .is_err()
                {
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

/// Deallocates a slab-backed small object.
fn dealloc_small(ptr: *mut u8, layout: Layout) {
    let page_size = mem::vmm::page_size();
    let obj_addr = VirtAddr::from_mut_ptr_of(ptr);
    let Some(base) = SlabPageHeader::base_from_obj_addr_unknown(obj_addr, page_size) else {
        return;
    };

    let owner_cpu = unsafe { (*base.as_ptr_of::<SlabPageHeader>()).owner_cpu as usize };
    let Some(nn_ptr) = NonNull::new(ptr) else {
        return;
    };

    if owner_cpu == mp::current_cpu_id() {
        match with_current_slab(|slab| slab.dealloc_local(nn_ptr, layout)) {
            SlabDeallocResult::Done => {}
            SlabDeallocResult::FreeSlab { base, pages } => {
                let paddr = pa!(base.as_usize() - mem::vmm::direct_mapping_offset());
                palloc::dealloc_frames(paddr, pages).expect("failed to dealloc frames");
            }
        }
    } else {
        unsafe {
            let owner_slab = CURRENT_SLAB.remote_ref_raw(owner_cpu);
            owner_slab.deref().dealloc_remote(nn_ptr);
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

unsafe impl GlobalAlloc for EcraosGlobalAlloc {
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

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let effective_size = layout.size().max(layout.align());
        if effective_size <= exslab::size_class::SLAB_MAX_SIZE
            && layout.align() <= exslab::size_class::SLAB_MAX_SIZE
        {
            dealloc_small(ptr, layout)
        } else {
            dealloc_large(ptr, layout)
        }
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: EcraosGlobalAlloc = EcraosGlobalAlloc;
