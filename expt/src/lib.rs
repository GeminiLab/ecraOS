#![no_std]
#![allow(incomplete_features)]
#![feature(generic_const_exprs)]
#![feature(generic_const_items)]

mod arch;
mod meta;
pub mod pte {
    pub use page_table_entry::*;
}

use core::marker::PhantomData;

use memory_addr::{AddrRange, MemoryAddr, PhysAddr, VirtAddr};

pub use meta::PageTableMeta;
use page_table_entry::{GenericPTE, MappingFlags};

macro_rules! trace {
    ($($arg:tt)*) => {
        // explat::dbcn_println!($($arg)*);
    };
}

pub struct X86Level4PageTableMeta;

impl PageTableMeta for X86Level4PageTableMeta {
    type VirtAddr = VirtAddr;

    const LEVELS: usize = 4;
    const PAGE_OFFSET_BITS: usize = 12;
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 9, 9, 9];

    // Max page size 1GiB, at level 2 of levels 0-3.
    const MAX_PAGE_LEVEL: usize = 2;
}

pub struct X86Level5PageTableMeta;

impl PageTableMeta for X86Level5PageTableMeta {
    type VirtAddr = VirtAddr;

    const LEVELS: usize = 5;
    const PAGE_OFFSET_BITS: usize = 12;
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 9, 9, 9, 9];

    // Max page size 512GiB, at level 3 of levels 0-4.
    const MAX_PAGE_LEVEL: usize = 3;
}

#[derive(Debug, thiserror::Error)]
pub enum PagingError {
    #[error("The page is not mapped")]
    NotMapped,
    #[error("The page is already mapped")]
    AlreadyMapped,
    #[error("The page is mapped to a huge page")]
    MappedToHugePage,
    #[error("Allocation failed")]
    AllocationFailed,
    #[error("The page cannot be a page at level {level}")]
    CannotBePage { level: usize },
}

pub type PagingResult<T = ()> = Result<T, PagingError>;

pub trait PagingHandler {
    fn alloc_frames(bytes_required: usize) -> Option<PhysAddr>;
    fn dealloc_frames(addr: PhysAddr, bytes_deallocated: usize);
    fn phys_to_virt(addr: PhysAddr) -> VirtAddr;
}

struct PageTableMetaAssertions<M: PageTableMeta> {
    _phantom: PhantomData<M>,
}

impl<M: PageTableMeta> PageTableMetaAssertions<M> {
    const VA_BITS_ASSERTIONS: () = assert!(M::VA_BITS <= 64, "Virtual address space must be no more than 64 bits") where [(); M::LEVELS]: Sized;
    const LEVELS_ASSERTIONS: () = assert!(
        M::LEVELS <= 5 && M::LEVELS > 0,
        "Page table level must be no more than 5 and greater than 0"
    );
    const MAX_PAGE_LEVEL_ASSERTIONS: () = assert!(
        // 0 <= M::MAX_PAGE_LEVEL && 
        M::MAX_PAGE_LEVEL < M::LEVELS,
        "`M::MAX_PAGE_LEVEL` must be greater or equal than 0 and less than `M::LEVELS`"
    ) where [(); M::LEVELS]: Sized;

    #[doc(hidden)]
    #[allow(clippy::let_unit_value)] // Make sure that the assertions are not ignored.
    pub const ASSERTIONS: () = {
        let _ = Self::VA_BITS_ASSERTIONS;
        let _ = Self::LEVELS_ASSERTIONS;
        let _ = Self::MAX_PAGE_LEVEL_ASSERTIONS;
    } where [(); M::LEVELS]: Sized;
}

pub struct PageTable<M: PageTableMeta, PTE: GenericPTE> {
    root: PhysAddr,
    _phantom: PhantomData<(PTE, M)>,
}

impl<M: PageTableMeta, PTE: GenericPTE> PageTable<M, PTE>
where
    [(); M::LEVELS]: Sized,
{
    /// Creates a new page table at the given physical address.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the physical address is valid.
    pub unsafe fn new_at(paddr: PhysAddr) -> Self {
        #[allow(clippy::let_unit_value)] // Make sure that the assertions are not ignored.
        let _ = PageTableMetaAssertions::<M>::ASSERTIONS;

        Self {
            root: paddr,
            _phantom: PhantomData,
        }
    }

    pub fn new_alloc<H: PagingHandler>() -> PagingResult<Self>
    where
        [(); M::LEVELS - 1]: Sized,
    {
        let paddr = Self::alloc_table::<{ M::LEVELS - 1 }, H>()?;
        Ok(unsafe { Self::new_at(paddr) })
    }

    pub const fn base_paddr(&self) -> PhysAddr {
        self.root
    }

    const fn table_size<const LEVEL: usize>() -> usize {
        size_of::<PTE>() << M::LEVEL_BITS[LEVEL]
    }

    const fn index<const LEVEL: usize>(vaddr: usize) -> usize {
        let (start, end) = M::LEVEL_BIT_RANGES[LEVEL];

        (vaddr >> start) & ((1 << (end - start)) - 1)
    }

    fn get_page_entry_mut<H: PagingHandler>(
        &mut self,
        vaddr: M::VirtAddr,
        level: usize,
        create_if_not_exists: bool,
        split_huge_page: bool,
    ) -> PagingResult<(&mut PTE, usize)> {
        let vaddr: usize = vaddr.into();

        if level > M::MAX_PAGE_LEVEL {
            return Err(PagingError::CannotBePage { level });
        }

        // We use 0-indexed level here, so the naming is different from other
        // kernels. PML5 in other kernels is p4 in ours, PML4 is p3, etc.
        let p0 = if M::LEVELS > 1 {
            let p1 = if M::LEVELS > 2 {
                let p2 = if M::LEVELS > 3 {
                    let p3 = if M::LEVELS > 4 {
                        let p4 = Self::table_of_mut::<4, H>(self.root);
                        let index4 = Self::index::<4>(vaddr);
                        let p4e = &mut p4[index4];

                        if level == 4 {
                            Self::clear_pte::<H>(p4e, 4)?;
                            return Ok((p4e, index4));
                        }

                        Self::next_table_mut::<3, H>(p4e, create_if_not_exists, split_huge_page)?
                    } else {
                        Self::table_of_mut::<3, H>(self.root)
                    };
                    let index3 = Self::index::<3>(vaddr);
                    let p3e = &mut p3[index3];

                    if level == 3 {
                        Self::clear_pte::<H>(p3e, 3)?;
                        return Ok((p3e, index3));
                    }

                    Self::next_table_mut::<2, H>(p3e, create_if_not_exists, split_huge_page)?
                } else {
                    Self::table_of_mut::<2, H>(self.root)
                };
                let index2 = Self::index::<2>(vaddr);
                let p2e = &mut p2[index2];

                if level == 2 {
                    Self::clear_pte::<H>(p2e, 2)?;
                    return Ok((p2e, index2));
                }

                Self::next_table_mut::<1, H>(p2e, create_if_not_exists, split_huge_page)?
            } else {
                Self::table_of_mut::<1, H>(self.root)
            };
            let index1 = Self::index::<1>(vaddr);
            let p1e = &mut p1[index1];

            if level == 1 {
                Self::clear_pte::<H>(p1e, 1)?;
                return Ok((p1e, index1));
            }

            Self::next_table_mut::<0, H>(p1e, create_if_not_exists, split_huge_page)?
        } else {
            Self::table_of_mut::<0, H>(self.root)
        };
        let index0 = Self::index::<0>(vaddr);
        let p0e = &mut p0[index0];

        Self::clear_pte::<H>(p0e, 0)?;
        Ok((p0e, index0))
    }

    /// Gets the table at level `LEVEL` from its physical address `paddr`.
    fn table_of_mut<'a, const LEVEL: usize, H: PagingHandler>(paddr: PhysAddr) -> &'a mut [PTE] {
        let entry_count = M::LEVEL_TABLE_SIZE[LEVEL];

        unsafe {
            let ptr: *mut PTE = H::phys_to_virt(paddr).as_mut_ptr_of();
            core::slice::from_raw_parts_mut(ptr, entry_count)
        }
    }

    /// Gets the table at level `level` from its physical address `paddr`.
    ///
    /// Basically the same as [`table_of_mut`], but the level is not a constant.
    fn table_of_mut_non_const<'a, H: PagingHandler>(
        paddr: PhysAddr,
        level: usize,
    ) -> &'a mut [PTE] {
        let entry_count = M::LEVEL_TABLE_SIZE[level];

        unsafe {
            let ptr: *mut PTE = H::phys_to_virt(paddr).as_mut_ptr_of();
            core::slice::from_raw_parts_mut(ptr, entry_count)
        }
    }

    /// Gets the table at level `LEVEL` from a PTE of the level above.
    fn next_table_mut<'a, const LEVEL: usize, H: PagingHandler>(
        entry: &mut PTE,
        create_if_not_exists: bool,
        split_huge_page: bool,
    ) -> PagingResult<&'a mut [PTE]> {
        if entry.is_unused() {
            if create_if_not_exists {
                let table = Self::alloc_table::<LEVEL, H>()?;
                *entry = GenericPTE::new_table(table);
                Ok(Self::table_of_mut::<LEVEL, H>(table))
            } else {
                Err(PagingError::NotMapped)
            }
        } else if entry.is_huge() {
            if split_huge_page {
                let paddr = entry.paddr();
                let flags = entry.flags();

                let table = Self::alloc_table::<LEVEL, H>()?;
                *entry = GenericPTE::new_table(table);

                let table = Self::table_of_mut::<LEVEL, H>(paddr);

                for (i, entry) in table.iter_mut().enumerate() {
                    *entry =
                        PTE::new_page(paddr + i * M::LEVEL_PAGE_SIZE[LEVEL], flags, LEVEL != 0);
                }

                Ok(table)
            } else {
                Err(PagingError::MappedToHugePage)
            }
        } else {
            Ok(Self::table_of_mut::<LEVEL, H>(entry.paddr()))
        }
    }

    fn alloc_table<const LEVEL: usize, H: PagingHandler>() -> PagingResult<PhysAddr> {
        let bytes_required = Self::table_size::<LEVEL>();

        if let Some(paddr) = H::alloc_frames(bytes_required) {
            let vaddr = H::phys_to_virt(paddr);
            unsafe {
                core::ptr::write_bytes(vaddr.as_mut_ptr(), 0, bytes_required);
            }
            Ok(paddr)
        } else {
            Err(PagingError::AllocationFailed)
        }
    }

    fn clear_pte<H: PagingHandler>(entry: &mut PTE, level: usize) -> PagingResult {
        if entry.is_unused() {
            // It's already cleared.
            Ok(())
        } else if level == 0 || entry.is_huge() {
            // It points to a page.
            entry.clear();
            Ok(())
        } else {
            // It points to a table, clear it recursively.
            let table = Self::table_of_mut_non_const::<H>(entry.paddr(), level - 1);

            for entry in table.iter_mut() {
                Self::clear_pte::<H>(entry, level - 1)?;
            }

            Ok(())
        }
    }

    /// Split the given range into pages (as large and less as possible), remove
    /// all current mappings on the range, and call the given function for PTEs
    /// of each page.
    ///
    /// The parameters of the function are:
    /// - The level of the PTE: `0` for the lowest level, `M::LEVELS - 1` for
    ///   the highest level,
    /// - The index of the PTE in the table of the level,
    /// - The start virtual address of the page, and
    /// - The mutable reference to the PTE of the page.
    ///
    /// Before calling the function, the PTE is [cleared](GenericPTE::clear). If
    /// the PTE points to a table, the table is freed recursively. If the range
    /// overlaps with a huge page, the page is split into smaller pages.
    ///
    /// The range is aligned to the page size of the lowest level.
    fn iter_pages_in_range<F, H: PagingHandler>(
        &mut self,
        range: AddrRange<M::VirtAddr>,
        mut f: F,
    ) -> PagingResult
    where
        F: FnMut(usize, usize, M::VirtAddr, &mut PTE) -> PagingResult,
    {
        let mut start_vaddr = range.start.align_down(M::LEVEL_PAGE_SIZE[0]);
        let end_vaddr = range.end.align_up(M::LEVEL_PAGE_SIZE[0]);

        while start_vaddr < end_vaddr {
            for level in (0..=M::MAX_PAGE_LEVEL).rev() {
                let page_size = M::LEVEL_PAGE_SIZE[level];
                if start_vaddr.is_aligned(page_size) && (start_vaddr + page_size) <= end_vaddr {
                    let (entry, index) =
                        self.get_page_entry_mut::<H>(start_vaddr, level, true, true)?;
                    f(level, index, start_vaddr, entry)?;
                    start_vaddr = start_vaddr + page_size;
                    break;
                }
            }
        }

        Ok(())
    }

    pub fn map<H: PagingHandler>(
        &mut self,
        vaddr: M::VirtAddr,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> PagingResult {
        trace!("Mapping {:#x} to {:#x}, size {:#x}", vaddr, paddr, size);
        let offset = usize::wrapping_sub(vaddr.into(), paddr.into());
        self.iter_pages_in_range::<_, H>(
            AddrRange::new(vaddr, vaddr + size),
            |level, _index, page_vaddr, entry| {
                let page_paddr = PhysAddr::from_usize(page_vaddr.wrapping_sub(offset).into());
                *entry = GenericPTE::new_page(page_paddr, flags, level != 0);
                Ok(())
            },
        )
    }

    pub fn unmap<H: PagingHandler>(&mut self, vaddr: M::VirtAddr, size: usize) -> PagingResult {
        self.iter_pages_in_range::<_, H>(
            AddrRange::new(vaddr, vaddr + size),
            |_level, _index, _page_vaddr, _entry| Ok(()),
        )
    }
}
