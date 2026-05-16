//! Virtual memory management.

use explat::mem::{VirtAddrSpaceMode, VirtAddrSpaceModes, VirtAddrSpaceProps};
use memory_addr::{VirtAddr, VirtAddrRange, va};
use size_disp::SizeDisplay;

use crate::kprintln;

/// The layout of the virtual address space.
pub struct VirtualAddressSpace {
    #[expect(dead_code)]
    mode: VirtAddrSpaceMode,
    page_shift: u8,
    direct_mapping_range: VirtAddrRange,
    vmalloc_range: VirtAddrRange,
}

static mut VIRTUAL_ADDRESS_SPACE: Option<VirtualAddressSpace> = None;

/// Reads the virtual address space from the static variable.
///
/// # Safety
///
/// The caller must ensure that this function is called only after the virtual
/// address space is initialized.
unsafe fn read_virtual_address_space() -> &'static VirtualAddressSpace {
    unsafe {
        (&raw const VIRTUAL_ADDRESS_SPACE)
            .as_ref_unchecked()
            .as_ref()
            .expect("Virtual address space not initialized")
    }
}

pub fn virt_phys_offset() -> usize {
    // SAFETY: We manually ensure that the read happens only after the write.
    unsafe {
        read_virtual_address_space()
            .direct_mapping_range
            .start
            .as_usize()
    }
}

#[expect(dead_code)]
pub fn vmalloc_base() -> VirtAddr {
    // SAFETY: We manually ensure that the read happens only after the write.
    unsafe { read_virtual_address_space().vmalloc_range.start }
}

pub fn page_size_shift() -> usize {
    // SAFETY: We manually ensure that the read happens only after the write.
    unsafe { read_virtual_address_space().page_shift as _ }
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

    kprintln!("  Selected virtual address space mode: {}", mode);

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

    unsafe {
        VIRTUAL_ADDRESS_SPACE = Some(VirtualAddressSpace {
            // TODO: read page size from the virtual address space status
            mode,
            page_shift,
            direct_mapping_range,
            vmalloc_range,
        });
    }
}

fn va_mode_good(mode: VirtAddrSpaceMode) -> Option<(VirtAddrSpaceMode, u8, u8)> {
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
