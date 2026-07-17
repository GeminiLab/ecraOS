use alloc::{boxed::Box, vec::Vec};

use expt::pte::MappingFlags;
use lazyinit::LazyInit;
use memory_addr::VirtAddr;

use crate::{mem, mp::LogicalCpuId};

static PERCPU_AREAS: LazyInit<Box<[VirtAddr]>> = LazyInit::new();

struct PerCpuAreaIfImpl;

#[crate_interface::impl_interface]
impl expercpu::PerCPUAreaIf for PerCpuAreaIfImpl {
    fn percpu_area_base_for(cpu_id: usize) -> VirtAddr {
        *PERCPU_AREAS
            .get()
            .expect("percpu areas must be initialized before remote access")
            .get(cpu_id)
            .expect("invalid remote percpu cpu id")
    }
}

pub fn section_size() -> usize {
    mem::sections::percpu_aligned().size()
}

/// Initializes the per-CPU data area using the early slot.
pub fn init_bsp_early() {
    unsafe { expercpu::init_in_early_slot() };
}

/// Re-initializes the per-CPU data area pointer after relocation.
pub fn init_bsp_after_reloc() {
    unsafe {
        expercpu::write_percpu_reg(expercpu::early_slot_start());
    };
}

/// Allocates the per-CPU data area for APs.
///
/// This should be done on the BSP because vmalloc requires malloc, which requires percpu, which
/// requires vmalloc, causing a circular dependency.
pub fn alloc_percpu_area_for_ap(ap_count: usize) {
    let mut percpu_area_bases = Vec::with_capacity(ap_count + 1);

    // BSP uses the early slot for percpu.
    percpu_area_bases.push(expercpu::early_slot_start());

    // APs use the percpu area allocated by the BSP.
    let percpu_page_count = mem::vmm::page_count_for_bytes(section_size());

    for _ in 0..ap_count {
        let percpu_area = mem::vmalloc::VMALLOC
            .lock()
            .alloc_allocated(
                percpu_page_count,
                1,
                MappingFlags::READ | MappingFlags::WRITE,
            )
            .expect("failed to allocate percpu area");

        percpu_area_bases.push(percpu_area.start);
    }

    PERCPU_AREAS.init_once(percpu_area_bases.into_boxed_slice());
}

pub fn init_ap(cpu_id: LogicalCpuId) {
    let percpu_area_base = PERCPU_AREAS
        .get()
        .expect("percpu areas must be initialized")
        .get(cpu_id)
        .copied()
        .expect("invalid cpu id");

    unsafe {
        expercpu::init(percpu_area_base);
    }
}
