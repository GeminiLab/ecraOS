use multiboot::information::{MemoryManagement, MemoryType, Multiboot, PAddr};
use raw_cpuid::CpuId;
use x86_64::registers::control::{Cr4, Cr4Flags};

use explat::{
    init::PlatformBootArg,
    mem::{
        BootMemoryRegion, BootMemoryRegionType, BootMemoryRegions, MemIf, VirtAddrSpaceHalfStatus,
        VirtAddrSpaceStatus,
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

fn get_multiboot_memory_regions(multiboot_arg: PlatformBootArg) -> BootMemoryRegions {
    let mut memory_regions = BootMemoryRegions::new();
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

            let region_type = match memory_region.memory_type() {
                MemoryType::Available => {
                    if region.start.as_usize() < LOW_MEMORY_END {
                        BootMemoryRegionType::BootService
                    } else {
                        BootMemoryRegionType::Usable
                    }
                }
                MemoryType::Reserved => BootMemoryRegionType::Reserved,
                MemoryType::ACPI => BootMemoryRegionType::Reserved,
                MemoryType::NVS => BootMemoryRegionType::Reserved,
                MemoryType::Defect => continue,
            };

            let push_result = memory_regions.push(BootMemoryRegion {
                range: region,
                type_: region_type,
            });

            if push_result.is_err() {
                break;
            }
        }
    }

    memory_regions
}

fn get_virtual_address_space_status() -> VirtAddrSpaceHalfStatus {
    const LA57_HALF_BITS: u32 = 56;
    const LA48_HALF_BITS: u32 = 47;

    let la57_supported = CpuId::new()
        .get_extended_feature_info()
        .map(|f| f.has_la57())
        .unwrap_or_default();
    let max_bits = if la57_supported {
        LA57_HALF_BITS
    } else {
        LA48_HALF_BITS
    };

    let la57_enabled = Cr4::read().contains(Cr4Flags::L5_PAGING);
    let current_bits = if la57_enabled {
        LA57_HALF_BITS
    } else {
        LA48_HALF_BITS
    };

    VirtAddrSpaceHalfStatus::Enabled {
        current_bits,
        max_bits,
    }
}

pub struct MemImpl;

#[crate_interface::impl_interface]
impl MemIf for MemImpl {
    fn boot_mem_regions(arg: PlatformBootArg) -> Option<BootMemoryRegions> {
        Some(get_multiboot_memory_regions(arg))
    }

    fn virt_addr_space_status() -> VirtAddrSpaceStatus {
        let half = get_virtual_address_space_status();
        VirtAddrSpaceStatus {
            lower_half: half,
            upper_half: half,
        }
    }
}
