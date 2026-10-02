use ecraldr_base::PlatformBootArg;
use expt::{
    arch::riscv64::{Sv39PageTableMeta, Sv48PageTableMeta, Sv57PageTableMeta},
    opaque::{OpaquePageTableRoot, OpaquePageTableType},
    pte::riscv::Rv64PTE,
};
use fdt_rs::{
    base::DevTree,
    prelude::{FallibleIterator, PropReader},
};
use memory_addr::{AddrRangeBounds, PhysAddrRange, VirtAddr, pa};

use crate::mem::{
    DEFAULT_RAM_DESC, DEFAULT_RAM_FLAGS, MemoryRegion, MemoryRegionFlags, RawMemoryRegions,
    VirtAddrSpaceMode, VirtAddrSpaceModes, VirtAddrSpaceProps,
};

core::arch::global_asm!(include_str!("mem.S"),);

/// The description attached to device-tree MMIO regions.
///
/// These ranges are reserved from allocation and mapped with device-memory attributes.
const DEVICE_MMIO_DESC: &str = "device-tree MMIO";

/// The description attached to the device tree memory range.
///
/// Identifies the reserved device-tree blob rather than MMIO described by its nodes.
const DEVICE_TREE_DESC: &str = "device tree";

/// The alignment used for the device tree memory reservation.
///
/// Covers whole base pages so the blob cannot share an allocatable page with RAM.
const DEVICE_TREE_ALIGN: usize = 4096;

/// Collects physical memory regions reported by the device tree.
///
/// Subtracts the page-aligned device-tree blob from each RAM range and reports it once as
/// reserved device memory, preserving usable RAM on both sides.
pub fn raw_mem_regions(arg: PlatformBootArg) -> RawMemoryRegions {
    let mut regions = RawMemoryRegions::new();

    if let PlatformBootArg::DeviceTree(dtb_addr) = arg {
        let dev_tree = unsafe {
            DevTree::from_raw_pointer(VirtAddr::from_usize(dtb_addr.as_usize()).as_ptr())
                .expect("failed to parse device tree")
        };
        let dtb_range = PhysAddrRange::from_start_size(dtb_addr, dev_tree.totalsize())
            .align_outwards(DEVICE_TREE_ALIGN)
            .expect("device tree range alignment overflow");

        let mems = dev_tree.nodes().filter(|node| {
            Ok(node
                .props()
                .find(|p| Ok(p.name()? == "device_type" && p.str()? == "memory"))
                .is_ok_and(|p| p.is_some()))
        });

        for mem in mems.iterator() {
            let mem = mem.unwrap();
            let reg = mem
                .props()
                .find(|p| Ok(p.name()? == "reg"))
                .unwrap()
                .unwrap();

            let mut start = reg.u64(0).unwrap();
            let mut size = reg.u64(1).unwrap();

            // TODO: remove this magic number
            if start == 0x8000_0000 {
                start = 0x8020_0000;
                size -= 0x20_0000;
            }

            let memory_range = PhysAddrRange::from_start_size(pa!(start as _), size as _);
            let (before_dtb, after_dtb) = memory_range.subtract(dtb_range);
            for range in [before_dtb, after_dtb]
                .into_iter()
                .flatten()
                .filter(|range| !range.is_empty())
            {
                regions
                    .push(MemoryRegion {
                        range,
                        flags: DEFAULT_RAM_FLAGS,
                        desc: DEFAULT_RAM_DESC,
                    })
                    .expect("too many platform memory regions");
            }
        }

        // TODO: handle other devices
        for node in dev_tree.nodes().iterator() {
            let node = node.expect("failed to inspect Device Tree node");
            let supported = node
                .props()
                .any(|prop| {
                    Ok(prop.name()? == "compatible"
                        && prop.str().is_ok_and(|compatible| {
                            matches!(compatible, "sifive,plic-1.0.0" | "riscv,plic0" | "ns16550a")
                        }))
                })
                .expect("failed to inspect Device Tree compatible property");
            if !supported {
                continue;
            }
            let Some(reg) = node
                .props()
                .find(|prop| Ok(prop.name()? == "reg"))
                .expect("failed to inspect Device Tree reg property")
            else {
                continue;
            };
            let start = reg.u64(0).expect("invalid device MMIO base");
            let size = reg.u64(1).expect("invalid device MMIO size");
            regions
                .push(MemoryRegion {
                    range: PhysAddrRange::from_start_size(pa!(start as usize), size as usize),
                    flags: MemoryRegionFlags::READ
                        | MemoryRegionFlags::WRITE
                        | MemoryRegionFlags::DEVICE
                        | MemoryRegionFlags::RESERVED,
                    desc: DEVICE_MMIO_DESC,
                })
                .expect("too many platform memory regions");
        }

        regions
            .push(MemoryRegion {
                range: dtb_range,
                flags: MemoryRegionFlags::READ
                    | MemoryRegionFlags::WRITE
                    | MemoryRegionFlags::RESERVED,
                desc: DEVICE_TREE_DESC,
            })
            .expect("too many platform memory regions");
    }

    regions
}

/// Returns supported and current RISC-V virtual address-space modes.
pub fn virt_addr_space_modes() -> VirtAddrSpaceModes {
    unsafe extern "C" {
        fn detect_max_va_bits() -> usize;
        fn detect_current_va_bits() -> usize;
    }

    let mut modes = VirtAddrSpaceModes::new();
    modes.current_index = usize::MAX;
    let max_va_bits = unsafe { detect_max_va_bits() };
    let current_va_bits = unsafe { detect_current_va_bits() };

    macro_rules! check_va_mode {
        ($va_bits:expr) => {
            if max_va_bits >= $va_bits {
                let _ = modes
                    .modes
                    .push(VirtAddrSpaceMode::Unified(VirtAddrSpaceProps {
                        page_shift: 12,
                        va_bits: $va_bits,
                    }));

                if current_va_bits == $va_bits {
                    modes.current_index = modes.modes.len() - 1;
                }
            }
        };
    }

    check_va_mode!(57);
    check_va_mode!(48);
    check_va_mode!(39);

    if max_va_bits < 39 {
        panic!(
            "This machine seems not to support virtual address space, max_va_bits is {}",
            max_va_bits
        );
    }
    if modes.current_index >= modes.modes.len() {
        panic!("Unexpected current_va_bits: {}", current_va_bits);
    }

    modes
}

/// Switches to the requested RISC-V virtual address-space mode.
pub fn set_virt_addr_space_mode(mode: VirtAddrSpaceMode) {
    if let VirtAddrSpaceMode::Unified(VirtAddrSpaceProps {
        page_shift: 12,
        va_bits,
    }) = mode
    {
        unsafe extern "C" {
            fn set_va_bits(mode: usize) -> bool;
        }

        if !unsafe { set_va_bits(va_bits as _) } {
            panic!("Failed to set virtual address space mode: {:?}", mode);
        }
    } else {
        panic!("Unexpected virtual address space mode: {:?}", mode);
    }
}

/// Returns the page-table implementation for a RISC-V address-space mode.
pub fn get_page_table_type(mode: VirtAddrSpaceMode) -> OpaquePageTableType<VirtAddr> {
    if let VirtAddrSpaceMode::Unified(VirtAddrSpaceProps {
        page_shift: 12,
        va_bits,
    }) = mode
    {
        if va_bits == 57 {
            OpaquePageTableType::new::<Sv57PageTableMeta, Rv64PTE>()
        } else if va_bits == 48 {
            OpaquePageTableType::new::<Sv48PageTableMeta, Rv64PTE>()
        } else if va_bits == 39 {
            OpaquePageTableType::new::<Sv39PageTableMeta, Rv64PTE>()
        } else {
            panic!("Unexpected virtual address space mode: {:?}", mode);
        }
    } else {
        panic!("Unexpected virtual address space mode: {:?}", mode);
    }
}

/// Sets the RISC-V page-table root.
pub fn set_page_table_root(root: OpaquePageTableRoot) {
    let OpaquePageTableRoot::Single(root) = root;
    let root = root.as_usize();

    unsafe {
        core::arch::asm!(
            "csrr {satp}, satp",
            "srli {root}, {root}, 12",      // Get the PPN of the root page table
            "srli {satp}, {satp}, 44",
            "slli {satp}, {satp}, 44",
            "or {satp}, {satp}, {root}",    // Set the new root PPN
            "csrw satp, {satp}",
            "sfence.vma",
            satp = out(reg) _,
            root = in(reg) root,
        )
    }
}
