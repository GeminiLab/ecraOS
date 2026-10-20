//! Frame number types.
//!
//! Defines semantic frame-number wrappers used to keep the code readable and understandable.

use core::ops::{Add, AddAssign, BitXor, Sub, SubAssign};

use memory_addr::{PhysAddr, VirtAddr, pa};

/// An absolute physical frame number.
///
/// Represents a frame number in the global physical address space.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct PhysFrameNumber(usize);

/// A section-local frame number.
///
/// Represents a page index inside a [`crate::section::BuddySection`] heap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct SectionFrameNumber(u32);

impl PhysFrameNumber {
    /// Creates an absolute physical frame number.
    ///
    /// # Examples
    ///
    /// ```
    /// use exbuddy::pfn::PhysFrameNumber;
    ///
    /// let pfn = PhysFrameNumber::new(0x12345);
    /// assert_eq!(pfn.as_usize(), 0x12345);
    /// ```
    #[inline]
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the raw representation of the `PhysFrameNumber` as a `usize`.
    ///
    /// # Examples
    ///
    /// ```
    /// use exbuddy::pfn::PhysFrameNumber;
    ///
    /// let pfn = PhysFrameNumber::new(0x12345);
    /// assert_eq!(pfn.as_usize(), 0x12345);
    /// ```
    #[inline]
    pub const fn as_usize(self) -> usize {
        self.0
    }

    /// Creates an absolute physical frame number from a physical address.
    ///
    /// The `page_size_shift` is the number of bits to shift the address right by to get the frame number.
    ///
    /// # Examples
    ///
    /// ```
    /// use exbuddy::pfn::PhysFrameNumber;
    /// use memory_addr::pa;
    ///
    /// let addr = pa!(0x1234_5000);
    /// let pfn = PhysFrameNumber::from_phys_addr(addr, 12);
    ///
    /// assert_eq!(pfn.as_usize(), 0x12345);
    /// ```
    #[inline]
    pub const fn from_phys_addr(addr: PhysAddr, page_size_shift: usize) -> Self {
        Self(addr.as_usize() >> page_size_shift)
    }

    /// Creates an absolute physical frame number from a virtual address.
    ///
    /// The `virt_phys_offset` is the offset between the virtual and physical address spaces, and
    /// the `page_size_shift` is the number of bits to shift the address right by to get the frame
    /// number.
    ///
    /// # Examples
    ///
    /// ```
    /// use exbuddy::pfn::PhysFrameNumber;
    /// use memory_addr::va;
    ///
    /// let addr = va!(0xffff_8000_1234_5000);
    /// let pfn = PhysFrameNumber::from_virt_addr(addr, 0xffff_8000_0000_0000, 12);
    ///
    /// assert_eq!(pfn.as_usize(), 0x12345);
    /// ```
    #[inline]
    pub const fn from_virt_addr(
        addr: VirtAddr,
        virt_phys_offset: usize,
        page_size_shift: usize,
    ) -> Self {
        Self((addr.as_usize().wrapping_sub(virt_phys_offset)) >> page_size_shift)
    }

    /// Converts this physical frame number to a physical address.
    ///
    /// Shifts the frame number back into a byte address.
    ///
    /// # Examples
    ///
    /// ```
    /// use exbuddy::pfn::PhysFrameNumber;
    /// use memory_addr::pa;
    ///
    /// let pfn = PhysFrameNumber::new(0x12345);
    /// let addr = pfn.to_phys_addr(12);
    ///
    /// assert_eq!(addr, pa!(0x1234_5000));
    /// ```
    #[inline]
    pub const fn to_phys_addr(self, page_size_shift: usize) -> PhysAddr {
        pa!(self.0 << page_size_shift)
    }

    /// Returns the maximal buddy order allowed for buddy blocks that start at this physical frame
    /// number.
    ///
    /// See [the documentation of `BuddyAllocator`](crate::BuddyAllocator) for more details about
    /// the alignment requirements for buddy blocks.
    ///
    /// The return value of this function is **NOT** clamped by the [`MAX_ORDER`](crate::MAX_ORDER).
    ///
    /// # Examples
    ///
    /// ```
    /// use exbuddy::pfn::PhysFrameNumber;
    ///
    /// let pfn = PhysFrameNumber::new(0x12345);
    /// assert_eq!(pfn.max_order(), 0);
    /// let pfn = PhysFrameNumber::new(0x2468a);
    /// assert_eq!(pfn.max_order(), 1);
    /// let pfn = PhysFrameNumber::new(0x48d14);
    /// assert_eq!(pfn.max_order(), 2);
    /// ```
    #[inline]
    pub const fn max_order(self) -> usize {
        self.0.trailing_zeros() as usize
    }

    /// Returns the physical frame number of the start page of the buddy of the block that starts at
    /// this physical frame number.
    ///
    /// # Examples
    ///
    /// ```
    /// use exbuddy::pfn::PhysFrameNumber;
    ///
    /// let pfn = PhysFrameNumber::new(0x12345);
    /// assert_eq!(pfn.buddy(0), PhysFrameNumber::new(0x12344));
    /// let pfn = PhysFrameNumber::new(0x2468a);
    /// assert_eq!(pfn.buddy(1), PhysFrameNumber::new(0x24688));
    /// let pfn = PhysFrameNumber::new(0x48d14);
    /// assert_eq!(pfn.buddy(2), PhysFrameNumber::new(0x48d10));
    /// ```
    #[inline]
    pub const fn buddy(self, order: usize) -> Self {
        Self(self.0 ^ (1usize << order))
    }
}

impl SectionFrameNumber {
    /// The maximal valid section-local frame number.
    ///
    /// [`SectionFrameNumber`] itself does not enforce this constraint.
    pub const MAX_VALID_SFN: Self = Self(SFN_NONE - 1);

    /// Creates a section-local frame number from `usize`.
    ///
    /// If the `value` is greater than `u32::MAX`, the highest bits will be truncated.
    ///
    /// # Examples
    ///
    /// ```
    /// use exbuddy::pfn::SectionFrameNumber;
    ///
    /// let sfn = SectionFrameNumber::new(0x12345);
    /// assert_eq!(sfn.as_usize(), 0x12345);
    /// assert_eq!(sfn.as_u32(), 0x12345);
    /// ```
    #[inline]
    pub const fn new(value: usize) -> Self {
        Self(value as u32)
    }

    /// Creates a section-local frame number from `u32`.
    ///
    /// # Examples
    ///
    /// ```
    /// use exbuddy::pfn::SectionFrameNumber;
    ///
    /// let sfn = SectionFrameNumber::from_u32(0x12345);
    /// assert_eq!(sfn.as_usize(), 0x12345);
    /// assert_eq!(sfn.as_u32(), 0x12345);
    /// ```
    #[inline]
    pub const fn from_u32(value: u32) -> Self {
        Self(value)
    }

    /// Returns the raw representation of the `SectionFrameNumber` as a `usize`.
    ///
    /// # Examples
    ///
    /// ```
    /// use exbuddy::pfn::SectionFrameNumber;
    ///
    /// let sfn = SectionFrameNumber::new(0x12345);
    /// assert_eq!(sfn.as_usize(), 0x12345);
    /// ```
    #[inline]
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }

    /// Returns the raw representation of the `SectionFrameNumber`.
    ///
    /// # Examples
    ///
    /// ```
    /// use exbuddy::pfn::SectionFrameNumber;
    ///
    /// let sfn = SectionFrameNumber::new(0x12345);
    /// assert_eq!(sfn.as_u32(), 0x12345);
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Returns this section-local frame number's byte offset from the heap
    /// start.
    ///
    /// # Examples
    ///
    /// ```
    /// use exbuddy::pfn::SectionFrameNumber;
    ///
    /// let sfn = SectionFrameNumber::new(0x12345);
    /// assert_eq!(sfn.byte_offset(12), 0x12345 << 12);
    /// ```
    #[inline]
    pub const fn byte_offset(self, page_size_shift: usize) -> usize {
        (self.0 as usize) << page_size_shift
    }

    /// Converts this section-local frame number to a virtual address.
    ///
    /// The `heap_start` is the virtual address of the start of the heap, and
    /// the `page_size_shift` is the number of bits to shift the frame number back by to get the
    /// byte offset.
    ///
    /// # Examples
    ///
    /// ```
    /// use exbuddy::pfn::SectionFrameNumber;
    /// use memory_addr::va;
    ///
    /// let sfn = SectionFrameNumber::new(0x12345);
    /// let addr = sfn.to_addr(va!(0xffff_8000_0000_0000), 12);
    /// assert_eq!(addr, va!(0xffff_8000_1234_5000));
    /// ```
    #[inline]
    pub fn to_addr(self, heap_start: VirtAddr, page_size_shift: usize) -> VirtAddr {
        heap_start + self.byte_offset(page_size_shift)
    }

    /// Converts this section-local frame number to an absolute physical frame number.
    ///
    /// The `heap_base_pfn` is the physical frame number of the start of the heap.
    ///
    /// # Examples
    ///
    /// ```
    /// use exbuddy::pfn::{SectionFrameNumber, PhysFrameNumber};
    ///
    /// let sfn = SectionFrameNumber::new(0x12345);
    /// let pfn = sfn.to_pfn(PhysFrameNumber::new(0x10000));
    /// assert_eq!(pfn.as_usize(), 0x12345 + 0x10000);
    /// ```
    #[inline]
    pub fn to_pfn(self, heap_base_pfn: PhysFrameNumber) -> PhysFrameNumber {
        heap_base_pfn + self.as_usize()
    }

    /// Converts an absolute physical frame number to a section-local frame number.
    ///
    /// The `heap_base_pfn` is the physical frame number of the start of the heap.
    ///
    /// # Examples
    ///
    /// ```
    /// use exbuddy::pfn::{SectionFrameNumber, PhysFrameNumber};
    ///
    /// let pfn = PhysFrameNumber::new(0x12345);
    /// let sfn = SectionFrameNumber::from_pfn(pfn, PhysFrameNumber::new(0x10000));
    /// assert_eq!(sfn.as_usize(), 0x12345 - 0x10000);
    /// ```
    #[inline]
    pub fn from_pfn(pfn: PhysFrameNumber, heap_base_pfn: PhysFrameNumber) -> Self {
        Self((pfn - heap_base_pfn) as _)
    }

    /// Returns the section-local frame number of the start page of the buddy of the block that
    /// starts at this section-local frame number.
    ///
    /// Returns `None` if the buddy is outside the section heap.
    ///
    /// # Examples
    ///
    /// ```
    /// use exbuddy::pfn::{SectionFrameNumber, PhysFrameNumber};
    ///
    /// let sfn = SectionFrameNumber::new(0x12345);
    /// let buddy = sfn.buddy(0, PhysFrameNumber::new(0x10001), 0x12347);
    /// assert_eq!(buddy, Some(SectionFrameNumber::new(0x12346)));
    /// let buddy = sfn.buddy(0, PhysFrameNumber::new(0x10001), 0x12346);
    /// assert_eq!(buddy, None);
    /// ```
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

/// Sentinel value indicating "no page" in free-list links.
const SFN_NONE: u32 = u32::MAX;

/// An optional [`SectionFrameNumber`] that fits into a `u32`.
///
/// `u32::MAX` is used as a sentinel value to indicate `None`. Therefore, a `SectionFrameNumber`
/// with value `u32::MAX` will cause undefined behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct OptionSectionFrameNumber(u32);

impl From<Option<SectionFrameNumber>> for OptionSectionFrameNumber {
    #[inline]
    fn from(value: Option<SectionFrameNumber>) -> Self {
        Self::from_option_sfn(value)
    }
}

impl From<OptionSectionFrameNumber> for Option<SectionFrameNumber> {
    #[inline]
    fn from(value: OptionSectionFrameNumber) -> Self {
        value.into_option_sfn()
    }
}

impl OptionSectionFrameNumber {
    /// The sentinel value indicating `None`.
    pub const NONE: Self = Self(SFN_NONE);

    /// Creates an `OptionSectionFrameNumber` from an `Option<SectionFrameNumber>`.
    ///
    /// # Examples
    ///
    /// ```
    /// use exbuddy::pfn::{SectionFrameNumber, OptionSectionFrameNumber};
    ///
    /// let sfn = SectionFrameNumber::new(0x12345);
    /// let osfn = OptionSectionFrameNumber::from_option_sfn(Some(sfn));
    /// assert_eq!(osfn.into_option_sfn(), Some(sfn));
    /// let osfn = OptionSectionFrameNumber::from_option_sfn(None);
    /// assert_eq!(osfn.into_option_sfn(), None);
    /// ```
    #[inline]
    pub const fn from_option_sfn(value: Option<SectionFrameNumber>) -> Self {
        match value {
            Some(pfn) => Self(pfn.as_u32()),
            None => Self::NONE,
        }
    }

    /// Converts this `OptionSectionFrameNumber` to an `Option<SectionFrameNumber>`.
    ///
    /// # Examples
    ///
    /// ```
    /// use exbuddy::pfn::{SectionFrameNumber, OptionSectionFrameNumber};
    ///
    /// let sfn = SectionFrameNumber::new(0x12345);
    /// let osfn = OptionSectionFrameNumber::from_option_sfn(Some(sfn));
    /// assert_eq!(osfn.into_option_sfn(), Some(sfn));
    /// let osfn = OptionSectionFrameNumber::from_option_sfn(None);
    /// assert_eq!(osfn.into_option_sfn(), None);
    /// ```
    #[inline]
    pub const fn into_option_sfn(self) -> Option<SectionFrameNumber> {
        if self.0 == SFN_NONE {
            None
        } else {
            Some(SectionFrameNumber::from_u32(self.0))
        }
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
