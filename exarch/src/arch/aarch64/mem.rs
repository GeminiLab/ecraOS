//! AArch64 translation-table metadata and early memory discovery.

use core::arch::asm;

use aarch64_cpu::{
    asm::barrier::{ISH, SY, dsb, isb},
    registers::{ID_AA64MMFR0_EL1, ID_AA64MMFR2_EL1, Readable, TCR_EL1, TTBR0_EL1, TTBR1_EL1},
};
use ecraldr_base::PlatformBootArg;
use expt::opaque::{OpaquePageTableRoot, OpaquePageTableType};
use fdt_rs::{
    base::DevTree,
    prelude::{FallibleIterator, PropReader},
};
use heapless::Vec as HeaplessVec;
use memory_addr::{PhysAddrRange, VirtAddr, pa};
use tock_registers::LocalRegisterCopy;

use crate::{
    dbcn_println,
    mem::{
        DEFAULT_RAM_DESC, DEFAULT_RAM_FLAGS, MemoryRegion, MemoryRegionFlags, RawMemoryRegions,
        VirtAddrSpaceMode, VirtAddrSpaceModes, VirtAddrSpaceProps,
    },
};

/// Temporary identity-mapped translation tables.
///
/// This child module owns the short-lived roots used while changing AArch64
/// translation geometry.
mod temp_pt;

/// The TCR bit selecting the 52-bit address-size format used by FEAT_LPA2.
// `aarch64-cpu` 11.2.0 does not expose TCR_EL1.DS as a bitfield.
const TCR_DS: u64 = 1 << 59;

macro_rules! props_tcr_conv_fn {
    (
        $props_from_tcr_doc:literal,
        $props_from_tcr_name:ident,
        $tcr_from_props_doc:literal,
        $tcr_from_props_name:ident,
        $epdx:ident,
        $enable_ttbrx_walks:ident,
        $disable_ttbrx_walks:ident,
        $tgx:ident,
        $txsz:ident,
    ) => {
        #[doc = $props_from_tcr_doc]
        fn $props_from_tcr_name(
            tcr: LocalRegisterCopy<u64, TCR_EL1::Register>,
        ) -> Option<VirtAddrSpaceProps> {
            use TCR_EL1::{$epdx, $tgx, $txsz};

            if !tcr.matches_all($epdx::$enable_ttbrx_walks) {
                return None;
            }

            let page_shift = match tcr.read_as_enum::<$tgx::Value>($tgx) {
                Some($tgx::Value::KiB_4) => 12,
                Some($tgx::Value::KiB_16) => 14,
                Some($tgx::Value::KiB_64) => 16,
                None => panic!("invalid TCR_EL1.{}", stringify!($tgx)),
            };

            let va_bits = match tcr.read($txsz) {
                12 => 52,
                16 => 48,
                _ => panic!(
                    "unsupported TCR_EL1.{}: {}",
                    stringify!($txsz),
                    tcr.read($txsz)
                ),
            };

            Some(VirtAddrSpaceProps {
                page_shift,
                va_bits,
            })
        }

        #[doc = $tcr_from_props_doc]
        fn $tcr_from_props_name(
            tcr: &mut LocalRegisterCopy<u64, TCR_EL1::Register>,
            props: Option<VirtAddrSpaceProps>,
        ) {
            use TCR_EL1::{$epdx, $tgx, $txsz};

            let Some(VirtAddrSpaceProps {
                page_shift,
                va_bits,
            }) = props
            else {
                tcr.modify($epdx::$disable_ttbrx_walks);
                return;
            };

            tcr.modify($epdx::$enable_ttbrx_walks + $txsz.val(64 - (va_bits as u64)));
            tcr.modify(match page_shift {
                12 => $tgx::KiB_4,
                14 => $tgx::KiB_16,
                16 => $tgx::KiB_64,
                _ => panic!("unsupported page shift: {}", page_shift),
            });

            if va_bits > 48 && page_shift < 16 {
                tcr.set(tcr.get() | TCR_DS);
            }
        }
    };
}

props_tcr_conv_fn!(
    "Returns the properties for the lower virtual address space from a TCR value.",
    lower_props_from_tcr,
    "Updates a TCR value for the lower virtual address space from its properties.",
    lower_tcr_from_props,
    EPD0,
    EnableTTBR0Walks,
    DisableTTBR0Walks,
    TG0,
    T0SZ,
);

props_tcr_conv_fn!(
    "Returns the properties for the upper virtual address space from a TCR value.",
    upper_props_from_tcr,
    "Updates a TCR value for the upper virtual address space from its properties.",
    upper_tcr_from_props,
    EPD1,
    EnableTTBR1Walks,
    DisableTTBR1Walks,
    TG1,
    T1SZ,
);

/// Decodes a virtual-address-space mode from a TCR value.
fn mode_from_tcr(tcr: LocalRegisterCopy<u64, TCR_EL1::Register>) -> Option<VirtAddrSpaceMode> {
    let lower = lower_props_from_tcr(tcr);
    let upper = upper_props_from_tcr(tcr);

    VirtAddrSpaceMode::from_dual_props(lower, upper)
}

/// Returns the currently active AArch64 virtual-address-space mode.
fn current_mode() -> VirtAddrSpaceMode {
    mode_from_tcr(TCR_EL1.extract())
        .expect("AArch64 has no valid active virtual address space mode")
}

/// Returns a TCR value for a requested AArch64 virtual-address-space mode.
fn tcr_for_mode(mode: VirtAddrSpaceMode) -> LocalRegisterCopy<u64, TCR_EL1::Register> {
    let (lower, upper) = VirtAddrSpaceMode::into_dual_props(mode)
        .expect("AArch64 does not support unified virtual address spaces");

    let mut tcr = LocalRegisterCopy::<u64, TCR_EL1::Register>::new(0);
    tcr.modify(
        TCR_EL1::IPS::Bits_48
            + TCR_EL1::SH0::Inner
            + TCR_EL1::ORGN0::WriteBack_ReadAlloc_WriteAlloc_Cacheable
            + TCR_EL1::IRGN0::WriteBack_ReadAlloc_WriteAlloc_Cacheable
            + TCR_EL1::SH1::Inner
            + TCR_EL1::ORGN1::WriteBack_ReadAlloc_WriteAlloc_Cacheable
            + TCR_EL1::IRGN1::WriteBack_ReadAlloc_WriteAlloc_Cacheable,
    );

    // Set the lower and upper virtual address space properties in the TCR.
    lower_tcr_from_props(&mut tcr, lower);
    upper_tcr_from_props(&mut tcr, upper);

    tcr
}

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
                let end = start
                    .checked_add(reg_values[1])
                    .expect("memory range overflow");
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

/// Returns whether a [`VirtAddrSpaceProps`] is supported by the CPU.
///
/// Returns `None` if the properties are not supported by the CPU. Returns
/// `Some(true)` if the properties are supported. Returns `Some(false)` if the
/// properties are supported by the crate but not by the CPU.
fn is_props_supported_inner(props: VirtAddrSpaceProps, mmfr0: u64, mmfr2: u64) -> Option<bool> {
    // FEAT_LPA2 uses the previously reserved granule encodings 1 and 2 for
    // 4 KiB and 16 KiB respectively.  A 64 KiB granule uses VARange instead.
    /// ID_AA64MMFR0_EL1.TGran4 for "4 KiB granule is supported".
    const TGRAN4_OK: u64 = 0b0000;
    /// ID_AA64MMFR0_EL1.TGran4 for "4 KiB granule supports 52-bit input addresses".
    const TGRAN4_LPA2: u64 = 0b0001;
    /// ID_AA64MMFR0_EL1.TGran16 for "16 KiB granule is supported".
    const TGRAN16_OK: u64 = 0b0001;
    /// ID_AA64MMFR0_EL1.TGran16 for "16 KiB granule supports 52-bit input addresses".
    const TGRAN16_LPA2: u64 = 0b0010;
    /// ID_AA64MMFR0_EL1.TGran64 for "64 KiB granule is supported".
    const TGRAN64_OK: u64 = 0b0000;
    /// ID_AA64MMFR2_EL1.VARange for "52-bit input addresses are supported".
    const VARANGE_52BIT: u64 = 0b0001;

    let VirtAddrSpaceProps {
        page_shift,
        va_bits,
    } = props;

    let is_52_bit = match va_bits {
        48 => false,
        52 => true,
        _ => return None,
    };

    Some(match page_shift {
        12 => {
            let tgran4 = ID_AA64MMFR0_EL1::TGran4.read(mmfr0);

            (tgran4 == TGRAN4_LPA2) || (!is_52_bit && tgran4 == TGRAN4_OK)
        }
        14 => {
            let tgran16 = ID_AA64MMFR0_EL1::TGran16.read(mmfr0);

            (tgran16 == TGRAN16_LPA2) || (!is_52_bit && tgran16 == TGRAN16_OK)
        }
        16 => {
            let tgran64 = ID_AA64MMFR0_EL1::TGran64.read(mmfr0);
            let va_range = ID_AA64MMFR2_EL1::VARange.read(mmfr2);

            (tgran64 == TGRAN64_OK) && (!is_52_bit || va_range == VARANGE_52BIT)
        }
        _ => return None,
    })
}

/// Returns whether a [`VirtAddrSpaceProps`] is supported by this crate and the
/// CPU.
fn is_props_supported(props: VirtAddrSpaceProps, mmfr0: u64, mmfr2: u64) -> bool {
    is_props_supported_inner(props, mmfr0, mmfr2).is_some_and(|b| b)
}

const MAX_PROPS_FOR_HALF: usize = 6;

/// Returns the AArch64 granules and address widths supported by the CPU.
fn supported_props() -> HeaplessVec<VirtAddrSpaceProps, MAX_PROPS_FOR_HALF> {
    let mmfr0 = ID_AA64MMFR0_EL1.get();
    let mmfr2 = ID_AA64MMFR2_EL1.get();

    let mut props_set = HeaplessVec::new();

    for page_shift in [12, 14, 16] {
        for va_bits in [48, 52] {
            let props = VirtAddrSpaceProps {
                page_shift,
                va_bits,
            };

            if is_props_supported_inner(props, mmfr0, mmfr2)
                .expect("invalid AArch64 virtual address-space properties: {props:?}")
            {
                props_set
                    .push(props)
                    .expect("too many AArch64 virtual address-space properties");
            }
        }
    }

    props_set
}

/// Returns whether a [`VirtAddrSpaceMode`] is supported by this crate and the CPU.
fn is_mode_supported(mode: VirtAddrSpaceMode) -> bool {
    let mmfr0 = ID_AA64MMFR0_EL1.get();
    let mmfr2 = ID_AA64MMFR2_EL1.get();

    match mode {
        VirtAddrSpaceMode::LowerOnly(prop) | VirtAddrSpaceMode::UpperOnly(prop) => {
            is_props_supported(prop, mmfr0, mmfr2)
        }
        VirtAddrSpaceMode::Independent { lower, upper } => {
            is_props_supported(lower, mmfr0, mmfr2) && is_props_supported(upper, mmfr0, mmfr2)
        }
        VirtAddrSpaceMode::Unified(_) => false,
    }
}

/// Returns the virtual-address modes supported by AArch64.
///
/// AArch64 uses separate lower and upper translation tables. For each table
/// there are three possible page granules (4 KiB, 16 KiB, and 64 KiB) and two
/// virtual address bits (48 and 52, there are actually more and in some cases
/// neither 48 nor 52 are supported, but we don't consider those cases here).
pub fn virt_addr_space_modes() -> VirtAddrSpaceModes {
    let props_for_half = supported_props();
    if props_for_half.is_empty() {
        panic!("AArch64 reports no supported translation granule");
    }
    let mut modes = VirtAddrSpaceModes::new();

    for p in props_for_half.iter().copied() {
        modes
            .modes
            .push(VirtAddrSpaceMode::LowerOnly(p))
            .expect("too many virtual address space modes");
        modes
            .modes
            .push(VirtAddrSpaceMode::UpperOnly(p))
            .expect("too many virtual address space modes");
    }

    for lower in props_for_half.iter().copied() {
        for upper in props_for_half.iter().copied() {
            modes
                .modes
                .push(VirtAddrSpaceMode::Independent { lower, upper })
                .expect("too many virtual address space modes")
        }
    }

    let current = current_mode();
    modes.current_index = modes
        .modes
        .iter()
        .position(|candidate| *candidate == current)
        .expect("active AArch64 virtual address-space mode is not supported");

    dbcn_println!("  Current mode: {}", current);
    for prop in props_for_half {
        dbcn_println!(
            "  Supported: {} KiB pages, {}-bit virtual addresses",
            1usize << (prop.page_shift - 10),
            prop.va_bits
        );
    }

    modes
}

/// Selects an AArch64 virtual-address-space mode.
///
/// Installs identity-mapped temporary roots before changing the translation
/// geometry. [`set_page_table_root`] replaces those roots with the final roots.
pub fn set_virt_addr_space_mode(mode: VirtAddrSpaceMode) {
    if !is_mode_supported(mode) {
        panic!("Unsupported AArch64 virtual address-space mode: {mode:?}");
    }

    temp_pt::switch_to_temporary_page_tables(mode, tcr_for_mode(mode));
}

/// Returns the page-table implementation for an AArch64 mode.
pub fn get_page_table_type(mode: VirtAddrSpaceMode) -> OpaquePageTableType<VirtAddr> {
    if !is_mode_supported(mode) {
        panic!("Unsupported AArch64 virtual address-space mode: {mode:?}");
    }

    use expt::{
        arch::aarch64::AArch64PageTableMeta as A,
        meta::{LowerCoverage as L, UpperCoverage as U},
        pte::aarch64::A64PTE,
    };
    use product::product;

    use crate::mem::VirtAddrSpaceProps as P;

    macro_rules! match_half_type {
        ([$(($page_shift:literal, $va_bits:literal)),+], $page_shift_var:ident, $va_bits_var:ident, $half:ident, $mode:ident) => {
            match ($page_shift_var, $va_bits_var) {
                $(($page_shift, $va_bits) => OpaquePageTableType::new::<A<$va_bits, $page_shift, $half<VirtAddr>>, A64PTE>(),)+
                _ => panic!("Unsupported AArch64 virtual address-space mode: {:?}", $mode),
            }
        };
    }

    macro_rules! match_dual_type {
        ([$(($lower_page_shift:literal, $lower_va_bits:literal, $upper_page_shift:literal, $upper_va_bits:literal)),+], $lower_page_shift_var:ident, $lower_va_bits_var:ident, $upper_page_shift_var:ident, $upper_va_bits_var:ident, $mode:ident) => {
            match ($lower_page_shift_var, $lower_va_bits_var, $upper_page_shift_var, $upper_va_bits_var) {
                $(
                    ($lower_page_shift, $lower_va_bits, $upper_page_shift, $upper_va_bits) =>
                        OpaquePageTableType::new_dual::<
                            A<$lower_va_bits, $lower_page_shift, L<VirtAddr>>,
                            A64PTE,
                            A<$upper_va_bits, $upper_page_shift, U<VirtAddr>>,
                            A64PTE,
                        >(),
                )+
                _ => panic!("Unsupported AArch64 virtual address-space mode: {:?}", $mode),
            }
        };
    }

    match mode {
        VirtAddrSpaceMode::LowerOnly(P {
            page_shift,
            va_bits,
        }) => product!([12, 14, 16], [48, 52] => match_half_type!(@, page_shift, va_bits, L, mode)),
        VirtAddrSpaceMode::UpperOnly(P {
            page_shift,
            va_bits,
        }) => product!([12, 14, 16], [48, 52] => match_half_type!(@, page_shift, va_bits, U, mode)),
        VirtAddrSpaceMode::Independent {
            lower:
                P {
                    page_shift: lower_page_shift,
                    va_bits: lower_va_bits,
                },
            upper:
                P {
                    page_shift: upper_page_shift,
                    va_bits: upper_va_bits,
                },
        } => {
            product!([12, 14, 16], [48, 52], [12, 14, 16], [48, 52] => match_dual_type!(@, lower_page_shift, lower_va_bits, upper_page_shift, upper_va_bits, mode))
        }
        VirtAddrSpaceMode::Unified(_) => {
            panic!("AArch64 does not support unified virtual address spaces")
        }
    }
}

/// Installs a selected AArch64 translation-table root.
///
/// Dual roots are installed in TTBR0 and TTBR1. A single root is installed in
/// the enabled translation regime for a lower-only or upper-only mode. The MMU
/// pipeline is synchronized before returning.
pub fn set_page_table_root(root: OpaquePageTableRoot) {
    let current_tcr = TCR_EL1.get();
    match root {
        OpaquePageTableRoot::Single(root) if TCR_EL1::EPD1.is_set(current_tcr) => {
            TTBR0_EL1.set_baddr(root.as_usize() as u64);
        }
        OpaquePageTableRoot::Single(root) if TCR_EL1::EPD0.is_set(current_tcr) => {
            TTBR1_EL1.set_baddr(root.as_usize() as u64);
        }
        OpaquePageTableRoot::Single(_) => {
            panic!("AArch64 single-root page table requires a disabled opposite regime");
        }
        OpaquePageTableRoot::Dual(root) => {
            TTBR0_EL1.set_baddr(root.lower.as_usize() as u64);
            TTBR1_EL1.set_baddr(root.upper.as_usize() as u64);
        }
    }
    dsb(ISH);
    // `aarch64-cpu` exposes architectural barriers and system registers, but
    // does not expose the EL1 TLB invalidation instruction.
    unsafe { asm!("tlbi vmalle1is", options(nostack)) };
    dsb(ISH);
    isb(SY);
}

#[cfg(test)]
mod tests {
    use aarch64_cpu::registers::TCR_EL1;
    use tock_registers::LocalRegisterCopy;

    use super::{mode_from_tcr, tcr_for_mode, virt_addr_space_modes};
    use crate::mem::VirtAddrSpaceMode;

    #[test]
    fn reports_independent_48_bit_halves_when_supported() {
        let modes = virt_addr_space_modes();
        assert!(modes.modes.contains(&VirtAddrSpaceMode::Independent {
            lower: crate::mem::VirtAddrSpaceProps {
                page_shift: 12,
                va_bits: 48,
            },
            upper: crate::mem::VirtAddrSpaceProps {
                page_shift: 12,
                va_bits: 48,
            },
        }));
        assert!(modes.current_index < modes.modes.len());
    }

    #[test]
    fn configures_split_translation_registers() {
        let tcr_value = TCR_EL1::T0SZ.val(64 - 48).value
            | TCR_EL1::T1SZ.val(64 - 48).value
            | TCR_EL1::TG0::KiB_4.value
            | TCR_EL1::TG1::KiB_4.value
            | TCR_EL1::SH0::Inner.value
            | TCR_EL1::SH1::Inner.value
            | TCR_EL1::ORGN0::WriteBack_ReadAlloc_WriteAlloc_Cacheable.value
            | TCR_EL1::ORGN1::WriteBack_ReadAlloc_WriteAlloc_Cacheable.value
            | TCR_EL1::IRGN0::WriteBack_ReadAlloc_WriteAlloc_Cacheable.value
            | TCR_EL1::IRGN1::WriteBack_ReadAlloc_WriteAlloc_Cacheable.value
            | TCR_EL1::IPS::Bits_48.value;
        let tcr = LocalRegisterCopy::<u64, TCR_EL1::Register>::new(tcr_value);
        assert_eq!(tcr.read(TCR_EL1::T0SZ), 16);
        assert_eq!(tcr.read(TCR_EL1::T1SZ), 16);
        assert_eq!(
            tcr.read_as_enum::<TCR_EL1::TG0::Value>(TCR_EL1::TG0),
            Some(TCR_EL1::TG0::Value::KiB_4)
        );
        assert_eq!(
            tcr.read_as_enum::<TCR_EL1::TG1::Value>(TCR_EL1::TG1),
            Some(TCR_EL1::TG1::Value::KiB_4)
        );
    }

    #[test]
    fn tcr_round_trips_translation_modes() {
        let modes = [
            VirtAddrSpaceMode::LowerOnly(crate::mem::VirtAddrSpaceProps {
                page_shift: 12,
                va_bits: 52,
            }),
            VirtAddrSpaceMode::UpperOnly(crate::mem::VirtAddrSpaceProps {
                page_shift: 14,
                va_bits: 48,
            }),
            VirtAddrSpaceMode::Independent {
                lower: crate::mem::VirtAddrSpaceProps {
                    page_shift: 16,
                    va_bits: 48,
                },
                upper: crate::mem::VirtAddrSpaceProps {
                    page_shift: 12,
                    va_bits: 52,
                },
            },
        ];

        for mode in modes {
            let tcr = tcr_for_mode(mode);
            assert_eq!(mode_from_tcr(tcr), Some(mode));
        }
    }
}
