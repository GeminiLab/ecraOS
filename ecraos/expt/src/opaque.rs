//! Runtime type-erased page-table types.
//!
//! [`OpaquePageTableType`] captures operations for one concrete metadata and
//! entry pair, while [`OpaquePageTable`] combines those operations with an
//! erased page-table root. This permits selecting a page-table format at
//! runtime while retaining a semantic virtual-address type.

use expalloc_trait::{DynPageAllocator, PageAllocator};
use memory_addr::{PhysAddr, pa};
use page_table_entry::{GenericPTE, MappingFlags};

#[cfg(target_arch = "aarch64")]
use crate::meta::{LowerCoverage, UpperCoverage};
use crate::{
    error::PagingResult,
    meta::{PageTableMeta, VirtAddr},
    pt::{PageTable, PageTableCursorLike},
};

#[cfg(target_arch = "aarch64")]
use crate::pt::DualPageTableRoot;

/// The actions that can be performed on a page table.
pub enum PageTableAction<A: VirtAddr> {
    /// Maps a virtual address range to a physical address range.
    Map {
        /// The start of the virtual address range.
        vaddr: A,
        /// The start of the physical address range.
        paddr: PhysAddr,
        /// The size of the ranges in bytes.
        size: usize,
        /// The flags to install in each mapping.
        flags: MappingFlags,
    },
    /// Removes mappings from a virtual address range.
    Unmap {
        /// The start of the virtual address range.
        vaddr: A,
        /// The size of the range in bytes.
        size: usize,
    },
}

/// The root of an opaque page table.
///
/// `Single` is for single-root page tables. `Dual` is for dual-root page
/// tables and is only available on aarch64.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpaquePageTableRoot {
    /// A single physical root.
    Single(PhysAddr),
    /// Independent lower and upper physical roots used by AArch64.
    #[cfg(target_arch = "aarch64")]
    Dual(DualPageTableRoot),
}

/// The operations for one concrete page-table type.
///
/// Each function reconstructs a temporary concrete page-table handle around a
/// root and dispatches through a runtime allocator table.
#[derive(Clone)]
struct PageTableMethods<A: VirtAddr> {
    /// Allocates a zeroed root table.
    ///
    /// The function returns the new table's erased root.
    pub new_alloc: fn(handler: &DynPageAllocator) -> PagingResult<A, OpaquePageTableRoot>,
    /// Applies a sequence of actions to an existing root table.
    pub apply_actions: for<'a> fn(
        root: OpaquePageTableRoot,
        handler: &DynPageAllocator,
        actions: &'a mut dyn Iterator<Item = PageTableAction<A>>,
    ) -> PagingResult<A>,
}

impl<A: VirtAddr> PageTableMethods<A> {
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
            apply_actions: |_, _, _| panic_im_dummy(),
        }
    }
    /// Creates a method table for a concrete page-table format.
    ///
    /// `M` supplies the table geometry and must use `A` for virtual addresses.
    /// `PTE` supplies the concrete page-table entry representation.
    pub const fn new<M, PTE>() -> Self
    where
        M: PageTableMeta<VirtAddr = A>,
        PTE: GenericPTE,
        [(); M::LEVELS - 1]: Sized,
    {
        Self {
            new_alloc: |handler| {
                PageTable::<M, PTE>::new_alloc_dyn(handler)
                    .map(|pt: PageTable<M, PTE>| OpaquePageTableRoot::Single(pt.root()))
            },
            apply_actions: |root, handler, actions| {
                let root = match root {
                    OpaquePageTableRoot::Single(root) => root,
                    #[cfg(target_arch = "aarch64")]
                    OpaquePageTableRoot::Dual(_) => {
                        panic!("passing dual-root page table to single-root page table operation")
                    }
                };
                let mut pt = unsafe { PageTable::<M, PTE>::new_at(root) };
                let mut cursor = pt.cursor();
                for action in actions {
                    match action {
                        PageTableAction::Map {
                            vaddr,
                            paddr,
                            size,
                            flags,
                        } => {
                            cursor.map_dyn(vaddr, paddr, size, flags, handler)?;
                        }
                        PageTableAction::Unmap { vaddr, size } => {
                            cursor.unmap_dyn(vaddr, size, handler)?;
                        }
                    }
                }

                Ok(())
            },
        }
    }

    /// Creates a method table for a dual-root page-table format.
    #[cfg(target_arch = "aarch64")]
    pub const fn new_dual<ML, PTEL, MU, PTEU>() -> Self
    where
        ML: PageTableMeta<VirtAddr = A, Coverage = LowerCoverage<A>>,
        PTEL: GenericPTE,
        MU: PageTableMeta<VirtAddr = A, Coverage = UpperCoverage<A>>,
        PTEU: GenericPTE,
        [(); ML::LEVELS - 1]: Sized,
        [(); MU::LEVELS - 1]: Sized,
    {
        use crate::pt::DualPageTable;

        Self {
            new_alloc: |handler| {
                DualPageTable::<A, ML, PTEL, MU, PTEU>::new_alloc_dyn(handler)
                    .map(|pt| OpaquePageTableRoot::Dual(pt.root()))
            },
            apply_actions: |root, handler, actions| {
                let OpaquePageTableRoot::Dual(root) = root else {
                    panic!("passing single-root page table to dual-root page table operation")
                };

                let mut pt = unsafe { DualPageTable::<A, ML, PTEL, MU, PTEU>::new_at(root) };
                let mut cursor = pt.cursor();

                for action in actions {
                    match action {
                        PageTableAction::Map {
                            vaddr,
                            paddr,
                            size,
                            flags,
                        } => {
                            cursor.map_dyn(vaddr, paddr, size, flags, handler)?;
                        }
                        PageTableAction::Unmap { vaddr, size } => {
                            cursor.unmap_dyn(vaddr, size, handler)?;
                        }
                    }
                }

                Ok(())
            },
        }
    }
}

/// A type-erased descriptor for a concrete page-table type.
///
/// The descriptor stores operations specialized for one concrete page-table
/// format. It does not store a root page table and can create any number of
/// [`OpaquePageTable`] handles using that format.
#[derive(Clone)]
pub struct OpaquePageTableType<A: VirtAddr> {
    /// The concrete page-table operations hidden by this descriptor.
    ///
    /// All operations use `A` as their semantic virtual-address type.
    methods: PageTableMethods<A>,
}

impl<A: VirtAddr> OpaquePageTableType<A> {
    /// Creates a descriptor for a concrete page-table format.
    ///
    /// `M` supplies the table geometry and must use `A` for virtual addresses.
    /// `PTE` supplies the concrete page-table entry representation.
    // NOTE: This direct bound currently compiles on nightly-2026-06-01, but
    // newer nightly trait solvers may report E0284 when downstream crates
    // instantiate it with architecture metadata.
    pub const fn new<M, PTE: GenericPTE>() -> Self
    where
        M: PageTableMeta<VirtAddr = A>,
        [(); M::LEVELS]: Sized,
        [(); M::LEVELS - 1]: Sized,
    {
        Self {
            methods: PageTableMethods::<A>::new::<M, PTE>(),
        }
    }

    /// Creates a descriptor for an AArch64 dual-root page-table format.
    ///
    /// `ML` and `PTEL` describe the lower-address root, while `MU` and `PTEU`
    /// describe the upper-address root. The two metadata types must use the
    /// same semantic virtual-address type and the corresponding lower and
    /// upper coverage policies.
    // NOTE: As with `new`, newer nightly trait solvers may fail to infer these
    // anonymous const bounds when a downstream crate instantiates this method.
    #[cfg(target_arch = "aarch64")]
    pub const fn new_dual<ML, PTEL, MU, PTEU>() -> Self
    where
        ML: PageTableMeta<VirtAddr = A, Coverage = LowerCoverage<A>>,
        PTEL: GenericPTE,
        MU: PageTableMeta<VirtAddr = A, Coverage = UpperCoverage<A>>,
        PTEU: GenericPTE,
        [(); ML::LEVELS - 1]: Sized,
        [(); MU::LEVELS - 1]: Sized,
    {
        Self {
            methods: PageTableMethods::<A>::new_dual::<ML, PTEL, MU, PTEU>(),
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
    /// The caller must ensure that `root` is valid and compatible with the
    /// concrete single-root or dual-root format captured by this descriptor.
    pub unsafe fn new_pagetable_at(&self, root: OpaquePageTableRoot) -> OpaquePageTable<A> {
        OpaquePageTable {
            root,
            methods: self.methods.clone(),
        }
    }

    /// Allocates and initializes a runtime type-erased page table through `H`.
    ///
    /// The returned handle uses this descriptor's concrete format and stores the
    /// root allocated by `H`.
    pub fn new_pagetable_alloc<H: PageAllocator>(&self) -> PagingResult<A, OpaquePageTable<A>> {
        unsafe {
            Ok(self.new_pagetable_at((self.methods.new_alloc)(&DynPageAllocator::new::<H>())?))
        }
    }
}

/// A page-table handle with a runtime type-erased concrete format.
///
/// The handle stores an erased root and the operations captured from an
/// [`OpaquePageTableType`]. Mapping operations still select their
/// [`PageAllocator`] statically.
pub struct OpaquePageTable<A: VirtAddr> {
    /// The erased root of the page table.
    ///
    /// Its concrete layout is described by `methods`.
    root: OpaquePageTableRoot,
    /// The type-erased operations for the root's concrete table format.
    ///
    /// These functions interpret `root` consistently for all mutations.
    methods: PageTableMethods<A>,
}

impl<A: VirtAddr> OpaquePageTable<A> {
    /// Creates a page-table handle whose operations panic.
    ///
    /// The sentinel has a zero root address and is suitable only for constant
    /// initialization before replacement with a real handle.
    pub const fn dummy() -> Self {
        Self {
            root: OpaquePageTableRoot::Single(pa!(0)),
            methods: PageTableMethods::dummy(),
        }
    }

    /// Returns the erased root of the page table.
    ///
    /// The returned root is not translated or dereferenced by this accessor.
    pub fn root(&self) -> OpaquePageTableRoot {
        self.root
    }

    /// Maps a virtual address range to a physical address range.
    ///
    /// The concrete table format comes from the originating descriptor. The
    /// range is rounded to base-page boundaries and existing mappings are
    /// replaced according to [`crate::pt::PageTableCursorLike::map`].
    pub fn map<H: PageAllocator>(
        &mut self,
        vaddr: A,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> PagingResult<A> {
        (self.methods.apply_actions)(
            self.root,
            &DynPageAllocator::new::<H>(),
            &mut core::iter::once(PageTableAction::Map {
                vaddr,
                paddr,
                size,
                flags,
            }),
        )
    }

    /// Removes mappings from a virtual address range.
    ///
    /// The concrete table format comes from the originating descriptor. Required
    /// TLB invalidations are performed before this method returns.
    pub fn unmap<H: PageAllocator>(&mut self, vaddr: A, size: usize) -> PagingResult<A> {
        (self.methods.apply_actions)(
            self.root,
            &DynPageAllocator::new::<H>(),
            &mut core::iter::once(PageTableAction::Unmap { vaddr, size }),
        )
    }

    /// Applies a sequence of page-table actions.
    ///
    /// The concrete table format comes from the originating descriptor. Each
    /// action is applied in order, and required TLB invalidations are performed
    /// before this method returns.
    ///
    /// If an action fails, earlier actions remain applied and their pending TLB
    /// invalidations are flushed before the error is returned. The operation is
    /// not transactional.
    pub fn apply_actions<H: PageAllocator, I: IntoIterator<Item = PageTableAction<A>>>(
        &mut self,
        actions: I,
    ) -> PagingResult<A> {
        let mut actions = actions.into_iter();
        (self.methods.apply_actions)(self.root, &DynPageAllocator::new::<H>(), &mut actions)
    }
}
