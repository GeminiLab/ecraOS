use expt::{
    arch::x86_64::{X86Level4PageTableMeta, X86Level5PageTableMeta},
    opaque::{OpaquePageTableRoot, OpaquePageTableType},
    pte::x86_64::X64PTE,
};
use memory_addr::{AddrRangeBounds, PhysAddr, PhysAddrRange, VirtAddr};
use multiboot::information::{MemoryManagement, MemoryType, Multiboot, PAddr};
use raw_cpuid::CpuId;
use x86_64::registers::control::{Cr4, Cr4Flags};

use crate::{
    arch::x86_64::imp::{self, gdt::GdtStruct},
    init::PlatformBootArg,
    mem::{
        DEFAULT_RAM_DESC, DEFAULT_RAM_FLAGS, DEFAULT_RESERVED_DESC, DEFAULT_RESERVED_FLAGS,
        MemoryRegion, MemoryRegionFlags, RawMemoryRegions, VirtAddrSpaceMode, VirtAddrSpaceModes,
        VirtAddrSpaceProps,
    },
};

/// The implementation of the [`MemoryManagement`] trait for the multiboot
/// information.
struct MultibootMem;

impl MemoryManagement for MultibootMem {
    unsafe fn paddr_to_slice(&self, addr: PAddr, length: usize) -> Option<&'static [u8]> {
        // SAFETY: We only use this implementation before the early
        // initialization is complete, when paddr is guaranteed to be equal to
        // vaddr.
        unsafe { Some(core::slice::from_raw_parts(addr as *const u8, length)) }
    }

    unsafe fn allocate(&mut self, _length: usize) -> Option<(PAddr, &mut [u8])> {
        None
    }

    unsafe fn deallocate(&mut self, _addr: PAddr) {}
}

/// The end of the low memory region (1 MiB).
const LOW_MEMORY_END: usize = 1 << 20;
/// The description for the low memory region.
const LOW_MEMORT_DESC: &str = "low memory";
/// The description for the ACPI memory region.
const ACPI_MEMORY_DESC: &str = "ACPI memory";
/// The description for the ACPI NVS memory region.
const NVS_MEMORY_DESC: &str = "ACPI NVS memory";
/// The description for the PCI MMIO memory region.
const PCI_MMIO_DESC: &str = "PCI mmio range";
/// The start of the PCI MMIO memory range.
const PCI_MMIO_START: usize = 0xc0000000;
/// The end of the PCI MMIO memory range.
const PCI_MMIO_END: usize = 0xfffc0000;

/// Get the memory regions from the multiboot information.
fn get_multiboot_memory_regions(multiboot_arg: PlatformBootArg) -> RawMemoryRegions {
    // Basically, this function just reads memory regions from the multiboot information, and
    // returns them. However, it handles two ranges in a special way:
    //
    // 1. The low memory range (0x00000000 - 0x000fffff), which is always reserved. Any conflicting
    //    multiboot memory region will be ignored.
    // 2. The ACPI memory range (0xc0000000 - 0xfffc0000), which is always reserved. Unlike the low
    //    memory range, any conflicting multiboot memory region will prevent this region from being
    //    added.

    let mut memory_regions = RawMemoryRegions::new();
    let mut mem = MultibootMem;
    let PlatformBootArg::Multiboot(arg) = multiboot_arg else {
        return memory_regions;
    };
    let info = unsafe { Multiboot::from_ptr(arg.as_usize() as _, &mut mem).unwrap() };

    // Add the low memory region.
    _ = memory_regions.push(MemoryRegion {
        range: PhysAddrRange::new(PhysAddr::from_usize(0), imp::ap::AP_START_PAGE_RANGE.start),
        flags: DEFAULT_RESERVED_FLAGS,
        desc: LOW_MEMORT_DESC,
    });
    _ = memory_regions.push(MemoryRegion {
        range: imp::ap::AP_START_PAGE_RANGE,
        flags: MemoryRegionFlags::READ
            | MemoryRegionFlags::WRITE
            | MemoryRegionFlags::EXECUTE
            | MemoryRegionFlags::RESERVED,
        desc: "AP start page",
    });
    _ = memory_regions.push(MemoryRegion {
        range: PhysAddrRange::new(
            imp::ap::AP_START_PAGE_RANGE.end,
            PhysAddr::from_usize(LOW_MEMORY_END),
        ),
        flags: DEFAULT_RESERVED_FLAGS,
        desc: LOW_MEMORT_DESC,
    });

    // Add the multiboot memory regions.
    if let Some(multiboot_memory_regions) = info.memory_regions() {
        for memory_region in multiboot_memory_regions {
            let region = PhysAddrRange::from_start_size(
                PhysAddr::from_usize(memory_region.base_address() as _),
                memory_region.length() as _,
            );

            if region.start.as_usize() < LOW_MEMORY_END {
                continue;
            }

            let (flags, desc) = match memory_region.memory_type() {
                MemoryType::Available => (DEFAULT_RAM_FLAGS, DEFAULT_RAM_DESC),
                MemoryType::Reserved => (DEFAULT_RESERVED_FLAGS, DEFAULT_RESERVED_DESC),
                MemoryType::ACPI => (DEFAULT_RESERVED_FLAGS, ACPI_MEMORY_DESC),
                MemoryType::NVS => (DEFAULT_RESERVED_FLAGS, NVS_MEMORY_DESC),
                MemoryType::Defect => continue,
            };

            let push_result = memory_regions.push(MemoryRegion {
                range: region,
                flags,
                desc,
            });

            if push_result.is_err() {
                break;
            }
        }
    }

    // Add the PCI MMIO region, if no other memory region conflicts with it.
    let pci_mmio_region = PhysAddrRange::new(
        PhysAddr::from_usize(PCI_MMIO_START),
        PhysAddr::from_usize(PCI_MMIO_END),
    );

    if memory_regions
        .iter()
        .all(|region| !region.range.overlaps(pci_mmio_region))
    {
        _ = memory_regions.push(MemoryRegion {
            range: pci_mmio_region,
            flags: MemoryRegionFlags::READ
                | MemoryRegionFlags::WRITE
                | MemoryRegionFlags::DEVICE
                | MemoryRegionFlags::RESERVED,
            desc: PCI_MMIO_DESC,
        });
    }

    memory_regions
}

const PAGE_SHIFT: u8 = 12;
const LA48_VA_BITS: u8 = 48;
const LA57_VA_BITS: u8 = 57;

/// Checks if the specified virtual address space mode is supported by the `x86_64`
/// architecture, and returns the result of the callback function.
///
/// When the mode is supported, the callback function `ok` will be called, the boolean argument
/// specifies whether the mode is LA48(`false`) or LA57(`true`).
///
/// When the mode is not supported, the callback function `err` will be called with the mode.
fn check_mode<T, O, E>(mode: VirtAddrSpaceMode, ok: O, err: E) -> T
where
    O: FnOnce(bool) -> T,
    E: FnOnce(VirtAddrSpaceMode) -> T,
{
    match mode {
        VirtAddrSpaceMode::Unified(VirtAddrSpaceProps {
            page_shift: PAGE_SHIFT,
            va_bits: va_bits @ (LA48_VA_BITS | LA57_VA_BITS),
        }) => ok(va_bits == LA57_VA_BITS),
        _ => err(mode),
    }
}

core::arch::global_asm!(
    include_str!("mem.S"),
    options(att_syntax),
    code32_selector = const GdtStruct::KCODE32_SELECTOR.0,
    code64_selector = const GdtStruct::KCODE64_SELECTOR.0,
    data_selector = const GdtStruct::KDATA_SELECTOR.0,
);

/// Collects physical memory regions reported by Multiboot.
pub fn raw_mem_regions(arg: PlatformBootArg) -> RawMemoryRegions {
    get_multiboot_memory_regions(arg)
}

/// Returns supported and current x86-64 virtual address-space modes.
pub fn virt_addr_space_modes() -> VirtAddrSpaceModes {
    let mut modes = VirtAddrSpaceModes::new();

    let _ = modes
        .modes
        .push(VirtAddrSpaceMode::Unified(VirtAddrSpaceProps {
            page_shift: PAGE_SHIFT,
            va_bits: LA48_VA_BITS,
        }));
    modes.current_index = modes.modes.len() - 1;

    let la57_supported = CpuId::new()
        .get_extended_feature_info()
        .map(|f| f.has_la57())
        .unwrap_or_default();
    if la57_supported {
        let _ = modes
            .modes
            .push(VirtAddrSpaceMode::Unified(VirtAddrSpaceProps {
                page_shift: PAGE_SHIFT,
                va_bits: LA57_VA_BITS,
            }));

        let la57_enabled = Cr4::read().contains(Cr4Flags::L5_PAGING);
        if la57_enabled {
            modes.current_index = modes.modes.len() - 1;
        }
    }

    modes
}

/// Switches to the requested x86-64 virtual address-space mode.
pub fn set_virt_addr_space_mode(mode: VirtAddrSpaceMode) {
    unsafe extern "sysv64" {
        fn switch_page_level(is_la57: bool);
    }

    check_mode(
        mode,
        |is_la57| unsafe { switch_page_level(is_la57) },
        |mode| panic!("Unsupported virtual address space mode: {:?}", mode),
    )
}

/// Returns the page-table implementation for an x86-64 address-space mode.
pub fn get_page_table_type(mode: VirtAddrSpaceMode) -> OpaquePageTableType<VirtAddr> {
    check_mode(
        mode,
        |is_la57| {
            if is_la57 {
                OpaquePageTableType::new::<X86Level5PageTableMeta, X64PTE>()
            } else {
                OpaquePageTableType::new::<X86Level4PageTableMeta, X64PTE>()
            }
        },
        |mode| panic!("Unsupported virtual address space mode: {:?}", mode),
    )
}

/// Sets the x86-64 page-table root.
pub fn set_page_table_root(root: OpaquePageTableRoot) {
    let OpaquePageTableRoot::Single(root) = root;
    unsafe {
        core::arch::asm!(
            "mov cr3, rax",
            in("rax") root.as_usize()
        )
    }
}
