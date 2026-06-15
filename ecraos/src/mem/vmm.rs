//! Virtual memory management.

use exarch::mem::{VirtAddrSpaceMode, VirtAddrSpaceModes};
use expalloc_trait::PageAllocator;
use expt::opaque::{OpaquePageTable, OpaquePageTableType};
use kspin::SpinNoIrq;
use lazyinit::LazyInit;
use log::info;
use memory_addr::{PhysAddr, VirtAddr, VirtAddrRange, va};
use size_disp::SizeDisplay;

use crate::{
    kprintln,
    mem::{self, pmm::MemoryRegions, region_flags_to_mapping},
};

/// The layout of the virtual address space.
pub struct VirtualAddressSpaceLayout {
    mode: VirtAddrSpaceMode,
    page_shift: u8,
    direct_mapping_range: VirtAddrRange,
    vmalloc_range: VirtAddrRange,
}

/// The virtual address space.
pub struct VirtualAddressSpace {
    layout: VirtualAddressSpaceLayout,
    page_table_type_mutex: SpinNoIrq<OpaquePageTableType<VirtAddr>>,
    page_table_mutex: SpinNoIrq<OpaquePageTable<VirtAddr>>,
}

static VIRTUAL_ADDRESS_SPACE: LazyInit<VirtualAddressSpace> = LazyInit::new();

#[inline]
#[expect(unused)]
pub fn mode() -> VirtAddrSpaceMode {
    VIRTUAL_ADDRESS_SPACE.layout.mode
}

#[inline]
pub fn page_size_shift() -> usize {
    VIRTUAL_ADDRESS_SPACE.layout.page_shift as usize
}

#[inline]
pub fn page_size() -> usize {
    1usize << page_size_shift()
}

#[inline]
pub fn page_count_for_bytes(bytes: usize) -> usize {
    bytes.div_ceil(page_size())
}

#[inline]
pub fn virt_phys_offset() -> usize {
    VIRTUAL_ADDRESS_SPACE
        .layout
        .direct_mapping_range
        .start
        .as_usize()
}

#[inline]
pub fn vmalloc_base() -> VirtAddr {
    vmalloc_range().start
}

#[inline]
pub fn vmalloc_range() -> VirtAddrRange {
    VIRTUAL_ADDRESS_SPACE.layout.vmalloc_range
}

#[inline]
pub fn with_page_table<F, T>(f: F) -> T
where
    F: FnOnce(&mut OpaquePageTable<VirtAddr>) -> T,
{
    let mut pt = VIRTUAL_ADDRESS_SPACE.page_table_mutex.lock();
    f(&mut pt)
}

/// Initializes the virtual address space.
///
/// This function detects and selects the virtual address space mode, and then calculates the layout
/// of the virtual address space.
pub(super) fn init_vmm_layout() {
    kprintln!("Virtual address space:");

    let mode = select_va_mode(exarch::mem::virt_addr_space_modes());
    exarch::mem::set_virt_addr_space_mode(mode);
    let layout = calculate_vmm_layout(mode);

    VIRTUAL_ADDRESS_SPACE.init_once(VirtualAddressSpace {
        layout,
        page_table_type_mutex: SpinNoIrq::new(exarch::mem::get_page_table_type(mode)),
        page_table_mutex: SpinNoIrq::new(OpaquePageTable::dummy()),
    });
}

fn select_va_mode(va_modes: VirtAddrSpaceModes) -> VirtAddrSpaceMode {
    kprintln!("  Platform virtual address space modes:");
    for mode in &va_modes.modes {
        kprintln!("    {}", *mode);
    }

    kprintln!(
        "  Current virtual address space mode:\n    {}",
        va_modes.modes[va_modes.current_index]
    );

    // currently, we choose the largest unified mode
    let mut chosen_index = None::<usize>;
    for (index, mode) in va_modes.modes.iter().enumerate() {
        if matches!(mode, VirtAddrSpaceMode::Unified(..))
            && chosen_index
                .map(|index| va_modes.modes[index].upper_va_bits() < mode.upper_va_bits())
                .unwrap_or(true)
        {
            chosen_index = Some(index);
        }
    }

    let chosen_index = chosen_index.expect("No good virtual address space mode found");
    let chosen_mode = va_modes.modes[chosen_index];

    kprintln!(
        "  Selected virtual address space mode:\n    {}",
        chosen_mode
    );

    chosen_mode
}

/// Calculates the layout of the virtual address space based on the given mode.
fn calculate_vmm_layout(mode: VirtAddrSpaceMode) -> VirtualAddressSpaceLayout {
    let upper_bits = mode.upper_va_bits().unwrap().get();
    let page_shift = mode.upper_page_shift().unwrap().get();
    let upper_start = va!((1usize << upper_bits).wrapping_neg());

    kprintln!("  Upper half start    : {:#x}", upper_start);

    // Top level memory areas:
    // - 1st half: direct mapping area
    // - 2nd half:
    //   - 3rd quarter: vmalloc area
    //   - 4th quarter: not used
    // This scheme gives us at least the same size of these areas as Linux does.
    let quarter_size = 1usize << (upper_bits - 2);
    let direct_mapping_range = VirtAddrRange::from_start_size(upper_start, quarter_size * 2);
    let vmalloc_range =
        VirtAddrRange::from_start_size(upper_start + quarter_size * 2, quarter_size);
    kprintln!(
        "  Direct mapping area : {:x}, {}",
        direct_mapping_range,
        direct_mapping_range.size().size_display_wide()
    );
    kprintln!(
        "  Vmalloc area        : {:x}, {}",
        vmalloc_range,
        vmalloc_range.size().size_display_wide()
    );

    VirtualAddressSpaceLayout {
        mode,
        page_shift,
        direct_mapping_range,
        vmalloc_range,
    }
}

/// Initializes the page table with both identical mappings and mappings
/// to the direct mapping area.
pub(super) fn init_vmm_mapping_early<H: PageAllocator>(phys_mem_regions: &MemoryRegions) {
    let virt_phys_offset = virt_phys_offset();
    let mut pt = VIRTUAL_ADDRESS_SPACE
        .page_table_type_mutex
        .lock()
        .new_pagetable_alloc::<H>()
        .unwrap();

    // TODO: the physical memory regions are not guaranteed to be page-aligned,
    // that may cause issues when mapping them. Luckily, it's not a serious or
    // urgent issue now, because free memory regions and kernel sections are
    // almost guaranteed to be page-aligned. We should fix this in the future.
    for region in phys_mem_regions {
        let mapping_flags = region_flags_to_mapping(region.flags);
        if mapping_flags.is_empty() {
            continue;
        }

        let paddr = region.range.start;
        let vaddr_low = VirtAddr::from_usize(paddr.as_usize());
        let vaddr_high = vaddr_low + virt_phys_offset;
        let size = region.range.size();

        pt.map::<H>(vaddr_low, paddr, size, mapping_flags).unwrap();
        pt.map::<H>(vaddr_high, paddr, size, mapping_flags).unwrap();
    }

    *VIRTUAL_ADDRESS_SPACE.page_table_mutex.lock() = pt;
}

pub(super) fn remove_identical_mapping<H: PageAllocator>(phys_mem_regions: &MemoryRegions) {
    info!("Removing identical mappings...");

    // Fix function addresses in the opaque page table type first.
    let page_table_type = exarch::mem::get_page_table_type(VIRTUAL_ADDRESS_SPACE.layout.mode);
    *VIRTUAL_ADDRESS_SPACE.page_table_type_mutex.lock() = page_table_type.clone();

    let page_table_guard = VIRTUAL_ADDRESS_SPACE.page_table_mutex.lock();
    let mut pt = unsafe { page_table_type.new_pagetable_at(page_table_guard.root_paddr()) };

    for region in phys_mem_regions {
        let paddr = region.range.start;
        let vaddr_low = VirtAddr::from_usize(paddr.as_usize());
        let size = region.range.size();

        pt.unmap::<H>(vaddr_low, size).unwrap();
    }

    *VIRTUAL_ADDRESS_SPACE.page_table_mutex.lock() = pt;
}

pub struct TmpGoodPagingHandler;

impl PageAllocator for TmpGoodPagingHandler {
    fn page_size_shift() -> usize {
        page_size_shift()
    }

    fn alloc_frame() -> Option<PhysAddr> {
        mem::palloc::alloc_frame().ok()
    }

    fn alloc_frames(page_count: usize) -> Option<PhysAddr> {
        mem::palloc::alloc_frames(page_count, 1 << page_size_shift()).ok()
    }

    fn dealloc_frame(addr: PhysAddr) {
        mem::palloc::dealloc_frame(addr).unwrap();
    }

    fn dealloc_frames(addr: PhysAddr, page_count: usize) {
        mem::palloc::dealloc_frames(addr, page_count).unwrap();
    }

    fn phys_to_virt(addr: exboot::PhysAddr) -> VirtAddr {
        VirtAddr::from_usize(addr.as_usize() + virt_phys_offset())
    }
}
