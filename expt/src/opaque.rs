//! Runtime type-erased multi-level page table types.
//!
//! [`OpaquePageTableType`] captures operations for concrete metadata, entry, and allocator types.
//! [`OpaquePageTable`] combines those operations with an erased root to represent different
//! multi-level page table formats in a single type. This permits runtime page table hierarchy
//! format selection.

use core::mem::ManuallyDrop;

use expalloc_trait::PageAllocator;
use expanic::bug;
use memory_addr::{PhysAddr, pa};
use page_table_entry::{GenericPTE, MappingFlags};

use crate::{
    error::PagingResult,
    meta::{PageTableMeta, VirtAddr},
    pt::{
        PageTable,
        cleanup::{PendingCleanup, TlbFlusher},
    },
};

/// The actions that mutate a page table hierarchy.
///
/// Each action maps or unmaps one range through an opaque multi-level page table.
pub enum PageTableAction<A: VirtAddr> {
    /// A mapping of a virtual address range to a physical address range.
    ///
    /// The range is rounded outward to base-page boundaries and replaces existing mappings.
    Map {
        /// The start of the virtual address range.
        ///
        /// This address also selects the lower or upper hierarchy for a dual-root value.
        vaddr: A,
        /// The start of the physical address range.
        ///
        /// The address is rounded down as described by [`crate::pt::PageTableCursor::map`].
        paddr: PhysAddr,
        /// The size of the ranges in bytes.
        ///
        /// The virtual range begins at `vaddr` and is expanded to base-page boundaries.
        size: usize,
        /// The flags to install in each mapping.
        ///
        /// The same flags are used for every leaf installed by this action.
        flags: MappingFlags,
    },
    /// The removal of mappings from a virtual address range.
    ///
    /// The range is rounded outward to base-page boundaries. Mapped data frames are retained.
    Unmap {
        /// The start of the virtual address range.
        ///
        /// This address also selects the lower or upper hierarchy for a dual-root value.
        vaddr: A,
        /// The size of the range in bytes.
        ///
        /// The virtual range begins at `vaddr` and is expanded to base-page boundaries.
        size: usize,
    },
}

/// The physical root addresses of an opaque multi-level page table.
///
/// `Single` identifies the root of one page table hierarchy. `Dual` identifies independent
/// lower-address and upper-address hierarchies and is only available on AArch64.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpaquePageTableRoot {
    /// A single physical root.
    ///
    /// This address identifies the root page table of one hierarchy.
    Single(PhysAddr),
    /// Independent lower and upper physical roots used by AArch64.
    ///
    /// Each address identifies the root page table of a separate hierarchy.
    #[cfg(any(test, target_arch = "aarch64"))]
    Dual(crate::pt::DualPageTableRoot),
}

/// The function type for applying a sequence of page table actions.
type ApplyActionsFn<A> = for<'a> fn(
    root: OpaquePageTableRoot,
    actions: &'a mut dyn Iterator<Item = PageTableAction<A>>,
) -> PagingResult<A, PendingCleanup<A>>;

/// The operations for one concrete multi-level page table type.
///
/// Mutation functions reconstruct a temporary concrete value while preserving its table frames.
/// Destruction deallocates table frames through the concrete value's destructor.
#[derive(Clone)]
struct PageTableMethods<A: VirtAddr> {
    /// Creates a new, empty and zeroed page table hierarchy.
    pub new_alloc: fn() -> PagingResult<A, OpaquePageTableRoot>,
    /// Applies a sequence of updates to a page table hierarchy and returns the pending cleanup.
    pub apply_actions: ApplyActionsFn<A>,
    /// Destroys and deallocates a page table hierarchy.
    pub destroy: fn(root: OpaquePageTableRoot),
}

/// Construction of statically specialized opaque operations.
///
/// Each method table retains the concrete format and allocator in its function pointers.
impl<A: VirtAddr> PageTableMethods<A> {
    /// Creates a method table that rejects allocation and mutation by reporting bugs.
    ///
    /// Destruction is a no-op so a constant-initialization sentinel can be dropped.
    pub const fn dummy() -> Self {
        /// Reports a bug when an operation is attempted through a dummy method table.
        ///
        /// Allocation and mutation both call this helper.
        fn bug_im_dummy() -> ! {
            bug!("dummy page table type does not support any actual operations")
        }

        Self {
            new_alloc: || bug_im_dummy(),
            apply_actions: |_, _| bug_im_dummy(),
            destroy: |_| {},
        }
    }

    /// Creates a method table for a concrete single-root multi-level page table format.
    ///
    /// `M` supplies the page table hierarchy geometry and must use `A` for virtual addresses.
    /// `PTE` supplies the concrete entry representation, and `AL` supplies table frame allocation,
    /// address translation, and destruction.
    pub const fn new<M, PTE, AL, F>() -> Self
    where
        M: PageTableMeta<VirtAddr = A>,
        PTE: GenericPTE,
        AL: PageAllocator,
        F: TlbFlusher<A>,
        [(); M::LEVELS - 1]: Sized,
    {
        Self {
            new_alloc: || {
                PageTable::<M, PTE, AL, F>::new_alloc()
                    .map(|pt| OpaquePageTableRoot::Single(pt.leak()))
            },
            apply_actions: |root, actions| {
                #[cfg(not(any(test, target_arch = "aarch64")))]
                let OpaquePageTableRoot::Single(root) = root;
                #[cfg(any(test, target_arch = "aarch64"))]
                let OpaquePageTableRoot::Single(root) = root else {
                    bug!("passing dual-root page table to single-root page table operation")
                };
                // Use `ManuallyDrop` to avoid dropping the hierarchy on error.
                let mut pt = ManuallyDrop::new(unsafe { PageTable::<M, PTE, AL, F>::new_at(root) });
                let mut cursor = pt.cursor();

                for action in actions {
                    match action {
                        PageTableAction::Map {
                            vaddr,
                            paddr,
                            size,
                            flags,
                        } => {
                            cursor.map(vaddr, paddr, size, flags)?;
                        }
                        PageTableAction::Unmap { vaddr, size } => {
                            cursor.unmap(vaddr, size)?;
                        }
                    }
                }

                let result = unsafe { cursor.take_cleanup() };
                drop(cursor);

                let new_root = ManuallyDrop::into_inner(pt).leak();
                debug_assert_eq!(root, new_root);

                Ok(result)
            },
            destroy: |root| match root {
                OpaquePageTableRoot::Single(root) => {
                    drop(unsafe { PageTable::<M, PTE, AL, F>::new_at(root) });
                }
                #[cfg(any(test, target_arch = "aarch64"))]
                OpaquePageTableRoot::Dual(_) => {
                    bug!("passing dual-root page table to single-root page table destruction")
                }
            },
        }
    }

    /// Creates a method table for a dual-root multi-level page table type.
    ///
    /// Both roots use the statically selected allocator for table frames and destruction.
    #[cfg(any(test, target_arch = "aarch64"))]
    pub const fn new_dual<ML, PTEL, MU, PTEU, AL, F>() -> Self
    where
        ML: PageTableMeta<VirtAddr = A, Coverage = crate::meta::LowerCoverage<A>>,
        PTEL: GenericPTE,
        MU: PageTableMeta<VirtAddr = A, Coverage = crate::meta::UpperCoverage<A>>,
        PTEU: GenericPTE,
        AL: PageAllocator,
        F: TlbFlusher<A>,
        [(); ML::LEVELS - 1]: Sized,
        [(); MU::LEVELS - 1]: Sized,
    {
        Self {
            new_alloc: || {
                crate::pt::DualPageTable::<A, ML, PTEL, MU, PTEU, AL, F>::new_alloc()
                    .map(|pt| OpaquePageTableRoot::Dual(pt.leak()))
            },
            apply_actions: |root, actions| {
                let OpaquePageTableRoot::Dual(root) = root else {
                    bug!("passing single-root page table to dual-root page table operation")
                };

                // Suppress hierarchy destruction on errors or panics while transferring cleanup.
                let mut pt = ManuallyDrop::new(unsafe {
                    crate::pt::DualPageTable::<A, ML, PTEL, MU, PTEU, AL, F>::new_at(root)
                });
                let mut cursor = pt.cursor();
                for action in actions {
                    match action {
                        PageTableAction::Map {
                            vaddr,
                            paddr,
                            size,
                            flags,
                        } => {
                            cursor.map(vaddr, paddr, size, flags)?;
                        }
                        PageTableAction::Unmap { vaddr, size } => {
                            cursor.unmap(vaddr, size)?;
                        }
                    }
                }

                let result = unsafe { cursor.take_cleanup() };
                drop(cursor);

                let new_root = ManuallyDrop::into_inner(pt).leak();
                debug_assert_eq!(root, new_root);

                Ok(result)
            },
            destroy: |root| {
                let OpaquePageTableRoot::Dual(root) = root else {
                    bug!("passing single-root page table to dual-root page table destruction")
                };
                drop(unsafe {
                    crate::pt::DualPageTable::<A, ML, PTEL, MU, PTEU, AL, F>::new_at(root)
                });
            },
        }
    }
}

/// A type-erased descriptor for a concrete multi-level page table type.
///
/// The descriptor stores operations specialized for one concrete multi-level page table
/// format. It does not store a root page table and can create any number of
/// [`OpaquePageTable`] values using that format.
///
/// The descriptor captures the allocator used for all table frame allocation and
/// deallocation performed through those values.
#[derive(Clone)]
pub struct OpaquePageTableType<A: VirtAddr> {
    /// The operations specialized for the selected concrete type.
    ///
    /// The function pointers retain the concrete metadata, entry, and allocator types.
    methods: PageTableMethods<A>,
}

/// Construction of descriptors and opaque multi-level page tables.
///
/// Descriptors select the concrete operations used by every value they construct.
impl<A: VirtAddr> OpaquePageTableType<A> {
    /// Creates a descriptor for a concrete single-root multi-level page table format.
    ///
    /// `M` supplies the page table hierarchy geometry and must use `A` for virtual addresses.
    /// `PTE` supplies the concrete page table entry representation and `AL`
    /// supplies the allocator for table frames.
    // NOTE: This direct bound currently compiles on nightly-2026-06-01, but
    // newer nightly trait solvers may report E0284 when downstream crates
    // instantiate it with architecture metadata.
    pub const fn new<M, PTE, AL, F>() -> Self
    where
        M: PageTableMeta<VirtAddr = A>,
        PTE: GenericPTE,
        AL: PageAllocator,
        F: TlbFlusher<A>,
        [(); M::LEVELS]: Sized,
        [(); M::LEVELS - 1]: Sized,
    {
        Self {
            methods: PageTableMethods::<A>::new::<M, PTE, AL, F>(),
        }
    }

    /// Creates a descriptor for an AArch64 dual-root multi-level page table type.
    ///
    /// `ML` and `PTEL` describe the lower-address root, while `MU` and `PTEU`
    /// describe the upper-address root. The two metadata types must use the
    /// same semantic virtual-address type and the corresponding lower and
    /// upper coverage policies.
    // NOTE: As with `new`, newer nightly trait solvers may fail to infer these
    // anonymous const bounds when a downstream crate instantiates this method.
    #[cfg(any(test, target_arch = "aarch64"))]
    pub const fn new_dual<ML, PTEL, MU, PTEU, AL, F>() -> Self
    where
        ML: PageTableMeta<VirtAddr = A, Coverage = crate::meta::LowerCoverage<A>>,
        PTEL: GenericPTE,
        MU: PageTableMeta<VirtAddr = A, Coverage = crate::meta::UpperCoverage<A>>,
        PTEU: GenericPTE,
        AL: PageAllocator,
        F: TlbFlusher<A>,
        [(); ML::LEVELS - 1]: Sized,
        [(); MU::LEVELS - 1]: Sized,
    {
        Self {
            methods: PageTableMethods::<A>::new_dual::<ML, PTEL, MU, PTEU, AL, F>(),
        }
    }

    /// Creates a descriptor whose allocation and mutation operations report bugs.
    ///
    /// This sentinel is suitable for constant initialization but must be
    /// replaced before use. Its destruction operation is a no-op.
    pub const fn dummy() -> Self {
        Self {
            methods: PageTableMethods::dummy(),
        }
    }

    /// Constructs an opaque multi-level page table from an existing hierarchy.
    ///
    /// The value uses the concrete format captured by this descriptor and does
    /// not inspect or initialize the referenced root page tables. The returned value
    /// assumes responsibility for their hierarchies' table-frame deallocation.
    ///
    /// # Safety
    ///
    /// The root variant must match the descriptor's single-root or dual-root format. The
    /// hierarchies must match its concrete metadata and entry types, and its allocator must
    /// be able to access and deallocate their table frames. For a single root, the remaining
    /// requirements are those of [`PageTable::new_at`]. For dual roots, they are those of
    /// [`crate::pt::DualPageTable::new_at`].
    pub unsafe fn new_pagetable_at(&self, root: OpaquePageTableRoot) -> OpaquePageTable<A> {
        OpaquePageTable {
            root,
            methods: self.methods.clone(),
        }
    }

    /// Allocates and initializes a runtime type-erased multi-level page table.
    ///
    /// The returned value uses this descriptor's concrete format and stores the
    /// root addresses allocated by the descriptor's captured allocator.
    pub fn new_pagetable_alloc(&self) -> PagingResult<A, OpaquePageTable<A>> {
        unsafe { Ok(self.new_pagetable_at((self.methods.new_alloc)()?)) }
    }
}

/// A runtime type-erased multi-level page table.
///
/// The value stores root addresses and operations specialized for its concrete format.
/// See the crate-level documentation for table-frame deallocation and hardware-use requirements.
pub struct OpaquePageTable<A: VirtAddr> {
    /// The physical root addresses of the managed hierarchies.
    ///
    /// The variant matches the format selected by the originating descriptor.
    root: OpaquePageTableRoot,
    /// The operations specialized for the managed hierarchies.
    ///
    /// The stored functions use the originating descriptor's concrete format and allocator.
    methods: PageTableMethods<A>,
}

/// Operations on an opaque multi-level page table.
///
/// Mutation uses a temporary concrete cursor. Cleanup uses the captured [`TlbFlusher`] or is
/// transferred explicitly to the caller.
impl<A: VirtAddr> OpaquePageTable<A> {
    /// Creates a sentinel multi-level page table.
    ///
    /// Allocation and mutation report bugs, while destruction is a no-op. The zero-root sentinel
    /// is suitable for constant initialization before replacement with a concrete value.
    pub const fn dummy() -> Self {
        Self {
            root: OpaquePageTableRoot::Single(pa!(0)),
            methods: PageTableMethods::dummy(),
        }
    }

    /// Returns the erased physical root addresses of the managed hierarchies.
    ///
    /// The returned root is not translated or dereferenced by this accessor. Its
    /// table frames remain associated with this value, and copying the addresses
    /// does not extend their lifetime.
    pub fn root(&self) -> OpaquePageTableRoot {
        self.root
    }

    /// Consumes the multi-level page table without deallocating the table frames.
    ///
    /// This operation reads only the stored root and calls none of the stored method pointers,
    /// including destruction. Another compatible allocator type can use the hierarchy, and the
    /// caller is otherwise responsible for arranging its deallocation.
    pub fn leak(self) -> OpaquePageTableRoot {
        let manually_dropped = ManuallyDrop::new(self);
        manually_dropped.root
    }

    /// Maps a virtual address range to a physical address range.
    ///
    /// The concrete page table hierarchy format comes from the originating descriptor. The
    /// range is rounded to base-page boundaries and existing mappings are
    /// replaced according to [`crate::pt::PageTableCursorLike::map`].
    /// See [`Self::apply_actions`] for cleanup and partial-failure behavior.
    pub fn map(
        &mut self,
        vaddr: A,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> PagingResult<A> {
        self.apply_actions(core::iter::once(PageTableAction::Map {
            vaddr,
            paddr,
            size,
            flags,
        }))
    }

    /// Removes mappings from a virtual address range.
    ///
    /// The range is rounded to base-page boundaries and mapped data frames remain caller-managed.
    /// See [`Self::apply_actions`] for cleanup and partial-failure behavior.
    pub fn unmap(&mut self, vaddr: A, size: usize) -> PagingResult<A> {
        self.apply_actions(core::iter::once(PageTableAction::Unmap { vaddr, size }))
    }

    /// Applies a sequence of page table hierarchy actions.
    ///
    /// The concrete page table hierarchy format comes from the originating descriptor. Each
    /// action is applied in order, including any immediate table-frame deallocation. Pending TLB
    /// invalidations are processed through the captured [`TlbFlusher`] before this method returns.
    ///
    /// If an action fails, earlier actions remain applied and their pending TLB
    /// invalidations are flushed before the error is returned. An iterator panic also flushes
    /// completed mutations during unwinding while retaining the table frames in the remaining
    /// hierarchy. The operation is not transactional. See [`PendingCleanup`] for synchronization
    /// requirements and [`Self::apply_actions_with_cleanup`] for transferring cleanup on success.
    pub fn apply_actions<I: IntoIterator<Item = PageTableAction<A>>>(
        &mut self,
        actions: I,
    ) -> PagingResult<A> {
        let cleanup = self.apply_actions_with_cleanup(actions)?;
        cleanup.clean_up();
        Ok(())
    }

    /// Applies actions and returns pending cleanup on success.
    ///
    /// Same mutation and partial-failure behavior as [`Self::apply_actions`]. Successful calls
    /// transfer cleanup without processing it. An action error or iterator panic instead causes
    /// the temporary cursor to process pending cleanup before returning the error or unwinding.
    /// The selected strategy and hardware-use preconditions must therefore permit automatic
    /// cleanup on these paths. See [`PendingCleanup`] for synchronization requirements.
    pub fn apply_actions_with_cleanup<I: IntoIterator<Item = PageTableAction<A>>>(
        &mut self,
        actions: I,
    ) -> PagingResult<A, PendingCleanup<A>> {
        let mut actions = actions.into_iter();
        (self.methods.apply_actions)(self.root, &mut actions)
    }
}

/// Opaque multi-level page table destruction.
///
/// See the crate-level documentation for table-frame deallocation and hardware-use requirements.
impl<A: VirtAddr> Drop for OpaquePageTable<A> {
    /// Deallocates table frames through the stored destruction operation.
    ///
    /// The operation uses the concrete type and allocator selected by the originating descriptor.
    fn drop(&mut self) {
        (self.methods.destroy)(self.root);
    }
}

/// Lifecycle regressions that require access to erased method pointers.
///
/// These tests verify that leaking a multi-level page table works after its stored operations
/// become unusable.
#[cfg(test)]
mod tests {
    use memory_addr::{VirtAddr, pa};
    use std::{panic::catch_unwind, string::String};

    use super::{OpaquePageTable, OpaquePageTableRoot, PageTableMethods};

    /// Verifies that leaking an opaque multi-level page table calls none of its stored methods.
    ///
    /// Kernel relocation can invalidate these method pointers before another compatible value uses
    /// the returned root, so even the destruction operation must remain untouched.
    #[test]
    fn opaque_leak_returns_root_without_calling_stored_methods() {
        let root = OpaquePageTableRoot::Single(pa!(0x123000));
        let page_table = OpaquePageTable::<VirtAddr> {
            root,
            methods: PageTableMethods {
                new_alloc: || panic!("leak called the stored allocation method"),
                apply_actions: |_, _| panic!("leak called the stored mutation method"),
                destroy: |_| panic!("leak called the stored destruction method"),
            },
        };

        assert_eq!(page_table.leak(), root);
    }

    /// Verifies that dummy operations report an internal bug before terminating.
    ///
    /// Dummy descriptors are initialization sentinels and must never be used as live tables.
    #[test]
    fn dummy_operation_reports_bug() {
        let payload = catch_unwind(|| {
            let methods = PageTableMethods::<VirtAddr>::dummy();
            let _ = (methods.new_alloc)();
        })
        .expect_err("dummy operation must terminate");
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .expect("default termination must carry a string message");
        assert!(
            message.contains("Bug at"),
            "unexpected bug report: {message}"
        );
    }
}
