//! Kernel per-CPU storage.
//!
//! [`expercpu`] provides macros for defining per-CPU variables and generating accessors. This
//! module allocates and initializes their storage and supplies methods for looking up per-CPU area
//! base addresses for remote accesses.
//!
//! The BSP uses the linker-reserved early slot as its per-CPU area, keeping its contents when the
//! kernel relocates. Once the allocators are ready, the BSP allocates separate vmalloc areas for
//! APs. Each AP copies the initial values from the linked `.percpu` section into its own area
//! during startup. All areas remain allocated for the kernel's lifetime.

use alloc::{boxed::Box, vec::Vec};

use expt::pte::MappingFlags;
use lazyinit::LazyInit;
use memory_addr::{AddrRangeBounds, VirtAddr};

use crate::{mem, mp::LogicalCpuId};

/// Per-CPU area base addresses indexed by logical CPU ID.
static PERCPU_AREAS: LazyInit<Box<[VirtAddr]>> = LazyInit::new();

/// The kernel implementation of the `expercpu` area lookup interface.
struct PerCpuAreaIfImpl;

/// Area lookup for `expercpu` remote accessors.
#[crate_interface::impl_interface]
impl expercpu::PerCPUAreaIf for PerCpuAreaIfImpl {
    /// Returns the area base for a logical CPU index.
    ///
    /// # Panics
    ///
    /// Panics before area allocation or if `cpu_id` is outside the area table.
    fn percpu_area_base_for(cpu_id: usize) -> VirtAddr {
        area_base_for(LogicalCpuId::new(cpu_id))
    }
}

/// Returns a CPU's per-CPU area base without checking whether its contents are initialized.
///
/// # Panics
///
/// Panics before area allocation or if `cpu_id` is outside the area table.
fn area_base_for(cpu_id: LogicalCpuId) -> VirtAddr {
    PERCPU_AREAS
        .get()
        .expect("percpu areas must be initialized")
        .get(cpu_id.into_repr())
        .copied()
        .expect("invalid cpu id")
}

/// Returns the per-CPU section size in bytes, including trailing alignment padding.
fn section_size() -> usize {
    mem::sections::percpu_aligned().size()
}

/// Copies initial per-CPU values into the early slot and installs the BSP's per-CPU register.
///
/// # Panics
///
/// Panics if the linker-reserved slot is too small or incorrectly aligned.
///
/// # Safety
///
/// The BSP must call this once after BSS clearing, with interrupts disabled and before using
/// per-CPU values. No other CPU may access the early slot.
pub(crate) unsafe fn init_bsp_early() {
    // SAFETY: The linker reserves a writable, aligned slot separate from the initial image.
    // The caller guarantees exclusive access during early BSP initialization.
    unsafe { expercpu::init_in_early_slot() };
}

/// Updates the BSP's per-CPU register after relocation without resetting stored values.
///
/// # Safety
///
/// The BSP must call this after early-slot initialization and relocation, with interrupts disabled
/// and no per-CPU references live. The relocated slot must map the same writable storage.
pub(crate) unsafe fn init_bsp_after_reloc() {
    // SAFETY: The caller guarantees that the relocated early slot is valid for this BSP.
    unsafe { expercpu::write_percpu_reg(expercpu::early_slot_start()) };
}

/// Allocates per-CPU areas for `ap_count` APs (with logical CPU IDs from 1 to `ap_count`) and
/// records all per-CPU areas' base addresses.
///
/// The BSP calls this once before starting APs, after heap, page allocator, and vmalloc
/// initialization. Each AP receives a read-write mapping with a guard page at each end. The BSP
/// keeps its early slot. APs initialize their areas later through [`init_ap`].
///
/// # Panics
///
/// Panics if an area cannot be allocated or the area table has already been initialized.
pub(crate) fn alloc_percpu_area_for_ap(ap_count: usize) {
    let mut percpu_area_bases = Vec::with_capacity(ap_count + 1);

    // BSP uses the early slot for percpu.
    percpu_area_bases.push(expercpu::early_slot_start());

    // APs use the percpu area allocated by the BSP.
    let percpu_page_count = mem::vmm::page_count_for_bytes(section_size());

    for _ in 0..ap_count {
        let percpu_area = mem::allocs::vmalloc::alloc_range_and_map_alloc(
            percpu_page_count,
            1,
            MappingFlags::READ | MappingFlags::WRITE,
        )
        .expect("failed to allocate percpu area");

        percpu_area_bases.push(percpu_area.start);
    }

    PERCPU_AREAS.init_once(percpu_area_bases.into_boxed_slice());
}

/// Copies initial per-CPU values into the current AP's area and installs its per-CPU register.
///
/// # Panics
///
/// Panics before area allocation or if `cpu_id` is outside the area table.
///
/// # Safety
///
/// `cpu_id` must identify the calling AP. It must call this once, with interrupts disabled and
/// before using per-CPU values. No other CPU may access its area during initialization.
pub(crate) unsafe fn init_ap(cpu_id: LogicalCpuId) {
    let percpu_area_base = area_base_for(cpu_id);

    // SAFETY: The allocated area is writable, page-aligned, large enough for the initial image,
    // and separate from it. The caller guarantees exclusive initialization on the matching AP.
    unsafe {
        expercpu::init(percpu_area_base);
    }
}
