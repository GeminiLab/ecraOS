use core::{
    alloc::{GlobalAlloc, Layout},
    ops::Deref,
    ptr::NonNull,
};

use expercpu::def_percpu;
use exslab::{
    SlabAllocResult,
    page::SlabPageHeader,
    slab::{PerCpuSlab, SlabDeallocResult},
};
use lazyinit::LazyInit;
use log::info;
use memory_addr::{PhysAddr, VirtAddr};

use crate::{mem, mp};

#[def_percpu]
static CURRENT_SLAB: LazyInit<PerCpuSlab> = LazyInit::new();

fn with_current_slab<R>(f: impl FnOnce(&PerCpuSlab) -> R) -> R {
    CURRENT_SLAB.with_current(|slot| f(&*slot))
}

pub fn init_malloc_current_cpu() {
    let cpu_id = crate::mp::current_cpu_id();

    CURRENT_SLAB.with_current(|slot| {
        slot.init_once(PerCpuSlab::new(cpu_id as _, mem::vmm::page_size()));
    });
    info!("Small object allocator initialized for CPU {}", cpu_id);
}

fn alloc_small(layout: Layout) -> *mut u8 {
    let page_size = mem::vmm::page_size();

    loop {
        match with_current_slab(|slab| slab.alloc(layout)) {
            Ok(SlabAllocResult::Allocated(ptr)) => return ptr.as_ptr(),
            Ok(SlabAllocResult::NeedsSlab { size_class, pages }) => {
                let paddr = match mem::palloc::alloc_frames(pages, page_size) {
                    Ok(paddr) => paddr,
                    Err(_) => return core::ptr::null_mut(),
                };
                let vaddr = VirtAddr::from_usize(paddr.as_usize() + mem::vmm::virt_phys_offset());
                with_current_slab(|slab| {
                    slab.add_slab(size_class, vaddr.as_usize(), pages * page_size)
                });
            }
            Err(_) => return core::ptr::null_mut(),
        }
    }
}

fn alloc_large(layout: Layout) -> *mut u8 {
    let page_size = mem::vmm::page_size();
    let bytes = layout.size().max(layout.align());
    let pages = bytes.div_ceil(page_size);
    let align = layout.align().max(page_size);

    match mem::palloc::alloc_frames(pages, align) {
        Ok(paddr) => (paddr.as_usize() + mem::vmm::virt_phys_offset()) as *mut u8,
        Err(_) => core::ptr::null_mut(),
    }
}

fn dealloc_small(ptr: *mut u8, layout: Layout) {
    let page_size = mem::vmm::page_size();
    let obj_addr = ptr as usize;
    let Some(base) = SlabPageHeader::base_from_obj_addr_unknown(obj_addr, page_size) else {
        return;
    };

    let owner_cpu = unsafe { (*(base as *const SlabPageHeader)).owner_cpu as usize };
    let Some(nn_ptr) = NonNull::new(ptr) else {
        return;
    };

    if owner_cpu == mp::current_cpu_id() {
        match with_current_slab(|slab| slab.dealloc_local(nn_ptr, layout)) {
            SlabDeallocResult::Done => {}
            SlabDeallocResult::FreeSlab { base, pages } => {
                let paddr = PhysAddr::from_usize(base.as_usize() - mem::vmm::virt_phys_offset());
                mem::palloc::dealloc_frames(paddr, pages).expect("failed to dealloc frames");
            }
        }
    } else {
        unsafe {
            let owner_slab = CURRENT_SLAB.remote_ref_raw(owner_cpu);
            owner_slab.deref().dealloc_remote(nn_ptr);
        }
    }
}

fn dealloc_large(ptr: *mut u8, layout: Layout) {
    let page_size = mem::vmm::page_size();
    let bytes = layout.size().max(layout.align());
    let pages = bytes.div_ceil(page_size);
    let paddr = PhysAddr::from_usize(ptr as usize - mem::vmm::virt_phys_offset());
    mem::palloc::dealloc_frames(paddr, pages).expect("failed to dealloc frames");
}

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
