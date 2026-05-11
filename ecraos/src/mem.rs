use core::num::NonZero;

use explat::init::{EarlyMemoryInfo, VAHalfStatus};
use memory_addr::{VirtAddrRange, va};

use crate::{AutoSize, reloc::sections};

pub fn init_mem_early(memory_info: EarlyMemoryInfo) {
    print_early_mem_info(&memory_info);

    let upper_bits = get_va_upper_bits(&memory_info).get();
    let upper_start = va!((1usize << upper_bits).wrapping_neg());

    explat::dbcn_println!("Virtual address space:");
    explat::dbcn_println!("  Upper half start    : {:#x}", upper_start);

    // Top level memory areas:
    // - 1st half: direct mapping area
    // - 2nd half:
    //   - 3rd quater: vmalloc area
    //   - 4th quater: not used
    // This scheme gives us at least the same size of these areas as Linux does.
    let quater_size = 1usize << (upper_bits - 2);
    let direct_mapping_range = VirtAddrRange::from_start_size(upper_start, quater_size * 2);
    let vmalloc_range = VirtAddrRange::from_start_size(upper_start + quater_size * 2, quater_size);
    explat::dbcn_println!(
        "  Direct mapping area : {:x}, {}",
        direct_mapping_range,
        AutoSize(direct_mapping_range.size())
    );
    explat::dbcn_println!(
        "  Vmalloc area        : {:x}, {}",
        vmalloc_range,
        AutoSize(vmalloc_range.size())
    );
}

fn print_early_mem_info(memory_info: &EarlyMemoryInfo) {
    explat::dbcn_println!("Early memory info:");
    explat::dbcn_println!("  Physical memory regions:");
    for region in &memory_info.memory_regions {
        explat::dbcn_println!(
            "    {:<#010x} - {:<#010x}, {}",
            region.start,
            region.start + region.size,
            AutoSize(region.size),
        );
    }

    explat::dbcn_println!("  Virtual address space:");
    fn print_half_support(which: &str, support: VAHalfStatus) {
        match support {
            VAHalfStatus::NotSupported => explat::dbcn_println!("    {:<10}: Not supported", which),
            VAHalfStatus::Disabled { max_bits } => explat::dbcn_println!(
                "    {:<10}: Supported but disabled, max {max_bits} bits",
                which
            ),
            VAHalfStatus::Enabled {
                current_bits,
                max_bits,
            } => explat::dbcn_println!(
                "    {:<10}: Supported and enabled with {current_bits} bits, max {max_bits} bits",
                which
            ),
        }
    }

    print_half_support("Lower half", memory_info.va_lower_half_status);
    print_half_support("Upper half", memory_info.va_upper_half_status);

    explat::dbcn_println!();
}

fn get_va_upper_bits(memory_info: &EarlyMemoryInfo) -> NonZero<u32> {
    if sections::kernel_range().start.as_usize() & (1 << (usize::BITS - 1)) != 0 {
        unimplemented!(
            "Booting directly in the upper half of the virtual address space is not supported yet"
        );
    }

    match memory_info.va_upper_half_status {
        VAHalfStatus::NotSupported => unimplemented!(
            "Upper half of the virtual address space is not supported, running in the lower half is not supported yet"
        ),
        VAHalfStatus::Disabled { .. } => unimplemented!(
            "Upper half of the virtual address space is disabled, VA adjustment is not supported yet"
        ),
        VAHalfStatus::Enabled {
            current_bits,
            max_bits,
        } => {
            if current_bits != max_bits {
                explat::dbcn_println!(
                    "Upper half of the virtual address space is not fully enabled, VA adjustment is not supported yet"
                );
                explat::dbcn_println!("Using the current VA bits ({current_bits})");
            }

            current_bits
        }
    }
}
