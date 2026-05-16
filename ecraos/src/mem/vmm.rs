use core::num::NonZeroUsize;

use memory_addr::{VirtAddr, VirtAddrRange, va};

use explat::mem::{VirtAddrSpaceHalfStatus, VirtAddrSpaceStatus};
use size_disp::SizeDisplay;

use crate::kprintln;

/// The layout of the virtual address space.
pub struct VirtualAddressSpace {
    page_size_shift: NonZeroUsize,
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

pub fn vmalloc_base() -> VirtAddr {
    // SAFETY: We manually ensure that the read happens only after the write.
    unsafe { read_virtual_address_space().vmalloc_range.start }
}

pub fn page_size_shift() -> usize {
    // SAFETY: We manually ensure that the read happens only after the write.
    unsafe { read_virtual_address_space().page_size_shift.get() }
}

pub(super) fn init_vmm_layout(va_status: VirtAddrSpaceStatus) {
    let upper_bits = get_va_upper_bits(&va_status);
    let upper_start = va!((1usize << upper_bits).wrapping_neg());

    kprintln!("Virtual address space:");
    fn print_half_support(which: &str, support: VirtAddrSpaceHalfStatus) {
        match support {
            VirtAddrSpaceHalfStatus::NotSupported => {
                kprintln!("  {:<10}: Not supported", which)
            }
            VirtAddrSpaceHalfStatus::Disabled { max_bits } => kprintln!(
                "  {:<10}: Supported but disabled, max {max_bits} bits",
                which
            ),
            VirtAddrSpaceHalfStatus::Enabled {
                current_bits,
                max_bits,
            } => kprintln!(
                "  {:<10}: Supported and enabled with {current_bits} bits, max {max_bits} bits",
                which
            ),
        }
    }

    print_half_support("Lower half", va_status.lower_half);
    print_half_support("Upper half", va_status.upper_half);

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
            page_size_shift: NonZeroUsize::new(12).unwrap(),
            direct_mapping_range,
            vmalloc_range,
        });
    }
}

fn get_va_upper_bits(memory_info: &VirtAddrSpaceStatus) -> u32 {
    if super::sections::kernel_range().start.as_usize() & (1 << (usize::BITS - 1)) != 0 {
        unimplemented!(
            "Booting directly in the upper half of the virtual address space is not supported yet"
        );
    }

    match memory_info.upper_half {
        VirtAddrSpaceHalfStatus::NotSupported => unimplemented!(
            "Upper half of the virtual address space is not supported, running in the lower half is not supported yet"
        ),
        VirtAddrSpaceHalfStatus::Disabled { .. } => unimplemented!(
            "Upper half of the virtual address space is disabled, VA adjustment is not supported yet"
        ),
        VirtAddrSpaceHalfStatus::Enabled {
            current_bits,
            max_bits,
        } => {
            if current_bits != max_bits {
                kprintln!(
                    "Upper half of the virtual address space is not fully enabled, VA adjustment is not supported yet"
                );
                kprintln!("Using the current VA bits ({current_bits})");
            }

            current_bits
        }
    }
}
