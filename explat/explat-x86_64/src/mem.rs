use explat::{
    init::PlatformBootArg,
    mem::{
        DEFAULT_RAM_DESC, DEFAULT_RAM_FLAGS, DEFAULT_RESERVED_DESC, DEFAULT_RESERVED_FLAGS, MemIf,
        MemoryRegion, RawMemoryRegions, VirtAddrSpaceMode, VirtAddrSpaceModes, VirtAddrSpaceProps,
    },
    reexport::{
        crate_interface,
        expt::{
            arch::x86_64::{X86Level4PageTableMeta, X86Level5PageTableMeta},
            opaque::OpaquePageTableType,
            pte::x86_64::X64PTE,
        },
        memery_addr::{PhysAddr, PhysAddrRange, VirtAddr},
    },
};
use multiboot::information::{MemoryManagement, MemoryType, Multiboot, PAddr};
use raw_cpuid::CpuId;
use x86_64::registers::control::{Cr4, Cr4Flags};

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

/// Get the memory regions from the multiboot information.
fn get_multiboot_memory_regions(multiboot_arg: PlatformBootArg) -> RawMemoryRegions {
    let mut memory_regions = RawMemoryRegions::new();
    let mut mem = MultibootMem;
    let PlatformBootArg::Multiboot(arg) = multiboot_arg else {
        return memory_regions;
    };
    let info = unsafe { Multiboot::from_ptr(arg.as_usize() as _, &mut mem).unwrap() };

    if let Some(multiboot_memory_regions) = info.memory_regions() {
        for memory_region in multiboot_memory_regions {
            let region = PhysAddrRange::from_start_size(
                PhysAddr::from_usize(memory_region.base_address() as _),
                memory_region.length() as _,
            );

            let (flags, desc) = match memory_region.memory_type() {
                MemoryType::Available if region.start.as_usize() < LOW_MEMORY_END => {
                    (DEFAULT_RESERVED_FLAGS, LOW_MEMORT_DESC)
                }
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

    memory_regions
}

pub struct MemImpl;

impl MemImpl {
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
                page_shift: Self::PAGE_SHIFT,
                va_bits: va_bits @ (Self::LA48_VA_BITS | Self::LA57_VA_BITS),
            }) => ok(va_bits == Self::LA57_VA_BITS),
            _ => err(mode),
        }
    }
}

core::arch::global_asm!(include_str!("mem.S"), options(att_syntax));

#[crate_interface::impl_interface]
impl MemIf for MemImpl {
    fn raw_mem_regions(arg: PlatformBootArg) -> RawMemoryRegions {
        get_multiboot_memory_regions(arg)
    }

    fn virt_addr_space_modes() -> VirtAddrSpaceModes {
        let mut modes = VirtAddrSpaceModes::new();

        let _ = modes
            .modes
            .push(VirtAddrSpaceMode::Unified(VirtAddrSpaceProps {
                page_shift: Self::PAGE_SHIFT,
                va_bits: Self::LA48_VA_BITS,
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
                    page_shift: Self::PAGE_SHIFT,
                    va_bits: Self::LA57_VA_BITS,
                }));

            let la57_enabled = Cr4::read().contains(Cr4Flags::L5_PAGING);
            if la57_enabled {
                modes.current_index = modes.modes.len() - 1;
            }
        }

        modes
    }

    fn set_virt_addr_space_mode(mode: VirtAddrSpaceMode) {
        Self::check_mode(
            mode,
            |is_la57| todo!(),
            |mode| panic!("Unsupported virtual address space mode: {:?}", mode),
        )
    }

    fn get_page_table_type(mode: VirtAddrSpaceMode) -> OpaquePageTableType<VirtAddr> {
        Self::check_mode(
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
}
