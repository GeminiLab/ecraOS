use multiboot::information::{MemoryManagement, MemoryType, Multiboot, PAddr};
use raw_cpuid::CpuId;
use x86_64::registers::control::{Cr4, Cr4Flags};

use explat::{
    init::PlatformBootArg,
    mem::{
        DEFAULT_RAM_FLAGS, DEFAULT_RESERVED_FLAGS, MemIf, MemoryRegion, MemoryRegionFlags,
        MemoryRegions, VirtAddrSpaceMode, VirtAddrSpaceModes, VirtAddrSpaceProps,
    },
    reexport::{
        crate_interface,
        memery_addr::{PhysAddr, PhysAddrRange},
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

fn get_multiboot_memory_regions(multiboot_arg: PlatformBootArg) -> MemoryRegions {
    let mut memory_regions = MemoryRegions::new();
    let mut mem = MultibootMem;
    let PlatformBootArg::Multiboot(arg) = multiboot_arg else {
        return memory_regions;
    };
    let info = unsafe { Multiboot::from_ptr(arg.as_usize() as _, &mut mem).unwrap() };

    if let Some(multiboot_memory_regions) = info.memory_regions() {
        for memory_region in multiboot_memory_regions {
            const LOW_MEMORY_END: usize = 1 << 20;

            let region = PhysAddrRange::from_start_size(
                PhysAddr::from_usize(memory_region.base_address() as _),
                memory_region.length() as _,
            );

            let flags: MemoryRegionFlags = match memory_region.memory_type() {
                MemoryType::Available => {
                    if region.start.as_usize() < LOW_MEMORY_END {
                        DEFAULT_RESERVED_FLAGS
                    } else {
                        DEFAULT_RAM_FLAGS
                    }
                }
                MemoryType::Reserved => DEFAULT_RESERVED_FLAGS,
                MemoryType::ACPI => DEFAULT_RESERVED_FLAGS,
                MemoryType::NVS => DEFAULT_RESERVED_FLAGS,
                MemoryType::Defect => continue,
            };

            let push_result = memory_regions.push(MemoryRegion {
                range: region,
                flags,
            });

            if push_result.is_err() {
                break;
            }
        }
    }

    memory_regions
}

pub struct MemImpl;

#[crate_interface::impl_interface]
impl MemIf for MemImpl {
    fn boot_mem_regions(arg: PlatformBootArg) -> Option<MemoryRegions> {
        Some(get_multiboot_memory_regions(arg))
    }

    fn virt_addr_space_modes() -> VirtAddrSpaceModes {
        let mut modes = VirtAddrSpaceModes::new();

        const PAGE_SHIFT: u8 = 12;
        const LA48_VA_BITS: u8 = 48;
        const LA57_VA_BITS: u8 = 57;

        let _ = modes
            .modes
            .push(VirtAddrSpaceMode::Unified(VirtAddrSpaceProps {
                page_shift: 12,
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

    fn set_virt_addr_space_mode(_mode: VirtAddrSpaceMode) {
        // TODO: Implement this
    }
}
