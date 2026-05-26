//! Page frame number types.
//!
//! Defines semantic page-frame-number wrappers used to keep the code readable
//! and understandable.

use core::ops::{Add, AddAssign, BitXor, Sub, SubAssign};

use memory_addr::{PhysAddr, VirtAddr, pa};

/// An absolute physical page frame number.
///
/// Represents a physical frame number in the global physical address space.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct PhysFrameNumber(usize);

/// A section-local page frame number.
///
/// Represents a page index inside a [`crate::section::BuddySection`] heap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct SectionFrameNumber(u32);

impl PhysFrameNumber {
    /// Creates an absolute physical page frame number.
    ///
    /// Wraps a raw page-frame index in the global physical address space.
    #[inline]
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the raw page-frame index.
    ///
    /// Exposes the numeric value for arithmetic at API boundaries.
    #[inline]
    pub const fn as_usize(self) -> usize {
        self.0
    }

    /// Creates an absolute physical page frame number from a physical address.
    ///
    /// Converts the address by shifting out the page offset bits.
    #[inline]
    pub const fn from_phys_addr(addr: PhysAddr, page_size_shift: usize) -> Self {
        Self(addr.as_usize() >> page_size_shift)
    }

    /// Creates an absolute physical page frame number from a virtual address.
    ///
    /// Removes the fixed virtual-physical offset before shifting out the page
    /// offset bits.
    #[inline]
    pub const fn from_virt_addr(
        addr: VirtAddr,
        virt_phys_offset: usize,
        page_size_shift: usize,
    ) -> Self {
        Self((addr.as_usize().wrapping_sub(virt_phys_offset)) >> page_size_shift)
    }

    /// Converts this page frame number to a physical address.
    ///
    /// Shifts the page-frame index back into a byte address.
    #[inline]
    pub const fn to_phys_addr(self, page_size_shift: usize) -> PhysAddr {
        pa!(self.0 << page_size_shift)
    }

    /// Returns the maximum buddy order allowed by this physical PFN alignment.
    ///
    /// Computes the number of trailing zero bits in the absolute PFN.
    #[inline]
    pub const fn max_order(self) -> usize {
        self.0.trailing_zeros() as usize
    }

    /// Returns this PFN's physical buddy at the given order.
    ///
    /// Flips the absolute PFN bit corresponding to a block of `2^order` pages.
    #[inline]
    pub const fn buddy(self, order: usize) -> Self {
        Self(self.0 ^ (1usize << order))
    }
}

impl SectionFrameNumber {
    /// Creates a section-local page frame number.
    ///
    /// Wraps a raw page index inside a section heap.
    #[inline]
    pub const fn new(value: usize) -> Self {
        Self(value as u32)
    }

    /// Creates a section-local page frame number from free-list storage.
    ///
    /// Converts the compact `u32` representation used by page metadata.
    #[inline]
    pub const fn from_u32(value: u32) -> Self {
        Self(value)
    }

    /// Returns the raw section-local page index.
    ///
    /// Exposes the numeric value for metadata indexing.
    #[inline]
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }

    /// Returns the compact free-list representation.
    ///
    /// Converts this section-local PFN to the storage type used by page metadata.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Returns this section-local PFN's byte offset from the heap start.
    ///
    /// Shifts the page index into a byte offset.
    #[inline]
    pub const fn byte_offset(self, page_size_shift: usize) -> usize {
        (self.0 as usize) << page_size_shift
    }

    /// Converts this section-local PFN to a virtual address.
    ///
    /// Adds the page offset represented by this PFN to the section heap start.
    #[inline]
    pub fn to_addr(self, heap_start: VirtAddr, page_size_shift: usize) -> VirtAddr {
        heap_start + self.byte_offset(page_size_shift)
    }

    /// Returns this section-local PFN's physical buddy.
    ///
    /// Converts this section-local PFN to an absolute physical PFN using
    /// `heap_base_pfn`, computes the physical buddy at `order`, then converts the
    /// result back to a section-local PFN. Returns `None` if the physical buddy is
    /// outside the section heap.
    #[inline]
    pub fn buddy(
        self,
        order: usize,
        heap_base_pfn: PhysFrameNumber,
        heap_pages: usize,
    ) -> Option<Self> {
        let abs_pfn = heap_base_pfn + self.as_usize();
        let buddy_abs_pfn = abs_pfn.buddy(order);
        if buddy_abs_pfn < heap_base_pfn {
            return None;
        }
        let buddy_offset = buddy_abs_pfn - heap_base_pfn;
        if buddy_offset >= heap_pages {
            return None;
        }
        Some(Self::new(buddy_offset))
    }
}

impl Add<usize> for PhysFrameNumber {
    type Output = Self;

    fn add(self, rhs: usize) -> Self::Output {
        Self(self.0 + rhs)
    }
}

impl Sub<usize> for PhysFrameNumber {
    type Output = Self;

    fn sub(self, rhs: usize) -> Self::Output {
        Self(self.0 - rhs)
    }
}

impl Sub<PhysFrameNumber> for PhysFrameNumber {
    type Output = usize;

    fn sub(self, rhs: PhysFrameNumber) -> Self::Output {
        self.0 - rhs.0
    }
}

impl AddAssign<usize> for PhysFrameNumber {
    fn add_assign(&mut self, rhs: usize) {
        self.0 += rhs;
    }
}

impl SubAssign<usize> for PhysFrameNumber {
    fn sub_assign(&mut self, rhs: usize) {
        self.0 -= rhs;
    }
}

impl Add<usize> for SectionFrameNumber {
    type Output = Self;

    fn add(self, rhs: usize) -> Self::Output {
        Self(self.0 + rhs as u32)
    }
}

impl Sub<usize> for SectionFrameNumber {
    type Output = Self;

    fn sub(self, rhs: usize) -> Self::Output {
        Self(self.0 - rhs as u32)
    }
}

impl Sub<SectionFrameNumber> for SectionFrameNumber {
    type Output = usize;

    fn sub(self, rhs: SectionFrameNumber) -> Self::Output {
        (self.0 - rhs.0) as usize
    }
}

impl BitXor<usize> for SectionFrameNumber {
    type Output = Self;

    fn bitxor(self, rhs: usize) -> Self::Output {
        Self(self.0 ^ rhs as u32)
    }
}

impl AddAssign<usize> for SectionFrameNumber {
    fn add_assign(&mut self, rhs: usize) {
        self.0 += rhs as u32;
    }
}

impl SubAssign<usize> for SectionFrameNumber {
    fn sub_assign(&mut self, rhs: usize) {
        self.0 -= rhs as u32;
    }
}

#[cfg(test)]
mod tests {
    use memory_addr::{pa, va};

    use super::*;

    #[test]
    fn phys_pfn_from_phys_addr_uses_shift() {
        let pfn = PhysFrameNumber::from_phys_addr(pa!(0x1234_5000), 12);

        assert_eq!(pfn.as_usize(), 0x12345);
    }

    #[test]
    fn phys_pfn_from_virt_addr_removes_offset() {
        let pfn =
            PhysFrameNumber::from_virt_addr(va!(0xffff_8000_0012_3000), 0xffff_8000_0000_0000, 12);

        assert_eq!(pfn.as_usize(), 0x123);
    }

    #[test]
    fn section_pfn_arithmetic_keeps_section_semantics() {
        let start = SectionFrameNumber::new(10);
        let end = start + 4;

        assert_eq!(end.as_usize(), 14);
        assert_eq!(end - start, 4);
    }

    #[test]
    fn section_pfn_has_u32_storage_size() {
        assert_eq!(
            core::mem::size_of::<SectionFrameNumber>(),
            core::mem::size_of::<u32>()
        );
    }

    #[test]
    fn section_pfn_buddy_uses_absolute_heap_base() {
        let base = PhysFrameNumber::new(5);

        assert_eq!(
            SectionFrameNumber::new(1).buddy(0, base, 8),
            Some(SectionFrameNumber::new(2))
        );
    }

    #[test]
    fn section_pfn_buddy_returns_none_outside_section_prefix() {
        let base = PhysFrameNumber::new(5);

        assert_eq!(SectionFrameNumber::new(0).buddy(2, base, 8), None);
    }

    #[test]
    fn section_pfn_buddy_returns_none_outside_heap_pages() {
        let base = PhysFrameNumber::new(5);

        assert_eq!(SectionFrameNumber::new(1).buddy(0, base, 2), None);
    }

    #[test]
    fn phys_pfn_subtraction_returns_distance() {
        let high = PhysFrameNumber::new(100);
        let low = PhysFrameNumber::new(64);

        assert_eq!(high - low, 36);
    }
}
