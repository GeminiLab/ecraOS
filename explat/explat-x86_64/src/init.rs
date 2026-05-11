//! [`explat::init::InitIf`] implementation for this platform.

use core::num::NonZero;

use multiboot::information::{MemoryManagement, MemoryType, Multiboot, PAddr};
use raw_cpuid::CpuId;
use x86_64::registers::control::{Cr4, Cr4Flags};

use explat::{
    crate_interface,
    init::{
        BootArg, EarlyInitResult, EarlyMemoryInfo, EarlyMemoryRegion, EarlyMemoryRegions, InitIf,
        VAHalfStatus,
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

fn get_multiboot_memory_regions(multiboot_arg: BootArg) -> EarlyMemoryRegions {
    let mut memory_regions = EarlyMemoryRegions::new();
    let mut mem = MultibootMem;
    let BootArg::Multiboot(arg) = multiboot_arg else {
        return memory_regions;
    };
    let info = unsafe { Multiboot::from_ptr(arg as _, &mut mem).unwrap() };

    if let Some(multiboot_memory_regions) = info.memory_regions() {
        for memory_region in multiboot_memory_regions {
            if memory_region.memory_type() == MemoryType::Available {
                if memory_regions
                    .push(EarlyMemoryRegion {
                        start: memory_region.base_address() as _,
                        size: memory_region.length() as _,
                    })
                    .is_err()
                {
                    break;
                }
            }
        }
    }

    memory_regions
}

fn get_virtual_address_space_status() -> VAHalfStatus {
    const LA57_HALF_BITS: NonZero<u32> = NonZero::new(56).unwrap();
    const LA48_HALF_BITS: NonZero<u32> = NonZero::new(47).unwrap();

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

    VAHalfStatus::Enabled {
        current_bits,
        max_bits,
    }
}

/// The implementation of the [`explat::init::InitIf`] trait.
pub struct InitImpl;

#[crate_interface::impl_interface]
impl InitIf for InitImpl {
    fn init_early(arg: BootArg) -> EarlyInitResult {
        crate::init_early();

        let memory_regions = get_multiboot_memory_regions(arg);
        let va_half_status = get_virtual_address_space_status();

        EarlyInitResult {
            memory_info: EarlyMemoryInfo {
                memory_regions,
                va_lower_half_status: va_half_status,
                va_upper_half_status: va_half_status,
            },
        }
    }

    fn init_later() {}
}
