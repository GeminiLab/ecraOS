//! RISC-V page table entries.

use core::fmt;

use memory_addr::PhysAddr;

use crate::{GenericPTE, MappingFlags};

bitflags::bitflags! {
    // RISC-V Spec ver. 20260120, Vol. II, Sect. 11.1.3.1, Table 6
    /// Page-table entry flags.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct PTEFlags: usize {
        /// Whether the PTE is valid.
        const V =   1 << 0;
        /// Whether the page is readable.
        const R =   1 << 1;
        /// Whether the page is writable.
        const W =   1 << 2;
        /// Whether the page is executable.
        const X =   1 << 3;
        /// Whether the page is accessible to user mode.
        const U =   1 << 4;
        /// Designates a global mapping.
        const G =   1 << 5;
        /// Indicates the virtual page has been read, written, or fetched from
        /// since the last time the A bit was cleared.
        const A =   1 << 6;
        /// Indicates the virtual page has been written since the last time the
        /// D bit was cleared.
        const D =   1 << 7;
        // XuanTie memory attribute fields
        // C906 User Manual section 3.7 and OpenC910 User Manual section 6.1 describe these fields.
        // SO, C, B are shared by C906 and OpenC910. SH is specific to OpenC910.
        /// SO – Strong ordered memory (1 << 63)
        #[cfg(feature = "xuantie-c9xx")]
        const SO =  (1 << 63);
        /// C – Cacheable  (1 << 62)
        #[cfg(feature = "xuantie-c9xx")]
        const C =   (1 << 62);
        /// B – Bufferable (1 << 61)
        #[cfg(feature = "xuantie-c9xx")]
        const B =   (1 << 61);
        /// SH – Shareable  (1 << 60)
        #[cfg(feature = "xuantie-openc910")]
        const SH =  (1 << 60);

        /// XuanTie device-memory attributes.
        #[cfg(feature = "xuantie-c9xx")]
        const XUANTIE_C9XX_DEVICE = Self::SO.bits() | Self::B.bits();
        /// XuanTie cacheable normal-memory attributes.
        #[cfg(feature = "xuantie-c9xx")]
        const XUANTIE_C9XX_NORMAL = Self::C.bits() | Self::B.bits();
        /// XuanTie non-cacheable normal-memory attributes.
        #[cfg(feature = "xuantie-c9xx")]
        const XUANTIE_C9XX_UNCACHED = Self::B.bits();
    }
}

impl From<PTEFlags> for MappingFlags {
    fn from(f: PTEFlags) -> Self {
        let mut ret = Self::empty();
        if !f.contains(PTEFlags::V) {
            return ret;
        }
        if f.contains(PTEFlags::R) {
            ret |= Self::READ;
        }
        if f.contains(PTEFlags::W) {
            ret |= Self::WRITE;
        }
        if f.contains(PTEFlags::X) {
            ret |= Self::EXECUTE;
        }
        if f.contains(PTEFlags::U) {
            ret |= Self::USER;
        }
        #[cfg(feature = "xuantie-c9xx")]
        if f.intersects(PTEFlags::R | PTEFlags::W | PTEFlags::X) {
            if f.contains(PTEFlags::SO) && !f.contains(PTEFlags::C) {
                ret |= Self::DEVICE;
            } else if !f.contains(PTEFlags::SO)
                && !f.contains(PTEFlags::C)
                && f.contains(PTEFlags::B)
            {
                ret |= Self::UNCACHED;
            }
        }
        ret
    }
}

impl From<MappingFlags> for PTEFlags {
    fn from(f: MappingFlags) -> Self {
        if f.is_empty() {
            return Self::empty();
        }
        let mut ret = Self::V | Self::A | Self::D;
        if f.contains(MappingFlags::READ) {
            ret |= Self::R;
        }
        if f.contains(MappingFlags::WRITE) {
            ret |= Self::W;
        }
        if f.contains(MappingFlags::EXECUTE) {
            ret |= Self::X;
        }
        if f.contains(MappingFlags::USER) {
            ret |= Self::U;
        }

        #[cfg(feature = "xuantie-c9xx")]
        if f.contains(MappingFlags::DEVICE) {
            ret |= Self::XUANTIE_C9XX_DEVICE;
        } else if f.contains(MappingFlags::UNCACHED) {
            ret |= Self::XUANTIE_C9XX_UNCACHED;
        } else {
            ret |= Self::XUANTIE_C9XX_NORMAL;
        }
        #[cfg(feature = "xuantie-openc910")]
        {
            ret |= Self::SH;
        }

        ret
    }
}

/// An Sv39, Sv48, or Sv57 page table entry for RV64 systems.
///
/// Leaf entries require READ or EXECUTE. WRITE additionally requires READ. The fallible methods
/// return the original mapping flags on error, and the convenience methods panic on error.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Rv64PTE(u64);

impl Rv64PTE {
    // bits 10..54
    const PHYS_ADDR_MASK: u64 = (1 << 54) - (1 << 10);

    /// Creates an empty descriptor with all bits set to zero.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Converts generic mapping flags to a valid RISC-V leaf flag set.
    ///
    /// Rejects entries without leaf permissions and write-only mappings, which the PTE format
    /// cannot represent.
    fn try_pte_flags(flags: MappingFlags) -> Result<PTEFlags, MappingFlags> {
        let pte_flags = PTEFlags::from(flags);
        if !pte_flags.intersects(PTEFlags::R | PTEFlags::W | PTEFlags::X)
            || (pte_flags.contains(PTEFlags::W) && !pte_flags.contains(PTEFlags::R))
        {
            Err(flags)
        } else {
            Ok(pte_flags)
        }
    }
}

/// Generic paging operations with validation of RISC-V leaf permissions.
impl GenericPTE for Rv64PTE {
    /// The rejected mapping flags.
    type FlagError = MappingFlags;

    /// Creates a leaf after checking the RISC-V permission encoding.
    fn try_new_page(
        paddr: PhysAddr,
        mflags: MappingFlags,
        _is_huge: bool,
    ) -> Result<Self, Self::FlagError> {
        let flags = Self::try_pte_flags(mflags)?;
        Ok(Self(
            flags.bits() as u64 | ((paddr.as_usize() >> 2) as u64 & Self::PHYS_ADDR_MASK),
        ))
    }

    fn new_table(paddr: PhysAddr) -> Self {
        Self(PTEFlags::V.bits() as u64 | ((paddr.as_usize() >> 2) as u64 & Self::PHYS_ADDR_MASK))
    }

    fn paddr(&self) -> PhysAddr {
        PhysAddr::from(((self.0 & Self::PHYS_ADDR_MASK) << 2) as usize)
    }

    fn flags(&self) -> MappingFlags {
        PTEFlags::from_bits_truncate(self.0 as usize).into()
    }

    fn set_paddr(&mut self, paddr: PhysAddr) {
        self.0 = (self.0 & !Self::PHYS_ADDR_MASK)
            | ((paddr.as_usize() as u64 >> 2) & Self::PHYS_ADDR_MASK);
    }

    /// Sets leaf flags after validation, preserving the entry when validation fails.
    fn try_set_flags(
        &mut self,
        flags: MappingFlags,
        _is_huge: bool,
    ) -> Result<(), Self::FlagError> {
        let flags = Self::try_pte_flags(flags)?;
        self.0 = (self.0 & Self::PHYS_ADDR_MASK) | flags.bits() as u64;
        Ok(())
    }

    fn bits(self) -> usize {
        self.0 as usize
    }

    fn is_unused(&self) -> bool {
        self.0 == 0
    }

    fn is_present(&self) -> bool {
        PTEFlags::from_bits_truncate(self.0 as usize).contains(PTEFlags::V)
    }

    fn is_huge(&self) -> bool {
        // Any of R, W, X being set indicates a huge page.
        PTEFlags::from_bits_truncate(self.0 as usize)
            .intersects(PTEFlags::R | PTEFlags::W | PTEFlags::X)
    }

    fn clear(&mut self) {
        self.0 = 0
    }
}

impl fmt::Debug for Rv64PTE {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut f = f.debug_struct("Rv64PTE");
        f.field("raw", &self.0)
            .field("paddr", &self.paddr())
            .field("flags", &self.flags())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    //! RISC-V page-table entry behavior tests.

    use super::*;

    /// Returns an aligned synthetic physical address for a PTE test.
    fn test_paddr() -> PhysAddr {
        PhysAddr::from(0x8000)
    }

    /// Lists all R/W/X combinations and their valid low-byte leaf encodings.
    fn permission_cases() -> [(MappingFlags, Option<usize>); 8] {
        [
            (MappingFlags::empty(), None),
            (MappingFlags::READ, Some(0xc3)),
            (MappingFlags::WRITE, None),
            (MappingFlags::READ | MappingFlags::WRITE, Some(0xc7)),
            (MappingFlags::EXECUTE, Some(0xc9)),
            (MappingFlags::READ | MappingFlags::EXECUTE, Some(0xcb)),
            (MappingFlags::WRITE | MappingFlags::EXECUTE, None),
            (
                MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE,
                Some(0xcf),
            ),
        ]
    }

    /// Checks every leaf permission encoding for base and huge pages.
    #[test]
    fn leaf_permission_combinations() {
        for (flags, expected_bits) in permission_cases() {
            for is_huge in [false, true] {
                let result = Rv64PTE::try_new_page(test_paddr(), flags, is_huge);
                match expected_bits {
                    Some(bits) => {
                        let pte = result.unwrap();
                        assert_eq!(pte.bits() & 0xff, bits);
                        assert_eq!(pte.paddr(), test_paddr());
                        assert_eq!(
                            pte.flags()
                                & (MappingFlags::READ
                                    | MappingFlags::WRITE
                                    | MappingFlags::EXECUTE),
                            flags
                        );
                    }
                    None => assert!(matches!(result, Err(error) if error == flags)),
                }

                let mut pte = Rv64PTE::new_page(test_paddr(), MappingFlags::READ, false);
                let original_bits = pte.bits();
                let result = pte.try_set_flags(flags, is_huge);
                match expected_bits {
                    Some(bits) => {
                        result.unwrap();
                        assert_eq!(pte.bits() & 0xff, bits);
                        assert_eq!(pte.paddr(), test_paddr());
                    }
                    None => {
                        assert_eq!(result, Err(flags));
                        assert_eq!(pte.bits(), original_bits);
                    }
                }
            }
        }
    }

    /// Rejects an invalid writable leaf through the legacy constructor in every profile.
    #[test]
    #[should_panic(expected = "invalid flags")]
    fn legacy_new_page_panics_on_invalid_permissions() {
        Rv64PTE::new_page(
            test_paddr(),
            MappingFlags::WRITE | MappingFlags::EXECUTE,
            false,
        );
    }

    /// Preserves the entry when the legacy flag setter panics.
    #[test]
    fn legacy_set_flags_panics_without_modifying_entry() {
        let mut pte = Rv64PTE::new_page(test_paddr(), MappingFlags::READ, false);
        let original_bits = pte.bits();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pte.set_flags(MappingFlags::WRITE | MappingFlags::EXECUTE, false);
        }));
        assert!(result.is_err());
        assert_eq!(pte.bits(), original_bits);
    }

    /// Encodes and decodes the three supported XuanTie memory types.
    #[cfg(feature = "xuantie-c9xx")]
    #[test]
    fn xuantie_memory_attributes_round_trip() {
        let permissions = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
        let shareable = if cfg!(feature = "xuantie-openc910") {
            0x1000_0000_0000_0000
        } else {
            0
        };
        let cases = [
            (
                MappingFlags::empty(),
                0x6000_0000_0000_0000,
                MappingFlags::empty(),
            ),
            (
                MappingFlags::UNCACHED,
                0x2000_0000_0000_0000,
                MappingFlags::UNCACHED,
            ),
            (
                MappingFlags::DEVICE,
                0xa000_0000_0000_0000,
                MappingFlags::DEVICE,
            ),
            (
                MappingFlags::DEVICE | MappingFlags::UNCACHED,
                0xa000_0000_0000_0000,
                MappingFlags::DEVICE,
            ),
        ];

        let mut pte = Rv64PTE::new_page(test_paddr(), permissions, false);
        for (attributes, expected_bits, decoded_attributes) in cases {
            let flags = permissions | attributes;
            let new_pte = Rv64PTE::try_new_page(test_paddr(), flags, false).unwrap();
            pte.try_set_flags(flags, false).unwrap();
            for entry in [new_pte, pte] {
                assert_eq!(entry.bits() as u64, shareable | expected_bits | 0x20d7);
                assert_eq!(entry.flags(), permissions | decoded_attributes);
                assert_eq!(entry.paddr(), test_paddr());
            }
        }
    }

    /// Decodes non-bufferable devices and ignores attributes on invalid and non-leaf entries.
    #[cfg(feature = "xuantie-c9xx")]
    #[test]
    fn xuantie_raw_attributes() {
        assert_eq!(
            Rv64PTE(0x8000_0000_0000_0003).flags(),
            MappingFlags::READ | MappingFlags::DEVICE
        );
        assert_eq!(
            Rv64PTE(0xa000_0000_0000_0000).flags(),
            MappingFlags::empty()
        );
        assert_eq!(
            Rv64PTE(0xa000_0000_0000_0001).flags(),
            MappingFlags::empty()
        );
        assert_eq!(
            Rv64PTE::new_table(test_paddr()).flags(),
            MappingFlags::empty()
        );
    }
}
