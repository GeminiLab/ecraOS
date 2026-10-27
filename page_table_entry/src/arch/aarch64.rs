//! AArch64 VMSAv8-64 translation table format descriptors.

use core::fmt;

use aarch64_cpu::registers::MAIR_EL1;
use memory_addr::PhysAddr;

use crate::{GenericPTE, MappingFlags};

bitflags::bitflags! {
    /// Memory attribute fields in the VMSAv8-64 translation table format descriptors.
    #[derive(Debug)]
    pub struct DescriptorAttr: u64 {
        // Attribute fields in stage 1 VMSAv8-64 Block and Page descriptors:

        /// Whether the descriptor is valid.
        const VALID =       1 << 0;
        /// The descriptor gives the address of the next level of translation table or 4KB page.
        /// (not a 2M, 1G block)
        const NON_BLOCK =   1 << 1;
        /// Memory attributes index field.
        const ATTR_INDX =   0b111 << 2;
        /// Non-secure bit. For memory accesses from Secure state, specifies whether the output
        /// address is in Secure or Non-secure memory.
        const NS =          1 << 5;
        /// Access permission: accessable at EL0.
        const AP_EL0 =      1 << 6;
        /// Access permission: read-only.
        const AP_RO =       1 << 7;
        /// Shareability: Inner Shareable (otherwise Outer Shareable).
        const INNER =       1 << 8;
        /// Shareability: Inner or Outer Shareable (otherwise Non-shareable).
        const SHAREABLE =   1 << 9;
        /// The Access flag.
        const AF =          1 << 10;
        /// The not global bit.
        const NG =          1 << 11;
        /// Indicates that 16 adjacent translation table entries point to contiguous memory regions.
        const CONTIGUOUS =  1 <<  52;
        /// The Privileged execute-never field.
        const PXN =         1 <<  53;
        /// The Execute-never or Unprivileged execute-never field.
        const UXN =         1 <<  54;

        // Next-level attributes in stage 1 VMSAv8-64 Table descriptors:

        /// PXN limit for subsequent levels of lookup.
        const PXN_TABLE =           1 << 59;
        /// XN limit for subsequent levels of lookup.
        const XN_TABLE =            1 << 60;
        /// Access permissions limit for subsequent levels of lookup: access at EL0 not permitted.
        const AP_NO_EL0_TABLE =     1 << 61;
        /// Access permissions limit for subsequent levels of lookup: write access not permitted.
        const AP_NO_WRITE_TABLE =   1 << 62;
        /// For memory accesses from Secure state, specifies the Security state for subsequent
        /// levels of lookup.
        const NS_TABLE =            1 << 63;
    }
}

/// The memory attributes index field in the descriptor, which is used to index
/// into the MAIR (Memory Attribute Indirection Register).
#[repr(u64)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum MemAttr {
    /// Device-nGnRE memory
    Device = 0,
    /// Normal memory
    Normal = 1,
    /// Normal non-cacheable memory
    NormalNonCacheable = 2,
}

impl DescriptorAttr {
    #[allow(clippy::unusual_byte_groupings)]
    const ATTR_INDEX_MASK: u64 = 0b111_00;

    /// Constructs a descriptor from the memory index, leaving the other fields
    /// empty.
    pub const fn from_mem_attr(idx: MemAttr) -> Self {
        let mut bits = (idx as u64) << 2;
        if matches!(idx, MemAttr::Normal | MemAttr::NormalNonCacheable) {
            bits |= Self::INNER.bits() | Self::SHAREABLE.bits();
        }
        Self::from_bits_retain(bits)
    }

    /// Returns the memory attribute index field.
    pub const fn mem_attr(&self) -> Option<MemAttr> {
        let idx = (self.bits() & Self::ATTR_INDEX_MASK) >> 2;
        Some(match idx {
            0 => MemAttr::Device,
            1 => MemAttr::Normal,
            2 => MemAttr::NormalNonCacheable,
            _ => return None,
        })
    }
}

impl MemAttr {
    /// The MAIR_ELx register should be set to this value to match the memory
    /// attributes in the descriptors.
    pub const MAIR_VALUE: u64 = {
        // Device-nGnRE memory
        let attr0 = MAIR_EL1::Attr0_Device::nonGathering_nonReordering_EarlyWriteAck.value;
        // Normal memory
        let attr1 = MAIR_EL1::Attr1_Normal_Inner::WriteBack_NonTransient_ReadWriteAlloc.value
            | MAIR_EL1::Attr1_Normal_Outer::WriteBack_NonTransient_ReadWriteAlloc.value;
        let attr2 = MAIR_EL1::Attr2_Normal_Inner::NonCacheable.value
            + MAIR_EL1::Attr2_Normal_Outer::NonCacheable.value;
        attr0 | attr1 | attr2 // 0x44_ff_04
    };
}

impl From<DescriptorAttr> for MappingFlags {
    fn from(attr: DescriptorAttr) -> Self {
        if !attr.contains(DescriptorAttr::VALID) {
            return Self::empty();
        }
        let mut flags = Self::READ;
        if !attr.contains(DescriptorAttr::AP_RO) {
            flags |= Self::WRITE;
        }
        #[cfg(not(feature = "arm-el2"))]
        {
            if attr.contains(DescriptorAttr::AP_EL0) {
                flags |= Self::USER;
                if !attr.contains(DescriptorAttr::UXN) {
                    flags |= Self::EXECUTE;
                }
            } else if !attr.intersects(DescriptorAttr::PXN) {
                flags |= Self::EXECUTE;
            }
        }
        #[cfg(feature = "arm-el2")]
        {
            if !attr.intersects(DescriptorAttr::UXN) {
                flags |= Self::EXECUTE;
            }
        }
        match attr.mem_attr() {
            Some(MemAttr::Device) => flags |= Self::DEVICE,
            Some(MemAttr::NormalNonCacheable) => flags |= Self::UNCACHED,
            _ => {}
        }
        flags
    }
}

impl From<MappingFlags> for DescriptorAttr {
    fn from(flags: MappingFlags) -> Self {
        if flags.is_empty() {
            return Self::empty();
        }
        let mut attr = if flags.contains(MappingFlags::DEVICE) {
            Self::from_mem_attr(MemAttr::Device)
        } else if flags.contains(MappingFlags::UNCACHED) {
            Self::from_mem_attr(MemAttr::NormalNonCacheable)
        } else {
            Self::from_mem_attr(MemAttr::Normal)
        };
        if flags.contains(MappingFlags::READ) {
            attr |= Self::VALID;
        }
        if !flags.contains(MappingFlags::WRITE) {
            attr |= Self::AP_RO;
        }
        #[cfg(not(feature = "arm-el2"))]
        {
            if flags.contains(MappingFlags::USER) {
                attr |= Self::AP_EL0 | Self::PXN;
                if !flags.contains(MappingFlags::EXECUTE) {
                    attr |= Self::UXN;
                }
            } else {
                attr |= Self::UXN;
                if !flags.contains(MappingFlags::EXECUTE) {
                    attr |= Self::PXN;
                }
            }
        }
        #[cfg(feature = "arm-el2")]
        {
            if !flags.contains(MappingFlags::EXECUTE) {
                attr |= Self::UXN;
            }
        }
        attr
    }
}

/// A VMSAv8-64 translation table descriptor.
///
/// Note that the **AttrIndx\[2:0\]** (bit\[4:2\]) field is set to `0` for
/// device memory, and `1` for normal memory. The system must configure the
/// MAIR_ELx system register accordingly.
///
/// Leaf mappings require READ permission. The fallible methods return the original mapping flags
/// on error, and the convenience methods panic on error.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct A64PTE(u64);

impl A64PTE {
    // bits 12..48
    const PHYS_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;

    /// Creates an empty descriptor with all bits set to zero.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Converts supported leaf flags to descriptor attributes.
    ///
    /// Requires READ because this implementation cannot represent leaf permissions without it.
    fn try_descriptor_attr(flags: MappingFlags) -> Result<DescriptorAttr, MappingFlags> {
        if !flags.contains(MappingFlags::READ) {
            return Err(flags);
        }
        Ok(DescriptorAttr::from(flags))
    }
}

/// Generic paging operations with validation of AArch64 leaf permissions.
impl GenericPTE for A64PTE {
    /// The rejected mapping flags.
    type FlagError = MappingFlags;

    /// Encodes a page or block after checking that read permission is present.
    fn try_new_page(
        paddr: PhysAddr,
        flags: MappingFlags,
        is_huge: bool,
    ) -> Result<Self, Self::FlagError> {
        let mut attr = Self::try_descriptor_attr(flags)? | DescriptorAttr::AF;
        if !is_huge {
            attr |= DescriptorAttr::NON_BLOCK;
        }
        Ok(Self(
            attr.bits() | (paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK),
        ))
    }

    fn new_table(paddr: PhysAddr) -> Self {
        let attr = DescriptorAttr::NON_BLOCK | DescriptorAttr::VALID;
        Self(attr.bits() | (paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK))
    }

    fn paddr(&self) -> PhysAddr {
        PhysAddr::from((self.0 & Self::PHYS_ADDR_MASK) as usize)
    }

    fn flags(&self) -> MappingFlags {
        DescriptorAttr::from_bits_truncate(self.0).into()
    }

    fn set_paddr(&mut self, paddr: PhysAddr) {
        self.0 = (self.0 & !Self::PHYS_ADDR_MASK) | (paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK)
    }

    /// Replaces mapping attributes after validation, preserving the entry on error.
    fn try_set_flags(&mut self, flags: MappingFlags, is_huge: bool) -> Result<(), Self::FlagError> {
        let mut attr = Self::try_descriptor_attr(flags)? | DescriptorAttr::AF;
        if !is_huge {
            attr |= DescriptorAttr::NON_BLOCK;
        }
        self.0 = (self.0 & Self::PHYS_ADDR_MASK) | attr.bits();
        Ok(())
    }

    fn bits(self) -> usize {
        self.0 as usize
    }

    fn is_unused(&self) -> bool {
        self.0 == 0
    }

    fn is_present(&self) -> bool {
        DescriptorAttr::from_bits_truncate(self.0).contains(DescriptorAttr::VALID)
    }

    fn is_huge(&self) -> bool {
        !DescriptorAttr::from_bits_truncate(self.0).contains(DescriptorAttr::NON_BLOCK)
    }

    fn clear(&mut self) {
        self.0 = 0
    }
}

impl fmt::Debug for A64PTE {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut f = f.debug_struct("A64PTE");
        f.field("raw", &self.0)
            .field("paddr", &self.paddr())
            .field("attr", &DescriptorAttr::from_bits_truncate(self.0))
            .field("flags", &self.flags())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    //! AArch64 page and block permission validation tests.

    use super::*;

    /// Lists all R/W/X combinations and whether they describe a supported leaf.
    fn permission_cases() -> [(MappingFlags, bool); 8] {
        [
            (MappingFlags::empty(), false),
            (MappingFlags::READ, true),
            (MappingFlags::WRITE, false),
            (MappingFlags::READ | MappingFlags::WRITE, true),
            (MappingFlags::EXECUTE, false),
            (MappingFlags::READ | MappingFlags::EXECUTE, true),
            (MappingFlags::WRITE | MappingFlags::EXECUTE, false),
            (
                MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE,
                true,
            ),
        ]
    }

    /// Validates leaf permissions without accepting an invalid descriptor or changing it on error.
    #[test]
    fn leaf_permission_combinations() {
        let paddr = PhysAddr::from(0x8000);
        let permission_mask = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE;
        let attributes = [
            MappingFlags::empty(),
            MappingFlags::USER,
            MappingFlags::DEVICE,
            MappingFlags::UNCACHED,
        ];
        for (permissions, valid) in permission_cases() {
            for attributes in attributes {
                let flags = permissions | attributes;
                for is_huge in [false, true] {
                    let result = A64PTE::try_new_page(paddr, flags, is_huge);
                    if valid {
                        let pte = result.unwrap();
                        assert!(pte.is_present());
                        assert_eq!(pte.paddr(), paddr);
                        assert_eq!(pte.is_huge(), is_huge);
                        assert_eq!(pte.flags() & permission_mask, permissions);
                    } else {
                        assert!(result.is_err(), "accepted {flags:?}");
                        assert_eq!(result.unwrap_err(), flags);
                    }
                }
            }
        }
    }

    /// Preserves the full original entry when a flag update lacks read permission.
    #[test]
    fn flag_updates_validate_before_modifying_entry() {
        let paddr = PhysAddr::from(0x8000);
        let original_flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::UNCACHED;
        for (flags, valid) in permission_cases() {
            for is_huge in [false, true] {
                let mut pte = A64PTE::new_page(paddr, original_flags, !is_huge);
                let original_bits = pte.bits();
                let result = pte.try_set_flags(flags, is_huge);
                if valid {
                    result.unwrap();
                    assert!(pte.is_present());
                    assert_eq!(pte.paddr(), paddr);
                    assert_eq!(pte.is_huge(), is_huge);
                    assert_eq!(pte.flags(), flags);
                } else {
                    assert!(result.is_err(), "accepted {flags:?}");
                    assert_eq!(result.unwrap_err(), flags);
                    assert_eq!(pte.bits(), original_bits);
                }
            }
        }
    }

    /// Rejects execute-only mappings through the legacy constructor in every profile.
    #[test]
    #[should_panic(expected = "invalid flags")]
    fn legacy_new_page_panics_without_read_permission() {
        A64PTE::new_page(PhysAddr::from(0x8000), MappingFlags::EXECUTE, false);
    }

    /// Preserves a mapped page when the legacy flag setter rejects write-only permissions.
    #[test]
    fn legacy_set_flags_panics_without_modifying_entry() {
        let mut pte = A64PTE::new_page(PhysAddr::from(0x8000), MappingFlags::READ, false);
        let original_bits = pte.bits();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pte.set_flags(MappingFlags::WRITE, false);
        }));
        assert!(result.is_err());
        assert_eq!(pte.bits(), original_bits);
    }
}
