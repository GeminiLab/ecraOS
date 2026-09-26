//! Runtime type-erased page-table types.
//!
//! [`OpaquePageTableType`] captures operations for one concrete metadata and
//! entry pair, while [`OpaquePageTable`] combines those operations with a root
//! physical address. This permits selecting a page-table format at runtime while
//! retaining a semantic virtual-address type.

use expalloc_trait::{DynPageAllocator, PageAllocator};
use memory_addr::{MemoryAddr, PhysAddr};
use page_table_entry::{GenericPTE, MappingFlags};

use crate::{PageTable, PageTableMeta, PagingResult};

/// The operations for one concrete page-table type.
///
/// Each function reconstructs a temporary concrete [`PageTable`] handle around
/// a root physical address and dispatches through a runtime allocator table.
#[derive(Clone)]
struct PageTableMethods<A: MemoryAddr> {
    /// Allocates a zeroed root table.
    ///
    /// The function returns the new table's root physical address.
    pub new_alloc: fn(handler: DynPageAllocator) -> PagingResult<PhysAddr>,
    /// Maps a virtual range in an existing root table.
    pub map: fn(
        root: PhysAddr,
        handler: DynPageAllocator,
        vaddr: A,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> PagingResult,
    /// Unmaps a virtual range in an existing root table.
    pub unmap: fn(root: PhysAddr, handler: DynPageAllocator, vaddr: A, size: usize) -> PagingResult,
}

impl<A: MemoryAddr> PageTableMethods<A> {
    /// Creates a method table that rejects every operation by panicking.
    ///
    /// The sentinel supports constant initialization before a concrete
    /// page-table type is selected.
    pub const fn dummy() -> Self {
        /// Panics when an operation is attempted through a dummy method table.
        ///
        /// Its never type coerces to every result expected by the stored
        /// function pointers.
        fn panic_im_dummy() -> ! {
            panic!("dummy page table type does not support any actual operations")
        }

        Self {
            new_alloc: |_| panic_im_dummy(),
            map: |_, _, _, _, _, _| panic_im_dummy(),
            unmap: |_, _, _, _| panic_im_dummy(),
        }
    }
}

/// A type-erased descriptor for a concrete page-table type.
///
/// The descriptor stores operations specialized for a [`PageTableMeta`] and
/// [`GenericPTE`] pair. It does not store a root page table and can create any
/// number of [`OpaquePageTable`] handles using that concrete format.
#[derive(Clone)]
pub struct OpaquePageTableType<A: MemoryAddr> {
    /// The concrete page-table operations hidden by this descriptor.
    ///
    /// All operations use `A` as their semantic virtual-address type.
    methods: PageTableMethods<A>,
}

impl<A: MemoryAddr> OpaquePageTableType<A> {
    /// Creates a descriptor for a concrete page-table format.
    ///
    /// `M` supplies the table geometry and must use `A` for virtual addresses.
    /// `PTE` supplies the concrete page-table entry representation.
    pub const fn new<M, PTE: GenericPTE>() -> Self
    where
        M: PageTableMeta<VirtAddr = A>,
        [(); M::LEVELS - 1]: Sized,
    {
        Self {
            methods: PageTableMethods {
                new_alloc: |handler| {
                    PageTable::<M, PTE>::new_alloc_dyn(handler).map(|pt: PageTable<M, PTE>| pt.root)
                },
                map: |root, handler, vaddr, paddr, size, flags| {
                    let mut pt = unsafe { PageTable::<M, PTE>::new_at(root) };
                    pt.cursor().map_dyn(vaddr, paddr, size, flags, handler)
                },
                unmap: |root, handler, vaddr, size| {
                    let mut pt = unsafe { PageTable::<M, PTE>::new_at(root) };
                    pt.cursor().unmap_dyn(vaddr, size, handler)
                },
            },
        }
    }

    /// Creates a descriptor whose operations panic.
    ///
    /// This sentinel is suitable for constant initialization but must be
    /// replaced before it is used to create or mutate a page table.
    pub const fn dummy() -> Self {
        Self {
            methods: PageTableMethods::dummy(),
        }
    }

    /// Creates a type-erased page-table handle for an existing root.
    ///
    /// The handle uses the concrete format captured by this descriptor and does
    /// not inspect or initialize the referenced table.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `paddr` identifies a valid root table in this
    /// descriptor's concrete format.
    pub unsafe fn new_pagetable_at(&self, paddr: PhysAddr) -> OpaquePageTable<A> {
        OpaquePageTable {
            root: paddr,
            methods: self.methods.clone(),
        }
    }

    /// Allocates and initializes a runtime type-erased page table through `H`.
    ///
    /// The returned handle uses this descriptor's concrete format and stores the
    /// physical root address allocated by `H`.
    pub fn new_pagetable_alloc<H: PageAllocator>(&self) -> PagingResult<OpaquePageTable<A>> {
        unsafe {
            Ok(self.new_pagetable_at((self.methods.new_alloc)(DynPageAllocator::new::<H>())?))
        }
    }
}

/// A page-table handle with a runtime type-erased concrete format.
///
/// The handle stores a root physical address and the operations captured from an
/// [`OpaquePageTableType`]. Mapping operations still select their
/// [`PageAllocator`] statically.
pub struct OpaquePageTable<A: MemoryAddr> {
    /// The physical address of the root page table.
    ///
    /// Its concrete layout is described by `methods`.
    root: PhysAddr,
    /// The type-erased operations for the root's concrete table format.
    ///
    /// These functions interpret `root` consistently for all mutations.
    methods: PageTableMethods<A>,
}

impl<A: MemoryAddr> OpaquePageTable<A> {
    /// Creates a page-table handle whose operations panic.
    ///
    /// The sentinel has a zero root address and is suitable only for constant
    /// initialization before replacement with a real handle.
    pub const fn dummy() -> Self {
        Self {
            root: PhysAddr::from_usize(0),
            methods: PageTableMethods::dummy(),
        }
    }

    /// Returns the physical address of the root page table.
    ///
    /// The address is not translated or dereferenced by this accessor.
    pub fn root_paddr(&self) -> PhysAddr {
        self.root
    }

    /// Maps a virtual address range to a physical address range.
    ///
    /// The concrete table format comes from the originating descriptor. The
    /// range is rounded to base-page boundaries and existing mappings are
    /// replaced according to [`PageTableCursor::map`](crate::PageTableCursor::map).
    pub fn map<H: PageAllocator>(
        &mut self,
        vaddr: A,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> PagingResult {
        (self.methods.map)(
            self.root,
            DynPageAllocator::new::<H>(),
            vaddr,
            paddr,
            size,
            flags,
        )
    }

    /// Removes mappings from a virtual address range.
    ///
    /// The concrete table format comes from the originating descriptor. Required
    /// TLB invalidations are performed before this method returns.
    pub fn unmap<H: PageAllocator>(&mut self, vaddr: A, size: usize) -> PagingResult {
        (self.methods.unmap)(self.root, DynPageAllocator::new::<H>(), vaddr, size)
    }

    /// Creates a mutation cursor for an opaque page table.
    ///
    /// This method is not implemented because an opaque handle does not store an
    /// instance of the concrete [`PageTable`] type or know its size. Consequently,
    /// it cannot produce the reference required by a concrete cursor.
    #[cfg(false)]
    pub fn cursor(&mut self) -> ! {
        todo!()
    }
}
