//! Contract tests for inactive page table hierarchies.
//!
//! Independent snapshots, strict allocation accounting, and recorded TLB requests check mappings,
//! table-frame lifetimes, partial failures, and cleanup separately. No hierarchy is installed in
//! hardware. Snapshots follow cleanup, and no test executes privileged instructions or claims to
//! prove hardware synchronization.

use core::marker::PhantomData;
use std::{
    alloc::{Layout, alloc, dealloc},
    cell::RefCell,
    collections::BTreeMap,
    panic::{AssertUnwindSafe, catch_unwind},
    vec,
    vec::Vec,
};

use expalloc_trait::PageAllocator;
use memory_addr::{AddrRange, MemoryAddr, PhysAddr, VirtAddr};
use page_table_entry::{GenericPTE, MappingFlags};

use crate::{
    error::PagingError,
    meta::{
        LowerCoverage, PageTableCoverage, PageTableMeta, SymmetricCoverage, UpperCoverage,
        level_end_bit, level_start_bit,
    },
    opaque::{OpaquePageTable, OpaquePageTableRoot, OpaquePageTableType, PageTableAction},
    pt::{
        DualPageTable, DualPageTableRoot, PageTable, PageTableCursorLike,
        cleanup::{
            DynTlbFlusher, PendingCleanup, SMALL_FLUSH_THRESHOLD, TlbFlush, TlbFlusher, TlbFlushes,
        },
        single::{checked_page_end, max_page_level_for_offset},
    },
};

/// The byte size of a base page and allocator frame.
///
/// Compact hierarchies retain 4 KiB mappings with four entries per page table.
const PAGE: usize = 4096;
/// The lower hierarchy's complete virtual-address coverage.
///
/// Three two-bit levels cover 256 KiB.
const LOWER_SPAN: usize = 0x40000;
/// The first virtual address in the asymmetric upper hierarchy.
///
/// Its four two-bit levels cover the final 1 MiB.
const UPPER_BASE: usize = usize::MAX - 0xfffff;

/// Constructs a semantic virtual address for a fixture.
///
/// Raw literals remain at the fixture boundary.
fn va(value: usize) -> VirtAddr {
    VirtAddr::from_usize(value)
}

/// Constructs a synthetic physical address for a fixture.
///
/// Physical addresses differ from host allocation pointers.
fn pa(value: usize) -> PhysAddr {
    PhysAddr::from_usize(value)
}

/// An independent test PTE encoding.
///
/// Two kind bits and six permission bits fit below the aligned address. Every bit pattern is valid
/// Rust data, allowing dirty allocator buffers to detect missing initialization.
#[derive(Clone, Copy, Debug)]
struct TestPte(usize);

/// Encodes independent test leaves and child-table descriptors.
impl GenericPTE for TestPte {
    /// Constructs a leaf with independent address and permission fields.
    ///
    /// The kind determines whether traversal terminates above level zero.
    fn new_page(paddr: PhysAddr, flags: MappingFlags, is_huge: bool) -> Self {
        assert!(paddr.is_aligned(PAGE));
        Self(paddr.as_usize() | (flags.bits() << 2) | if is_huge { 3 } else { 2 })
    }
    /// Constructs a child page table descriptor.
    ///
    /// Child descriptors contain no leaf permissions.
    fn new_table(paddr: PhysAddr) -> Self {
        assert!(paddr.is_aligned(PAGE));
        Self(paddr.as_usize() | 1)
    }
    /// Extracts the physical address from an entry.
    ///
    /// The low twelve bits belong to the test encoding.
    fn paddr(&self) -> PhysAddr {
        pa(self.0 & !(PAGE - 1))
    }
    /// Extracts leaf mapping permissions.
    ///
    /// Kind and address bits are excluded.
    fn flags(&self) -> MappingFlags {
        MappingFlags::from_bits_truncate((self.0 >> 2) & 63)
    }
    /// Changes an address while preserving kind and flags.
    ///
    /// Base-page alignment is required by the encoding.
    fn set_paddr(&mut self, paddr: PhysAddr) {
        assert!(paddr.is_aligned(PAGE));
        self.0 = paddr.as_usize() | (self.0 & (PAGE - 1));
    }
    /// Changes leaf permissions and kind.
    ///
    /// The physical address remains unchanged.
    fn set_flags(&mut self, flags: MappingFlags, is_huge: bool) {
        *self = Self::new_page(self.paddr(), flags, is_huge);
    }
    /// Returns the complete encoded entry.
    ///
    /// Zero initialization is distinguishable from absent permissions.
    fn bits(self) -> usize {
        self.0
    }
    /// Reports whether every entry bit is zero.
    ///
    /// Dirty allocator memory is not treated as unused.
    fn is_unused(&self) -> bool {
        self.0 == 0
    }
    /// Reports whether a descriptor or leaf is present.
    ///
    /// Every nonzero kind represents an installed entry.
    fn is_present(&self) -> bool {
        self.0 & 3 != 0
    }
    /// Reports whether the entry is a huge leaf.
    ///
    /// Base leaves and child descriptors return false.
    fn is_huge(&self) -> bool {
        self.0 & 3 == 3
    }
    /// Clears every bit of the entry.
    ///
    /// Clearing alone deallocates neither table nor data frames.
    fn clear(&mut self) {
        self.0 = 0;
    }
}

/// A compact geometry with independent depth, leaf limit, and coverage.
///
/// Two bits per level exercise one through five levels with small allocations.
struct Geometry<const DEPTH: usize, const MAX_LEAF: usize, C>(PhantomData<C>);

/// Describes compact test hierarchies with a selectable depth, leaf limit, and coverage.
impl<const DEPTH: usize, const MAX_LEAF: usize, C> PageTableMeta for Geometry<DEPTH, MAX_LEAF, C>
where
    C: const PageTableCoverage<VirtAddr = VirtAddr> + Send + Sync,
{
    /// The fixture's virtual-address representation.
    type VirtAddr = VirtAddr;
    /// The selected canonical coverage policy.
    type Coverage = C;
    /// The configured hierarchy depth.
    const LEVELS: usize = DEPTH;
    /// The twelve-bit base-page offset.
    const PAGE_OFFSET_BITS: usize = 12;
    /// Two index bits at each level.
    const LEVEL_BITS: [usize; Self::LEVELS] = [2; Self::LEVELS] where [(); Self::LEVELS]: Sized;
    /// The highest level allowed to contain a leaf.
    const MAX_PAGE_LEVEL: usize = MAX_LEAF;
}

/// The compact lower-address hierarchy.
///
/// Supported leaves cover 4 KiB, 16 KiB, or 64 KiB.
type Lower = Geometry<3, 2, LowerCoverage<VirtAddr>>;
/// The asymmetric upper-address hierarchy.
///
/// An additional table-only root sits above its leaf levels.
type Upper = Geometry<4, 2, UpperCoverage<VirtAddr>>;
/// A compact geometry with a canonical hole.
///
/// Mapping requests cannot join its lower and upper ranges.
type Symmetric = Geometry<3, 2, SymmetricCoverage<VirtAddr>>;
/// The primary concrete multi-level page table for mapping tests.
///
/// Allocator and invalidation strategy remain separate capabilities.
type LowerTable = PageTable<Lower, TestPte, TestAllocator<1>, RecordingFlusher<1>>;
/// Asymmetric hierarchies using one allocator and invalidation strategy.
///
/// Different depths expose incorrect lower/upper dispatch.
type DualTable =
    DualPageTable<VirtAddr, Lower, TestPte, Upper, TestPte, TestAllocator<1>, RecordingFlusher<1>>;

/// A geometry with multiple allocator frames per child page table.
///
/// Child levels require 8192 and 16384 bytes, whereas the root requires only sixteen bytes.
struct MultiFrame;

/// Describes the non-uniform fixture's multi-frame tables.
impl PageTableMeta for MultiFrame {
    /// The larger geometry's virtual-address representation.
    type VirtAddr = VirtAddr;
    /// The lower-address coverage policy.
    type Coverage = LowerCoverage<VirtAddr>;
    /// The three hierarchy levels.
    const LEVELS: usize = 3;
    /// The twelve-bit base-page offset.
    const PAGE_OFFSET_BITS: usize = 12;
    /// Different child and root index widths.
    const LEVEL_BITS: [usize; 3] = [10, 11, 1];
    /// The highest supported leaf level.
    const MAX_PAGE_LEVEL: usize = 2;
}

/// Observable allocation and invalidation boundary events.
///
/// A combined log compares mutation-time deallocation with deferred invalidation.
#[derive(Clone, Debug, PartialEq)]
enum Event {
    /// A newly allocated run.
    Allocated {
        /// The selected allocator identifier.
        allocator: usize,
        /// The synthetic physical address.
        paddr: PhysAddr,
        /// The requested bytes before frame rounding.
        bytes: usize,
    },
    /// A deallocated run.
    Deallocated {
        /// The deallocating allocator identifier.
        allocator: usize,
        /// The run's physical address.
        paddr: PhysAddr,
        /// The original requested byte size.
        bytes: usize,
    },
    /// A completed invalidation strategy call.
    Flushed {
        /// The selected strategy identifier.
        strategy: usize,
        /// One mapping address or all addresses.
        address: Option<VirtAddr>,
    },
}

/// One host allocation backing synthetic physical frames.
///
/// A data tag rejects erroneous traversal and deallocation of caller-managed memory.
struct Allocation {
    /// The host pointer used at raw access boundaries.
    pointer: *mut u8,
    /// The host layout covering rounded frames.
    layout: Layout,
    /// The requested bytes before frame rounding.
    bytes: usize,
    /// The allocating strategy identifier.
    allocator: usize,
    /// Whether the caller manages this data run.
    data: bool,
}

/// Per-test physical memory and boundary instrumentation.
///
/// Allocation budgets inject deterministic failure independently of test execution order.
struct TestState {
    /// Live allocations indexed by physical address.
    allocations: BTreeMap<PhysAddr, Allocation>,
    /// The next unused synthetic physical address.
    next: PhysAddr,
    /// Allocation attempts including failures.
    attempts: usize,
    /// Remaining allocations before persistent failure.
    budget: Option<usize>,
    /// The combined boundary log.
    events: Vec<Event>,
}

/// Initializes and releases per-test physical-memory state.
impl TestState {
    /// Creates an isolated physical-memory registry.
    ///
    /// Tables and data share one physical-address domain.
    fn new() -> Self {
        Self {
            allocations: BTreeMap::new(),
            next: pa(0x1000_0000),
            attempts: 0,
            budget: None,
            events: Vec::new(),
        }
    }
    /// Discards host buffers after a test has failed.
    ///
    /// Successful tests instead prove the allocation registry is empty.
    fn discard_after_failure(&mut self) {
        for (_, record) in core::mem::take(&mut self.allocations) {
            // SAFETY: the record retains the original pointer and allocation layout.
            unsafe { dealloc(record.pointer, record.layout) };
        }
    }
}

/// Releases host memory left in a worker's allocation registry.
impl Drop for TestState {
    /// Discards leftover host buffers when a test thread exits.
    ///
    /// This fallback does not replace the explicit per-test leak assertion.
    fn drop(&mut self) {
        self.discard_after_failure();
    }
}

std::thread_local! {
    /// The current worker's allocator and event state.
    ///
    /// Parallel workers never share synthetic physical memory.
    static STATE: RefCell<TestState> = RefCell::new(TestState::new());
}

/// A scope requiring all test allocations to be accounted for.
///
/// Declare it before tested values so their destructors precede the leak assertion.
struct TestContext;

/// Creates a scope for isolated allocation accounting.
impl TestContext {
    /// Begins a test without inherited allocations or events.
    ///
    /// A prior test cannot silently normalize a lifecycle failure.
    fn new() -> Self {
        STATE.with_borrow_mut(|state| {
            assert!(
                state.allocations.is_empty(),
                "previous test left live allocations"
            );
            *state = TestState::new();
        });
        Self
    }
}

/// Checks allocation accounting or releases buffers after a failed test.
impl Drop for TestContext {
    /// Checks complete deallocation unless the test is already unwinding.
    ///
    /// Failed tests discard host buffers without hiding a successful-path leak.
    fn drop(&mut self) {
        STATE.with_borrow_mut(|state| {
            if std::thread::panicking() {
                state.discard_after_failure();
            } else {
                assert!(
                    state.allocations.is_empty(),
                    "live allocations: {:?}",
                    allocations_in(state)
                );
            }
        });
    }
}

/// A strict allocator sharing one test's physical-memory registry.
///
/// Different identifiers expose allocator selection after leak/new_at transfers.
struct TestAllocator<const ID: usize>;

/// Allocates and translates instrumented table-frame runs.
impl<const ID: usize> PageAllocator for TestAllocator<ID> {
    /// Returns the fixed 4 KiB allocator frame shift.
    fn page_size_shift() -> usize {
        12
    }

    /// Allocates a contiguous run measured in physical frames.
    ///
    /// Exact byte requests use the overridden method below.
    fn alloc_frames(count: usize) -> Option<PhysAddr> {
        Self::alloc_frames_of_size(count * PAGE)
    }
    /// Allocates dirty memory while preserving the requested byte size.
    ///
    /// expt must initialize every byte it later interprets as an entry.
    fn alloc_frames_of_size(bytes: usize) -> Option<PhysAddr> {
        STATE.with_borrow_mut(|state| {
            state.attempts += 1;
            if let Some(remaining) = &mut state.budget {
                if *remaining == 0 {
                    return None;
                }
                *remaining -= 1;
            }
            let layout = Layout::from_size_align(bytes.div_ceil(PAGE) * PAGE, PAGE).unwrap();
            // SAFETY: the nonzero layout uses a supported host alignment.
            let pointer = unsafe { alloc(layout) };
            assert!(!pointer.is_null(), "host allocation failed");
            // SAFETY: the buffer covers every initialized byte.
            unsafe { pointer.write_bytes(0xa5, layout.size()) };
            let paddr = state.next;
            state.next += layout.size();
            assert!(
                state
                    .allocations
                    .insert(
                        paddr,
                        Allocation {
                            pointer,
                            layout,
                            bytes,
                            allocator: ID,
                            data: false,
                        }
                    )
                    .is_none()
            );
            state.events.push(Event::Allocated {
                allocator: ID,
                paddr,
                bytes,
            });
            Some(paddr)
        })
    }
    /// Deallocates a run using a physical frame count.
    ///
    /// Exact byte allocations require the matching exact-size method.
    fn dealloc_frames(paddr: PhysAddr, count: usize) {
        Self::dealloc_frames_of_size(paddr, count * PAGE);
    }
    /// Deallocates one original run with its original requested byte size.
    ///
    /// Unknown addresses, wrong sizes, and data-frame deallocations fail immediately.
    fn dealloc_frames_of_size(paddr: PhysAddr, bytes: usize) {
        STATE.with_borrow_mut(|state| {
            let record = state
                .allocations
                .get(&paddr)
                .expect("unknown or duplicate deallocation");
            assert_eq!(
                record.bytes, bytes,
                "deallocation size differs from allocation"
            );
            assert!(!record.data, "expt deallocated caller-managed data");
            let record = state.allocations.remove(&paddr).unwrap();
            state.events.push(Event::Deallocated {
                allocator: ID,
                paddr,
                bytes,
            });
            // SAFETY: removing the record prevents duplicate host deallocation.
            unsafe { dealloc(record.pointer, record.layout) };
        });
    }
    /// Translates a live table allocation into a host buffer.
    ///
    /// An invalid physical access fails before dereferencing a dangling host pointer.
    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        STATE.with_borrow(|state| {
            let record = state
                .allocations
                .get(&paddr)
                .expect("access to unallocated table frames");
            assert!(!record.data, "mapped data traversed as a page table");
            VirtAddr::from_mut_ptr_of(record.pointer)
        })
    }
}

/// A strategy recording the privileged-instruction boundary.
///
/// Identifiers expose selection errors without simulating a CPU TLB.
struct RecordingFlusher<const ID: usize>;

/// Records invalidations without executing privileged instructions.
impl<const ID: usize> TlbFlusher<VirtAddr> for RecordingFlusher<ID> {
    /// Records a completed call through the selected strategy.
    ///
    /// No hardware hierarchy or privileged instruction participates.
    fn flush(address: Option<VirtAddr>) {
        STATE.with_borrow_mut(|state| {
            state.events.push(Event::Flushed {
                strategy: ID,
                address,
            })
        });
    }
}

/// Returns allocation identities without retaining the registry borrow.
///
/// Each tuple contains physical address, requested bytes, and allocator identifier.
fn allocations_in(state: &TestState) -> Vec<(PhysAddr, usize, usize)> {
    state
        .allocations
        .iter()
        .map(|(&paddr, record)| (paddr, record.bytes, record.allocator))
        .collect()
}

/// Returns live allocation identities and exact byte sizes.
///
/// Synthetic addresses remain stable across host-buffer reuse.
fn allocations() -> Vec<(PhysAddr, usize, usize)> {
    STATE.with_borrow(allocations_in)
}

/// Returns the count of whole live allocation runs.
///
/// One run may cover multiple physical frames.
fn allocation_count() -> usize {
    STATE.with_borrow(|state| state.allocations.len())
}

/// Returns the allocation attempt count including failures.
///
/// This exposes unexpected allocation on replacement paths.
fn attempts() -> usize {
    STATE.with_borrow(|state| state.attempts)
}

/// Copies the combined boundary log.
///
/// Its ordering compares immediate deallocation with deferred invalidation.
fn events() -> Vec<Event> {
    STATE.with_borrow(|state| state.events.clone())
}

/// Returns deallocation identifiers, addresses, and exact bytes in observed order.
///
/// Caller-managed data deallocation is explicit in the fixture.
fn deallocations() -> Vec<(usize, PhysAddr, usize)> {
    events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Deallocated {
                allocator,
                paddr,
                bytes,
            } => Some((allocator, paddr, bytes)),
            _ => None,
        })
        .collect()
}

/// Returns invalidation strategy identifiers and arguments.
///
/// These calls come from production cleanup code.
fn flushes() -> Vec<(usize, Option<VirtAddr>)> {
    events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Flushed { strategy, address } => Some((strategy, address)),
            _ => None,
        })
        .collect()
}

/// Clears events without changing allocations or failure injection.
///
/// Allocation attempt counts and table contents remain unchanged.
fn clear_events() {
    STATE.with_borrow_mut(|state| state.events.clear());
}

/// Allows exactly the given count of allocations before persistent failure.
///
/// Deallocation does not consume the budget.
fn fail_after(count: usize) {
    STATE.with_borrow_mut(|state| state.budget = Some(count));
}

/// Disables allocation failure injection.
///
/// Earlier mutations remain intact.
fn allow_allocations() {
    STATE.with_borrow_mut(|state| state.budget = None);
}

/// Allocates caller-managed data protected from expt deallocation.
///
/// These bytes are never interpreted as page table entries.
fn allocate_data(bytes: usize) -> PhysAddr {
    let paddr = TestAllocator::<1>::alloc_frames_of_size(bytes).unwrap();
    STATE.with_borrow_mut(|state| state.allocations.get_mut(&paddr).unwrap().data = true);
    paddr
}

/// Explicitly deallocates a caller-managed data allocation.
///
/// Tested multi-level page tables are dropped before this fixture operation.
fn deallocate_data(paddr: PhysAddr) {
    let bytes = STATE.with_borrow_mut(|state| {
        let record = state.allocations.get_mut(&paddr).unwrap();
        assert!(record.data);
        record.data = false;
        record.bytes
    });
    TestAllocator::<1>::dealloc_frames_of_size(paddr, bytes);
}

/// Copies a PTE without retaining a reference to table memory.
///
/// Snapshot reads are performed after pending cleanup has completed.
fn read_entry(table: PhysAddr, index: usize) -> TestPte {
    STATE.with_borrow(|state| {
        let record = state
            .allocations
            .get(&table)
            .expect("snapshot of deallocated page table");
        assert!(!record.data);
        assert!((index + 1) * size_of::<TestPte>() <= record.bytes);
        // SAFETY: the live buffer contains the checked initialized entry.
        unsafe { record.pointer.cast::<TestPte>().add(index).read() }
    })
}

/// A complete mapping copied from a hierarchy.
///
/// The physical result includes the queried offset within its leaf.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Mapping {
    /// The translated physical address.
    paddr: PhysAddr,
    /// The leaf permissions.
    flags: MappingFlags,
    /// The leaf's covered byte size.
    page_size: usize,
}

/// Walks copied entries using an independent accumulation of index widths.
///
/// It does not call production traversal or derived address-bit helpers.
fn mapping<M: PageTableMeta<VirtAddr = VirtAddr>>(
    root: PhysAddr,
    address: VirtAddr,
) -> Option<Mapping>
where
    [(); M::LEVELS]: Sized,
{
    let mut table = root;
    let mut shift = M::PAGE_OFFSET_BITS + M::LEVEL_BITS.iter().sum::<usize>();
    for level in (0..M::LEVELS).rev() {
        shift -= M::LEVEL_BITS[level];
        let index = (address.as_usize() >> shift) & ((1 << M::LEVEL_BITS[level]) - 1);
        let entry = read_entry(table, index);
        if entry.is_unused() {
            return None;
        }
        if level == 0 || entry.is_huge() {
            return Some(Mapping {
                paddr: entry.paddr() + (address.as_usize() & ((1 << shift) - 1)),
                flags: entry.flags(),
                page_size: 1 << shift,
            });
        }
        table = entry.paddr();
    }
    unreachable!("valid hierarchies terminate at a leaf or unused entry")
}

/// Checks a mapping against independently chosen fixture values.
///
/// Geometry and permissions remain observable separately from the translated address.
fn assert_mapping<M: PageTableMeta<VirtAddr = VirtAddr>>(
    root: PhysAddr,
    address: VirtAddr,
    paddr: PhysAddr,
    flags: MappingFlags,
    page_size: usize,
) where
    [(); M::LEVELS]: Sized,
{
    assert_eq!(
        mapping::<M>(root, address),
        Some(Mapping {
            paddr,
            flags,
            page_size
        })
    );
}

/// Checks that no leaf covers the queried address.
///
/// Retained empty intermediate page tables do not constitute a mapping.
fn assert_unmapped<M: PageTableMeta<VirtAddr = VirtAddr>>(root: PhysAddr, address: VirtAddr)
where
    [(); M::LEVELS]: Sized,
{
    assert_eq!(mapping::<M>(root, address), None);
}

mod lifecycle {
    //! Construction, transfer, and exact table-frame lifecycle tests.
    //!
    //! These assertions distinguish deallocating a hierarchy from retaining caller-managed data.

    use super::*;

    /// Checks root initialization against deliberately nonzero allocator memory.
    ///
    /// Construction must allocate only one table and request no invalidation.
    #[test]
    fn construction_zeroes_only_the_required_root_allocation() {
        let _test = TestContext::new();
        let table = LowerTable::new_alloc().unwrap();
        assert_eq!(allocations(), vec![(table.root(), 32, 1)]);
        assert_eq!(attempts(), 1);
        for index in 0..4 {
            assert_eq!(read_entry(table.root(), index).bits(), 0);
        }
        assert!(flushes().is_empty());
        drop(table);
        assert!(flushes().is_empty());
    }

    /// Checks failure before any root allocation becomes live.
    ///
    /// A failed constructor must not flush or deallocate nonexistent memory.
    #[test]
    fn failed_root_allocation_has_no_cleanup_effects() {
        let _test = TestContext::new();
        fail_after(0);
        assert!(matches!(
            LowerTable::new_alloc(),
            Err(PagingError::AllocationFailed)
        ));
        assert_eq!(attempts(), 1);
        assert!(events().is_empty());
    }

    /// Checks independent leaf paths at every supported hierarchy depth.
    ///
    /// A base-only geometry requires exactly one table at each level.
    fn check_depth<M: PageTableMeta<VirtAddr = VirtAddr>>(expected_tables: usize)
    where
        [(); M::LEVELS]: Sized,
        [(); M::LEVELS - 1]: Sized,
    {
        let mut table =
            PageTable::<M, TestPte, TestAllocator<1>, RecordingFlusher<1>>::new_alloc().unwrap();
        table
            .cursor()
            .map(va(0), pa(0x800000), PAGE, MappingFlags::READ)
            .unwrap();
        assert_eq!(allocation_count(), expected_tables);
        assert_mapping::<M>(
            table.root(),
            va(0x123),
            pa(0x800123),
            MappingFlags::READ,
            PAGE,
        );
        clear_events();
        drop(table);
        assert_eq!(deallocations().len(), expected_tables);
        assert!(flushes().is_empty());
    }

    /// Checks all manually unrolled traversal branches.
    ///
    /// Single-level roots and four/five-level table-only ancestors must behave identically.
    #[test]
    fn base_mapping_and_drop_cover_one_through_five_levels() {
        let _test = TestContext::new();
        check_depth::<Geometry<1, 0, LowerCoverage<VirtAddr>>>(1);
        check_depth::<Geometry<2, 0, LowerCoverage<VirtAddr>>>(2);
        check_depth::<Geometry<3, 0, LowerCoverage<VirtAddr>>>(3);
        check_depth::<Geometry<4, 0, LowerCoverage<VirtAddr>>>(4);
        check_depth::<Geometry<5, 0, LowerCoverage<VirtAddr>>>(5);
    }

    /// Checks independent paths with nonzero indices above level two.
    ///
    /// Exhaustive snapshots detect aliases between siblings and untouched branches.
    fn check_high_indices<M: PageTableMeta<VirtAddr = VirtAddr>>(addresses: &[usize], span: usize)
    where
        [(); M::LEVELS]: Sized,
        [(); M::LEVELS - 1]: Sized,
    {
        let mut table =
            PageTable::<M, TestPte, TestAllocator<1>, RecordingFlusher<1>>::new_alloc().unwrap();
        for (index, &address) in addresses.iter().enumerate() {
            table
                .cursor()
                .map(
                    va(address),
                    pa(0x800000 + index * PAGE),
                    PAGE,
                    MappingFlags::READ,
                )
                .unwrap();
        }
        for address in (0..span).step_by(PAGE) {
            let observed = mapping::<M>(table.root(), va(address + 0x123));
            let expected = addresses
                .iter()
                .position(|&mapped| mapped == address)
                .map(|index| Mapping {
                    paddr: pa(0x800123 + index * PAGE),
                    flags: MappingFlags::READ,
                    page_size: PAGE,
                });
            assert_eq!(observed, expected, "virtual address {address:#x}");
        }
        table.cursor().unmap(va(addresses[1]), PAGE).unwrap();
        for (index, &address) in addresses.iter().enumerate() {
            if index == 1 {
                assert_unmapped::<M>(table.root(), va(address));
            } else {
                assert_mapping::<M>(
                    table.root(),
                    va(address),
                    pa(0x800000 + index * PAGE),
                    MappingFlags::READ,
                    PAGE,
                );
            }
        }
    }

    /// Checks index extraction and sibling isolation at levels three and four.
    ///
    /// Each fixture mixes zero and nonzero high indices within the same hierarchy.
    #[test]
    fn high_level_indices_select_distinct_branches() {
        let _test = TestContext::new();
        check_high_indices::<Geometry<4, 0, LowerCoverage<VirtAddr>>>(
            &[0, 0x3f000, 0x40000, 0x9a000, 0xff000],
            0x100000,
        );
        check_high_indices::<Geometry<5, 0, LowerCoverage<VirtAddr>>>(
            &[0, 0x40000, 0xc0000, 0x100000, 0x2a9000, 0x3ff000],
            0x400000,
        );
    }

    /// Checks the high-level leaf branches in four- and five-level geometries.
    ///
    /// Huge leaves at levels three and four must not be traversed or deallocated as tables.
    #[test]
    fn highest_supported_leaves_terminate_deallocation() {
        let _test = TestContext::new();
        let mut four = PageTable::<
            Geometry<4, 3, LowerCoverage<VirtAddr>>,
            TestPte,
            TestAllocator<1>,
            RecordingFlusher<1>,
        >::new_alloc()
        .unwrap();
        four.cursor()
            .map(va(0), pa(0x800000), 0x40000, MappingFlags::READ)
            .unwrap();
        assert_mapping::<Geometry<4, 3, LowerCoverage<VirtAddr>>>(
            four.root(),
            va(0x12345),
            pa(0x812345),
            MappingFlags::READ,
            0x40000,
        );
        let mut five = PageTable::<
            Geometry<5, 4, LowerCoverage<VirtAddr>>,
            TestPte,
            TestAllocator<1>,
            RecordingFlusher<1>,
        >::new_alloc()
        .unwrap();
        five.cursor()
            .map(va(0), pa(0x800000), 0x100000, MappingFlags::READ)
            .unwrap();
        assert_mapping::<Geometry<5, 4, LowerCoverage<VirtAddr>>>(
            five.root(),
            va(0x98765),
            pa(0x898765),
            MappingFlags::READ,
            0x100000,
        );
        assert_eq!(allocation_count(), 2);
    }

    /// Checks post-order deallocation across sibling branches.
    ///
    /// Roots must be last, data frames must remain live, and destruction must not flush.
    #[test]
    fn drop_deallocates_each_table_once_in_post_order_and_retains_data() {
        let _test = TestContext::new();
        let data = allocate_data(PAGE);
        let mut table = LowerTable::new_alloc().unwrap();
        for address in [0, 0x4000, 0x10000] {
            table
                .cursor()
                .map(va(address), data, PAGE, MappingFlags::READ)
                .unwrap();
        }
        let root = table.root();
        let middle0 = read_entry(root, 0).paddr();
        let middle1 = read_entry(root, 1).paddr();
        let leaf0 = read_entry(middle0, 0).paddr();
        let leaf1 = read_entry(middle0, 1).paddr();
        let leaf4 = read_entry(middle1, 0).paddr();
        clear_events();
        drop(table);
        assert_eq!(
            deallocations(),
            vec![
                (1, leaf0, 32),
                (1, leaf1, 32),
                (1, middle0, 32),
                (1, leaf4, 32),
                (1, middle1, 32),
                (1, root, 32),
            ]
        );
        assert_eq!(allocations(), vec![(data, PAGE, 1)]);
        assert!(flushes().is_empty());
        deallocate_data(data);
    }

    /// Checks exact byte requests for tables spanning multiple allocator frames.
    ///
    /// Frame rounding must not replace the original size passed to deallocation.
    #[test]
    fn multi_frame_tables_use_exact_original_byte_sizes() {
        let _test = TestContext::new();
        let mut table =
            PageTable::<MultiFrame, TestPte, TestAllocator<1>, RecordingFlusher<1>>::new_alloc()
                .unwrap();
        table
            .cursor()
            .map(va(0x80200000), pa(0x800000), PAGE, MappingFlags::READ)
            .unwrap();
        let root = table.root();
        let middle = read_entry(root, 0).paddr();
        let leaf = read_entry(middle, 512).paddr();
        assert_eq!(
            allocations(),
            vec![(root, 16, 1), (middle, 16384, 1), (leaf, 8192, 1)]
        );
        for index in 0..2048 {
            if index != 512 {
                assert_eq!(read_entry(middle, index).bits(), 0);
            }
        }
        for index in 0..1024 {
            if index != 512 {
                assert_eq!(read_entry(leaf, index).bits(), 0);
            }
        }
        assert_mapping::<MultiFrame>(root, va(0x80200123), pa(0x800123), MappingFlags::READ, PAGE);
        clear_events();
        drop(table);
        assert_eq!(
            deallocations(),
            vec![(1, leaf, 8192), (1, middle, 16384), (1, root, 16)]
        );
    }

    /// Checks transfer of a populated hierarchy between compatible allocator types.
    ///
    /// leak must retain all tables and new_at must adopt them without allocating or clearing.
    #[test]
    fn leak_and_new_at_transfer_allocator_and_flusher_capabilities() {
        let _test = TestContext::new();
        let mut original = LowerTable::new_alloc().unwrap();
        original
            .cursor()
            .map(va(0), pa(0x900000), PAGE, MappingFlags::READ)
            .unwrap();
        let before = allocations();
        clear_events();
        let root = original.leak();
        let mut adopted = unsafe {
            PageTable::<Lower, TestPte, TestAllocator<2>, RecordingFlusher<2>>::new_at(root)
        };
        assert_eq!(allocations(), before);
        assert!(events().is_empty());
        assert_mapping::<Lower>(root, va(0x345), pa(0x900345), MappingFlags::READ, PAGE);
        adopted.cursor().unmap(va(0), PAGE).unwrap();
        assert_eq!(flushes(), vec![(2, Some(va(0)))]);
        clear_events();
        drop(adopted);
        assert_eq!(deallocations().len(), 3);
        assert!(
            deallocations()
                .iter()
                .all(|(allocator, _, _)| *allocator == 2)
        );
    }

    /// Checks destruction of tables created by a huge split.
    ///
    /// Split-created allocations remain part of the hierarchy after the cursor ends.
    #[test]
    fn drop_includes_every_split_created_table() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        table
            .cursor()
            .map(va(0), pa(0x800000), 0x10000, MappingFlags::READ)
            .unwrap();
        table.cursor().unmap(va(PAGE), PAGE).unwrap();
        assert_eq!(allocation_count(), 3);
        clear_events();
        drop(table);
        assert_eq!(deallocations().len(), 3);
    }
}

mod mutations {
    //! Mapping and unmapping effects independently of allocation failure.
    //!
    //! Snapshots follow explicit or cursor-Drop cleanup and compare literal expected mappings.

    use super::*;

    /// Checks lower and upper canonical mappings within one root.
    ///
    /// Upper recursive clearing must retain full virtual addresses in invalidation requests.
    #[test]
    fn symmetric_root_keeps_canonical_sides_independent() {
        let _test = TestContext::new();
        let mut table =
            PageTable::<Symmetric, TestPte, TestAllocator<1>, RecordingFlusher<1>>::new_alloc()
                .unwrap();
        let upper = usize::MAX - 0x1ffff;
        let addresses = [0, 0x1f000, upper, upper + 0x10000];
        for (index, &address) in addresses.iter().enumerate() {
            table
                .cursor()
                .map(
                    va(address),
                    pa(0x800000 + index * PAGE),
                    PAGE,
                    MappingFlags::READ,
                )
                .unwrap();
        }
        let original = allocations();
        clear_events();
        assert!(matches!(
            table
                .cursor()
                .map(va(0x1f000), pa(0), 2 * PAGE, MappingFlags::READ),
            Err(PagingError::NonCanonical { .. })
        ));
        assert_eq!(allocations(), original);
        assert!(events().is_empty());
        table.cursor().unmap(va(upper), 0x10000).unwrap();
        assert_eq!(flushes(), vec![(1, Some(va(upper)))]);
        assert_eq!(allocations(), original);
        for (index, &address) in addresses.iter().enumerate() {
            if index == 2 {
                assert_unmapped::<Symmetric>(table.root(), va(address));
            } else {
                assert_mapping::<Symmetric>(
                    table.root(),
                    va(address + 0x123),
                    pa(0x800123 + index * PAGE),
                    MappingFlags::READ,
                    PAGE,
                );
            }
        }
    }

    /// Checks base mapping, permission replacement, and one-leaf clearing.
    ///
    /// Existing table paths require address-specific invalidation and no table allocation.
    #[test]
    fn existing_base_leaf_replace_and_clear_keep_table_frames() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        let root = table.root();
        table
            .cursor()
            .map(va(0x2000), pa(0x812000), PAGE, MappingFlags::READ)
            .unwrap();
        assert_mapping::<Lower>(root, va(0x2345), pa(0x812345), MappingFlags::READ, PAGE);
        let original = allocations();
        clear_events();
        table
            .cursor()
            .map(
                va(0x2000),
                pa(0x912000),
                PAGE,
                MappingFlags::READ | MappingFlags::WRITE,
            )
            .unwrap();
        assert_mapping::<Lower>(
            root,
            va(0x2345),
            pa(0x912345),
            MappingFlags::READ | MappingFlags::WRITE,
            PAGE,
        );
        assert_eq!(allocations(), original);
        assert_eq!(
            flushes(),
            vec![(1, Some(va(0x2000))), (1, Some(va(0x2000)))]
        );
        clear_events();
        table.cursor().unmap(va(0x2000), PAGE).unwrap();
        assert_unmapped::<Lower>(root, va(0x2345));
        assert_eq!(allocations(), original);
        assert_eq!(flushes(), vec![(1, Some(va(0x2000)))]);
        clear_events();
        table.cursor().unmap(va(0x2000), PAGE).unwrap();
        assert!(events().is_empty());
    }

    /// Checks independently rounded virtual and physical starts.
    ///
    /// Outward virtual expansion must cover both boundary pages without leaking past the end.
    #[test]
    fn unaligned_map_preserves_offset_between_rounded_starts() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        table
            .cursor()
            .map(va(0x1234), pa(0x9abc), 0x2100, MappingFlags::READ)
            .unwrap();
        for (address, physical) in [(0x1000, 0x9000), (0x1234, 0x9234), (0x3333, 0xb333)] {
            assert_mapping::<Lower>(
                table.root(),
                va(address),
                pa(physical),
                MappingFlags::READ,
                PAGE,
            );
        }
        assert_unmapped::<Lower>(table.root(), va(0));
        assert_unmapped::<Lower>(table.root(), va(0x4000));
        table.cursor().unmap(va(0x1fff), 2).unwrap();
        assert_unmapped::<Lower>(table.root(), va(0x1000));
        assert_unmapped::<Lower>(table.root(), va(0x2000));
        assert_mapping::<Lower>(
            table.root(),
            va(0x3000),
            pa(0xb000),
            MappingFlags::READ,
            PAGE,
        );
    }

    /// Checks physical alignment before selecting huge leaves.
    ///
    /// A virtual range aligned for huge pages must still use base pages for a misaligned PA.
    #[test]
    fn physical_alignment_limits_leaf_size() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        table
            .cursor()
            .map(va(0), pa(0x803000), 0x10000, MappingFlags::READ)
            .unwrap();
        for offset in (0..0x10000).step_by(PAGE) {
            assert_mapping::<Lower>(
                table.root(),
                va(offset),
                pa(0x803000 + offset),
                MappingFlags::READ,
                PAGE,
            );
        }
    }

    /// Checks largest-page selection at the beginning, middle, and end of a range.
    ///
    /// An aligned offset permits larger leaves only when VA alignment and remaining length agree.
    #[test]
    fn mixed_range_uses_largest_fitting_leaf_at_each_position() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        table
            .cursor()
            .map(va(0x1000), pa(0x801000), 0x18000, MappingFlags::READ)
            .unwrap();
        for (address, size) in [
            (0x1000, PAGE),
            (0x3000, PAGE),
            (0x4000, 0x4000),
            (0x8000, 0x4000),
            (0xc000, 0x4000),
            (0x10000, 0x4000),
            (0x18000, PAGE),
        ] {
            assert_mapping::<Lower>(
                table.root(),
                va(address),
                pa(0x800000 + address),
                MappingFlags::READ,
                size,
            );
        }
        assert_unmapped::<Lower>(table.root(), va(0x19000));
        table
            .cursor()
            .map(va(0x20000), pa(0x820000), 0x10000, MappingFlags::EXECUTE)
            .unwrap();
        assert_mapping::<Lower>(
            table.root(),
            va(0x2ffff),
            pa(0x82ffff),
            MappingFlags::EXECUTE,
            0x10000,
        );
    }

    /// Checks the explicit zero-length no-op before validation and allocation.
    ///
    /// Existing mappings, allocation budgets, and invalidation logs must remain unchanged.
    #[test]
    fn zero_length_single_root_operations_have_no_effect() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        table
            .cursor()
            .map(va(0), pa(0x800000), PAGE, MappingFlags::READ)
            .unwrap();
        let before = allocations();
        let attempts_before = attempts();
        clear_events();
        fail_after(0);
        {
            let mut cursor = table.cursor();
            cursor.map(va(1), pa(17), 0, MappingFlags::WRITE).unwrap();
            cursor.unmap(va(1), 0).unwrap();
            cursor
                .map(va(usize::MAX), pa(0), 0, MappingFlags::READ)
                .unwrap();
            cursor.unmap(va(usize::MAX), 0).unwrap();
        }
        assert_eq!(attempts(), attempts_before);
        assert_eq!(allocations(), before);
        assert!(events().is_empty());
        assert_mapping::<Lower>(table.root(), va(1), pa(0x800001), MappingFlags::READ, PAGE);
    }

    /// Checks current absent-unmap traversal semantics without hiding allocated tables.
    ///
    /// Fresh paths allocate zeroed child tables and require a topology invalidation.
    #[test]
    fn absent_unmap_can_allocate_tables_but_repeat_unmap_is_inert() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        clear_events();
        table.cursor().unmap(va(PAGE), PAGE).unwrap();
        assert_eq!(allocation_count(), 3);
        assert_eq!(flushes(), vec![(1, None)]);
        assert_unmapped::<Lower>(table.root(), va(PAGE));
        let before = allocations();
        clear_events();
        table.cursor().unmap(va(PAGE), PAGE).unwrap();
        assert_eq!(allocations(), before);
        assert!(events().is_empty());
    }

    /// Checks recursive clearing without replacing child descriptors.
    ///
    /// Leaf invalidations must carry their own virtual addresses and all table frames remain live.
    #[test]
    fn whole_subtree_unmap_clears_leaves_and_retains_descriptors() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        table
            .cursor()
            .map(va(0), pa(0x803000), 0x10000, MappingFlags::READ)
            .unwrap();
        let original = allocations();
        let descriptor = read_entry(table.root(), 0).bits();
        clear_events();
        table.cursor().unmap(va(0), 0x10000).unwrap();
        assert_eq!(allocations(), original);
        assert_eq!(read_entry(table.root(), 0).bits(), descriptor);
        assert_eq!(
            flushes(),
            (0..0x10000)
                .step_by(PAGE)
                .map(|address| (1, Some(va(address))))
                .collect::<Vec<_>>()
        );
        assert!(deallocations().is_empty());
        for offset in (0..0x10000).step_by(PAGE) {
            assert_unmapped::<Lower>(table.root(), va(offset));
        }
    }

    /// Checks huge-leaf clearing without a data-frame lifecycle operation.
    ///
    /// A huge invalidation must cover all addresses through the selected strategy.
    #[test]
    fn whole_huge_unmap_requests_full_invalidation() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        table
            .cursor()
            .map(va(0), pa(0x800000), 0x10000, MappingFlags::READ)
            .unwrap();
        clear_events();
        table.cursor().unmap(va(0), 0x10000).unwrap();
        assert_eq!(flushes(), vec![(1, None)]);
        assert!(deallocations().is_empty());
        assert_eq!(allocation_count(), 1);
        assert_eq!(read_entry(table.root(), 0).bits(), 0);
    }

    /// Checks invalidation classification for fresh and replaced huge leaves.
    ///
    /// A fresh root leaf requests its address, while replacement invalidates the previous huge
    /// translation with a full request and never allocates or deallocates table frames.
    #[test]
    fn huge_leaf_replacement_promotes_fresh_address_request_to_full() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        clear_events();
        table
            .cursor()
            .map(va(0x10000), pa(0x800000), 0x10000, MappingFlags::READ)
            .unwrap();
        assert_eq!(flushes(), vec![(1, Some(va(0x10000)))]);
        let original = allocations();
        clear_events();
        fail_after(0);
        table
            .cursor()
            .map(va(0x10000), pa(0x900000), 0x10000, MappingFlags::EXECUTE)
            .unwrap();
        assert_eq!(allocations(), original);
        assert!(deallocations().is_empty());
        assert_eq!(flushes(), vec![(1, None)]);
        assert_mapping::<Lower>(
            table.root(),
            va(0x12345),
            pa(0x902345),
            MappingFlags::EXECUTE,
            0x10000,
        );
    }

    /// Checks two-level huge splitting and preservation of untouched translations.
    ///
    /// Only the replaced base page changes physical address and permissions.
    #[test]
    fn split_preserves_unaffected_physical_addresses_and_flags() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        let original_flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
        table
            .cursor()
            .map(va(0), pa(0x800000), 0x10000, original_flags)
            .unwrap();
        clear_events();
        table
            .cursor()
            .map(va(0x2000), pa(0x912000), PAGE, MappingFlags::EXECUTE)
            .unwrap();
        assert_eq!(allocation_count(), 3);
        assert_eq!(flushes(), vec![(1, None)]);
        assert!(deallocations().is_empty());
        for address in (0..0x10000).step_by(PAGE) {
            let (physical, flags) = if address == 0x2000 {
                (0x912000, MappingFlags::EXECUTE)
            } else {
                (0x800000 + address, original_flags)
            };
            let size = if address < 0x4000 { PAGE } else { 0x4000 };
            assert_mapping::<Lower>(table.root(), va(address), pa(physical), flags, size);
        }
    }

    /// Checks partial unmap splitting without deallocating newly allocated tables.
    ///
    /// Adjacent mappings retain the old huge leaf's flags and physical offsets.
    #[test]
    fn partial_huge_unmap_removes_only_covered_base_pages() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        table
            .cursor()
            .map(va(0), pa(0x800000), 0x10000, MappingFlags::READ)
            .unwrap();
        clear_events();
        table.cursor().unmap(va(0x1fff), 2).unwrap();
        assert_eq!(flushes(), vec![(1, None)]);
        assert_unmapped::<Lower>(table.root(), va(0x1000));
        assert_unmapped::<Lower>(table.root(), va(0x2000));
        assert_mapping::<Lower>(table.root(), va(0), pa(0x800000), MappingFlags::READ, PAGE);
        assert_mapping::<Lower>(
            table.root(),
            va(0x3000),
            pa(0x803000),
            MappingFlags::READ,
            PAGE,
        );
        assert_mapping::<Lower>(
            table.root(),
            va(0xffff),
            pa(0x80ffff),
            MappingFlags::READ,
            0x4000,
        );
    }

    /// Checks subtree replacement without any allocation or delayed deallocation records.
    ///
    /// Exact child-first deallocation occurs before cleanup, while unrelated siblings survive.
    #[test]
    fn table_to_huge_replacement_deallocates_subtree_immediately() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        for address in [0, 0x4000, 0x10000] {
            table
                .cursor()
                .map(va(address), pa(0x803000), PAGE, MappingFlags::READ)
                .unwrap();
        }
        let root = table.root();
        let middle = read_entry(root, 0).paddr();
        let leaf0 = read_entry(middle, 0).paddr();
        let leaf1 = read_entry(middle, 1).paddr();
        let attempts_before = attempts();
        clear_events();
        fail_after(0);
        let cleanup = {
            let mut cursor = table.cursor();
            cursor
                .map(va(0), pa(0x900000), 0x10000, MappingFlags::EXECUTE)
                .unwrap();
            assert_eq!(attempts(), attempts_before);
            assert_eq!(
                deallocations(),
                vec![(1, leaf0, 32), (1, leaf1, 32), (1, middle, 32)]
            );
            assert!(flushes().is_empty());
            unsafe { cursor.take_cleanup() }
        };
        assert_eq!(allocation_count(), 3);
        assert!(flushes().is_empty());
        cleanup.clean_up();
        assert_eq!(flushes(), vec![(1, None)]);
        assert_mapping::<Lower>(
            root,
            va(0x1234),
            pa(0x901234),
            MappingFlags::EXECUTE,
            0x10000,
        );
        assert_mapping::<Lower>(root, va(0x10000), pa(0x803000), MappingFlags::READ, PAGE);
    }

    /// Checks replacement of multi-frame tables with different per-level byte sizes.
    ///
    /// Recursive deallocation must use per-level byte sizes on mutation and destruction paths.
    #[test]
    fn multi_frame_subtree_replacement_uses_exact_sizes() {
        let _test = TestContext::new();
        let mut table =
            PageTable::<MultiFrame, TestPte, TestAllocator<1>, RecordingFlusher<1>>::new_alloc()
                .unwrap();
        table
            .cursor()
            .map(va(0x80200000), pa(0x800000), PAGE, MappingFlags::READ)
            .unwrap();
        let middle = read_entry(table.root(), 0).paddr();
        let leaf = read_entry(middle, 512).paddr();
        clear_events();
        fail_after(0);
        table
            .cursor()
            .map(va(0), pa(0), 1usize << 33, MappingFlags::READ)
            .unwrap();
        assert_eq!(deallocations(), vec![(1, leaf, 8192), (1, middle, 16384)]);
        assert_eq!(allocation_count(), 1);
        assert_eq!(flushes(), vec![(1, None)]);
        assert_mapping::<MultiFrame>(
            table.root(),
            va(0x80200123),
            pa(0x80200123),
            MappingFlags::READ,
            1usize << 33,
        );
    }
}

mod failures_and_batching {
    //! Partial completion and cleanup behavior at cursor boundaries.
    //!
    //! Allocation budgets force failures at exact hierarchy depths without timing assumptions.

    use super::*;

    /// Checks every allocation boundary of a five-level base-page path.
    ///
    /// Installed ancestors survive failure and must still request topology invalidation.
    #[test]
    fn each_failed_path_depth_retains_only_installed_tables() {
        let _test = TestContext::new();
        for successful_children in 0..4 {
            allow_allocations();
            let mut table = PageTable::<
                Geometry<5, 0, LowerCoverage<VirtAddr>>,
                TestPte,
                TestAllocator<1>,
                RecordingFlusher<1>,
            >::new_alloc()
            .unwrap();
            let root = table.root();
            clear_events();
            fail_after(successful_children);
            {
                let mut cursor = table.cursor();
                assert!(matches!(
                    cursor.map(va(0), pa(0x800000), PAGE, MappingFlags::READ),
                    Err(PagingError::AllocationFailed)
                ));
                assert!(flushes().is_empty());
                assert_eq!(allocation_count(), successful_children + 1);
            }
            assert_unmapped::<Geometry<5, 0, LowerCoverage<VirtAddr>>>(root, va(0));
            let expected = if successful_children == 0 {
                vec![]
            } else {
                vec![(1, None)]
            };
            assert_eq!(flushes(), expected);
            drop(table);
            assert_eq!(allocation_count(), 0);
        }
    }

    /// Checks huge splitting failure before the first new descriptor is installed.
    ///
    /// The original huge leaf and its flags remain unchanged with no new flush request.
    #[test]
    fn failed_first_split_leaves_original_huge_mapping_intact() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        table
            .cursor()
            .map(va(0), pa(0x800000), 0x10000, MappingFlags::READ)
            .unwrap();
        clear_events();
        fail_after(0);
        assert!(matches!(
            table
                .cursor()
                .map(va(PAGE), pa(0x900000), PAGE, MappingFlags::EXECUTE),
            Err(PagingError::AllocationFailed)
        ));
        assert_eq!(allocation_count(), 1);
        assert!(events().is_empty());
        assert_mapping::<Lower>(
            table.root(),
            va(PAGE),
            pa(0x801000),
            MappingFlags::READ,
            0x10000,
        );
    }

    /// Checks failure after one huge split has already changed topology.
    ///
    /// Translations remain equivalent but the installed child table and full invalidation persist.
    #[test]
    fn failed_second_split_retains_first_split_and_can_be_retried() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        let flags = MappingFlags::READ | MappingFlags::WRITE;
        table
            .cursor()
            .map(va(0), pa(0x800000), 0x10000, flags)
            .unwrap();
        clear_events();
        fail_after(1);
        assert!(matches!(
            table.cursor().unmap(va(PAGE), PAGE),
            Err(PagingError::AllocationFailed)
        ));
        assert_eq!(allocation_count(), 2);
        assert_eq!(flushes(), vec![(1, None)]);
        for offset in (0..0x10000).step_by(PAGE) {
            assert_mapping::<Lower>(
                table.root(),
                va(offset),
                pa(0x800000 + offset),
                flags,
                0x4000,
            );
        }
        allow_allocations();
        clear_events();
        table.cursor().unmap(va(PAGE), PAGE).unwrap();
        assert_eq!(allocation_count(), 3);
        assert_unmapped::<Lower>(table.root(), va(PAGE));
        assert_mapping::<Lower>(table.root(), va(0), pa(0x800000), flags, PAGE);
        assert_eq!(flushes(), vec![(1, None)]);
    }

    /// Checks a successful prefix followed by allocation failure in one map call.
    ///
    /// Earlier leaves remain installed, while the failed address has no mapping.
    #[test]
    fn map_failure_preserves_completed_huge_prefix_and_flushes_on_drop() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        let root = table.root();
        clear_events();
        fail_after(0);
        {
            let mut cursor = table.cursor();
            assert!(matches!(
                cursor.map(va(0), pa(0x800000), 0x11000, MappingFlags::READ),
                Err(PagingError::AllocationFailed)
            ));
            assert!(flushes().is_empty());
        }
        assert_mapping::<Lower>(root, va(0xffff), pa(0x80ffff), MappingFlags::READ, 0x10000);
        assert_unmapped::<Lower>(root, va(0x10000));
        assert_eq!(flushes(), vec![(1, Some(va(0)))]);
        assert_eq!(allocation_count(), 1);
    }

    /// Checks failure after an earlier mutation has already deallocated a subtree.
    ///
    /// Deallocation cannot be rolled back, but cleanup must still flush the completed replacement.
    #[test]
    fn later_failure_preserves_completed_deallocation_and_replacement() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        table
            .cursor()
            .map(va(0), pa(0x803000), PAGE, MappingFlags::READ)
            .unwrap();
        let middle = read_entry(table.root(), 0).paddr();
        let old_leaf = read_entry(middle, 0).paddr();
        clear_events();
        fail_after(0);
        {
            let mut cursor = table.cursor();
            assert!(matches!(
                cursor.map(va(0), pa(0x900000), 0x5000, MappingFlags::EXECUTE),
                Err(PagingError::AllocationFailed)
            ));
            assert_eq!(deallocations(), vec![(1, old_leaf, 32)]);
            assert!(flushes().is_empty());
        }
        assert_eq!(
            events(),
            vec![
                Event::Deallocated {
                    allocator: 1,
                    paddr: old_leaf,
                    bytes: 32
                },
                Event::Flushed {
                    strategy: 1,
                    address: None
                },
            ]
        );
        assert_mapping::<Lower>(
            table.root(),
            va(0x3456),
            pa(0x903456),
            MappingFlags::EXECUTE,
            0x4000,
        );
        assert_unmapped::<Lower>(table.root(), va(0x4000));
    }

    /// Checks partial unmap before a later missing path cannot be allocated.
    ///
    /// The successful cleared prefix and its invalidation remain observable after error.
    #[test]
    fn unmap_failure_retains_cleared_prefix() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        table
            .cursor()
            .map(va(0), pa(0x800000), 0x10000, MappingFlags::READ)
            .unwrap();
        clear_events();
        fail_after(0);
        assert!(matches!(
            table.cursor().unmap(va(0), 0x11000),
            Err(PagingError::AllocationFailed)
        ));
        assert_unmapped::<Lower>(table.root(), va(0));
        assert_unmapped::<Lower>(table.root(), va(0x10000));
        assert_eq!(allocation_count(), 1);
        assert_eq!(flushes(), vec![(1, None)]);
    }

    /// Checks whole-range validation before any mapping mutation.
    ///
    /// Overflow and out-of-coverage requests must preserve existing entries and cleanup state.
    #[test]
    fn invalid_ranges_do_not_mutate_an_existing_hierarchy() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        table
            .cursor()
            .map(va(0), pa(0x800000), PAGE, MappingFlags::READ)
            .unwrap();
        let before = allocations();
        clear_events();
        for (start, size) in [
            (LOWER_SPAN, PAGE),
            (LOWER_SPAN - PAGE, PAGE * 2),
            (usize::MAX - 1, PAGE),
        ] {
            let mut cursor = table.cursor();
            assert!(matches!(
                cursor.map(va(start), pa(0), size, MappingFlags::READ),
                Err(PagingError::NonCanonical { .. })
            ));
            assert!(matches!(
                cursor.unmap(va(start), size),
                Err(PagingError::NonCanonical { .. })
            ));
        }
        assert_eq!(allocations(), before);
        assert!(events().is_empty());
        assert_mapping::<Lower>(table.root(), va(0), pa(0x800000), MappingFlags::READ, PAGE);
    }

    /// Checks repeated explicit cleanup followed by further mutation and Drop.
    ///
    /// Each batch must be consumed once without disabling cleanup of later operations.
    #[test]
    fn cursor_cleanup_drains_each_batch_once() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        let mut cursor = table.cursor();
        cursor
            .map(va(0), pa(0x800000), PAGE, MappingFlags::READ)
            .unwrap();
        assert!(flushes().is_empty());
        cursor.clean_up();
        assert_eq!(flushes(), vec![(1, None)]);
        clear_events();
        cursor.clean_up();
        assert!(events().is_empty());
        cursor.unmap(va(0), PAGE).unwrap();
        assert!(flushes().is_empty());
        drop(cursor);
        assert_eq!(flushes(), vec![(1, Some(va(0)))]);
    }

    /// Checks transferring a batch without accidentally flushing it through cursor destruction.
    ///
    /// The transferred value must be processed before further cursor operations.
    #[test]
    fn taking_cleanup_leaves_cursor_empty_and_usable_after_processing() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        let mut cursor = table.cursor();
        cursor
            .map(va(0), pa(0x800000), PAGE, MappingFlags::READ)
            .unwrap();
        let cleanup = unsafe { PageTableCursorLike::take_cleanup(&mut cursor) };
        assert!(flushes().is_empty());
        cleanup.clean_up();
        assert_eq!(flushes(), vec![(1, None)]);
        clear_events();
        PageTableCursorLike::unmap(&mut cursor, va(0), PAGE).unwrap();
        PageTableCursorLike::clean_up(&mut cursor);
        drop(cursor);
        assert_eq!(flushes(), vec![(1, Some(va(0)))]);
    }

    /// Checks deterministic cursor unwinding after a completed base mapping.
    ///
    /// The cursor processes invalidations while the enclosing multi-level page table survives.
    #[test]
    fn cursor_unwind_flushes_without_destroying_the_hierarchy() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut cursor = table.cursor();
            cursor
                .map(va(0), pa(0x800000), PAGE, MappingFlags::READ)
                .unwrap();
            panic!("intentional caller panic after completed mutation");
        }));
        assert!(result.is_err());
        assert_eq!(allocation_count(), 3);
        assert_eq!(flushes(), vec![(1, None)]);
        assert_mapping::<Lower>(
            table.root(),
            va(0x123),
            pa(0x800123),
            MappingFlags::READ,
            PAGE,
        );
    }

    /// Checks address-only batching across already existing paths.
    ///
    /// Exactly thirty-two requests remain individual and the next promotes to one full request.
    #[test]
    fn cursor_page_batches_promote_only_after_capacity_is_exceeded() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        table
            .cursor()
            .map(va(0), pa(0x803000), 0x40000, MappingFlags::READ)
            .unwrap();
        clear_events();
        {
            let mut cursor = table.cursor();
            for page in 0..32 {
                cursor.unmap(va(page * PAGE), PAGE).unwrap();
            }
            assert!(flushes().is_empty());
            cursor.clean_up();
        }
        assert_eq!(
            flushes(),
            (0..32)
                .map(|page| (1, Some(va(page * PAGE))))
                .collect::<Vec<_>>()
        );
        table.cursor().unmap(va(32 * PAGE), PAGE).unwrap();
        clear_events();
        {
            let mut cursor = table.cursor();
            for page in 0..33 {
                cursor
                    .map(
                        va(page * PAGE),
                        pa(0x903000 + page * PAGE),
                        PAGE,
                        MappingFlags::READ,
                    )
                    .unwrap();
            }
        }
        assert_eq!(flushes(), vec![(1, None)]);
    }
}

mod model {
    //! Deterministic mixed mutation checks against an independent base-page model.
    //!
    //! Every completed operation is followed by cleanup and exhaustive physical/permission checks.

    use super::*;

    /// Advances a deterministic sequence without external randomness.
    ///
    /// Different bit fields select operation, size, and position to avoid parity correlation.
    fn next(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    /// Checks broad sequences including huge replacement, partial splits, and unaligned ranges.
    ///
    /// The reference model stores only base-page translations and does not reuse page-table logic.
    #[test]
    fn mixed_operations_match_physical_and_permission_model() {
        let _test = TestContext::new();
        let mut table = LowerTable::new_alloc().unwrap();
        let mut shadow = [None; 64];
        let mut seed = 0x36d8_8a71_1dcb_5e29;
        let mut mapped = 0;
        let mut unmapped = 0;
        for step in 0..512 {
            let sample = next(&mut seed);
            let page_count = [1, 2, 4, 16][((sample >> 8) & 3) as usize];
            let start_page =
                ((sample >> 16) as usize % (64 - page_count + 1)) / page_count * page_count;
            let unaligned = sample & 4 != 0;
            let start = start_page * PAGE + if unaligned { 7 } else { 0 };
            let size = page_count * PAGE - if unaligned { 14 } else { 0 };
            if sample & 1 == 0 {
                let physical = 0x800000 + step * 0x10000 + if sample & 2 == 0 { 0 } else { PAGE };
                let flags = if sample & 32 == 0 {
                    MappingFlags::READ | MappingFlags::WRITE
                } else {
                    MappingFlags::READ | MappingFlags::EXECUTE | MappingFlags::USER
                };
                table
                    .cursor()
                    .map(
                        va(start),
                        pa(physical + if unaligned { 19 } else { 0 }),
                        size,
                        flags,
                    )
                    .unwrap();
                for offset in 0..page_count {
                    shadow[start_page + offset] = Some((pa(physical + offset * PAGE), flags));
                }
                mapped += 1;
            } else {
                table.cursor().unmap(va(start), size).unwrap();
                shadow[start_page..start_page + page_count].fill(None);
                unmapped += 1;
            }
            for (page, expected) in shadow.iter().enumerate() {
                for byte_offset in [0, PAGE - 1] {
                    let observed = mapping::<Lower>(table.root(), va(page * PAGE + byte_offset))
                        .map(|mapping| (mapping.paddr, mapping.flags));
                    assert_eq!(
                        observed,
                        expected.map(|(physical, flags)| (physical + byte_offset, flags)),
                        "step {step}, page {page}"
                    );
                }
            }
        }
        assert!(mapped > 100 && unmapped > 100);
    }
}

mod flush_and_coverage {
    //! Flush request processing and virtual-address coverage tests.
    //!
    //! Recording strategies exercise dispatch without executing processor invalidation
    //! instructions.

    use super::*;

    /// Verifies that empty cleanup performs no dispatch.
    ///
    /// Explicit cleanup and the destructor both leave an empty request batch untouched.
    #[test]
    fn empty_cleanup_performs_no_dispatch() {
        let _test = TestContext::new();
        PendingCleanup::<VirtAddr>::new_with::<RecordingFlusher<1>>().clean_up();
        drop(PendingCleanup::<VirtAddr>::new_with::<RecordingFlusher<2>>());
        assert!(flushes().is_empty());
    }

    /// Verifies that page requests preserve duplicates and insertion order.
    ///
    /// Draining the batch clears its state so a second drain performs no dispatch.
    #[test]
    fn page_requests_preserve_duplicates_and_drain_once() {
        let _test = TestContext::new();
        let mut requests = TlbFlushes::None + TlbFlush::Page(va(2 * PAGE));
        requests += TlbFlush::None;
        requests += TlbFlush::Page(va(PAGE));
        requests += TlbFlush::Page(va(2 * PAGE));
        requests.flush_with(&DynTlbFlusher::new::<RecordingFlusher<1>>());

        assert_eq!(
            flushes(),
            vec![
                (1, Some(va(2 * PAGE))),
                (1, Some(va(PAGE))),
                (1, Some(va(2 * PAGE))),
            ]
        );
        assert!(matches!(requests, TlbFlushes::None));
        requests.flush_with(&DynTlbFlusher::new::<RecordingFlusher<2>>());
        assert_eq!(flushes().len(), 3);
    }

    /// Verifies the exact capacity for individual page requests.
    ///
    /// The final available slot still dispatches every page individually in insertion order.
    #[test]
    fn exact_page_capacity_keeps_individual_requests() {
        let _test = TestContext::new();
        assert_eq!(SMALL_FLUSH_THRESHOLD, 32);
        let mut requests = TlbFlushes::None;
        for index in 0..SMALL_FLUSH_THRESHOLD {
            requests += TlbFlush::Page(va(index * PAGE));
        }
        assert!(matches!(
            &requests,
            TlbFlushes::Pages(pages) if pages.len() == SMALL_FLUSH_THRESHOLD
        ));

        requests.flush_with(&DynTlbFlusher::new::<RecordingFlusher<1>>());
        let expected: Vec<_> = (0..SMALL_FLUSH_THRESHOLD)
            .map(|index| (1, Some(va(index * PAGE))))
            .collect();
        assert_eq!(flushes(), expected);
        assert!(matches!(requests, TlbFlushes::None));
    }

    /// Verifies that one request beyond capacity promotes the batch.
    ///
    /// Repeated addresses consume capacity because individual requests are not deduplicated.
    #[test]
    fn page_capacity_overflow_promotes_to_one_full_dispatch() {
        let _test = TestContext::new();
        let mut requests = TlbFlushes::None;
        for _ in 0..=SMALL_FLUSH_THRESHOLD {
            requests += TlbFlush::Page(va(PAGE));
        }
        assert!(matches!(requests, TlbFlushes::Full));

        requests.flush_with(&DynTlbFlusher::new::<RecordingFlusher<2>>());
        assert_eq!(flushes(), vec![(2, None)]);
        assert!(matches!(requests, TlbFlushes::None));
    }

    /// Verifies that full invalidation remains dominant until drained.
    ///
    /// Additional empty, individual, and full requests cannot weaken the accumulated state.
    #[test]
    fn full_requests_remain_sticky_until_drained() {
        let _test = TestContext::new();
        let mut requests = TlbFlushes::None + TlbFlush::Page(va(PAGE));
        requests += TlbFlush::Full;
        requests += TlbFlush::None;
        requests += TlbFlush::Page(va(2 * PAGE));
        requests.append(TlbFlushes::None + TlbFlush::Page(va(3 * PAGE)));
        requests.append(TlbFlushes::Full);
        assert!(matches!(requests, TlbFlushes::Full));

        requests.flush_with(&DynTlbFlusher::new::<RecordingFlusher<1>>());
        requests += TlbFlush::Page(va(4 * PAGE));
        requests.flush_with(&DynTlbFlusher::new::<RecordingFlusher<1>>());
        assert_eq!(flushes(), vec![(1, None), (1, Some(va(4 * PAGE)))]);
    }

    /// Verifies that appending page batches preserves their sequence.
    ///
    /// The merge performs no dispatch and retains duplicate addresses across both batches.
    #[test]
    fn appending_page_batches_preserves_request_order() {
        let _test = TestContext::new();
        let mut requests = TlbFlushes::None + TlbFlush::Page(va(3 * PAGE));
        let appended = TlbFlushes::None + TlbFlush::Page(va(PAGE)) + TlbFlush::Page(va(3 * PAGE));
        requests.append(TlbFlushes::None);
        requests.append(appended);
        assert!(flushes().is_empty());

        requests.flush_with(&DynTlbFlusher::new::<RecordingFlusher<2>>());
        assert_eq!(
            flushes(),
            vec![
                (2, Some(va(3 * PAGE))),
                (2, Some(va(PAGE))),
                (2, Some(va(3 * PAGE))),
            ]
        );
    }

    /// Verifies promotion when an appended batch exceeds capacity.
    ///
    /// Two individually bounded collections require one full dispatch after they are merged.
    #[test]
    fn appending_page_batches_can_promote_to_full() {
        let _test = TestContext::new();
        let mut requests = TlbFlushes::None;
        for index in 0..SMALL_FLUSH_THRESHOLD - 1 {
            requests += TlbFlush::Page(va(index * PAGE));
        }
        requests.append(
            TlbFlushes::None
                + TlbFlush::Page(va(SMALL_FLUSH_THRESHOLD * PAGE))
                + TlbFlush::Page(va((SMALL_FLUSH_THRESHOLD + 1) * PAGE)),
        );
        assert!(matches!(requests, TlbFlushes::Full));
        assert!(flushes().is_empty());

        requests.flush_with(&DynTlbFlusher::new::<RecordingFlusher<1>>());
        assert_eq!(flushes(), vec![(1, None)]);
    }

    /// Verifies that clearing requests discards them without dispatch.
    ///
    /// Both individual and full states become empty and can accept a fresh request afterward.
    #[test]
    fn clearing_requests_discards_pages_and_full_state() {
        let _test = TestContext::new();
        let mut requests = TlbFlushes::None + TlbFlush::Page(va(PAGE));
        requests.clear();
        requests.flush_with(&DynTlbFlusher::new::<RecordingFlusher<1>>());
        requests += TlbFlush::Full;
        requests.clear();
        requests.flush_with(&DynTlbFlusher::new::<RecordingFlusher<1>>());
        assert!(flushes().is_empty());

        requests += TlbFlush::Page(va(2 * PAGE));
        requests.flush_with(&DynTlbFlusher::new::<RecordingFlusher<2>>());
        assert_eq!(flushes(), vec![(2, Some(va(2 * PAGE)))]);
    }

    /// Verifies that dropping bare request collections performs no dispatch.
    ///
    /// Automatic processing belongs to pending cleanup rather than the request collection.
    #[test]
    fn bare_request_collections_do_not_dispatch_on_drop() {
        let _test = TestContext::new();
        drop(TlbFlushes::None + TlbFlush::Page(va(PAGE)));
        drop(TlbFlushes::<VirtAddr>::Full);
        assert!(flushes().is_empty());
    }

    /// Verifies that pending cleanup defers all appended requests.
    ///
    /// Explicit cleanup dispatches the combined sequence through its selected strategy.
    #[test]
    fn pending_cleanup_defers_requests_until_explicit_cleanup() {
        let _test = TestContext::new();
        let mut cleanup = PendingCleanup::<VirtAddr>::new_with::<RecordingFlusher<1>>();
        cleanup.push_flush(TlbFlush::Page(va(2 * PAGE)));
        cleanup.append_flushes(
            TlbFlushes::None + TlbFlush::Page(va(PAGE)) + TlbFlush::Page(va(2 * PAGE)),
        );
        assert!(flushes().is_empty());

        cleanup.clean_up();
        assert_eq!(
            flushes(),
            vec![
                (1, Some(va(2 * PAGE))),
                (1, Some(va(PAGE))),
                (1, Some(va(2 * PAGE))),
            ]
        );
    }

    /// Verifies that pending cleanup dispatches through its destructor.
    ///
    /// A full request supersedes any recorded individual addresses before automatic processing.
    #[test]
    fn dropping_pending_cleanup_dispatches_its_selected_strategy() {
        let _test = TestContext::new();
        let mut cleanup = PendingCleanup::<VirtAddr>::new_with::<RecordingFlusher<2>>();
        cleanup.push_flush(TlbFlush::Page(va(PAGE)));
        cleanup.push_flush(TlbFlush::Full);
        assert!(flushes().is_empty());

        drop(cleanup);
        assert_eq!(flushes(), vec![(2, None)]);
    }

    /// Verifies that manual extraction preserves requests and strategy.
    ///
    /// Extraction performs no dispatch and the returned collection can be processed explicitly.
    #[test]
    fn extracting_pending_cleanup_defers_manual_dispatch() {
        let _test = TestContext::new();
        let mut cleanup = PendingCleanup::<VirtAddr>::new_with::<RecordingFlusher<2>>();
        cleanup.push_flush(TlbFlush::Page(va(PAGE)));
        cleanup.push_flush(TlbFlush::Page(va(2 * PAGE)));

        // SAFETY: the requests are immediately processed through the returned recording strategy.
        let (mut requests, strategy) = unsafe { cleanup.into_inner() };
        assert!(flushes().is_empty());
        requests.flush_with(&strategy);
        assert_eq!(
            flushes(),
            vec![(2, Some(va(PAGE))), (2, Some(va(2 * PAGE)))]
        );
    }

    /// Verifies that merging cleanup retains the destination strategy.
    ///
    /// The transferred requests are delayed and processed in order through the destination.
    #[test]
    fn merging_pending_cleanup_uses_destination_strategy() {
        let _test = TestContext::new();
        let mut destination = PendingCleanup::<VirtAddr>::new_with::<RecordingFlusher<1>>();
        destination.push_flush(TlbFlush::Page(va(2 * PAGE)));
        let mut source = PendingCleanup::<VirtAddr>::new_with::<RecordingFlusher<2>>();
        source.push_flush(TlbFlush::Page(va(PAGE)));

        // SAFETY: both strategies only record requests, so the destination is suitable for both.
        unsafe { destination.append(source) };
        assert!(flushes().is_empty());
        destination.clean_up();
        assert_eq!(
            flushes(),
            vec![(1, Some(va(2 * PAGE))), (1, Some(va(PAGE)))]
        );
    }

    /// Verifies that cleanup copies a supplied dynamic strategy.
    ///
    /// The dynamic strategy can leave scope before cleanup dispatches its recorded requests.
    #[test]
    fn pending_cleanup_retains_a_supplied_dynamic_strategy() {
        let _test = TestContext::new();
        let mut cleanup = {
            let strategy = DynTlbFlusher::new::<RecordingFlusher<2>>();
            PendingCleanup::<VirtAddr>::new(&strategy)
        };
        cleanup.push_flush(TlbFlush::Page(va(PAGE)));
        cleanup.clean_up();
        assert_eq!(flushes(), vec![(2, Some(va(PAGE)))]);
    }

    /// Verifies the first and last addresses of lower coverage.
    ///
    /// The first address above the implemented width and an upper address are rejected verbatim.
    #[test]
    fn lower_coverage_accepts_only_zero_extended_addresses() {
        let _test = TestContext::new();
        for address in [0, PAGE, LOWER_SPAN - 1] {
            assert_eq!(
                LowerCoverage::<VirtAddr>::validate_and_truncate_vaddr(va(address), Lower::VA_BITS),
                Ok(address)
            );
        }
        for address in [LOWER_SPAN, usize::MAX] {
            assert_eq!(
                LowerCoverage::<VirtAddr>::validate_and_truncate_vaddr(va(address), Lower::VA_BITS),
                Err(address)
            );
        }
    }

    /// Verifies the address boundaries of upper coverage.
    ///
    /// Canonical upper addresses lose their high extension bits and the preceding byte is rejected.
    #[test]
    fn upper_coverage_truncates_only_one_extended_addresses() {
        let _test = TestContext::new();
        for (address, offset) in [
            (UPPER_BASE, 0),
            (UPPER_BASE + PAGE, PAGE),
            (usize::MAX, 0xfffff),
        ] {
            assert_eq!(
                UpperCoverage::<VirtAddr>::validate_and_truncate_vaddr(va(address), Upper::VA_BITS),
                Ok(offset)
            );
        }
        for address in [0, UPPER_BASE - 1] {
            assert_eq!(
                UpperCoverage::<VirtAddr>::validate_and_truncate_vaddr(va(address), Upper::VA_BITS),
                Err(address)
            );
        }
    }

    /// Verifies both boundaries of symmetric coverage.
    ///
    /// The adjacent addresses in the canonical hole fail with their original raw address.
    #[test]
    fn symmetric_coverage_rejects_both_sides_of_the_hole() {
        let _test = TestContext::new();
        let half = LOWER_SPAN / 2;
        let upper = usize::MAX - half + 1;
        for (address, offset) in [
            (0, 0),
            (half - 1, half - 1),
            (upper, half),
            (usize::MAX, LOWER_SPAN - 1),
        ] {
            assert_eq!(
                SymmetricCoverage::<VirtAddr>::validate_and_truncate_vaddr(
                    va(address),
                    Symmetric::VA_BITS,
                ),
                Ok(offset)
            );
        }
        for address in [half, upper - 1] {
            assert_eq!(
                SymmetricCoverage::<VirtAddr>::validate_and_truncate_vaddr(
                    va(address),
                    Symmetric::VA_BITS,
                ),
                Err(address)
            );
        }
    }

    /// Verifies half-open lower range validation.
    ///
    /// The exclusive limit may be noncanonical when every included byte remains canonical.
    #[test]
    fn lower_ranges_accept_the_exclusive_coverage_limit() {
        let _test = TestContext::new();
        assert_eq!(
            LowerCoverage::<VirtAddr>::validate_and_truncate_vaddr_range(
                AddrRange::new(va(LOWER_SPAN - PAGE), va(LOWER_SPAN)),
                Lower::VA_BITS,
            ),
            Ok((LOWER_SPAN - PAGE, LOWER_SPAN))
        );
        assert_eq!(
            LowerCoverage::<VirtAddr>::validate_and_truncate_vaddr_range(
                AddrRange::new(va(PAGE), va(PAGE)),
                Lower::VA_BITS,
            ),
            Ok((PAGE, PAGE))
        );
        for range in [
            AddrRange::new(va(LOWER_SPAN), va(LOWER_SPAN)),
            AddrRange::new(va(LOWER_SPAN - PAGE), va(LOWER_SPAN + 1)),
        ] {
            assert_eq!(
                LowerCoverage::<VirtAddr>::validate_and_truncate_vaddr_range(range, Lower::VA_BITS),
                Err(LOWER_SPAN)
            );
        }
    }

    /// Verifies upper range offset conversion and the top sentinel.
    ///
    /// Coverage defines an exclusive zero end as the address-space limit for a nonempty range.
    #[test]
    fn upper_ranges_truncate_offsets_and_accept_the_top_sentinel() {
        let _test = TestContext::new();
        assert_eq!(
            UpperCoverage::<VirtAddr>::validate_and_truncate_vaddr_range(
                AddrRange::new(va(UPPER_BASE + PAGE), va(UPPER_BASE + 2 * PAGE)),
                Upper::VA_BITS,
            ),
            Ok((PAGE, 2 * PAGE))
        );
        // The coverage API explicitly accepts this sentinel without using AddrRange methods.
        let to_top = AddrRange {
            start: va(usize::MAX - PAGE + 1),
            end: va(0),
        };
        assert_eq!(
            UpperCoverage::<VirtAddr>::validate_and_truncate_vaddr_range(to_top, Upper::VA_BITS),
            Ok((0x100000 - PAGE, 0))
        );
        assert_eq!(
            UpperCoverage::<VirtAddr>::validate_and_truncate_vaddr_range(
                AddrRange::new(va(UPPER_BASE - 1), va(UPPER_BASE)),
                Upper::VA_BITS,
            ),
            Err(UPPER_BASE - 1)
        );
    }

    /// Verifies symmetric ranges at both canonical edges.
    ///
    /// Ranges may end at the lower exclusive boundary or extend from the upper range to the top.
    #[test]
    fn symmetric_ranges_accept_each_canonical_side() {
        let _test = TestContext::new();
        let half = LOWER_SPAN / 2;
        let upper = usize::MAX - half + 1;
        for (range, expected) in [
            (
                AddrRange::new(va(half - PAGE), va(half)),
                (half - PAGE, half),
            ),
            (
                AddrRange::new(va(upper), va(upper + PAGE)),
                (half, half + PAGE),
            ),
            (AddrRange::new(va(upper), va(upper)), (half, half)),
        ] {
            assert_eq!(
                SymmetricCoverage::<VirtAddr>::validate_and_truncate_vaddr_range(
                    range,
                    Symmetric::VA_BITS,
                ),
                Ok(expected)
            );
        }
        // The coverage API explicitly accepts this sentinel without using AddrRange methods.
        let to_top = AddrRange {
            start: va(upper),
            end: va(0),
        };
        assert_eq!(
            SymmetricCoverage::<VirtAddr>::validate_and_truncate_vaddr_range(
                to_top,
                Symmetric::VA_BITS,
            ),
            Ok((half, 0))
        );
    }

    /// Verifies rejection of symmetric ranges that cross the hole.
    ///
    /// Canonical endpoints cannot conceal invalid included bytes, including a zero end sentinel.
    #[test]
    fn symmetric_ranges_reject_crossing_the_canonical_hole() {
        let _test = TestContext::new();
        let half = LOWER_SPAN / 2;
        let upper = usize::MAX - half + 1;
        for range in [
            AddrRange::new(va(half - PAGE), va(upper + PAGE)),
            AddrRange::new(va(half - PAGE), va(half + 1)),
            AddrRange::new(va(half), va(half)),
            // The zero end denotes the address-space limit under the coverage API contract.
            AddrRange {
                start: va(PAGE),
                end: va(0),
            },
        ] {
            assert_eq!(
                SymmetricCoverage::<VirtAddr>::validate_and_truncate_vaddr_range(
                    range,
                    Symmetric::VA_BITS,
                ),
                Err(half)
            );
        }
    }

    /// Verifies constants derived from the compact hierarchy metadata.
    ///
    /// Different depths exercise page sizes, table widths, address widths, and bit boundaries.
    #[test]
    fn metadata_derives_level_geometry_from_all_depths() {
        let _test = TestContext::new();
        assert_eq!(Lower::PAGE_SIZE, PAGE);
        assert_eq!(Lower::LEVEL_TABLE_SIZE, [4, 4, 4]);
        assert_eq!(Lower::LEVEL_PAGE_SIZE, [PAGE, 4 * PAGE, 16 * PAGE]);
        assert_eq!(Lower::LEVEL_BIT_RANGES, [(12, 14), (14, 16), (16, 18)]);
        assert_eq!(Lower::VA_BITS, 18);
        assert_eq!(1usize << Lower::VA_BITS, LOWER_SPAN);
        assert_eq!(Upper::LEVEL_TABLE_SIZE, [4, 4, 4, 4]);
        assert_eq!(
            Upper::LEVEL_PAGE_SIZE,
            [PAGE, 4 * PAGE, 16 * PAGE, 64 * PAGE]
        );
        assert_eq!(Upper::VA_BITS, 20);
        assert_eq!(level_start_bit::<Lower>(0), 12);
        assert_eq!(level_start_bit::<Lower>(Lower::LEVELS), Lower::VA_BITS);
        assert_eq!(level_end_bit::<Upper>(0), 14);
        assert_eq!(level_end_bit::<Upper>(Upper::LEVELS - 1), Upper::VA_BITS);
    }

    /// Verifies page-level selection from alignment and permitted leaf levels.
    ///
    /// Unaligned offsets fail, zero admits the maximum leaf level, and metadata caps huge pages.
    #[test]
    fn maximum_page_level_obeys_alignment_and_leaf_limit() {
        let _test = TestContext::new();
        /// A geometry restricted to base-page leaves.
        ///
        /// Its upper hierarchy levels exist solely for traversal.
        type BaseOnly = Geometry<3, 0, LowerCoverage<VirtAddr>>;

        for (offset, expected) in [
            (0, Some(2)),
            (1, None),
            (PAGE - 1, None),
            (PAGE, Some(0)),
            (3 * PAGE, Some(0)),
            (4 * PAGE, Some(1)),
            (12 * PAGE, Some(1)),
            (16 * PAGE, Some(2)),
            (64 * PAGE, Some(2)),
        ] {
            assert_eq!(max_page_level_for_offset::<Lower>(offset), expected);
        }
        assert_eq!(max_page_level_for_offset::<BaseOnly>(0), Some(0));
        assert_eq!(max_page_level_for_offset::<BaseOnly>(16 * PAGE), Some(0));
    }

    /// Verifies that checked page ends respect a bounded exclusive end.
    ///
    /// An exact fit succeeds while a page that exceeds the remaining bytes is rejected.
    #[test]
    fn checked_page_end_accepts_exact_fit_and_rejects_short_ranges() {
        let _test = TestContext::new();
        assert_eq!(
            checked_page_end(va(PAGE), va(2 * PAGE), PAGE),
            Some(va(2 * PAGE))
        );
        assert_eq!(checked_page_end(va(PAGE), va(2 * PAGE - 1), PAGE), None);
        assert_eq!(checked_page_end(va(2 * PAGE), va(PAGE), PAGE), None);
        assert_eq!(
            checked_page_end(pa(PAGE), pa(3 * PAGE), PAGE),
            Some(pa(2 * PAGE))
        );
    }

    /// Verifies that checked page ends reject address arithmetic overflow.
    ///
    /// A representable advance to the largest address succeeds and one additional byte fails.
    #[test]
    fn checked_page_end_rejects_overflow_before_range_comparison() {
        let _test = TestContext::new();
        let start = va(usize::MAX - PAGE);
        let end = va(usize::MAX);
        assert_eq!(checked_page_end(start, end, PAGE), Some(end));
        assert_eq!(checked_page_end(start, end, PAGE + 1), None);
        assert_eq!(checked_page_end(va(usize::MAX), end, 1), None);
        assert_eq!(checked_page_end(pa(usize::MAX), pa(usize::MAX), PAGE), None);
    }
}

mod dual_and_opaque {
    //! Asymmetric dual-root and erased-format lifecycle checks.
    //!
    //! These cases exercise independent geometries, allocator transfers, partial failures, and
    //! cleanup while keeping all page table activity inside the in-memory test backing.

    use super::*;

    /// Extracts the physical root of an erased single-root table.
    ///
    /// A different root variant indicates an incorrectly selected test descriptor.
    fn single_root(table: &OpaquePageTable<VirtAddr>) -> PhysAddr {
        match table.root() {
            OpaquePageTableRoot::Single(root) => root,
            OpaquePageTableRoot::Dual(_) => panic!("expected a single root"),
        }
    }

    /// Extracts the physical roots of an erased dual-root table.
    ///
    /// A different root variant indicates an incorrectly selected test descriptor.
    fn dual_roots(table: &OpaquePageTable<VirtAddr>) -> DualPageTableRoot {
        match table.root() {
            OpaquePageTableRoot::Dual(roots) => roots,
            OpaquePageTableRoot::Single(_) => panic!("expected two roots"),
        }
    }

    /// Verifies independent zeroed roots and their exact destruction requests.
    ///
    /// Both roots contain four entries even though their hierarchy depths differ.
    #[test]
    fn dual_empty_roots_are_separate_zeroed_allocations() {
        let _context = TestContext::new();
        let table = DualTable::new_alloc().unwrap();
        let roots = table.root();
        assert_ne!(roots.lower, roots.upper);
        assert_eq!(allocation_count(), 2);
        assert!(
            allocations().iter().all(|&(_, bytes, allocator)| {
                bytes == 4 * size_of::<TestPte>() && allocator == 1
            })
        );
        for index in 0..4 {
            assert!(read_entry(roots.lower, index).is_unused());
            assert!(read_entry(roots.upper, index).is_unused());
        }
        assert_eq!(table.lower().root(), roots.lower);
        assert_eq!(table.upper().root(), roots.upper);

        drop(table);

        assert_eq!(
            deallocations(),
            vec![(1, roots.lower, 32), (1, roots.upper, 32)]
        );
        assert!(flushes().is_empty());
    }

    /// Verifies that failure of the second root deallocates the first root.
    ///
    /// Construction must leave no table allocations or pending invalidations behind.
    #[test]
    fn dual_second_root_failure_deallocates_first_root() {
        let _context = TestContext::new();
        fail_after(1);

        assert!(matches!(
            DualTable::new_alloc(),
            Err(PagingError::AllocationFailed)
        ));

        assert_eq!(attempts(), 2);
        assert_eq!(allocation_count(), 0);
        let freed = deallocations();
        assert_eq!(freed.len(), 1);
        assert_eq!(freed[0].0, 1);
        assert_eq!(freed[0].2, 32);
        assert!(flushes().is_empty());
        allow_allocations();
    }

    /// Verifies routing through hierarchies with different depths.
    ///
    /// A base page needs two intermediate tables on the lower side and three on the upper side.
    #[test]
    fn dual_routes_base_pages_through_asymmetric_geometries() {
        let _context = TestContext::new();
        let mut table = DualTable::new_alloc().unwrap();
        let roots = table.root();
        let low = va(PAGE);
        let high = va(UPPER_BASE + PAGE);
        {
            let mut cursor = table.cursor();
            cursor
                .map(low, pa(0x2000), PAGE, MappingFlags::READ)
                .unwrap();
            cursor
                .map(high, pa(0x3000), PAGE, MappingFlags::WRITE)
                .unwrap();
        }

        assert_eq!(allocation_count(), 7);
        assert_eq!(flushes(), vec![(1, None), (1, None)]);
        assert_mapping::<Lower>(roots.lower, low, pa(0x2000), MappingFlags::READ, PAGE);
        assert_mapping::<Upper>(roots.upper, high, pa(0x3000), MappingFlags::WRITE, PAGE);
        assert!(!read_entry(roots.upper, 0).is_huge());
        assert!(read_entry(roots.lower, 1).is_unused());
    }

    /// Verifies huge leaves below the upper side's table-only root.
    ///
    /// A lower huge leaf fits directly in its root while the same upper leaf needs one table.
    #[test]
    fn dual_upper_root_stays_a_table_above_largest_leaf() {
        let _context = TestContext::new();
        let mut table = DualTable::new_alloc().unwrap();
        let roots = table.root();
        {
            let mut cursor = table.cursor();
            cursor
                .map(va(0), pa(0x40000), 0x10000, MappingFlags::READ)
                .unwrap();
            cursor
                .map(va(UPPER_BASE), pa(0x80000), 0x10000, MappingFlags::EXECUTE)
                .unwrap();
        }

        assert_eq!(allocation_count(), 3);
        assert!(read_entry(roots.lower, 0).is_huge());
        let upper_entry = read_entry(roots.upper, 0);
        assert!(!upper_entry.is_unused());
        assert!(!upper_entry.is_huge());
        assert!(read_entry(upper_entry.paddr(), 0).is_huge());
        assert_mapping::<Lower>(roots.lower, va(0), pa(0x40000), MappingFlags::READ, 0x10000);
        assert_mapping::<Upper>(
            roots.upper,
            va(UPPER_BASE),
            pa(0x80000),
            MappingFlags::EXECUTE,
            0x10000,
        );
    }

    /// Verifies gap, side-boundary, and overflowing range rejection.
    ///
    /// Invalid requests must not allocate descendant tables or alter either root.
    #[test]
    fn dual_rejects_gap_crossings_and_upper_overflow() {
        let _context = TestContext::new();
        let mut table = DualTable::new_alloc().unwrap();
        {
            let mut cursor = table.cursor();
            assert!(matches!(
                cursor.map(va(LOWER_SPAN), pa(0), PAGE, MappingFlags::READ),
                Err(PagingError::NonCanonical { .. })
            ));
            assert!(matches!(
                cursor.unmap(va(LOWER_SPAN), PAGE),
                Err(PagingError::NonCanonical { .. })
            ));
            assert!(matches!(
                cursor.map(va(LOWER_SPAN - PAGE), pa(0), PAGE * 2, MappingFlags::READ,),
                Err(PagingError::NonCanonical { .. })
            ));
            assert!(matches!(
                cursor.map(va(usize::MAX - PAGE + 1), pa(0), PAGE, MappingFlags::READ,),
                Err(PagingError::NonCanonical { .. })
            ));
            assert!(matches!(
                cursor.map(va(LOWER_SPAN), pa(0), 0, MappingFlags::READ),
                Err(PagingError::NonCanonical { .. })
            ));
        }

        assert_eq!(allocation_count(), 2);
        assert!(flushes().is_empty());
        let roots = table.root();
        assert_unmapped::<Lower>(roots.lower, va(LOWER_SPAN - PAGE));
        assert_unmapped::<Upper>(roots.upper, va(UPPER_BASE));
    }

    /// Verifies deferred cleanup combines both canonical sides.
    ///
    /// Unmapping existing base pages records one address per side without freeing table frames.
    #[test]
    fn dual_take_cleanup_batches_both_sides_without_early_flush() {
        let _context = TestContext::new();
        let mut table = DualTable::new_alloc().unwrap();
        let roots = table.root();
        let low = va(PAGE);
        let high = va(UPPER_BASE + PAGE);
        {
            let mut cursor = table.cursor();
            cursor
                .map(low, pa(0x2000), PAGE, MappingFlags::READ)
                .unwrap();
            cursor
                .map(high, pa(0x3000), PAGE, MappingFlags::READ)
                .unwrap();
        }
        clear_events();
        let cleanup = {
            let mut cursor = table.cursor();
            cursor.unmap(low, PAGE).unwrap();
            cursor.unmap(high, PAGE).unwrap();
            // SAFETY: the batch is processed before inspecting or mutating either hierarchy.
            unsafe { cursor.take_cleanup() }
        };
        assert!(flushes().is_empty());
        assert!(deallocations().is_empty());
        assert_eq!(allocation_count(), 7);

        cleanup.clean_up();

        assert_eq!(flushes(), vec![(1, Some(low)), (1, Some(high))]);
        assert_unmapped::<Lower>(roots.lower, low);
        assert_unmapped::<Upper>(roots.upper, high);
    }

    /// Verifies cursor unwinding flushes completed mutations on both sides.
    ///
    /// The containing dual table keeps its allocations and remains usable after the panic.
    #[test]
    fn dual_cursor_unwind_flushes_both_surviving_hierarchies() {
        let _context = TestContext::new();
        let mut table = DualTable::new_alloc().unwrap();
        let roots = table.root();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut cursor = table.cursor();
            cursor
                .map(va(PAGE), pa(0x2000), PAGE, MappingFlags::READ)
                .unwrap();
            cursor
                .map(va(UPPER_BASE + PAGE), pa(0x3000), PAGE, MappingFlags::WRITE)
                .unwrap();
            panic!("unwind the dual cursor");
        }));

        assert!(result.is_err());
        assert_eq!(flushes(), vec![(1, None), (1, None)]);
        assert_eq!(allocation_count(), 7);
        assert_mapping::<Lower>(roots.lower, va(PAGE), pa(0x2000), MappingFlags::READ, PAGE);
        assert_mapping::<Upper>(
            roots.upper,
            va(UPPER_BASE + PAGE),
            pa(0x3000),
            MappingFlags::WRITE,
            PAGE,
        );
    }

    /// Verifies transfer of both geometries to a compatible allocator type.
    ///
    /// Leaking performs no deallocation and the replacement allocator destroys all seven tables.
    #[test]
    fn dual_leak_transfers_every_table_to_compatible_allocator() {
        let _context = TestContext::new();
        let mut table = DualTable::new_alloc().unwrap();
        {
            let mut cursor = table.cursor();
            cursor
                .map(va(PAGE), pa(0x2000), PAGE, MappingFlags::READ)
                .unwrap();
            cursor
                .map(va(UPPER_BASE + PAGE), pa(0x3000), PAGE, MappingFlags::READ)
                .unwrap();
        }
        clear_events();
        let roots = table.leak();
        assert!(events().is_empty());
        // SAFETY: both distinct hierarchies are valid and allocator 2 shares the test backing.
        let transferred = unsafe {
            DualPageTable::<
                VirtAddr,
                Lower,
                TestPte,
                Upper,
                TestPte,
                TestAllocator<2>,
                RecordingFlusher<2>,
            >::new_at_roots(roots.lower, roots.upper)
        };
        assert_eq!(transferred.root(), roots);
        assert_mapping::<Lower>(roots.lower, va(PAGE), pa(0x2000), MappingFlags::READ, PAGE);
        assert_mapping::<Upper>(
            roots.upper,
            va(UPPER_BASE + PAGE),
            pa(0x3000),
            MappingFlags::READ,
            PAGE,
        );

        drop(transferred);

        assert_eq!(deallocations().len(), 7);
        assert!(
            deallocations()
                .iter()
                .all(|&(allocator, _, bytes)| { allocator == 2 && bytes == 32 })
        );
        assert_eq!(allocation_count(), 0);
        assert!(flushes().is_empty());
    }

    /// Verifies replacement of either side preserves the returned hierarchy.
    ///
    /// Lower and upper swaps retain their respective metadata and do not destroy prior tables.
    #[test]
    fn dual_swaps_preserve_previous_lower_and_upper_tables() {
        let _context = TestContext::new();
        let mut table = DualTable::new_alloc().unwrap();
        {
            let mut cursor = table.cursor();
            cursor
                .map(va(PAGE), pa(0x2000), PAGE, MappingFlags::READ)
                .unwrap();
            cursor
                .map(va(UPPER_BASE + PAGE), pa(0x3000), PAGE, MappingFlags::READ)
                .unwrap();
        }
        let old_roots = table.root();
        let mut lower = LowerTable::new_alloc().unwrap();
        lower
            .cursor()
            .map(va(PAGE * 2), pa(0x4000), PAGE, MappingFlags::WRITE)
            .unwrap();
        let mut upper =
            PageTable::<Upper, TestPte, TestAllocator<1>, RecordingFlusher<1>>::new_alloc()
                .unwrap();
        upper
            .cursor()
            .map(
                va(UPPER_BASE + PAGE * 2),
                pa(0x5000),
                PAGE,
                MappingFlags::EXECUTE,
            )
            .unwrap();
        let replacement_roots = DualPageTableRoot {
            lower: lower.root(),
            upper: upper.root(),
        };
        clear_events();

        let previous_lower = table.swap_lower(lower);
        let previous_upper = table.swap_upper(upper);

        assert_eq!(table.root(), replacement_roots);
        assert_eq!(previous_lower.root(), old_roots.lower);
        assert_eq!(previous_upper.root(), old_roots.upper);
        assert!(events().is_empty());
        assert_mapping::<Lower>(
            previous_lower.root(),
            va(PAGE),
            pa(0x2000),
            MappingFlags::READ,
            PAGE,
        );
        assert_mapping::<Upper>(
            previous_upper.root(),
            va(UPPER_BASE + PAGE),
            pa(0x3000),
            MappingFlags::READ,
            PAGE,
        );
        assert_mapping::<Lower>(
            replacement_roots.lower,
            va(PAGE * 2),
            pa(0x4000),
            MappingFlags::WRITE,
            PAGE,
        );
        assert_mapping::<Upper>(
            replacement_roots.upper,
            va(UPPER_BASE + PAGE * 2),
            pa(0x5000),
            MappingFlags::EXECUTE,
            PAGE,
        );
    }

    /// Verifies erased single-root unmapping and destruction retain mapped data.
    ///
    /// Table destruction uses the captured allocator with exact table sizes and adds no flushes.
    #[test]
    fn opaque_single_lifecycle_keeps_data_frames() {
        let _context = TestContext::new();
        let data = allocate_data(PAGE);
        let descriptor = OpaquePageTableType::<VirtAddr>::new::<
            Lower,
            TestPte,
            TestAllocator<1>,
            RecordingFlusher<1>,
        >();
        let mut table = descriptor.new_pagetable_alloc().unwrap();
        let root = single_root(&table);
        table.map(va(PAGE), data, PAGE, MappingFlags::READ).unwrap();
        assert_mapping::<Lower>(root, va(PAGE), data, MappingFlags::READ, PAGE);
        table.unmap(va(PAGE), PAGE).unwrap();
        assert_unmapped::<Lower>(root, va(PAGE));
        assert_eq!(allocation_count(), 4);
        clear_events();

        drop(table);

        assert_eq!(allocation_count(), 1);
        assert_eq!(deallocations().len(), 3);
        assert!(
            deallocations().iter().all(|&(allocator, paddr, bytes)| {
                allocator == 1 && paddr != data && bytes == 32
            })
        );
        assert!(flushes().is_empty());
        deallocate_data(data);
    }

    /// Verifies cloned descriptors create independent tables with captured strategies.
    ///
    /// Constructed tables keep working after both descriptor values have been dropped.
    #[test]
    fn opaque_descriptor_clones_keep_allocator_and_flusher_selection() {
        let _context = TestContext::new();
        let (mut first, second) = {
            let descriptor = OpaquePageTableType::<VirtAddr>::new::<
                Lower,
                TestPte,
                TestAllocator<2>,
                RecordingFlusher<2>,
            >();
            let cloned = descriptor.clone();
            (
                descriptor.new_pagetable_alloc().unwrap(),
                cloned.new_pagetable_alloc().unwrap(),
            )
        };
        assert_ne!(first.root(), second.root());

        first
            .map(va(PAGE), pa(0x3000), PAGE, MappingFlags::READ)
            .unwrap();

        assert_eq!(flushes(), vec![(2, None)]);
        assert!(
            allocations()
                .iter()
                .all(|&(_, _, allocator)| allocator == 2)
        );
        assert_mapping::<Lower>(
            single_root(&first),
            va(PAGE),
            pa(0x3000),
            MappingFlags::READ,
            PAGE,
        );
        assert_unmapped::<Lower>(single_root(&second), va(PAGE));
        drop(first);
        drop(second);
        assert_eq!(deallocations().len(), 4);
        assert!(
            deallocations()
                .iter()
                .all(|&(allocator, _, _)| allocator == 2)
        );
    }

    /// Verifies erased roots can transfer to a compatible allocator and flusher.
    ///
    /// Reconstruction does not allocate or initialize existing tables and subsequent calls use
    /// the replacement descriptor's statically selected methods.
    #[test]
    fn opaque_leak_transfers_single_root_between_compatible_allocators() {
        let _context = TestContext::new();
        let first_descriptor = OpaquePageTableType::<VirtAddr>::new::<
            Lower,
            TestPte,
            TestAllocator<1>,
            RecordingFlusher<1>,
        >();
        let mut first = first_descriptor.new_pagetable_alloc().unwrap();
        first
            .map(va(0), pa(0x2000), PAGE, MappingFlags::READ)
            .unwrap();
        let root = single_root(&first);
        clear_events();
        let erased_root = first.leak();
        let second_descriptor = OpaquePageTableType::<VirtAddr>::new::<
            Lower,
            TestPte,
            TestAllocator<2>,
            RecordingFlusher<2>,
        >();
        // SAFETY: the format is unchanged and allocator 2 shares the test backing.
        let mut second = unsafe { second_descriptor.new_pagetable_at(erased_root) };
        assert!(events().is_empty());
        assert_eq!(single_root(&second), root);
        assert_mapping::<Lower>(root, va(0), pa(0x2000), MappingFlags::READ, PAGE);

        second
            .map(va(PAGE), pa(0x3000), PAGE, MappingFlags::WRITE)
            .unwrap();

        assert_eq!(flushes(), vec![(2, Some(va(PAGE)))]);
        assert_mapping::<Lower>(root, va(PAGE), pa(0x3000), MappingFlags::WRITE, PAGE);
        drop(second);
        assert_eq!(deallocations().len(), 3);
        assert!(
            deallocations()
                .iter()
                .all(|&(allocator, _, bytes)| { allocator == 2 && bytes == 32 })
        );
    }

    /// Verifies successful erased actions transfer cleanup without processing it.
    ///
    /// The captured flusher runs when the batch is explicitly processed and an empty batch is
    /// subsequently inert.
    #[test]
    fn opaque_success_returns_cleanup_and_empty_actions_stay_inert() {
        let _context = TestContext::new();
        let descriptor = OpaquePageTableType::<VirtAddr>::new::<
            Lower,
            TestPte,
            TestAllocator<1>,
            RecordingFlusher<2>,
        >();
        let mut table = descriptor.new_pagetable_alloc().unwrap();
        let root = single_root(&table);
        let cleanup = table
            .apply_actions_with_cleanup([PageTableAction::Map {
                vaddr: va(PAGE),
                paddr: pa(0x2000),
                size: PAGE,
                flags: MappingFlags::READ,
            }])
            .unwrap();
        assert!(flushes().is_empty());
        assert_eq!(allocation_count(), 3);

        cleanup.clean_up();

        assert_eq!(flushes(), vec![(2, None)]);
        assert_mapping::<Lower>(root, va(PAGE), pa(0x2000), MappingFlags::READ, PAGE);
        clear_events();
        table
            .apply_actions_with_cleanup(core::iter::empty::<PageTableAction<VirtAddr>>())
            .unwrap()
            .clean_up();
        assert!(events().is_empty());
    }

    /// Verifies action order through huge-page splitting and remapping.
    ///
    /// The immediate wrapper processes the combined full invalidation before returning.
    #[test]
    fn opaque_ordered_actions_split_huge_page_and_replace_one_base_page() {
        let _context = TestContext::new();
        let descriptor = OpaquePageTableType::<VirtAddr>::new::<
            Lower,
            TestPte,
            TestAllocator<1>,
            RecordingFlusher<1>,
        >();
        let mut table = descriptor.new_pagetable_alloc().unwrap();
        let root = single_root(&table);

        table
            .apply_actions([
                PageTableAction::Map {
                    vaddr: va(0),
                    paddr: pa(0x40000),
                    size: 0x10000,
                    flags: MappingFlags::READ,
                },
                PageTableAction::Unmap {
                    vaddr: va(PAGE),
                    size: PAGE,
                },
                PageTableAction::Map {
                    vaddr: va(PAGE),
                    paddr: pa(0x90000),
                    size: PAGE,
                    flags: MappingFlags::WRITE,
                },
            ])
            .unwrap();

        assert_eq!(flushes(), vec![(1, None)]);
        assert_mapping::<Lower>(root, va(0), pa(0x40000), MappingFlags::READ, PAGE);
        assert_mapping::<Lower>(root, va(PAGE), pa(0x90000), MappingFlags::WRITE, PAGE);
        assert_mapping::<Lower>(root, va(0x4000), pa(0x44000), MappingFlags::READ, 0x4000);
    }

    /// Verifies an invalid later action preserves and flushes earlier actions.
    ///
    /// Returning an error keeps the erased hierarchy intact and permits a subsequent mutation.
    #[test]
    fn opaque_single_action_error_keeps_prior_mapping_and_flushes() {
        let _context = TestContext::new();
        let descriptor = OpaquePageTableType::<VirtAddr>::new::<
            Lower,
            TestPte,
            TestAllocator<1>,
            RecordingFlusher<1>,
        >();
        let mut table = descriptor.new_pagetable_alloc().unwrap();
        let root = single_root(&table);
        let result = table.apply_actions_with_cleanup([
            PageTableAction::Map {
                vaddr: va(0),
                paddr: pa(0x2000),
                size: PAGE,
                flags: MappingFlags::READ,
            },
            PageTableAction::Unmap {
                vaddr: va(LOWER_SPAN),
                size: PAGE,
            },
        ]);

        assert!(matches!(result, Err(PagingError::NonCanonical { .. })));
        assert_eq!(flushes(), vec![(1, None)]);
        assert_eq!(single_root(&table), root);
        assert_eq!(allocation_count(), 3);
        assert_mapping::<Lower>(root, va(0), pa(0x2000), MappingFlags::READ, PAGE);
        table
            .map(va(PAGE), pa(0x3000), PAGE, MappingFlags::WRITE)
            .unwrap();
        assert_mapping::<Lower>(root, va(PAGE), pa(0x3000), MappingFlags::WRITE, PAGE);
    }

    /// Verifies allocation failure keeps a partially installed path valid.
    ///
    /// The temporary cursor flushes the new table descriptor and later allocation can complete
    /// the same path without replacing its root.
    #[test]
    fn opaque_allocation_error_flushes_partial_path_and_allows_retry() {
        let _context = TestContext::new();
        let descriptor = OpaquePageTableType::<VirtAddr>::new::<
            Lower,
            TestPte,
            TestAllocator<1>,
            RecordingFlusher<1>,
        >();
        let mut table = descriptor.new_pagetable_alloc().unwrap();
        let root = single_root(&table);
        fail_after(1);

        let result = table.apply_actions_with_cleanup([PageTableAction::Map {
            vaddr: va(PAGE),
            paddr: pa(0x2000),
            size: PAGE,
            flags: MappingFlags::READ,
        }]);

        assert!(matches!(result, Err(PagingError::AllocationFailed)));
        assert_eq!(flushes(), vec![(1, None)]);
        assert_eq!(allocation_count(), 2);
        assert_eq!(single_root(&table), root);
        assert_unmapped::<Lower>(root, va(PAGE));
        assert!(deallocations().is_empty());
        allow_allocations();
        table
            .map(va(PAGE), pa(0x2000), PAGE, MappingFlags::READ)
            .unwrap();
        assert_eq!(allocation_count(), 3);
        assert_mapping::<Lower>(root, va(PAGE), pa(0x2000), MappingFlags::READ, PAGE);
    }

    /// Verifies an action iterator panic flushes completed single-root mutations.
    ///
    /// Unwinding must not destroy the temporary concrete hierarchy used by erased operations.
    #[test]
    fn opaque_single_iterator_panic_keeps_hierarchy_and_flushes() {
        let _context = TestContext::new();
        let descriptor = OpaquePageTableType::<VirtAddr>::new::<
            Lower,
            TestPte,
            TestAllocator<1>,
            RecordingFlusher<1>,
        >();
        let mut table = descriptor.new_pagetable_alloc().unwrap();
        let root = single_root(&table);
        let actions = core::iter::once(PageTableAction::Map {
            vaddr: va(PAGE),
            paddr: pa(0x2000),
            size: PAGE,
            flags: MappingFlags::READ,
        })
        .chain(core::iter::from_fn(
            || -> Option<PageTableAction<VirtAddr>> {
                panic!("panic after a single-root action");
            },
        ));

        let result = catch_unwind(AssertUnwindSafe(|| {
            table.apply_actions_with_cleanup(actions)
        }));

        assert!(result.is_err());
        assert_eq!(flushes(), vec![(1, None)]);
        assert_eq!(single_root(&table), root);
        assert_eq!(allocation_count(), 3);
        assert!(deallocations().is_empty());
        assert_mapping::<Lower>(root, va(PAGE), pa(0x2000), MappingFlags::READ, PAGE);
        table.unmap(va(PAGE), PAGE).unwrap();
        assert_unmapped::<Lower>(root, va(PAGE));
    }

    /// Verifies subtree deallocation happens during erased mutation before cleanup.
    ///
    /// Replacing descendants with a huge leaf deallocates only table allocations in post-order
    /// and retains the formerly mapped data allocation.
    #[test]
    fn opaque_subtree_replacement_deallocates_before_deferred_flush() {
        let _context = TestContext::new();
        let data = allocate_data(PAGE);
        let descriptor = OpaquePageTableType::<VirtAddr>::new::<
            Lower,
            TestPte,
            TestAllocator<1>,
            RecordingFlusher<1>,
        >();
        let mut table = descriptor.new_pagetable_alloc().unwrap();
        let root = single_root(&table);
        table.map(va(PAGE), data, PAGE, MappingFlags::READ).unwrap();
        let middle = read_entry(root, 0).paddr();
        let bottom = read_entry(middle, 0).paddr();
        clear_events();

        let cleanup = table
            .apply_actions_with_cleanup([PageTableAction::Map {
                vaddr: va(0),
                paddr: pa(0x80000),
                size: 0x10000,
                flags: MappingFlags::WRITE,
            }])
            .unwrap();

        assert_eq!(deallocations(), vec![(1, bottom, 32), (1, middle, 32)]);
        assert_eq!(allocation_count(), 2);
        assert!(flushes().is_empty());
        assert!(
            events()
                .iter()
                .all(|event| matches!(event, Event::Deallocated { .. }))
        );
        cleanup.clean_up();
        assert_eq!(flushes(), vec![(1, None)]);
        assert_mapping::<Lower>(root, va(0), pa(0x80000), MappingFlags::WRITE, 0x10000);
        assert!(allocations().iter().any(|&(paddr, _, _)| paddr == data));
        drop(table);
        assert_eq!(allocation_count(), 1);
        deallocate_data(data);
    }

    /// Verifies erased dual-root success combines both roots' cleanup.
    ///
    /// Topology changes become one full flush while later base-page removals retain both
    /// individual virtual addresses and the descriptor's selected strategy.
    #[test]
    fn opaque_dual_success_combines_both_sides_into_returned_batch() {
        let _context = TestContext::new();
        let descriptor = OpaquePageTableType::<VirtAddr>::new_dual::<
            Lower,
            TestPte,
            Upper,
            TestPte,
            TestAllocator<1>,
            RecordingFlusher<2>,
        >();
        let mut table = descriptor.new_pagetable_alloc().unwrap();
        let roots = dual_roots(&table);
        let low = va(PAGE);
        let high = va(UPPER_BASE + PAGE);
        let cleanup = table
            .apply_actions_with_cleanup([
                PageTableAction::Map {
                    vaddr: low,
                    paddr: pa(0x2000),
                    size: PAGE,
                    flags: MappingFlags::READ,
                },
                PageTableAction::Map {
                    vaddr: high,
                    paddr: pa(0x3000),
                    size: PAGE,
                    flags: MappingFlags::WRITE,
                },
            ])
            .unwrap();
        assert!(flushes().is_empty());
        cleanup.clean_up();
        assert_eq!(flushes(), vec![(2, None)]);
        assert_mapping::<Lower>(roots.lower, low, pa(0x2000), MappingFlags::READ, PAGE);
        assert_mapping::<Upper>(roots.upper, high, pa(0x3000), MappingFlags::WRITE, PAGE);
        clear_events();

        table
            .apply_actions([
                PageTableAction::Unmap {
                    vaddr: low,
                    size: PAGE,
                },
                PageTableAction::Unmap {
                    vaddr: high,
                    size: PAGE,
                },
            ])
            .unwrap();

        assert_eq!(flushes(), vec![(2, Some(low)), (2, Some(high))]);
        assert_unmapped::<Lower>(roots.lower, low);
        assert_unmapped::<Upper>(roots.upper, high);
    }

    /// Verifies an erased dual-root error flushes both temporary cursors.
    ///
    /// Earlier actions on both sides persist and the two independent full requests are each
    /// processed during cursor destruction on the error path.
    #[test]
    fn opaque_dual_action_error_preserves_both_sides_and_flushes_each() {
        let _context = TestContext::new();
        let descriptor = OpaquePageTableType::<VirtAddr>::new_dual::<
            Lower,
            TestPte,
            Upper,
            TestPte,
            TestAllocator<1>,
            RecordingFlusher<1>,
        >();
        let mut table = descriptor.new_pagetable_alloc().unwrap();
        let roots = dual_roots(&table);

        let result = table.apply_actions_with_cleanup([
            PageTableAction::Map {
                vaddr: va(PAGE),
                paddr: pa(0x2000),
                size: PAGE,
                flags: MappingFlags::READ,
            },
            PageTableAction::Map {
                vaddr: va(UPPER_BASE + PAGE),
                paddr: pa(0x3000),
                size: PAGE,
                flags: MappingFlags::WRITE,
            },
            PageTableAction::Unmap {
                vaddr: va(LOWER_SPAN),
                size: PAGE,
            },
        ]);

        assert!(matches!(result, Err(PagingError::NonCanonical { .. })));
        assert_eq!(flushes(), vec![(1, None), (1, None)]);
        assert_eq!(dual_roots(&table), roots);
        assert_eq!(allocation_count(), 7);
        assert!(deallocations().is_empty());
        assert_mapping::<Lower>(roots.lower, va(PAGE), pa(0x2000), MappingFlags::READ, PAGE);
        assert_mapping::<Upper>(
            roots.upper,
            va(UPPER_BASE + PAGE),
            pa(0x3000),
            MappingFlags::WRITE,
            PAGE,
        );
    }

    /// Verifies an iterator panic preserves mutations to both erased roots.
    ///
    /// Unwinding flushes both concrete cursors while the erased table retains all allocations.
    #[test]
    fn opaque_dual_iterator_panic_keeps_both_hierarchies_and_flushes() {
        let _context = TestContext::new();
        let descriptor = OpaquePageTableType::<VirtAddr>::new_dual::<
            Lower,
            TestPte,
            Upper,
            TestPte,
            TestAllocator<1>,
            RecordingFlusher<1>,
        >();
        let mut table = descriptor.new_pagetable_alloc().unwrap();
        let roots = dual_roots(&table);
        let actions = [
            PageTableAction::Map {
                vaddr: va(PAGE),
                paddr: pa(0x2000),
                size: PAGE,
                flags: MappingFlags::READ,
            },
            PageTableAction::Map {
                vaddr: va(UPPER_BASE + PAGE),
                paddr: pa(0x3000),
                size: PAGE,
                flags: MappingFlags::WRITE,
            },
        ]
        .into_iter()
        .chain(core::iter::from_fn(
            || -> Option<PageTableAction<VirtAddr>> {
                panic!("panic after actions on both roots");
            },
        ));

        let result = catch_unwind(AssertUnwindSafe(|| {
            table.apply_actions_with_cleanup(actions)
        }));

        assert!(result.is_err());
        assert_eq!(flushes(), vec![(1, None), (1, None)]);
        assert_eq!(dual_roots(&table), roots);
        assert_eq!(allocation_count(), 7);
        assert!(deallocations().is_empty());
        assert_mapping::<Lower>(roots.lower, va(PAGE), pa(0x2000), MappingFlags::READ, PAGE);
        assert_mapping::<Upper>(
            roots.upper,
            va(UPPER_BASE + PAGE),
            pa(0x3000),
            MappingFlags::WRITE,
            PAGE,
        );
        table.unmap(va(PAGE), PAGE).unwrap();
        table.unmap(va(UPPER_BASE + PAGE), PAGE).unwrap();
        assert_unmapped::<Lower>(roots.lower, va(PAGE));
        assert_unmapped::<Upper>(roots.upper, va(UPPER_BASE + PAGE));
    }

    /// Verifies dummy allocation and mutation report bugs while access and destruction are inert.
    ///
    /// The zero-root sentinel never translates a physical address or allocates table memory.
    #[test]
    fn opaque_dummy_reports_bugs_for_operations_and_keeps_sentinel_inert() {
        let _context = TestContext::new();
        let descriptor = OpaquePageTableType::<VirtAddr>::dummy();
        let allocation = catch_unwind(AssertUnwindSafe(|| descriptor.new_pagetable_alloc()));
        assert!(allocation.is_err());
        let mut table = OpaquePageTable::<VirtAddr>::dummy();
        assert_eq!(table.root(), OpaquePageTableRoot::Single(pa(0)));
        let mutation = catch_unwind(AssertUnwindSafe(|| {
            table.map(va(0), pa(0), PAGE, MappingFlags::READ)
        }));
        assert!(mutation.is_err());
        drop(table);
        assert_eq!(
            OpaquePageTable::<VirtAddr>::dummy().leak(),
            OpaquePageTableRoot::Single(pa(0))
        );
        assert_eq!(allocation_count(), 0);
        assert!(events().is_empty());
    }
}
