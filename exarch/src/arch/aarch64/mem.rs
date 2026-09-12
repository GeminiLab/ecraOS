//! AArch64 four-level translation-table metadata and early memory discovery.

use core::arch::asm;

use aarch64_cpu::{
    asm::barrier::{SY, dsb, isb},
    registers::{MAIR_EL1, TCR_EL1, TTBR0_EL1, TTBR1_EL1, Writeable},
};
use ecraldr_base::PlatformBootArg;
use expt::{
    arch::aarch64::Aarch64PageTableMeta, opaque::OpaquePageTableType, pte::aarch64::A64PTE,
};
use fdt_rs::{
    base::DevTree,
    prelude::{FallibleIterator, PropReader},
};
use memory_addr::{PhysAddr, PhysAddrRange, VirtAddr, pa};

use crate::mem::{
    DEFAULT_RAM_DESC, DEFAULT_RAM_FLAGS, MemoryRegion, MemoryRegionFlags, RawMemoryRegions,
    VirtAddrSpaceMode, VirtAddrSpaceModes, VirtAddrSpaceProps,
};

const AARCH64_MAIR_EL1: u64 = 0x0000_0000_0000_ff04;
const AARCH64_TCR_EL1_48BIT_SPLIT: u64 = 0x0000_0005_b510_3510;

/// Collects the conservative RAM region used before full FDT reservation parsing.
pub fn raw_mem_regions(arg: PlatformBootArg) -> RawMemoryRegions {
    let mut regions = RawMemoryRegions::new();
    if let PlatformBootArg::DeviceTree(dtb_addr) = arg {
        let dtb = unsafe {
            DevTree::from_raw_pointer(VirtAddr::from_usize(dtb_addr.as_usize()).as_ptr())
                .expect("failed to parse device tree")
        };
        for node in dtb.nodes().iterator() {
            let node = node.expect("failed to inspect device tree node");
            let mut is_memory = false;
            let mut is_supported = false;
            let mut is_gic = false;
            let mut reg_values = [0u64; 4];
            let mut reg_count = 0;
            for prop in node.props().iterator() {
                let prop = prop.expect("failed to inspect device-tree property");
                match prop.name().expect("invalid device-tree property name") {
                    "device_type" => {
                        is_memory = prop.str().is_ok_and(|value| value == "memory");
                    }
                    "compatible" => {
                        for value in prop.iter_str().iterator() {
                            let value = value.expect("invalid compatible string");
                            is_gic |= value == "arm,gic-v3";
                            is_supported |=
                                matches!(value, "arm,gic-v3" | "arm,pl011" | "arm,armv8-timer");
                        }
                    }
                    "reg" => {
                        while reg_count < reg_values.len() {
                            let Some(value) = prop.u64(reg_count).ok() else {
                                break;
                            };
                            reg_values[reg_count] = value;
                            reg_count += 1;
                        }
                    }
                    _ => {}
                }
            }

            if is_memory && reg_count >= 2 {
                let start = reg_values[0];
                let mut end = start
                    .checked_add(reg_values[1])
                    .expect("memory range overflow");
                if (start..end).contains(&(dtb_addr.as_usize() as u64)) {
                    end = dtb_addr.as_usize() as u64;
                }
                let _ = regions.push(MemoryRegion {
                    range: PhysAddrRange::new(pa!(start as usize), pa!(end as usize)),
                    flags: DEFAULT_RAM_FLAGS,
                    desc: DEFAULT_RAM_DESC,
                });
            }

            if is_supported && reg_count >= 2 {
                let _ = regions.push(MemoryRegion {
                    range: PhysAddrRange::from_start_size(
                        pa!(reg_values[0] as usize),
                        reg_values[1] as usize,
                    ),
                    flags: MemoryRegionFlags::READ
                        | MemoryRegionFlags::WRITE
                        | MemoryRegionFlags::DEVICE
                        | MemoryRegionFlags::RESERVED,
                    desc: "device-tree MMIO",
                });
                if is_gic && reg_count >= 4 {
                    let _ = regions.push(MemoryRegion {
                        range: PhysAddrRange::from_start_size(
                            pa!(reg_values[2] as usize),
                            reg_values[3] as usize,
                        ),
                        flags: MemoryRegionFlags::READ
                            | MemoryRegionFlags::WRITE
                            | MemoryRegionFlags::DEVICE
                            | MemoryRegionFlags::RESERVED,
                        desc: "device-tree MMIO",
                    });
                }
            }
        }
    }
    regions
}

/// Returns the fixed AArch64 VA mode used by the kernel.
///
/// AArch64 uses separate lower and upper translation regimes. The same
/// translation-table implementation may back both roots, but the address-space
/// contract must describe TTBR0 and TTBR1 independently so the common VMM lays
/// out the high-half mappings at the correct canonical boundary.
pub fn virt_addr_space_modes() -> VirtAddrSpaceModes {
    let mode = VirtAddrSpaceMode::Independent {
        lower: VirtAddrSpaceProps {
            page_shift: 12,
            va_bits: 48,
        },
        upper: VirtAddrSpaceProps {
            page_shift: 12,
            va_bits: 48,
        },
    };
    let mut modes = VirtAddrSpaceModes::new();
    let _ = modes.modes.push(mode);
    modes.current_index = 0;
    modes
}

/// Keeps the active AArch64 VA mode unchanged.
pub fn set_virt_addr_space_mode(mode: VirtAddrSpaceMode) {
    assert!(matches!(
        mode,
        VirtAddrSpaceMode::Independent {
            lower: VirtAddrSpaceProps {
                page_shift: 12,
                va_bits: 48
            },
            upper: VirtAddrSpaceProps {
                page_shift: 12,
                va_bits: 48
            }
        }
    ));

    #[cfg(target_arch = "aarch64")]
    {
        // Attr0 is Device-nGnRE and Attr1 is Normal inner-shareable WB.

        MAIR_EL1.set(AARCH64_MAIR_EL1);
        TCR_EL1.set(AARCH64_TCR_EL1_48BIT_SPLIT);
        dsb(SY);
        isb(SY);
    }
}

/// Returns the AArch64 page-table implementation.
pub fn get_page_table_type(mode: VirtAddrSpaceMode) -> OpaquePageTableType<VirtAddr> {
    assert!(matches!(
        mode,
        VirtAddrSpaceMode::Independent {
            lower: VirtAddrSpaceProps {
                page_shift: 12,
                va_bits: 48
            },
            upper: VirtAddrSpaceProps {
                page_shift: 12,
                va_bits: 48
            }
        }
    ));
    OpaquePageTableType::new::<Aarch64PageTableMeta, A64PTE>()
}

/// Installs a translation-table root in TTBR0 and synchronizes the MMU pipeline.
pub fn set_page_table_root(root: PhysAddr) {
    TTBR0_EL1.set_baddr(root.as_usize() as u64);
    TTBR1_EL1.set_baddr(root.as_usize() as u64);
    aarch64_cpu::asm::barrier::dsb(aarch64_cpu::asm::barrier::ISH);
    // `aarch64-cpu` exposes architectural barriers and system registers, but
    // does not expose the EL1 TLB invalidation instruction.
    unsafe { asm!("tlbi vmalle1is", options(nostack)) };
    aarch64_cpu::asm::barrier::dsb(aarch64_cpu::asm::barrier::ISH);
    aarch64_cpu::asm::barrier::isb(aarch64_cpu::asm::barrier::SY);
}

#[cfg(test)]
mod tests {
    use super::{AARCH64_MAIR_EL1, AARCH64_TCR_EL1_48BIT_SPLIT, virt_addr_space_modes};
    use crate::mem::VirtAddrSpaceMode;

    #[test]
    fn reports_independent_48_bit_halves() {
        let modes = virt_addr_space_modes();
        assert!(matches!(
            modes.modes[modes.current_index],
            VirtAddrSpaceMode::Independent {
                lower: crate::mem::VirtAddrSpaceProps {
                    page_shift: 12,
                    va_bits: 48
                },
                upper: crate::mem::VirtAddrSpaceProps {
                    page_shift: 12,
                    va_bits: 48
                }
            }
        ));
    }

    #[test]
    fn configures_split_translation_registers() {
        assert_eq!(AARCH64_MAIR_EL1 & 0xff, 0x04);
        assert_eq!((AARCH64_MAIR_EL1 >> 8) & 0xff, 0xff);
        assert_eq!(AARCH64_TCR_EL1_48BIT_SPLIT & 0x3f, 16);
        assert_eq!((AARCH64_TCR_EL1_48BIT_SPLIT >> 16) & 0x3f, 16);
        assert_eq!((AARCH64_TCR_EL1_48BIT_SPLIT >> 14) & 0x3, 0);
        assert_eq!((AARCH64_TCR_EL1_48BIT_SPLIT >> 30) & 0x3, 2);
    }
}
