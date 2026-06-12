//! Virtual memory management.

use exboot::PhysAddr;
use explat::mem::{VirtAddrSpaceMode, VirtAddrSpaceModes, VirtAddrSpaceProps};
use kspin::SpinNoIrq;
use lazyinit::LazyInit;
use memory_addr::{VirtAddr, VirtAddrRange, pa, va};
use size_disp::SizeDisplay;

use crate::{kprintln, mem};

pub struct VirtualAddressSpace {
    layout: VirtualAddressSpaceLayout,
    page_table_root: PhysAddr,
    page_table_mutex: SpinNoIrq<()>,
}

static VIRTUAL_ADDRESS_SPACE: LazyInit<VirtualAddressSpace> = LazyInit::new();

/// The layout of the virtual address space.
pub struct VirtualAddressSpaceLayout {
    #[expect(dead_code)]
    mode: VirtAddrSpaceMode,
    page_shift: u8,
    direct_mapping_range: VirtAddrRange,
    vmalloc_range: VirtAddrRange,
}

static VIRTUAL_ADDRESS_SPACE_LAYOUT: LazyInit<VirtualAddressSpaceLayout> = LazyInit::new();

pub fn virt_phys_offset() -> usize {
    VIRTUAL_ADDRESS_SPACE_LAYOUT
        .direct_mapping_range
        .start
        .as_usize()
}

pub fn vmalloc_base() -> VirtAddr {
    VIRTUAL_ADDRESS_SPACE_LAYOUT.vmalloc_range.start
}

pub fn page_size_shift() -> usize {
    VIRTUAL_ADDRESS_SPACE_LAYOUT.page_shift as usize
}

pub(super) fn init_vmm_layout() {
    let va_modes = explat::mem::virt_addr_space_modes();

    kprintln!("Virtual address space:");
    kprintln!("  Platform virtual address space modes:");
    for mode in &va_modes.modes {
        kprintln!("    {}", *mode);
    }

    let (mode, page_shift, upper_va_bits) = select_va_mode(&va_modes);
    explat::mem::set_virt_addr_space_mode(mode);

    kprintln!("  Selected virtual address space mode:\n    {}", mode);

    let upper_bits = upper_va_bits;
    let upper_start = va!((1usize << upper_bits).wrapping_neg());

    kprintln!("  Upper half start    : {:#x}", upper_start);

    // Top level memory areas:
    // - 1st half: direct mapping area
    // - 2nd half:
    //   - 3rd quater: vmalloc area
    //   - 4th quater: not used
    // This scheme gives us at least the same size of these areas as Linux does.
    let quater_size = 1usize << (upper_bits - 2);
    let direct_mapping_range = VirtAddrRange::from_start_size(upper_start, quater_size * 2);
    let vmalloc_range = VirtAddrRange::from_start_size(upper_start + quater_size * 2, quater_size);
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

    kprintln!();

    VIRTUAL_ADDRESS_SPACE_LAYOUT.init_once(VirtualAddressSpaceLayout {
        // TODO: read page size from the virtual address space status
        mode,
        page_shift,
        direct_mapping_range,
        vmalloc_range,
    });
}

fn va_mode_good(mode: VirtAddrSpaceMode) -> Option<(VirtAddrSpaceMode, u8, u8)> {
    // TODO: support other modes
    match mode {
        VirtAddrSpaceMode::Unified(VirtAddrSpaceProps {
            page_shift,
            va_bits,
        }) => Some((mode, page_shift, va_bits - 1)),
        _ => None,
    }
}

fn select_va_mode(va_modes: &VirtAddrSpaceModes) -> (VirtAddrSpaceMode, u8, u8) {
    let current = va_modes.modes[va_modes.current_index];
    if let Some(result) = va_mode_good(current) {
        return result;
    }

    for mode in &va_modes.modes {
        if let Some(result) = va_mode_good(*mode) {
            return result;
        }
    }

    panic!("No good virtual address space mode found");
}

/// Temporary for page table root physical address.
pub static mut TEMP_PAGE_TABLE_ROOT: memory_addr::PhysAddr = pa!(0);

pub struct TmpGoodPagingHandler;

impl expt::PagingHandler for TmpGoodPagingHandler {
    fn alloc_page_aligned(bytes_required: usize) -> Option<exboot::PhysAddr> {
        let page_size_shift = page_size_shift();
        let count = bytes_required >> page_size_shift;
        let align = 1 << page_size_shift;
        mem::alloc::alloc_frames(count, align).ok()
    }

    fn dealloc_page_aligned(addr: exboot::PhysAddr, bytes_deallocated: usize) {
        let page_size_shift = page_size_shift();
        let count = bytes_deallocated >> page_size_shift;
        mem::alloc::dealloc_frames(addr, count).unwrap();
    }

    fn phys_to_virt(addr: exboot::PhysAddr) -> VirtAddr {
        VirtAddr::from_usize(addr.as_usize() + virt_phys_offset())
    }
}

pub static mut TEMP_BSP_KERNEL_STACK: VirtAddrRange =
    unsafe { VirtAddrRange::new_unchecked(VirtAddr::from_usize(0), VirtAddr::from_usize(0)) };
