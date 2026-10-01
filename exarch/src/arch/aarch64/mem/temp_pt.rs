//! Temporary identity-mapped AArch64 translation tables.
//!
//! The tables in this module keep the currently executing low physical addresses
//! accessible while the translation geometry is changed.

use core::{arch::asm, ptr::addr_of_mut};

use aarch64_cpu::{
    asm::barrier::{SY, dsb, isb},
    registers::{
        MAIR_EL1, ReadWriteable, Readable, SCTLR_EL1, TCR_EL1, TTBR0_EL1, TTBR1_EL1, Writeable,
    },
};
use expt::pte::aarch64::A64PTE;
use memory_addr::PhysAddr;
use page_table_entry::{GenericPTE, MappingFlags};
use tock_registers::LocalRegisterCopy;

use super::{VirtAddrSpaceMode, VirtAddrSpaceProps};

/// The MAIR value used for device and normal memory mappings.
///
/// Attribute 0 is Device-nGnRE and attribute 1 is inner-shareable normal
/// write-back memory.
const AARCH64_MAIR_EL1: u64 = MAIR_EL1::Attr0_Device::nonGathering_nonReordering_EarlyWriteAck
    .value
    | MAIR_EL1::Attr1_Normal_Outer::WriteBack_NonTransient_ReadWriteAlloc.value
    | MAIR_EL1::Attr1_Normal_Inner::WriteBack_NonTransient_ReadWriteAlloc.value;

/// The number of entries reserved for each temporary page-table level.
///
/// This is large enough for the largest supported AArch64 translation granule.
const TEMPORARY_TABLE_ENTRIES: usize = 8192;
/// The number of temporary page-table levels reserved per translation regime.
///
/// The storage covers the levels below the root and alignment slack separately.
const TEMPORARY_TABLE_LEVELS: usize = 3;
/// The maximum supported translation granule size.
///
/// Temporary roots are aligned to this value because it is the strictest
/// alignment required by the supported granules.
const MAX_PAGE_SIZE: usize = 1 << 16;
/// The size of the temporary identity-mapped physical window.
///
/// The window covers the low physical addresses used during early boot.
const TEMPORARY_IDENTITY_SIZE: usize = 1 << 39;
/// The size of the device-memory prefix in the temporary identity mapping.
///
/// The prefix preserves the loader's device-memory attributes for MMIO.
const TEMPORARY_DEVICE_SIZE: usize = 1 << 30;
/// The number of entries reserved to include runtime-alignment slack.
///
/// The extra table accommodates a root aligned within the statically allocated
/// storage after PIE relocation.
const TEMPORARY_STORAGE_ENTRIES: usize = TEMPORARY_TABLE_ENTRIES * (TEMPORARY_TABLE_LEVELS + 1);

/// The statically allocated temporary translation tables.
///
/// The backing storage is over-allocated so the runtime-selected root can be
/// aligned even when the PIE load offset changes the link-time address.
#[repr(C, align(65536))]
struct TemporaryPageTables {
    entries: [A64PTE; TEMPORARY_STORAGE_ENTRIES],
}

impl TemporaryPageTables {
    /// Creates empty temporary translation tables.
    ///
    /// The entries are populated immediately before the roots are installed.
    const fn new() -> Self {
        Self {
            entries: [A64PTE::empty(); TEMPORARY_STORAGE_ENTRIES],
        }
    }
}

/// The temporary lower-regime translation tables.
///
/// This storage is used when TTBR0 participates in the selected mode.
static mut TEMPORARY_LOWER_TABLES: TemporaryPageTables = TemporaryPageTables::new();
/// The temporary upper-regime translation tables.
///
/// This storage is used when TTBR1 participates in the selected mode.
static mut TEMPORARY_UPPER_TABLES: TemporaryPageTables = TemporaryPageTables::new();

/// Builds a temporary identity-mapped root for one translation regime.
///
/// The root and all subordinate tables are constructed while the old identity
/// mapping is still active.
fn temporary_page_table_root(
    storage: *mut TemporaryPageTables,
    props: VirtAddrSpaceProps,
) -> PhysAddr {
    let page_shift = usize::from(props.page_shift);
    let level_bits = page_shift - 3;
    let entries_per_table = 1usize << level_bits;
    let levels = (usize::from(props.va_bits) - page_shift).div_ceil(level_bits);
    let normal_flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE;
    let device_flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::DEVICE;

    assert!(entries_per_table <= TEMPORARY_TABLE_ENTRIES);
    assert!((3..=5).contains(&levels));

    // Build the tables while the old identity mapping is still active. A PIE
    // load offset does not preserve the static's link-time 64 KiB alignment,
    // so align the root inside the over-allocated storage at runtime.
    unsafe {
        let storage_entries = addr_of_mut!((*storage).entries).cast::<A64PTE>();
        let root = (storage_entries as usize).next_multiple_of(MAX_PAGE_SIZE) as *mut A64PTE;
        for index in 0..TEMPORARY_TABLE_ENTRIES * TEMPORARY_TABLE_LEVELS {
            root.add(index).write(A64PTE::empty());
        }

        let mut table = root;
        let mut table_slot = 0;
        let mut level = levels - 1;

        while level > 2 {
            let next_table = root.add((table_slot + 1) * TEMPORARY_TABLE_ENTRIES);
            table
                .add(0)
                .write(A64PTE::new_table(PhysAddr::from_usize(next_table as usize)));
            table = next_table;
            table_slot += 1;
            level -= 1;
        }

        let level_two_shift = page_shift + level_bits * 2;
        let level_two_size = 1usize << level_two_shift;

        if props.page_shift == 12 {
            // A 4 KiB level-two block is exactly 1 GiB, matching the loader's
            // initial device/normal memory split.
            for index in 0..TEMPORARY_IDENTITY_SIZE / level_two_size {
                let paddr = PhysAddr::from_usize(index * level_two_size);
                let flags = if index == 0 {
                    device_flags
                } else {
                    normal_flags
                };
                table.add(index).write(A64PTE::new_page(paddr, flags, true));
            }
        } else {
            // A 16 KiB or 64 KiB level-two block is larger than the loader's
            // first 1 GiB device window. Split its first block at level one so
            // kernel code remains normal memory while MMIO remains Device.
            let next_table = root.add((table_slot + 1) * TEMPORARY_TABLE_ENTRIES);
            table
                .add(0)
                .write(A64PTE::new_table(PhysAddr::from_usize(next_table as usize)));

            let level_one_shift = page_shift + level_bits;
            let level_one_size = 1usize << level_one_shift;
            let device_entries = TEMPORARY_DEVICE_SIZE / level_one_size;
            for index in 0..entries_per_table {
                let paddr = PhysAddr::from_usize(index * level_one_size);
                let flags = if index < device_entries {
                    device_flags
                } else {
                    normal_flags
                };
                next_table
                    .add(index)
                    .write(A64PTE::new_page(paddr, flags, true));
            }

            let level_two_entries = TEMPORARY_IDENTITY_SIZE / level_two_size;
            for index in 1..level_two_entries {
                let paddr = PhysAddr::from_usize(index * level_two_size);
                table
                    .add(index)
                    .write(A64PTE::new_page(paddr, normal_flags, true));
            }
        }

        PhysAddr::from_usize(root as usize)
    }
}

/// Installs temporary roots and switches to a requested TCR geometry.
///
/// The function disables stage-one translation only while the old and new
/// translation geometries are disconnected, then restores the saved SCTLR.
pub(super) fn switch_to_temporary_page_tables(
    mode: VirtAddrSpaceMode,
    tcr: LocalRegisterCopy<u64, TCR_EL1::Register>,
) {
    let (lower, upper) = VirtAddrSpaceMode::into_dual_props(mode)
        .expect("AArch64 does not support unified virtual address spaces");

    let lower_root =
        lower.map(|props| temporary_page_table_root(addr_of_mut!(TEMPORARY_LOWER_TABLES), props));
    let upper_root =
        upper.map(|props| temporary_page_table_root(addr_of_mut!(TEMPORARY_UPPER_TABLES), props));

    MAIR_EL1.set(AARCH64_MAIR_EL1);

    // Changing a translation granule while stage-one translation is enabled
    // is not a valid live transition. The caller guarantees that the current
    // mapping is an identity mapping, so execution remains at the same
    // physical addresses during this short disabled interval.
    let saved_sctlr = SCTLR_EL1.extract();
    dsb(SY);
    SCTLR_EL1.modify(SCTLR_EL1::M::Disable);
    isb(SY);

    if let Some(root) = lower_root {
        TTBR0_EL1.set_baddr(root.as_usize() as u64);
    }
    if let Some(root) = upper_root {
        TTBR1_EL1.set_baddr(root.as_usize() as u64);
    }

    dsb(SY);
    TCR_EL1.set(tcr.get());
    dsb(SY);
    isb(SY);
    unsafe { asm!("tlbi vmalle1is", options(nostack)) };
    dsb(aarch64_cpu::asm::barrier::ISH);
    SCTLR_EL1.set(saved_sctlr.get());
    isb(SY);
}

/// Tests temporary AArch64 translation-table configuration.
///
/// These tests keep the memory-attribute contract next to the temporary-table
/// implementation.
#[cfg(test)]
mod tests {
    use super::AARCH64_MAIR_EL1;

    /// Checks the memory attributes used by temporary mappings.
    ///
    /// The values must remain compatible with the loader's device and normal
    /// memory attributes.
    #[test]
    fn configures_temporary_mapping_attributes() {
        assert_eq!(AARCH64_MAIR_EL1 & 0xff, 0x04);
        assert_eq!((AARCH64_MAIR_EL1 >> 8) & 0xff, 0xff);
    }
}
