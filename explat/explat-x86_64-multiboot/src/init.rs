//! [`explat::init::InitIf`] implementation for this platform.

use explat::{
    crate_interface,
    init::{EarlyInitResult, EarlyMemoryRegion, EarlyMemoryRegions, InitIf},
};
use multiboot::information::{MemoryManagement, MemoryType, Multiboot, PAddr};

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

/// The implementation of the [`explat::init::InitIf`] trait.
pub struct InitImpl;

#[crate_interface::impl_interface]
impl InitIf for InitImpl {
    fn init_early(arg: usize) -> EarlyInitResult {
        crate::init_early();

        let mut memory_regions = EarlyMemoryRegions::new();
        let mut mem = MultibootMem;
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

        EarlyInitResult { memory_regions }
    }

    fn init_later() {}
}
