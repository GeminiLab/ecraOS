#![cfg_attr(not(test), no_std)]
#![cfg_attr(ecraos_doc, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

use core::fmt;

use memory_addr::PhysAddr;

mod arch;
pub use self::arch::*;

bitflags::bitflags! {
    /// Generic page table entry flags that indicate the corresponding mapped
    /// memory region permissions and attributes.
    #[derive(Clone, Copy, PartialEq)]
    pub struct MappingFlags: usize {
        /// The memory is readable.
        const READ          = 1 << 0;
        /// The memory is writable.
        const WRITE         = 1 << 1;
        /// The memory is executable.
        const EXECUTE       = 1 << 2;
        /// The memory is user accessible.
        const USER          = 1 << 3;
        /// The memory is device memory.
        const DEVICE        = 1 << 4;
        /// The memory is uncached.
        const UNCACHED      = 1 << 5;
    }
}

impl fmt::Debug for MappingFlags {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

/// A generic page table entry.
///
/// All architecture-specific page table entry types implement this trait.
///
/// Flag validation is architecture-specific, and conversions can discard or imply flags that
/// cannot be represented independently by the hardware.
pub trait GenericPTE: fmt::Debug + Clone + Copy + Sync + Send + Sized {
    /// The error type returned when the implementation rejects a flag combination.
    ///
    /// Implementations can use a custom error type or [`MappingFlags`]. Implementations that
    /// never reject flags can use [`core::convert::Infallible`].
    type FlagError: fmt::Debug;

    /// Creates a page table entry pointing to a page or block.
    ///
    /// Returns an architecture-specific error when the flag combination is rejected.
    fn try_new_page(
        paddr: PhysAddr,
        flags: MappingFlags,
        is_huge: bool,
    ) -> Result<Self, Self::FlagError>;
    /// Creates a page table entry pointing to a terminal page or block.
    ///
    /// # Panics
    ///
    /// Panics if the flags are invalid.
    fn new_page(paddr: PhysAddr, flags: MappingFlags, is_huge: bool) -> Self {
        Self::try_new_page(paddr, flags, is_huge).expect("invalid flags")
    }
    /// Creates a page table entry pointing to a next-level page table.
    fn new_table(paddr: PhysAddr) -> Self;

    /// Returns the physical address mapped by this entry.
    fn paddr(&self) -> PhysAddr;
    /// Returns the flags of this entry.
    fn flags(&self) -> MappingFlags;

    /// Sets the mapped physical address of the entry.
    fn set_paddr(&mut self, paddr: PhysAddr);
    /// Sets the flags of the entry.
    ///
    /// Returns an architecture-specific error when the flag combination is rejected. The entry
    /// remains unchanged on error.
    fn try_set_flags(&mut self, flags: MappingFlags, is_huge: bool) -> Result<(), Self::FlagError>;
    /// Sets the flags of the entry.
    ///
    /// # Panics
    ///
    /// Panics if the flags are invalid.
    fn set_flags(&mut self, flags: MappingFlags, is_huge: bool) {
        Self::try_set_flags(self, flags, is_huge).expect("invalid flags")
    }

    /// Returns the raw bits of this entry.
    fn bits(self) -> usize;
    /// Returns whether this entry is zero.
    fn is_unused(&self) -> bool;
    /// Returns whether this entry flag indicates present.
    fn is_present(&self) -> bool;
    /// For non-last level translation, returns whether this entry maps to a
    /// huge frame.
    fn is_huge(&self) -> bool;
    /// Sets this entry to zero.
    fn clear(&mut self);
}
