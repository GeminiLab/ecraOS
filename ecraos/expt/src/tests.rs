//! Unit tests for multi-level page table management.
//!
//! Compact and multi-frame formats exercise hierarchy traversal, mapping changes,
//! table-frame management, and local TLB invalidation with a deterministic host allocator.

use std::{
    alloc::{Layout, alloc, dealloc},
    cell::RefCell,
    panic::{AssertUnwindSafe, catch_unwind},
    vec::Vec,
};

use expalloc_trait::{DynPageAllocator, PageAllocator};
use memory_addr::{AddrRange, PhysAddr, VirtAddr};
use page_table_entry::{GenericPTE, MappingFlags};

use crate::{
    error::PagingError,
    meta::{LowerCoverage, PageTableCoverage, PageTableMeta, SymmetricCoverage, UpperCoverage},
    opaque::{OpaquePageTable, OpaquePageTableRoot, OpaquePageTableType, PageTableAction},
    pt::{
        DualPageTable, PageTable, PageTableCursorLike,
        cleanup::{SMALL_FLUSH_THRESHOLD, TlbFlush, TlbFlushes},
        single::checked_page_end,
    },
};

/// The tag for an unused test entry.
///
/// Zero makes freshly cleared table memory represent unused entries.
const PTE_UNUSED: u8 = 0;
/// The tag for a child page table test entry.
///
/// Entries with this tag point to a page table at the next lower level.
const PTE_TABLE: u8 = 1;
/// The tag for a base-page test entry.
///
/// Entries with this tag map one level-zero page.
const PTE_PAGE: u8 = 2;
/// The tag for a huge-page test entry.
///
/// Entries with this tag terminate traversal above level zero.
const PTE_HUGE: u8 = 3;

/// A compact page table entry used by the unit tests.
///
/// The explicit kind byte distinguishes child tables, base pages, huge pages,
/// and the all-zero unused representation.
#[derive(Clone, Copy, Debug)]
struct TestPte {
    /// The physical address stored in the entry.
    ///
    /// It identifies either a child table or the start of a mapped page.
    paddr: usize,
    /// The mapping flags stored for a leaf entry.
    ///
    /// Child page table and unused entries keep this value empty.
    flags: MappingFlags,
    /// The entry kind encoded by one of the `PTE_*` constants.
    ///
    /// This field drives the [`GenericPTE`] classification methods.
    kind: u8,
    /// Padding that gives test entries a nontrivial fixed size.
    ///
    /// The bytes are always initialized to zero and carry no semantics.
    _reserved: [u8; 7],
}

impl TestPte {
    /// Creates an unused, all-zero test entry.
    ///
    /// This matches the representation produced by clearing an allocated table.
    fn empty() -> Self {
        Self {
            paddr: 0,
            flags: MappingFlags::empty(),
            kind: PTE_UNUSED,
            _reserved: [0; 7],
        }
    }
}

impl Default for TestPte {
    /// Returns an unused test entry.
    ///
    /// The default representation is intentionally identical to zeroed memory.
    fn default() -> Self {
        Self::empty()
    }
}

impl GenericPTE for TestPte {
    /// Creates a base-page or huge-page test entry.
    ///
    /// `is_huge` selects the entry tag without changing the stored address or
    /// flags.
    fn new_page(paddr: PhysAddr, flags: MappingFlags, is_huge: bool) -> Self {
        Self {
            paddr: paddr.as_usize(),
            flags,
            kind: if is_huge { PTE_HUGE } else { PTE_PAGE },
            _reserved: [0; 7],
        }
    }
    /// Creates a child page table test entry.
    ///
    /// Table entries store no leaf mapping flags.
    fn new_table(paddr: PhysAddr) -> Self {
        Self {
            paddr: paddr.as_usize(),
            flags: MappingFlags::empty(),
            kind: PTE_TABLE,
            _reserved: [0; 7],
        }
    }
    /// Returns the physical address stored by the entry.
    ///
    /// The raw integer field is converted back to its semantic address type.
    fn paddr(&self) -> PhysAddr {
        PhysAddr::from_usize(self.paddr)
    }
    /// Returns the leaf mapping flags stored by the entry.
    ///
    /// Unused and child page table entries return an empty flag set.
    fn flags(&self) -> MappingFlags {
        self.flags
    }
    /// Replaces the physical address stored by the entry.
    ///
    /// This operation preserves the entry kind and mapping flags.
    fn set_paddr(&mut self, paddr: PhysAddr) {
        self.paddr = paddr.as_usize();
    }
    /// Replaces the mapping flags and leaf kind.
    ///
    /// `is_huge` selects between the base-page and huge-page tags.
    fn set_flags(&mut self, flags: MappingFlags, is_huge: bool) {
        self.flags = flags;
        self.kind = if is_huge { PTE_HUGE } else { PTE_PAGE };
    }
    /// Returns a compact bit representation for test assertions.
    ///
    /// The representation combines the aligned physical address with the entry
    /// kind stored in its low bits.
    fn bits(self) -> usize {
        self.paddr | self.kind as usize
    }
    /// Reports whether the entry is unused.
    ///
    /// Only the all-zero entry kind is considered unused.
    fn is_unused(&self) -> bool {
        self.kind == PTE_UNUSED
    }
    /// Reports whether the entry is present.
    ///
    /// Child tables and both leaf kinds are considered present.
    fn is_present(&self) -> bool {
        matches!(self.kind, PTE_TABLE | PTE_PAGE | PTE_HUGE)
    }
    /// Reports whether the entry maps a huge page.
    ///
    /// Base-page leaves and child tables return `false`.
    fn is_huge(&self) -> bool {
        self.kind == PTE_HUGE
    }
    /// Clears the entry to its unused representation.
    ///
    /// The address, flags, kind, and padding are reset together.
    fn clear(&mut self) {
        *self = Self::empty();
    }
}

/// Metadata for the compact page table hierarchy geometry used in tests.
///
/// Three two-bit levels keep test allocations and address ranges small while
/// still exercising every traversal branch.
struct TestMeta;

impl PageTableMeta for TestMeta {
    /// The virtual-address type used by the test format.
    ///
    /// Tests use the repository's standard [`VirtAddr`] wrapper.
    type VirtAddr = VirtAddr;
    /// The three levels used by the test format.
    ///
    /// This exercises traversal beyond a single intermediate table.
    const LEVELS: usize = 3;
    /// The twelve-bit offset of a 4 KiB base page.
    ///
    /// Test allocations use the same alignment.
    const PAGE_OFFSET_BITS: usize = 12;
    /// The two virtual-address bits handled at each test level.
    ///
    /// Every table therefore contains four entries.
    const LEVEL_BITS: [usize; Self::LEVELS] = [2, 2, 2];
    /// The compact test format covers the lower virtual-address range.
    ///
    /// Addresses outside that canonical range are rejected before traversal.
    type Coverage = LowerCoverage<Self::VirtAddr>;
    /// The highest level permitted to contain a test leaf.
    ///
    /// Every level in the compact format supports page mappings.
    const MAX_PAGE_LEVEL: usize = 2;
}

/// Metadata for the upper half of the dual-root page table hierarchy tests.
///
/// Its geometry matches the lower test format while restricting mappings to upper addresses.
struct TestUpperMeta;

impl PageTableMeta for TestUpperMeta {
    /// The virtual-address type used by the test format.
    ///
    /// Both halves use the repository's standard semantic address wrapper.
    type VirtAddr = VirtAddr;
    /// The three levels used by the dual-root test format.
    ///
    /// The upper page table hierarchy can contain two levels of child tables.
    const LEVELS: usize = 3;
    /// The twelve-bit offset of a 4 KiB base page.
    ///
    /// The shared test allocator uses the same frame size.
    const PAGE_OFFSET_BITS: usize = 12;
    /// The two virtual-address bits handled at each test level.
    ///
    /// Every upper table contains four entries.
    const LEVEL_BITS: [usize; Self::LEVELS] = [2, 2, 2];
    /// The compact test format covers the upper virtual-address range.
    ///
    /// Lower addresses are dispatched to the separate lower page table hierarchy.
    type Coverage = UpperCoverage<Self::VirtAddr>;
    /// Every level in the compact format supports page mappings.
    ///
    /// Huge leaves therefore exercise the same destruction paths as lower mappings.
    const MAX_PAGE_LEVEL: usize = 2;
}

/// Metadata for testing symmetric canonical-address validation.
///
/// The compact geometry keeps the canonical-boundary tests independent from a
/// target architecture's page table hierarchy implementation.
struct SymmetricTestMeta;

impl PageTableMeta for SymmetricTestMeta {
    /// The virtual-address type used by the symmetric validation tests.
    ///
    /// Tests validate the repository's standard semantic address wrapper.
    type VirtAddr = VirtAddr;
    /// The three levels used by the validation tests.
    ///
    /// The compact geometry leaves an easily constructed canonical hole.
    const LEVELS: usize = 3;
    /// The twelve-bit offset of a 4 KiB base page.
    ///
    /// Every mapped page therefore uses the same size as the host allocator's frame.
    const PAGE_OFFSET_BITS: usize = 12;
    /// The two virtual-address bits handled at each test level.
    ///
    /// Each page table contains four entries.
    const LEVEL_BITS: [usize; Self::LEVELS] = [2, 2, 2];
    /// The format covers canonical lower and upper ranges symmetrically.
    ///
    /// Its coverage includes both canonical sides with a gap between them.
    type Coverage = SymmetricCoverage<Self::VirtAddr>;
}

/// Metadata whose table levels each require multiple allocator frames.
///
/// The differing entry counts exercise child page table and root byte sizes during destruction.
struct MultiFrameTestMeta;

impl PageTableMeta for MultiFrameTestMeta {
    /// The semantic virtual-address type used by the large test format.
    ///
    /// Host tests use the same address wrapper as compact page table hierarchies.
    type VirtAddr = VirtAddr;
    /// The number of levels in the large test format.
    ///
    /// A base-page mapping allocates two child tables beneath its root.
    const LEVELS: usize = 3;
    /// The offset width of a 4 KiB mapped page.
    ///
    /// The allocator also uses 4 KiB frames.
    const PAGE_OFFSET_BITS: usize = 12;
    /// The index widths for the three table levels.
    ///
    /// The middle level contains twice as many entries as the other levels.
    const LEVEL_BITS: [usize; Self::LEVELS] = [9, 10, 9];
    /// The highest leaf level used to exercise replacement of a large subtree.
    ///
    /// Level two covers the entire middle-level table in the record-growth failure test.
    const MAX_PAGE_LEVEL: usize = 2;
    /// The canonical coverage of the large test format.
    ///
    /// Every mapping belongs to its lower virtual-address range.
    type Coverage = LowerCoverage<Self::VirtAddr>;
}

/// The resolved mapping observed by the test page table hierarchy walker.
///
/// A snapshot includes the physical address corresponding to the queried
/// virtual address, the leaf flags, and the leaf's mapping size.
#[derive(Clone, Copy, Debug, PartialEq)]
struct MappingSnapshot {
    /// The translated physical address for the query.
    ///
    /// This includes the query's offset within its mapped page.
    paddr: usize,
    /// The flags stored in the mapping's leaf entry.
    ///
    /// These are compared with the flags supplied to mapping operations.
    flags: MappingFlags,
    /// The byte size mapped by the leaf entry.
    ///
    /// This distinguishes base-page mappings from huge mappings.
    page_size: usize,
}

/// A host allocation backing one or more simulated physical frames.
///
/// The raw pointer doubles as the physical address because the test allocator
/// uses an identity translation.
#[derive(Clone, Copy)]
struct AllocationRecord {
    /// The host allocation address stored as an integer.
    ///
    /// It is converted to a pointer only at allocation boundaries.
    ptr: usize,
    /// The allocation layout required for deallocation.
    ///
    /// Its size also records how many frames belong to this allocation.
    layout: Layout,
}

/// The thread-local state of the test frame allocator.
///
/// It tracks live host allocations, the allocation sequence number, and an
/// optional sequence number at which allocation should fail.
#[derive(Default)]
struct AllocState {
    /// The host allocations that remain live.
    ///
    /// Records are removed by explicit deallocation or test-state cleanup.
    allocations: Vec<AllocationRecord>,
    /// The number of allocation attempts since the last reset.
    ///
    /// The count starts at one for failure-injection comparisons.
    alloc_count: usize,
    /// The allocation attempt that should return [`None`].
    ///
    /// A missing value disables failure injection.
    fail_on_alloc: Option<usize>,
    /// The allocation attempt that should panic before allocating memory.
    panic_on_alloc: Option<usize>,
    /// The byte sizes requested when deallocating table frames.
    ///
    /// The record is captured before frame rounding so incorrect byte arguments remain visible.
    dealloc_requests: Vec<(PhysAddr, usize)>,
    /// The successfully deallocated frames in their observed order.
    ///
    /// Each record identifies an allocation that was removed from the live set exactly once.
    deallocations: Vec<(PhysAddr, usize)>,
    /// The completed local flush count observed at each frame deallocation.
    deallocation_flush_counts: Vec<usize>,
}

impl AllocState {
    /// Deallocates every live host allocation.
    ///
    /// The allocation list is drained so repeated cleanup is harmless.
    fn dealloc_all(&mut self) {
        for record in self.allocations.drain(..) {
            unsafe { dealloc(record.ptr as *mut u8, record.layout) };
        }
    }
}

impl Drop for AllocState {
    /// Deallocates allocations left in the test state.
    ///
    /// This prevents leaked host memory if a test exits before explicit cleanup.
    fn drop(&mut self) {
        self.dealloc_all();
    }
}

std::thread_local! {
    /// The allocator state isolated to the current test thread.
    ///
    /// Thread-local storage prevents concurrently executed tests from sharing
    /// allocations or failure-injection counters.
    static ALLOC_STATE: RefCell<AllocState> = RefCell::new(AllocState::default());
    /// The TLB invalidations observed on the current test thread.
    ///
    /// [`Some`] values record page invalidations and [`None`] records full flushes.
    static FLUSH_LOG: RefCell<Vec<Option<usize>>> = const { RefCell::new(Vec::new()) };
}

/// A host-backed [`PageAllocator`] used by the page table hierarchy tests.
///
/// Host pointers act as both physical and virtual addresses, and every
/// allocation uses 4 KiB alignment.
struct TestPagingHandler;

impl PageAllocator for TestPagingHandler {
    /// Returns the shift for the allocator's 4 KiB frame size.
    ///
    /// This matches [`TestMeta::PAGE_OFFSET_BITS`].
    fn page_size_shift() -> usize {
        12
    }

    /// Allocates a page-aligned host buffer for contiguous test frames.
    ///
    /// The attempt is recorded and may fail at the sequence number configured by
    /// [`fail_on_alloc`].
    fn alloc_frames(page_count: usize) -> Option<PhysAddr> {
        ALLOC_STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.alloc_count += 1;
            assert_ne!(
                state.panic_on_alloc,
                Some(state.alloc_count),
                "injected allocator panic"
            );
            if state.fail_on_alloc == Some(state.alloc_count) {
                return None;
            }
            let bytes_required = page_count << Self::page_size_shift();
            let layout = Layout::from_size_align(bytes_required, 4096).ok()?;
            let ptr = unsafe { alloc(layout) };
            if ptr.is_null() {
                return None;
            }
            state.allocations.push(AllocationRecord {
                ptr: ptr as usize,
                layout,
            });
            Some(PhysAddr::from_usize(ptr as usize))
        })
    }

    /// Deallocates a host buffer previously returned by the test allocator.
    ///
    /// The frame count is checked against the original allocation size.
    fn dealloc_frames(addr: PhysAddr, page_count: usize) {
        ALLOC_STATE.with(|state| {
            let mut state = state.borrow_mut();
            let index = state
                .allocations
                .iter()
                .position(|record| record.ptr == addr.as_usize())
                .expect("deallocating an unknown test page-table allocation");
            let record = state.allocations[index];
            assert_eq!(record.layout.size(), page_count << Self::page_size_shift());
            let record = state.allocations.remove(index);
            state.deallocations.push((addr, page_count));
            state.deallocation_flush_counts.push(flush_log().len());
            unsafe { dealloc(record.ptr as *mut u8, record.layout) };
        });
    }

    /// Records an exact byte request before deallocating its covering frames.
    ///
    /// Tests can distinguish table entry counts from byte sizes even when both round to one frame.
    fn dealloc_frames_of_size(addr: PhysAddr, size_in_bytes: usize) {
        ALLOC_STATE.with(|state| {
            state
                .borrow_mut()
                .dealloc_requests
                .push((addr, size_in_bytes));
        });
        Self::dealloc_frames(
            addr,
            size_in_bytes.div_ceil(1usize << Self::page_size_shift()),
        );
    }
    /// Converts a simulated physical address to its host virtual address.
    ///
    /// The test allocator uses an identity mapping between the two domains.
    fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
        VirtAddr::from_usize(addr.as_usize())
    }
}

/// A second allocator type for table-frame transfer tests.
///
/// It delegates to the same deterministic allocation state as the primary test allocator.
struct AlternateTestPagingHandler;

impl PageAllocator for AlternateTestPagingHandler {
    /// Returns the shift for the shared 4 KiB frame size.
    ///
    /// Both allocator types use the page size required by the test metadata.
    fn page_size_shift() -> usize {
        TestPagingHandler::page_size_shift()
    }

    /// Allocates through the shared test allocation state.
    ///
    /// The allocator type differs while the returned test frames remain interchangeable.
    fn alloc_frames(page_count: usize) -> Option<PhysAddr> {
        TestPagingHandler::alloc_frames(page_count)
    }

    /// Deallocates through the shared test allocation state.
    ///
    /// This lets a value created with the alternate type deallocate transferred roots.
    fn dealloc_frames(addr: PhysAddr, page_count: usize) {
        TestPagingHandler::dealloc_frames(addr, page_count)
    }

    /// Converts a physical address using the shared identity mapping.
    ///
    /// Test frame addresses are host pointers for either allocator type.
    fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
        TestPagingHandler::phys_to_virt(addr)
    }
}

/// Restores the allocator and TLB log to their initial state.
///
/// Live allocations are deallocated and failure injection is disabled.
fn reset_test_state() {
    ALLOC_STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.dealloc_all();
        state.alloc_count = 0;
        state.fail_on_alloc = None;
        state.panic_on_alloc = None;
        state.dealloc_requests.clear();
        state.deallocations.clear();
        state.deallocation_flush_counts.clear();
    });
    FLUSH_LOG.with(|log| log.borrow_mut().clear());
}

/// Configures one allocation attempt to fail.
///
/// `n` is compared with the one-based allocation count since the last reset.
fn fail_on_alloc(n: usize) {
    ALLOC_STATE.with(|state| state.borrow_mut().fail_on_alloc = Some(n));
}
/// Reports whether an exact allocation record exists.
///
/// Both the starting physical address and byte size must match a live record.
fn allocation_contains(paddr: PhysAddr, bytes: usize) -> bool {
    ALLOC_STATE.with(|state| {
        state
            .borrow()
            .allocations
            .iter()
            .any(|record| record.ptr == paddr.as_usize() && record.layout.size() == bytes)
    })
}

/// Returns the number of live host allocations in the current test state.
///
/// The count distinguishes deallocated table frames from caller-managed data frames.
fn allocation_count() -> usize {
    ALLOC_STATE.with(|state| state.borrow().allocations.len())
}

/// Returns the exact byte requests observed during table destruction.
///
/// The cloned sequence preserves child-before-parent ordering for assertions.
fn dealloc_requests() -> Vec<(PhysAddr, usize)> {
    ALLOC_STATE.with(|state| state.borrow().dealloc_requests.clone())
}

/// Returns the successfully deallocated frame allocations.
///
/// The cloned log makes repeated or missing deallocations observable after multi-level page
/// tables are dropped.
fn deallocations() -> Vec<(PhysAddr, usize)> {
    ALLOC_STATE.with(|state| state.borrow().deallocations.clone())
}
/// Returns a snapshot of recorded TLB invalidations.
///
/// Cloning the log allows assertions without retaining a thread-local borrow.
fn flush_log() -> Vec<Option<usize>> {
    FLUSH_LOG.with(|log| log.borrow().clone())
}
/// Clears all recorded TLB invalidations.
///
/// Allocator state and page table hierarchy contents are left unchanged.
fn clear_flush_log() {
    FLUSH_LOG.with(|log| log.borrow_mut().clear());
}

/// Records an architecture-local invalidation without executing privileged instructions.
///
/// This replaces only the hardware boundary in host tests, independently of hierarchy metadata.
pub(crate) fn record_local_flush(vaddr: Option<VirtAddr>) {
    FLUSH_LOG.with(|log| log.borrow_mut().push(vaddr.map(VirtAddr::as_usize)));
}

/// Checks explicit, implicit, and ignored processing of transferred invalidations.
///
/// Explicit processing must not repeat during Drop, and ignore must suppress all local operations.
#[test]
fn standalone_flushes_are_consumed_exactly_once() {
    clear_flush_log();
    let mut flushes = TlbFlushes::None;
    flushes += TlbFlush::Page(VirtAddr::from_usize(0x1000));
    flushes.flush_locally();
    assert_eq!(flush_log(), std::vec![Some(0x1000)]);

    let mut flushes = TlbFlushes::None;
    flushes += TlbFlush::Page(VirtAddr::from_usize(0x2000));
    drop(flushes);
    assert_eq!(flush_log(), std::vec![Some(0x1000), Some(0x2000)]);

    let flushes = TlbFlushes::<VirtAddr>::Full;
    // SAFETY: this test has no hardware translations to invalidate.
    unsafe { flushes.ignore() };
    assert_eq!(flush_log(), std::vec![Some(0x1000), Some(0x2000)]);
}

/// Checks bounded accumulation without flushing records during promotion.
///
/// Duplicate addresses occupy slots, all 32 slots remain individual, and the 33rd promotes to Full.
#[test]
fn standalone_flushes_preserve_duplicates_and_promote_without_early_flush() {
    clear_flush_log();
    let mut flushes = TlbFlushes::None;
    for _ in 0..32 {
        flushes += TlbFlush::Page(VirtAddr::from_usize(0x1000));
    }
    assert!(matches!(&flushes, TlbFlushes::Pages(pages) if pages.len() == 32));
    assert!(flush_log().is_empty());
    flushes += TlbFlush::Page(VirtAddr::from_usize(0x2000));
    assert!(matches!(&flushes, TlbFlushes::Full));
    assert!(flush_log().is_empty());
    flushes.flush_locally();
    assert_eq!(flush_log(), std::vec![None]);
}

/// Checks growing record storage and exact byte-size deallocation across multiple record pages.
///
/// Every independent allocation must be processed exactly once, including non-frame-sized requests.
#[test]
fn deallocation_list_grows_and_deallocates_exact_allocations() {
    use crate::pt::cleanup::{DeallocationItem, DeallocationList};

    reset_test_state();
    let allocator = DynPageAllocator::new::<TestPagingHandler>();
    let mut list = DeallocationList::new(&allocator);
    for _ in 0..600 {
        let paddr = TestPagingHandler::alloc_frames_of_size(5000).unwrap();
        // SAFETY: each newly allocated run is exclusively transferred into the list.
        unsafe { list.push(DeallocationItem { paddr, size: 5000 }) }.unwrap();
    }
    assert_eq!(list.iter().count(), 600);
    assert_eq!(allocation_count(), 603);
    list.deallocate_all();
    assert_eq!(allocation_count(), 0);
    assert_eq!(dealloc_requests().len(), 600);
    assert!(dealloc_requests().iter().all(|(_, size)| *size == 5000));
}

/// Checks appending lists whose allocators manage the same allocation pool.
///
/// The destination allocator deallocates all records, including records from the appended list.
#[test]
fn deallocation_list_append_uses_compatible_destination_allocator() {
    use crate::pt::cleanup::{DeallocationItem, DeallocationList};

    reset_test_state();
    let first = DeallocationItem {
        paddr: TestPagingHandler::alloc_frames_of_size(5000).unwrap(),
        size: 5000,
    };
    let second = DeallocationItem {
        paddr: AlternateTestPagingHandler::alloc_frames_of_size(7000).unwrap(),
        size: 7000,
    };
    let third = DeallocationItem {
        paddr: AlternateTestPagingHandler::alloc_frames_of_size(8193).unwrap(),
        size: 8193,
    };
    let mut list = DeallocationList::new(&DynPageAllocator::new::<TestPagingHandler>());
    // SAFETY: each allocation is exclusively transferred into its corresponding list.
    unsafe { list.push(first) }.unwrap();
    let mut other = DeallocationList::new(&DynPageAllocator::new::<AlternateTestPagingHandler>());
    unsafe { other.push(second) }.unwrap();
    unsafe { other.push(third) }.unwrap();
    let attempts_before_append = ALLOC_STATE.with(|state| state.borrow().alloc_count);

    list.append(other);
    assert_eq!(
        ALLOC_STATE.with(|state| state.borrow().alloc_count),
        attempts_before_append
    );
    assert_eq!(
        list.iter().copied().collect::<Vec<_>>(),
        [first, second, third]
    );
    assert!(deallocations().is_empty());
    list.deallocate_all();

    // Both allocator types manage the same pool, so the destination can process every record.
    assert_eq!(
        dealloc_requests(),
        std::vec![
            (first.paddr, 5000),
            (second.paddr, 7000),
            (third.paddr, 8193)
        ]
    );
    assert_eq!(allocation_count(), 0);
    assert!(flush_log().is_empty());
}

/// Checks allocation failure when a record page is full.
///
/// Existing records remain intact and the rejected allocation stays under the caller's management.
#[test]
fn deallocation_list_growth_failure_preserves_prior_records() {
    use crate::pt::cleanup::{DeallocationItem, DeallocationList};

    reset_test_state();
    let allocator = DynPageAllocator::new::<TestPagingHandler>();
    let mut list = DeallocationList::new(&allocator);
    let mut accepted = 0;
    loop {
        ALLOC_STATE.with(|state| state.borrow_mut().fail_on_alloc = None);
        let paddr = TestPagingHandler::alloc_frame().unwrap();
        let next_attempt = ALLOC_STATE.with(|state| state.borrow().alloc_count + 1);
        if accepted > 0 {
            fail_on_alloc(next_attempt);
        }
        // SAFETY: the allocation is not otherwise managed after a successful push.
        let result = unsafe { list.push(DeallocationItem { paddr, size: 4096 }) };
        if result.is_err() {
            assert_eq!(list.iter().count(), accepted);
            assert!(allocation_contains(paddr, 4096));
            TestPagingHandler::dealloc_frame(paddr);
            break;
        }
        accepted += 1;
    }
    assert!(accepted > 1);
    list.deallocate_all();
    assert_eq!(allocation_count(), 0);
}

/// Checks that a failed record collection can discard storage without deallocating table frames.
///
/// This is the rollback path while the original hierarchy still contains all recorded tables.
#[test]
fn discarded_deallocation_records_leave_allocations_live() {
    use crate::pt::cleanup::{DeallocationItem, DeallocationList};

    reset_test_state();
    let allocator = DynPageAllocator::new::<TestPagingHandler>();
    let paddr = TestPagingHandler::alloc_frame().unwrap();
    let mut list = DeallocationList::new(&allocator);
    // SAFETY: the test retains exclusive control and will discard this provisional record.
    unsafe { list.push(DeallocationItem { paddr, size: 4096 }) }.unwrap();
    // SAFETY: the caller resumes responsibility for the allocation.
    unsafe { list.clear_list() };
    assert_eq!(allocation_count(), 1);
    TestPagingHandler::dealloc_frame(paddr);
    assert_eq!(allocation_count(), 0);
}

/// Checks transfer of both invalidation and detached table-frame deallocation.
///
/// Dropping an emptied cursor must not process records retained by the caller.
#[test]
fn detached_tables_wait_for_taken_cleanup() {
    reset_test_state();
    let mut table = PageTable::<TestMeta, TestPte, TestPagingHandler>::new_alloc().unwrap();
    let mut cursor = table.cursor();
    cursor
        .map(
            VirtAddr::from_usize(0),
            PhysAddr::from_usize(0x100000),
            4096,
            MappingFlags::READ,
        )
        .unwrap();
    cursor.clean_up();
    clear_flush_log();
    cursor
        .map(
            VirtAddr::from_usize(0),
            PhysAddr::from_usize(0x200000),
            65536,
            MappingFlags::READ,
        )
        .unwrap();
    let (flushes, deallocations) = cursor.take_cleanup().into_parts();
    assert_eq!(deallocations.iter().count(), 2);
    let detached: Vec<_> = deallocations.iter().copied().collect();
    drop(cursor);
    assert!(flush_log().is_empty());
    assert_eq!(allocation_count(), 4);
    flushes.flush_locally();
    deallocations.deallocate_all();
    assert_eq!(flush_log(), std::vec![None]);
    ALLOC_STATE.with(|state| {
        assert!(
            state
                .borrow()
                .deallocation_flush_counts
                .iter()
                .all(|count| *count == 1)
        )
    });
    for item in detached {
        assert!(dealloc_requests().contains(&(item.paddr, 96)));
    }
    assert_eq!(allocation_count(), 1);
    drop(table);
    assert_eq!(allocation_count(), 0);
}

/// Checks failure before a subtree can be recorded for deallocation.
///
/// Neither the old descriptor nor its descendants may change on record-storage failure.
#[test]
fn detached_table_record_failure_preserves_original_subtree() {
    reset_test_state();
    let mut table = PageTable::<TestMeta, TestPte, TestPagingHandler>::new_alloc().unwrap();
    let root = table.root();
    let mut cursor = table.cursor();
    cursor
        .map(
            VirtAddr::from_usize(0),
            PhysAddr::from_usize(0x100000),
            4096,
            MappingFlags::READ,
        )
        .unwrap();
    cursor.clean_up();
    clear_flush_log();
    let next_attempt = ALLOC_STATE.with(|state| state.borrow().alloc_count + 1);
    fail_on_alloc(next_attempt);
    assert!(matches!(
        cursor.map(
            VirtAddr::from_usize(0),
            PhysAddr::from_usize(0x200000),
            65536,
            MappingFlags::READ
        ),
        Err(PagingError::AllocationFailed)
    ));
    drop(cursor);
    assert_eq!(allocation_count(), 3);
    assert!(flush_log().is_empty());
    assert_eq!(lookup_root(root, 0).unwrap().paddr, 0x100000);
    drop(table);
    assert_eq!(allocation_count(), 0);
}

/// Checks that both roots transfer cleanup without independently processing it.
///
/// Full invalidation dominates across roots and both detached subtrees remain live until processed.
#[test]
fn dual_cursor_transfers_combined_cleanup() {
    reset_test_state();
    let mut table = DualPageTable::<
        VirtAddr,
        TestMeta,
        TestPte,
        TestUpperMeta,
        TestPte,
        TestPagingHandler,
    >::new_alloc()
    .unwrap();
    let upper_base = !((1usize << TestUpperMeta::VA_BITS) - 1);
    let mut cursor = table.cursor();
    for base in [0, upper_base] {
        cursor
            .map(
                VirtAddr::from_usize(base),
                PhysAddr::from_usize(0x100000),
                4096,
                MappingFlags::READ,
            )
            .unwrap();
    }
    cursor.clean_up();
    clear_flush_log();
    for base in [0, upper_base] {
        cursor
            .map(
                VirtAddr::from_usize(base),
                PhysAddr::from_usize(0x200000),
                65536,
                MappingFlags::READ,
            )
            .unwrap();
    }
    let (flushes, deallocations) = cursor.take_cleanup().into_parts();
    assert_eq!(deallocations.iter().count(), 4);
    drop(cursor);
    assert!(flush_log().is_empty());
    flushes.flush_locally();
    deallocations.deallocate_all();
    assert_eq!(flush_log(), std::vec![None]);
    assert_eq!(allocation_count(), 2);
    drop(table);
    assert_eq!(allocation_count(), 0);
}

/// Checks deferred opaque cleanup after a later action fails or its iterator panics.
///
/// Completed replacement and its cleanup survive both exits without prematurely deallocating
/// tables.
#[test]
fn opaque_deferred_cleanup_survives_error_and_unwind() {
    for panic_after_replacement in [false, true] {
        reset_test_state();
        let format = OpaquePageTableType::<VirtAddr>::new::<TestMeta, TestPte, TestPagingHandler>();
        let mut table = format.new_pagetable_alloc().unwrap();
        table
            .map(
                VirtAddr::from_usize(0),
                PhysAddr::from_usize(0x100000),
                4096,
                MappingFlags::READ,
            )
            .unwrap();
        clear_flush_log();
        let mut cleanup = table.new_cleanup();
        let mut count = 0;
        let actions = core::iter::from_fn(|| {
            count += 1;
            if count == 1 {
                Some(PageTableAction::Map {
                    vaddr: VirtAddr::from_usize(0),
                    paddr: PhysAddr::from_usize(0x200000),
                    size: 65536,
                    flags: MappingFlags::READ,
                })
            } else {
                assert!(!panic_after_replacement, "injected iterator panic");
                Some(PageTableAction::Unmap {
                    vaddr: VirtAddr::from_usize(usize::MAX),
                    size: 4096,
                })
            }
        });
        let result = catch_unwind(AssertUnwindSafe(|| {
            table.apply_actions_with_cleanup(actions, &mut cleanup)
        }));
        if panic_after_replacement {
            assert!(result.is_err());
        } else {
            assert!(matches!(
                result.unwrap(),
                Err(PagingError::NonCanonical { .. })
            ));
        }
        assert!(flush_log().is_empty());
        let (flushes, deallocations) = cleanup.into_parts();
        assert_eq!(deallocations.iter().count(), 2);
        assert_eq!(allocation_count(), 4);
        flushes.flush_locally();
        deallocations.deallocate_all();
        assert_eq!(flush_log(), std::vec![None]);
        assert_eq!(allocation_count(), 1);
        drop(table);
        assert_eq!(allocation_count(), 0);
    }
}

/// Checks failure after provisional records have filled one storage frame.
///
/// Discarding provisional storage must leave all original table allocations and mappings intact.
#[test]
fn subtree_record_growth_failure_preserves_all_tables() {
    reset_test_state();
    let mut table =
        PageTable::<MultiFrameTestMeta, TestPte, TestPagingHandler>::new_alloc().unwrap();
    let root = table.root();
    let mut cursor = table.cursor();
    for index in 0..260 {
        cursor
            .map(
                VirtAddr::from_usize(index * 0x200000),
                PhysAddr::from_usize(0x400000),
                4096,
                MappingFlags::READ,
            )
            .unwrap();
    }
    cursor.clean_up();
    clear_flush_log();
    let second_record_page = ALLOC_STATE.with(|state| state.borrow().alloc_count + 2);
    fail_on_alloc(second_record_page);
    assert!(matches!(
        cursor.map(
            VirtAddr::from_usize(0),
            PhysAddr::from_usize(0x80000000),
            0x80000000,
            MappingFlags::READ
        ),
        Err(PagingError::AllocationFailed)
    ));
    drop(cursor);
    assert_eq!(allocation_count(), 262);
    assert!(flush_log().is_empty());
    // The root still points to its original middle-level table and all 260 base-page tables.
    let root_table = unsafe { core::slice::from_raw_parts(root.as_usize() as *const TestPte, 512) };
    assert_eq!(root_table[0].kind, PTE_TABLE);
    let middle = unsafe {
        core::slice::from_raw_parts(root_table[0].paddr().as_usize() as *const TestPte, 1024)
    };
    for entry in &middle[..260] {
        assert_eq!(entry.kind, PTE_TABLE);
        let leaf = unsafe { &*(entry.paddr().as_usize() as *const TestPte) };
        assert_eq!(leaf.kind, PTE_PAGE);
        assert_eq!(leaf.paddr(), PhysAddr::from_usize(0x400000));
    }
    drop(table);
    assert_eq!(allocation_count(), 0);
}

/// Checks unwinding after provisional records have filled one storage frame.
///
/// An allocator panic must discard only record storage while the original hierarchy stays intact.
#[test]
fn subtree_record_growth_panic_preserves_all_tables() {
    reset_test_state();
    let mut table =
        PageTable::<MultiFrameTestMeta, TestPte, TestPagingHandler>::new_alloc().unwrap();
    let mut cursor = table.cursor();
    for index in 0..260 {
        cursor
            .map(
                VirtAddr::from_usize(index * 0x200000),
                PhysAddr::from_usize(0x400000),
                4096,
                MappingFlags::READ,
            )
            .unwrap();
    }
    cursor.clean_up();
    clear_flush_log();
    ALLOC_STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.panic_on_alloc = Some(state.alloc_count + 2);
    });
    let result = catch_unwind(AssertUnwindSafe(|| {
        cursor.map(
            VirtAddr::from_usize(0),
            PhysAddr::from_usize(0x80000000),
            0x80000000,
            MappingFlags::READ,
        )
    }));
    drop(cursor);
    // Keep a broken implementation from dereferencing deallocated tables during test unwinding.
    let root = table.leak();
    assert!(result.is_err());
    assert_eq!(allocation_count(), 262);
    assert!(flush_log().is_empty());
    assert_eq!(deallocations().len(), 1);
    assert!(dealloc_requests().is_empty());
    let root_table = unsafe { core::slice::from_raw_parts(root.as_usize() as *const TestPte, 512) };
    assert_eq!(root_table[0].kind, PTE_TABLE);
    let middle = unsafe {
        core::slice::from_raw_parts(root_table[0].paddr().as_usize() as *const TestPte, 1024)
    };
    for entry in &middle[..260] {
        assert_eq!(entry.kind, PTE_TABLE);
        let leaf = unsafe { &*(entry.paddr().as_usize() as *const TestPte) };
        assert_eq!(leaf.kind, PTE_PAGE);
        assert_eq!(leaf.paddr(), PhysAddr::from_usize(0x400000));
    }
    drop(unsafe { PageTable::<MultiFrameTestMeta, TestPte, TestPagingHandler>::new_at(root) });
    assert_eq!(allocation_count(), 0);
}

/// Checks partial progress when a second huge-page split cannot allocate its table.
///
/// The first split remains installed and its Full invalidation survives the later failure.
#[test]
fn huge_split_failure_retains_prior_split_and_cleanup() {
    reset_test_state();
    let mut table = PageTable::<TestMeta, TestPte, TestPagingHandler>::new_alloc().unwrap();
    let root = table.root();
    let mut cursor = table.cursor();
    cursor
        .map(
            VirtAddr::from_usize(0),
            PhysAddr::from_usize(0x100000),
            65536,
            MappingFlags::READ,
        )
        .unwrap();
    cursor.clean_up();
    clear_flush_log();
    fail_on_alloc(3);
    assert!(matches!(
        cursor.unmap(VirtAddr::from_usize(4096), 4096),
        Err(PagingError::AllocationFailed)
    ));
    let (flushes, deallocations) = cursor.take_cleanup().into_parts();
    assert_eq!(deallocations.iter().count(), 0);
    assert!(matches!(&flushes, TlbFlushes::Full));
    drop(cursor);
    assert_eq!(allocation_count(), 2);
    assert_eq!(
        lookup_root(root, 4096).unwrap(),
        MappingSnapshot {
            paddr: 0x101000,
            flags: MappingFlags::READ,
            page_size: 16384
        }
    );
    flushes.flush_locally();
    deallocations.deallocate_all();
    assert_eq!(flush_log(), std::vec![None]);
    drop(table);
    assert_eq!(allocation_count(), 0);
}

/// Computes the test page table index for a virtual address and level.
///
/// The calculation uses the bit ranges derived from [TestMeta].
fn index_for(level: usize, vaddr: usize) -> usize {
    let (start, end) = TestMeta::LEVEL_BIT_RANGES[level];
    (vaddr >> start) & ((1 << (end - start)) - 1)
}

/// Checks fallback processing on both normal exit and iterator unwinding.
///
/// Local invalidation must precede every detached table-frame and record-storage deallocation.
#[test]
fn opaque_fallback_cleanup_flushes_before_deallocation() {
    for panic_after_replacement in [false, true] {
        reset_test_state();
        let format = OpaquePageTableType::<VirtAddr>::new::<TestMeta, TestPte, TestPagingHandler>();
        let mut table = format.new_pagetable_alloc().unwrap();
        table
            .map(
                VirtAddr::from_usize(0),
                PhysAddr::from_usize(0x100000),
                4096,
                MappingFlags::READ,
            )
            .unwrap();
        clear_flush_log();
        let mut count = 0;
        let actions = core::iter::from_fn(|| {
            count += 1;
            if count == 1 {
                Some(PageTableAction::Map {
                    vaddr: VirtAddr::from_usize(0),
                    paddr: PhysAddr::from_usize(0x200000),
                    size: 65536,
                    flags: MappingFlags::READ,
                })
            } else {
                assert!(!panic_after_replacement, "injected iterator panic");
                None
            }
        });
        let result = catch_unwind(AssertUnwindSafe(|| table.apply_actions(actions)));
        assert_eq!(result.is_err(), panic_after_replacement);
        assert_eq!(flush_log(), std::vec![None]);
        assert_eq!(allocation_count(), 1);
        ALLOC_STATE.with(|state| {
            let state = state.borrow();
            assert_eq!(state.deallocation_flush_counts, std::vec![1, 1, 1]);
        });
        drop(table);
        assert_eq!(allocation_count(), 0);
    }
}

/// Copies the test table entries stored at a physical address.
///
/// The snapshot avoids references whose lifetime could outlive the multi-level page table value.
/// The test allocator performs identity mapping to inspect each live table.
fn table_slice(paddr: PhysAddr, level: usize) -> Vec<TestPte> {
    let entry_count = TestMeta::LEVEL_TABLE_SIZE[level];
    let ptr = TestPagingHandler::phys_to_virt(paddr).as_ptr() as *const TestPte;
    unsafe { core::slice::from_raw_parts(ptr, entry_count).to_vec() }
}

/// Resolves one virtual address through the compact test page table hierarchy.
///
/// The walk stops at an unused entry or at the first base-page or huge-page leaf.
fn lookup_root(root: PhysAddr, vaddr: usize) -> Option<MappingSnapshot> {
    let mut table_paddr = root;
    let mut level = TestMeta::LEVELS - 1;
    loop {
        let entries = table_slice(table_paddr, level);
        let entry = entries[index_for(level, vaddr)];
        if entry.is_unused() {
            return None;
        }
        if level == 0 || entry.is_huge() {
            let page_size = TestMeta::LEVEL_PAGE_SIZE[level];
            return Some(MappingSnapshot {
                paddr: entry.paddr().as_usize() + vaddr % page_size,
                flags: entry.flags(),
                page_size,
            });
        }
        table_paddr = entry.paddr();
        level -= 1;
    }
}

/// Resolves one virtual address through a concrete test page table hierarchy.
///
/// The hierarchy root is passed to the raw walker without constructing another multi-level
/// page table value.
fn lookup(
    table: &PageTable<TestMeta, TestPte, TestPagingHandler>,
    vaddr: usize,
) -> Option<MappingSnapshot> {
    lookup_root(table.root(), vaddr)
}

/// Allocates a fresh compact test multi-level page table.
///
/// Allocation failure is unexpected unless a test configures it explicitly.
fn new_table() -> PageTable<TestMeta, TestPte, TestPagingHandler> {
    PageTable::<TestMeta, TestPte, TestPagingHandler>::new_alloc().unwrap()
}

/// Asserts that a virtual address resolves to an expected mapping.
///
/// The assertion compares the translated physical address, mapping flags, and
/// leaf page size.
fn assert_mapping(
    table: &PageTable<TestMeta, TestPte, TestPagingHandler>,
    vaddr: usize,
    paddr: usize,
    flags: MappingFlags,
    page_size: usize,
) {
    assert_mapping_at_root(table.root(), vaddr, paddr, flags, page_size);
}

/// Asserts that a page table hierarchy root address resolves to an expected mapping.
///
/// The raw walker inspects the root without constructing another multi-level page table value.
fn assert_mapping_at_root(
    root: PhysAddr,
    vaddr: usize,
    paddr: usize,
    flags: MappingFlags,
    page_size: usize,
) {
    assert_eq!(
        lookup_root(root, vaddr),
        Some(MappingSnapshot {
            paddr,
            flags,
            page_size,
        })
    );
}

/// Asserts that a virtual address has no leaf mapping.
///
/// Intermediate tables may still exist below the root.
fn assert_unmapped(table: &PageTable<TestMeta, TestPte, TestPagingHandler>, vaddr: usize) {
    assert_eq!(lookup(table, vaddr), None);
}

/// Asserts recursively that a page table hierarchy contains no leaf mappings.
///
/// Every live entry encountered above level zero must point to another table.
fn assert_subtree_has_no_leaf_mappings(paddr: PhysAddr, level: usize) {
    for entry in table_slice(paddr, level) {
        if entry.is_unused() {
            continue;
        }

        assert!(level > 0);
        assert_eq!(entry.kind, PTE_TABLE);
        assert_subtree_has_no_leaf_mappings(entry.paddr(), level - 1);
    }
}

/// Verifies root allocation, clearing, and allocation failure.
///
/// A new root must occupy one frame, contain only unused entries, and propagate
/// a failure from its first allocator request.
#[test]
fn new_alloc_initializes_zeroed_root_table() {
    reset_test_state();

    let table = PageTable::<TestMeta, TestPte, TestPagingHandler>::new_alloc().unwrap();
    let root_paddr = table.root();
    assert_ne!(root_paddr.as_usize(), 0);
    assert!(allocation_contains(
        root_paddr,
        1 << TestPagingHandler::page_size_shift()
    ));
    for entry in table_slice(root_paddr, TestMeta::LEVELS - 1) {
        assert!(entry.is_unused());
        assert_eq!(entry.paddr().as_usize(), 0);
        assert!(entry.flags().is_empty());
        assert_eq!(entry.bits(), 0);
    }

    drop(table);
    reset_test_state();
    fail_on_alloc(1);
    assert!(matches!(
        PageTable::<TestMeta, TestPte, TestPagingHandler>::new_alloc(),
        Err(PagingError::AllocationFailed)
    ));
    reset_test_state();
}

/// Verifies that dropping a concrete multi-level page table deallocates its table frames.
///
/// Mapped data frames remain caller-managed.
#[test]
fn concrete_drop_deallocates_tables_but_not_mapped_data() {
    reset_test_state();
    let data_frame = TestPagingHandler::alloc_frames(1).unwrap();
    let mut table = new_table();
    table
        .cursor()
        .map(
            VirtAddr::from_usize(0x3000),
            data_frame,
            TestMeta::PAGE_SIZE,
            MappingFlags::READ,
        )
        .unwrap();
    {
        let mut cursor = table.cursor();
        for (vaddr, size) in [(0x4000, 4096), (0x13000, 4096), (0x20000, 65536)] {
            cursor
                .map(
                    VirtAddr::from_usize(vaddr),
                    PhysAddr::from_usize(0x100000),
                    size,
                    MappingFlags::READ,
                )
                .unwrap();
        }
    }
    assert_eq!(allocation_count(), 7);
    assert_eq!(
        lookup(&table, 0x21000),
        Some(MappingSnapshot {
            paddr: 0x101000,
            flags: MappingFlags::READ,
            page_size: 65536,
        })
    );

    drop(table);

    assert_eq!(allocation_count(), 1);
    assert_eq!(deallocations().len(), 6);
    assert!(allocation_contains(
        data_frame,
        1 << TestPagingHandler::page_size_shift()
    ));
    reset_test_state();
}

/// Verifies exact multi-frame table sizes and descendant-first destruction.
///
/// Dropping the multi-level page table once also leaves its mapped data frame allocated.
#[test]
fn concrete_drop_deallocates_multiframe_tables_with_exact_bytes_in_order() {
    reset_test_state();
    let data_frame = TestPagingHandler::alloc_frames(1).unwrap();
    let mut table =
        PageTable::<MultiFrameTestMeta, TestPte, TestPagingHandler>::new_alloc().unwrap();
    table
        .cursor()
        .map(
            VirtAddr::from_usize(0x1000),
            data_frame,
            4096,
            MappingFlags::READ,
        )
        .unwrap();
    let root = table.root();
    let middle = unsafe { (*TestPagingHandler::phys_to_virt(root).as_ptr_of::<TestPte>()).paddr() };
    let leaf = unsafe { (*TestPagingHandler::phys_to_virt(middle).as_ptr_of::<TestPte>()).paddr() };
    let expected_requests = std::vec![
        (leaf, 512 * size_of::<TestPte>()),
        (middle, 1024 * size_of::<TestPte>()),
        (root, 512 * size_of::<TestPte>()),
    ];

    // Consume the value even if the allocator rejects a wrong frame count during drop.
    let drop_result = catch_unwind(AssertUnwindSafe(|| drop(table)));
    let requests = dealloc_requests();
    let remaining_count = allocation_count();
    let data_is_live = allocation_contains(data_frame, 4096);
    reset_test_state();

    assert_eq!(requests, expected_requests);
    assert!(drop_result.is_ok());
    assert_eq!(remaining_count, 1);
    assert!(data_is_live);
}

/// Verifies that a leaked concrete root can be used with another allocator type.
///
/// Both allocator types share test storage so the adopting multi-level page table can deallocate
/// its table frames.
#[test]
fn leaked_concrete_root_can_be_adopted_by_another_allocator_type() {
    reset_test_state();
    let data_frame = TestPagingHandler::alloc_frames(1).unwrap();
    let mut table = new_table();
    table
        .cursor()
        .map(
            VirtAddr::from_usize(0x3000),
            data_frame,
            TestMeta::PAGE_SIZE,
            MappingFlags::READ,
        )
        .unwrap();
    let root = table.leak();

    assert_eq!(allocation_count(), 4);
    let adopted =
        unsafe { PageTable::<TestMeta, TestPte, AlternateTestPagingHandler>::new_at(root) };
    drop(adopted);

    assert_eq!(allocation_count(), 1);
    assert!(allocation_contains(
        data_frame,
        1 << TestPagingHandler::page_size_shift()
    ));
    reset_test_state();
}

/// Verifies deallocation of child table frames created by splitting a huge mapping.
///
/// Splitting twice creates tables at both lower levels while data frames remain caller-managed.
#[test]
fn concrete_drop_deallocates_table_frames_created_by_huge_mapping_split() {
    reset_test_state();
    let mut table = new_table();
    table
        .cursor()
        .map(
            VirtAddr::from_usize(0),
            PhysAddr::from_usize(0x100000),
            65536,
            MappingFlags::READ,
        )
        .unwrap();
    assert_eq!(allocation_count(), 1);
    table
        .cursor()
        .map(
            VirtAddr::from_usize(0x3000),
            PhysAddr::from_usize(0x200000),
            4096,
            MappingFlags::WRITE,
        )
        .unwrap();
    assert_eq!(allocation_count(), 3);
    assert_mapping(&table, 0x2000, 0x102000, MappingFlags::READ, 4096);
    assert_mapping(&table, 0x3000, 0x200000, MappingFlags::WRITE, 4096);
    assert_mapping(&table, 0x4000, 0x104000, MappingFlags::READ, 16384);

    drop(table);

    assert_eq!(allocation_count(), 0);
    assert_eq!(deallocations().len(), 3);
    reset_test_state();
}

/// Verifies mapping and unmapping one base page through a cursor.
///
/// The test also checks that explicit and drop-time flushes invalidate the
/// affected virtual mapping.
#[test]
fn cursor_maps_and_unmaps_single_base_page() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor();
        cursor
            .map(
                VirtAddr::from_usize(0x3000),
                PhysAddr::from_usize(0xa000),
                TestMeta::PAGE_SIZE,
                MappingFlags::READ | MappingFlags::WRITE,
            )
            .unwrap();
        assert_mapping(
            &*cursor.table,
            0x3000,
            0xa000,
            MappingFlags::READ | MappingFlags::WRITE,
            TestMeta::PAGE_SIZE,
        );

        cursor.clean_up();
        cursor
            .unmap(VirtAddr::from_usize(0x3000), TestMeta::PAGE_SIZE)
            .unwrap();
    }

    assert_unmapped(&table, 0x3000);
    assert_eq!(flush_log(), std::vec![None, Some(0x3000)]);
    drop(table);
    reset_test_state();
}

/// Flushes newly created intermediate tables while unmapping an absent page.
///
/// Traversal retains these tables without leaf mappings. Installing their non-leaf descriptors
/// requires full local invalidation even though no leaf mapping was removed.
#[test]
fn cursor_unmaps_absent_range_with_full_flush_for_new_tables() {
    reset_test_state();
    let mut table = new_table();
    let root_index = index_for(TestMeta::LEVELS - 1, 0x4000);
    assert!(table_slice(table.root(), TestMeta::LEVELS - 1)[root_index].is_unused());

    {
        let mut cursor = table.cursor();
        cursor
            .unmap(VirtAddr::from_usize(0x4000), TestMeta::PAGE_SIZE)
            .unwrap();
        assert_unmapped(&*cursor.table, 0x4000);
    }

    assert_eq!(
        table_slice(table.root(), TestMeta::LEVELS - 1)[root_index].kind,
        PTE_TABLE
    );
    assert_subtree_has_no_leaf_mappings(table.root(), TestMeta::LEVELS - 1);
    assert_unmapped(&table, 0x4000);
    assert_eq!(flush_log(), std::vec![None]);
    drop(table);
    reset_test_state();
}

/// Verifies page rounding for an unaligned mapping range.
///
/// The virtual-to-physical offset is preserved across all rounded base pages.
#[test]
fn cursor_maps_unaligned_range_with_virtual_physical_offset() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor();
        cursor
            .map(
                VirtAddr::from_usize(0x1803),
                PhysAddr::from_usize(0x9803),
                0x2800,
                MappingFlags::READ,
            )
            .unwrap();
    }

    for (vaddr, paddr) in [
        (0x1000, 0x9000),
        (0x2000, 0xa000),
        (0x3000, 0xb000),
        (0x4000, 0xc000),
    ] {
        assert_mapping(
            &table,
            vaddr,
            paddr,
            MappingFlags::READ,
            TestMeta::PAGE_SIZE,
        );
    }
    assert_unmapped(&table, 0x5000);
    drop(table);
    reset_test_state();
}

/// Verifies selection of the largest aligned supported leaf levels.
///
/// Aligned ranges become level-two and level-one mappings instead of collections
/// of base-page leaves.
#[test]
fn cursor_uses_largest_possible_page_levels() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor();
        cursor
            .map(
                VirtAddr::from_usize(0x10000),
                PhysAddr::from_usize(0x20000),
                TestMeta::LEVEL_PAGE_SIZE[2],
                MappingFlags::READ,
            )
            .unwrap();
        cursor
            .map(
                VirtAddr::from_usize(0x4000),
                PhysAddr::from_usize(0x30000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ | MappingFlags::WRITE,
            )
            .unwrap();
    }

    assert_mapping(
        &table,
        0x18000,
        0x28000,
        MappingFlags::READ,
        TestMeta::LEVEL_PAGE_SIZE[2],
    );
    assert_mapping(
        &table,
        0x5000,
        0x31000,
        MappingFlags::READ | MappingFlags::WRITE,
        TestMeta::LEVEL_PAGE_SIZE[1],
    );
    drop(table);
    reset_test_state();
}

/// Verifies that page-end calculation rejects address overflow.
///
/// A representable smaller page size still advances the same high-half address
/// correctly.
#[test]
fn page_level_selection_rejects_wrapping_end_addresses() {
    let high_start = VirtAddr::from_usize(0xff00_0000_8000_0000);
    let high_end = VirtAddr::from_usize(0xff00_0000_8800_0000);

    assert_eq!(checked_page_end(high_start, high_end, 1usize << 56), None);
    assert_eq!(
        checked_page_end(high_start, high_end, 2 * 1024 * 1024),
        Some(VirtAddr::from_usize(0xff00_0000_8020_0000))
    );
}

/// Verifies rejection of a mapping range with an unrepresentable exclusive end.
///
/// Overflow must return a validation error without mutating the hierarchy.
#[test]
fn mapping_rejects_wrapping_range_end() {
    reset_test_state();
    let mut table = new_table();
    let mut cursor = table.cursor();

    assert!(matches!(
        cursor.map(
            VirtAddr::from_usize(usize::MAX),
            PhysAddr::from_usize(0),
            1,
            MappingFlags::READ,
        ),
        Err(PagingError::NonCanonical { .. })
    ));

    drop(cursor);
    drop(table);
    reset_test_state();
}

/// Verifies canonical validation for symmetric virtual-address coverage.
///
/// Both canonical ranges are accepted while the gap and cross-gap ranges are rejected.
#[test]
fn symmetric_vaddr_validation_rejects_canonical_hole() {
    /// The multi-level page table type used for symmetric coverage validation.
    ///
    /// Its compact geometry isolates the test from architecture-specific formats.
    type Table = PageTable<SymmetricTestMeta, TestPte, TestPagingHandler>;
    let low_end = 1usize << (SymmetricTestMeta::VA_BITS - 1);
    let upper_start = usize::MAX - low_end + 1;

    assert!(matches!(
        Table::validate_and_truncate_vaddr_range(AddrRange::new(
            VirtAddr::from_usize(0),
            VirtAddr::from_usize(low_end),
        )),
        Ok((0, end)) if end == low_end
    ));
    assert!(matches!(
        Table::validate_and_truncate_vaddr_range(AddrRange::new(
            VirtAddr::from_usize(upper_start),
            VirtAddr::from_usize(upper_start + TestMeta::PAGE_SIZE),
        )),
        Ok((start, end)) if start == low_end && end == low_end + TestMeta::PAGE_SIZE
    ));
    assert!(matches!(
        Table::validate_and_truncate_vaddr_range(AddrRange::new(
            VirtAddr::from_usize(low_end - TestMeta::PAGE_SIZE),
            VirtAddr::from_usize(upper_start + TestMeta::PAGE_SIZE),
        )),
        Err(PagingError::NonCanonical { .. })
    ));
}

/// Verifies the lower and upper type-level coverage policies.
///
/// Each canonical-side policy accepts its own range and rejects the opposite side.
#[test]
fn coverage_policies_validate_zero_and_sign_extensions() {
    let va_bits = TestMeta::VA_BITS;
    let valid_mask = (1usize << va_bits) - 1;
    let upper_base = usize::MAX - valid_mask;

    assert_eq!(
        <LowerCoverage<VirtAddr> as PageTableCoverage>::validate_and_truncate_vaddr(
            VirtAddr::from_usize(0x1234),
            va_bits,
        ),
        Ok(0x1234)
    );
    assert!(
        <LowerCoverage<VirtAddr> as PageTableCoverage>::validate_and_truncate_vaddr(
            VirtAddr::from_usize(upper_base),
            va_bits,
        )
        .is_err()
    );
    assert_eq!(
        <UpperCoverage<VirtAddr> as PageTableCoverage>::validate_and_truncate_vaddr(
            VirtAddr::from_usize(upper_base | 0x1234),
            va_bits,
        ),
        Ok(0x1234)
    );
    assert!(
        <UpperCoverage<VirtAddr> as PageTableCoverage>::validate_and_truncate_vaddr(
            VirtAddr::from_usize(0x1234),
            va_bits,
        )
        .is_err()
    );
}

/// Verifies that opaque action batches share one concrete cursor.
///
/// Two mutations remain visible through the hierarchy after the shared cursor flushes.
#[test]
fn opaque_page_table_applies_action_batch() {
    reset_test_state();
    let page_table_type =
        OpaquePageTableType::<VirtAddr>::new::<TestMeta, TestPte, TestPagingHandler>();
    let mut opaque_page_table = page_table_type.new_pagetable_alloc().unwrap();

    opaque_page_table
        .apply_actions([
            PageTableAction::Map {
                vaddr: VirtAddr::from_usize(0x3000),
                paddr: PhysAddr::from_usize(0xa000),
                size: TestMeta::PAGE_SIZE,
                flags: MappingFlags::READ,
            },
            PageTableAction::Map {
                vaddr: VirtAddr::from_usize(0x4000),
                paddr: PhysAddr::from_usize(0xb000),
                size: TestMeta::PAGE_SIZE,
                flags: MappingFlags::READ | MappingFlags::WRITE,
            },
        ])
        .unwrap();

    let OpaquePageTableRoot::Single(root) = opaque_page_table.root() else {
        panic!("single-root opaque table returned a dual root");
    };
    assert_mapping_at_root(
        root,
        0x3000,
        0xa000,
        MappingFlags::READ,
        TestMeta::PAGE_SIZE,
    );
    assert_mapping_at_root(
        root,
        0x4000,
        0xb000,
        MappingFlags::READ | MappingFlags::WRITE,
        TestMeta::PAGE_SIZE,
    );
    assert_eq!(flush_log(), std::vec![None]);
    drop(opaque_page_table);
    reset_test_state();
}

/// Verifies that dropping an opaque multi-level page table deallocates table frames.
///
/// The descriptor supplies the allocator type, and mapped data frames remain caller-managed.
#[test]
fn opaque_drop_deallocates_tables_but_not_mapped_data() {
    reset_test_state();
    let data_frame = TestPagingHandler::alloc_frames(1).unwrap();
    let page_table_type =
        OpaquePageTableType::<VirtAddr>::new::<TestMeta, TestPte, TestPagingHandler>();
    let mut table = page_table_type.new_pagetable_alloc().unwrap();
    table
        .map(
            VirtAddr::from_usize(0x3000),
            data_frame,
            TestMeta::PAGE_SIZE,
            MappingFlags::READ,
        )
        .unwrap();

    drop(table);

    assert_eq!(allocation_count(), 1);
    assert!(allocation_contains(
        data_frame,
        1 << TestPagingHandler::page_size_shift()
    ));
    reset_test_state();
}

/// Verifies that an opaque action error leaves the root allocated.
///
/// The temporary concrete value must not deallocate table frames while reporting an
/// allocation error.
#[test]
fn opaque_failed_action_preserves_root_allocation() {
    reset_test_state();
    let page_table_type =
        OpaquePageTableType::<VirtAddr>::new::<TestMeta, TestPte, TestPagingHandler>();
    let mut table = page_table_type.new_pagetable_alloc().unwrap();
    let OpaquePageTableRoot::Single(root) = table.root() else {
        panic!("single-root descriptor returned a dual root");
    };
    fail_on_alloc(2);

    let result = table.map(
        VirtAddr::from_usize(0x3000),
        PhysAddr::from_usize(0xa000),
        TestMeta::PAGE_SIZE,
        MappingFlags::READ,
    );
    let root_is_live = allocation_contains(root, 1 << TestPagingHandler::page_size_shift());
    if root_is_live {
        drop(table);
    } else {
        // Avoid destroying a dangling root when running this regression against the broken code.
        core::mem::forget(table);
    }

    assert!(matches!(result, Err(PagingError::AllocationFailed)));
    assert!(root_is_live);
    assert_eq!(allocation_count(), 0);
    reset_test_state();
}

/// Verifies preservation and flushing of earlier mutations when a later action fails.
///
/// The successful replacement remains mapped until the opaque multi-level page table is dropped.
#[test]
fn opaque_later_action_failure_preserves_earlier_mapping_and_pending_flush() {
    reset_test_state();
    let page_table_type =
        OpaquePageTableType::<VirtAddr>::new::<TestMeta, TestPte, TestPagingHandler>();
    let mut table = page_table_type.new_pagetable_alloc().unwrap();
    table
        .map(
            VirtAddr::from_usize(0x3000),
            PhysAddr::from_usize(0xa000),
            4096,
            MappingFlags::READ,
        )
        .unwrap();
    let OpaquePageTableRoot::Single(root) = table.root() else {
        panic!("single-root descriptor returned a dual root");
    };
    clear_flush_log();
    fail_on_alloc(4);

    let result = table.apply_actions([
        PageTableAction::Map {
            vaddr: VirtAddr::from_usize(0x3000),
            paddr: PhysAddr::from_usize(0xb000),
            size: 4096,
            flags: MappingFlags::READ | MappingFlags::WRITE,
        },
        PageTableAction::Map {
            vaddr: VirtAddr::from_usize(0x4000),
            paddr: PhysAddr::from_usize(0xc000),
            size: 4096,
            flags: MappingFlags::READ,
        },
    ]);
    let root_is_live = allocation_contains(root, 4096);
    let earlier_mapping = root_is_live.then(|| lookup_root(root, 0x3000)).flatten();
    let later_mapping = root_is_live.then(|| lookup_root(root, 0x4000)).flatten();
    let frees_before_drop = deallocations();
    let pending_flushes = flush_log();
    if root_is_live {
        drop(table);
    } else {
        // The original failure deallocates the root, so drop only while it remains allocated.
        core::mem::forget(table);
    }
    let frees_after_drop = deallocations();
    let remaining_count = allocation_count();
    reset_test_state();

    assert!(matches!(result, Err(PagingError::AllocationFailed)));
    assert!(root_is_live);
    assert_eq!(
        earlier_mapping,
        Some(MappingSnapshot {
            paddr: 0xb000,
            flags: MappingFlags::READ | MappingFlags::WRITE,
            page_size: 4096,
        })
    );
    assert_eq!(later_mapping, None);
    assert_eq!(pending_flushes, std::vec![Some(0x3000), Some(0x3000)]);
    assert!(frees_before_drop.is_empty());
    assert_eq!(frees_after_drop.len(), 3);
    assert_eq!(remaining_count, 0);
}

/// Verifies that an iterator panic preserves the hierarchy and flushes its completed mutation.
///
/// Unwinding must drop the cursor while leaving table frames for the opaque value's drop.
#[test]
fn opaque_panicking_iterator_preserves_mapping_flush_and_hierarchy() {
    reset_test_state();
    let page_table_type =
        OpaquePageTableType::<VirtAddr>::new::<TestMeta, TestPte, TestPagingHandler>();
    let mut table = page_table_type.new_pagetable_alloc().unwrap();
    table
        .map(
            VirtAddr::from_usize(0x3000),
            PhysAddr::from_usize(0xa000),
            4096,
            MappingFlags::READ,
        )
        .unwrap();
    let OpaquePageTableRoot::Single(root) = table.root() else {
        panic!("single-root descriptor returned a dual root");
    };
    clear_flush_log();
    let mut yielded = false;
    let actions = core::iter::from_fn(|| {
        assert!(
            !yielded,
            "injected iterator panic after one completed action"
        );
        yielded = true;
        Some(PageTableAction::Map {
            vaddr: VirtAddr::from_usize(0x3000),
            paddr: PhysAddr::from_usize(0xb000),
            size: 4096,
            flags: MappingFlags::WRITE,
        })
    });

    let result = catch_unwind(AssertUnwindSafe(|| table.apply_actions(actions)));
    let root_is_live = allocation_contains(root, 4096);
    let mapping = root_is_live.then(|| lookup_root(root, 0x3000)).flatten();
    let frees_before_drop = deallocations();
    let pending_flushes = flush_log();
    if root_is_live {
        drop(table);
    } else {
        // Avoid invoking Drop on storage already deallocated by unwinding.
        core::mem::forget(table);
    }
    let frees_after_drop = deallocations();
    let remaining_count = allocation_count();
    reset_test_state();

    assert!(result.is_err());
    assert!(root_is_live);
    assert_eq!(
        mapping,
        Some(MappingSnapshot {
            paddr: 0xb000,
            flags: MappingFlags::WRITE,
            page_size: 4096,
        })
    );
    assert_eq!(pending_flushes, std::vec![Some(0x3000), Some(0x3000)]);
    assert!(frees_before_drop.is_empty());
    assert_eq!(frees_after_drop.len(), 3);
    assert_eq!(remaining_count, 0);
}

/// Verifies harmless destruction of the constant-initialization sentinel.
///
/// No method pointer dereferences its zero root and no allocator operation is attempted.
#[test]
fn opaque_dummy_drop_is_harmless() {
    reset_test_state();
    drop(OpaquePageTable::<VirtAddr>::dummy());

    assert_eq!(allocation_count(), 0);
    assert!(deallocations().is_empty());
    assert!(dealloc_requests().is_empty());
    assert!(flush_log().is_empty());
    reset_test_state();
}

/// Verifies that an opaque root can be adopted using another allocator type.
///
/// Leaking the original value transfers its root to a descriptor for the new type.
#[test]
fn opaque_root_can_be_adopted_by_another_allocator_type() {
    reset_test_state();
    let data_frame = TestPagingHandler::alloc_frames(1).unwrap();
    let original_type =
        OpaquePageTableType::<VirtAddr>::new::<TestMeta, TestPte, TestPagingHandler>();
    let mut original = original_type.new_pagetable_alloc().unwrap();
    original
        .map(
            VirtAddr::from_usize(0x3000),
            data_frame,
            TestMeta::PAGE_SIZE,
            MappingFlags::READ,
        )
        .unwrap();
    let previous_root = original.root();
    let root = original.leak();
    assert_eq!(root, previous_root);
    assert_eq!(allocation_count(), 4);
    assert!(deallocations().is_empty());

    let alternate_type =
        OpaquePageTableType::<VirtAddr>::new::<TestMeta, TestPte, AlternateTestPagingHandler>();
    let adopted = unsafe { alternate_type.new_pagetable_at(root) };
    drop(adopted);

    assert_eq!(allocation_count(), 1);
    assert!(allocation_contains(
        data_frame,
        1 << TestPagingHandler::page_size_shift()
    ));
    reset_test_state();
}

/// Verifies that an opaque dual-root multi-level page table dispatches actions to both roots.
///
/// Dropping it deallocates all table frames in both page table hierarchies.
#[test]
fn opaque_dual_page_table_applies_actions_to_both_roots() {
    reset_test_state();
    let page_table_type = OpaquePageTableType::<VirtAddr>::new_dual::<
        TestMeta,
        TestPte,
        TestUpperMeta,
        TestPte,
        TestPagingHandler,
    >();
    let mut opaque_page_table = page_table_type.new_pagetable_alloc().unwrap();
    let upper_base = !((1usize << (TestUpperMeta::VA_BITS - 1)) - 1);

    opaque_page_table
        .apply_actions([
            PageTableAction::Map {
                vaddr: VirtAddr::from_usize(0x3000),
                paddr: PhysAddr::from_usize(0xa000),
                size: TestMeta::PAGE_SIZE,
                flags: MappingFlags::READ,
            },
            PageTableAction::Map {
                vaddr: VirtAddr::from_usize(upper_base + 0x3000),
                paddr: PhysAddr::from_usize(0xb000),
                size: TestUpperMeta::PAGE_SIZE,
                flags: MappingFlags::READ | MappingFlags::WRITE,
            },
        ])
        .unwrap();

    let OpaquePageTableRoot::Dual(root) = opaque_page_table.root() else {
        panic!("dual-root opaque table returned a single root");
    };
    assert_ne!(root.lower, root.upper);
    assert_mapping_at_root(root.lower, 0x3000, 0xa000, MappingFlags::READ, 4096);
    assert_mapping_at_root(
        root.upper,
        upper_base + 0x3000,
        0xb000,
        MappingFlags::READ | MappingFlags::WRITE,
        4096,
    );
    assert_eq!(flush_log(), std::vec![None]);
    drop(opaque_page_table);
    assert_eq!(allocation_count(), 0);
    assert_eq!(deallocations().len(), 6);
    reset_test_state();
}

/// Verifies preserved table frames and local flushes when a later dual action fails.
///
/// Mutations in both canonical ranges survive a failed child page table allocation.
#[test]
fn opaque_dual_later_action_failure_preserves_both_hierarchies_and_flushes() {
    reset_test_state();
    let page_table_type = OpaquePageTableType::<VirtAddr>::new_dual::<
        TestMeta,
        TestPte,
        TestUpperMeta,
        TestPte,
        TestPagingHandler,
    >();
    let mut table = page_table_type.new_pagetable_alloc().unwrap();
    let upper_base = !((1usize << (TestUpperMeta::VA_BITS - 1)) - 1);
    table
        .apply_actions([0, upper_base].map(|base| PageTableAction::Map {
            vaddr: VirtAddr::from_usize(base + 0x3000),
            paddr: PhysAddr::from_usize(0xa000),
            size: 4096,
            flags: MappingFlags::READ,
        }))
        .unwrap();
    let OpaquePageTableRoot::Dual(root) = table.root() else {
        panic!("dual-root descriptor returned a single root");
    };
    clear_flush_log();
    fail_on_alloc(7);

    let result = table.apply_actions([
        PageTableAction::Map {
            vaddr: VirtAddr::from_usize(0x3000),
            paddr: PhysAddr::from_usize(0xb000),
            size: 4096,
            flags: MappingFlags::WRITE,
        },
        PageTableAction::Map {
            vaddr: VirtAddr::from_usize(upper_base + 0x3000),
            paddr: PhysAddr::from_usize(0xc000),
            size: 4096,
            flags: MappingFlags::WRITE,
        },
        PageTableAction::Map {
            vaddr: VirtAddr::from_usize(upper_base + 0x4000),
            paddr: PhysAddr::from_usize(0xd000),
            size: 4096,
            flags: MappingFlags::READ,
        },
    ]);
    let roots_are_live =
        allocation_contains(root.lower, 4096) && allocation_contains(root.upper, 4096);
    let lower_mapping = roots_are_live
        .then(|| lookup_root(root.lower, 0x3000))
        .flatten();
    let upper_mapping = roots_are_live
        .then(|| lookup_root(root.upper, upper_base + 0x3000))
        .flatten();
    let pending_flushes = flush_log();
    let frees_before_drop = deallocations();
    if roots_are_live {
        drop(table);
    } else {
        // A broken temporary value may have already deallocated both roots.
        core::mem::forget(table);
    }
    let remaining_count = allocation_count();
    let frees_after_drop = deallocations();
    reset_test_state();

    assert!(matches!(result, Err(PagingError::AllocationFailed)));
    assert!(roots_are_live);
    assert_eq!(lower_mapping.unwrap().paddr, 0xb000);
    assert_eq!(upper_mapping.unwrap().paddr, 0xc000);
    assert_eq!(
        pending_flushes,
        std::vec![
            Some(0x3000),
            Some(0x3000),
            Some(upper_base + 0x3000),
            Some(upper_base + 0x3000),
        ]
    );
    assert!(frees_before_drop.is_empty());
    assert_eq!(frees_after_drop.len(), 6);
    assert_eq!(remaining_count, 0);
}

/// Verifies that a dual iterator panic flushes its completed mutation and retains both roots.
///
/// Unwinding an upper-range mutation must preserve both independent page table hierarchies.
#[test]
fn opaque_dual_panicking_iterator_preserves_both_hierarchies_and_flushes() {
    reset_test_state();
    let page_table_type = OpaquePageTableType::<VirtAddr>::new_dual::<
        TestMeta,
        TestPte,
        TestUpperMeta,
        TestPte,
        TestPagingHandler,
    >();
    let mut table = page_table_type.new_pagetable_alloc().unwrap();
    let upper_base = !((1usize << (TestUpperMeta::VA_BITS - 1)) - 1);
    table
        .map(
            VirtAddr::from_usize(upper_base + 0x3000),
            PhysAddr::from_usize(0xa000),
            4096,
            MappingFlags::READ,
        )
        .unwrap();
    let OpaquePageTableRoot::Dual(root) = table.root() else {
        panic!("dual-root descriptor returned a single root");
    };
    clear_flush_log();
    let mut yielded = false;
    let actions = core::iter::from_fn(|| {
        assert!(
            !yielded,
            "injected dual iterator panic after one completed action"
        );
        yielded = true;
        Some(PageTableAction::Map {
            vaddr: VirtAddr::from_usize(upper_base + 0x3000),
            paddr: PhysAddr::from_usize(0xb000),
            size: 4096,
            flags: MappingFlags::WRITE,
        })
    });

    let result = catch_unwind(AssertUnwindSafe(|| table.apply_actions(actions)));
    let roots_are_live =
        allocation_contains(root.lower, 4096) && allocation_contains(root.upper, 4096);
    let upper_mapping = roots_are_live
        .then(|| lookup_root(root.upper, upper_base + 0x3000))
        .flatten();
    let pending_flushes = flush_log();
    let frees_before_drop = deallocations();
    if roots_are_live {
        drop(table);
    } else {
        // A broken temporary value may have already deallocated both roots.
        core::mem::forget(table);
    }
    let remaining_count = allocation_count();
    let frees_after_drop = deallocations();
    reset_test_state();

    assert!(result.is_err());
    assert!(roots_are_live);
    assert_eq!(upper_mapping.unwrap().paddr, 0xb000);
    assert_eq!(
        pending_flushes,
        std::vec![Some(upper_base + 0x3000), Some(upper_base + 0x3000)]
    );
    assert!(frees_before_drop.is_empty());
    assert_eq!(frees_after_drop.len(), 4);
    assert_eq!(remaining_count, 0);
}

/// Verifies that dropping a dual-root multi-level page table deallocates both hierarchies.
///
/// The two address ranges create independent page table hierarchies using one allocator type.
#[test]
fn dual_drop_deallocates_both_hierarchies() {
    reset_test_state();
    /// The dual-root multi-level page table type used for deallocation tests.
    ///
    /// Both canonical sides use independent compact page table hierarchies.
    type Table =
        DualPageTable<VirtAddr, TestMeta, TestPte, TestUpperMeta, TestPte, TestPagingHandler>;
    let data_frame = TestPagingHandler::alloc_frames(1).unwrap();
    let mut table = Table::new_alloc().unwrap();
    let upper_base = !((1usize << (TestUpperMeta::VA_BITS - 1)) - 1);
    {
        let mut cursor = table.cursor();
        cursor
            .map(
                VirtAddr::from_usize(0x3000),
                data_frame,
                TestMeta::PAGE_SIZE,
                MappingFlags::READ,
            )
            .unwrap();
        cursor
            .map(
                VirtAddr::from_usize(upper_base + 0x3000),
                PhysAddr::from_usize(0xb000),
                TestUpperMeta::PAGE_SIZE,
                MappingFlags::READ,
            )
            .unwrap();
        for base in [0, upper_base] {
            cursor
                .map(
                    VirtAddr::from_usize(base + 0x4000),
                    PhysAddr::from_usize(0xc000),
                    4096,
                    MappingFlags::WRITE,
                )
                .unwrap();
            cursor
                .map(
                    VirtAddr::from_usize(base + 0x8000),
                    PhysAddr::from_usize(0x100000),
                    16384,
                    MappingFlags::READ,
                )
                .unwrap();
        }
    }
    assert_eq!(allocation_count(), 9);
    assert_mapping_at_root(
        table.root().lower,
        0x9000,
        0x101000,
        MappingFlags::READ,
        16384,
    );
    assert_mapping_at_root(
        table.root().upper,
        upper_base + 0x9000,
        0x101000,
        MappingFlags::READ,
        16384,
    );

    drop(table);

    assert_eq!(allocation_count(), 1);
    assert!(allocation_contains(data_frame, 4096));
    assert_eq!(deallocations().len(), 8);
    reset_test_state();
}

/// Verifies that failing to allocate the second dual root deallocates the first root.
///
/// The result must leave no table frames allocated for an incomplete dual-root multi-level
/// page table.
#[test]
fn dual_new_alloc_failure_deallocates_first_root() {
    reset_test_state();
    /// The dual-root multi-level page table type used for allocation failure tests.
    ///
    /// Both roots allocate through the shared deterministic host allocator.
    type Table =
        DualPageTable<VirtAddr, TestMeta, TestPte, TestUpperMeta, TestPte, TestPagingHandler>;
    fail_on_alloc(2);

    assert!(matches!(
        Table::new_alloc(),
        Err(PagingError::AllocationFailed)
    ));
    assert_eq!(allocation_count(), 0);
    assert_eq!(deallocations().len(), 1);
    reset_test_state();
}

/// Verifies transfer of both concrete roots to another compatible allocator type.
///
/// Leaking preserves every table frame, and dropping the adopted value deallocates both page table
/// hierarchies once.
#[test]
fn leaked_dual_roots_can_be_adopted_by_another_allocator_type() {
    reset_test_state();
    /// The dual-root multi-level page table using the original allocator type.
    ///
    /// Both roots share the compact geometry and entry representation.
    type Table =
        DualPageTable<VirtAddr, TestMeta, TestPte, TestUpperMeta, TestPte, TestPagingHandler>;
    /// The compatible dual-root multi-level page table using the alternate allocator type.
    ///
    /// Its allocator can access and deallocate all frames created by the original type.
    type AdoptedTable = DualPageTable<
        VirtAddr,
        TestMeta,
        TestPte,
        TestUpperMeta,
        TestPte,
        AlternateTestPagingHandler,
    >;
    let data_frame = TestPagingHandler::alloc_frames(1).unwrap();
    let mut table = Table::new_alloc().unwrap();
    let upper_base = !((1usize << (TestUpperMeta::VA_BITS - 1)) - 1);
    {
        let mut cursor = table.cursor();
        for base in [0, upper_base] {
            cursor
                .map(
                    VirtAddr::from_usize(base + 0x3000),
                    data_frame,
                    4096,
                    MappingFlags::READ,
                )
                .unwrap();
        }
    }
    let previous_root = table.root();
    let root = table.leak();

    assert_eq!(root, previous_root);
    assert_ne!(root.lower, root.upper);
    assert_eq!(allocation_count(), 7);
    assert!(deallocations().is_empty());
    let adopted = unsafe { AdoptedTable::new_at(root) };
    assert_mapping_at_root(
        root.lower,
        0x3000,
        data_frame.as_usize(),
        MappingFlags::READ,
        4096,
    );
    assert_mapping_at_root(
        root.upper,
        upper_base + 0x3000,
        data_frame.as_usize(),
        MappingFlags::READ,
        4096,
    );
    drop(adopted);

    assert_eq!(allocation_count(), 1);
    assert!(allocation_contains(data_frame, 4096));
    assert_eq!(deallocations().len(), 6);
    reset_test_state();
}

/// Verifies replacement of an existing mapping at the same level.
///
/// The later physical range and flags must completely replace the earlier huge
/// mapping.
#[test]
fn cursor_overwrites_existing_mappings() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor();
        cursor
            .map(
                VirtAddr::from_usize(0x8000),
                PhysAddr::from_usize(0x18000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ,
            )
            .unwrap();
        cursor
            .map(
                VirtAddr::from_usize(0x8000),
                PhysAddr::from_usize(0x28000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ | MappingFlags::WRITE,
            )
            .unwrap();
    }

    assert_mapping(
        &table,
        0x9000,
        0x29000,
        MappingFlags::READ | MappingFlags::WRITE,
        TestMeta::LEVEL_PAGE_SIZE[1],
    );
    drop(table);
    reset_test_state();
}

/// Verifies replacement of a child table with one huge mapping.
///
/// Existing base-page leaves below that entry must no longer be visible after
/// the replacement.
#[test]
fn cursor_overwrites_lower_level_table_with_huge_mapping() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor();
        for (vaddr, paddr) in [(0xc000, 0x100000), (0xd000, 0x101000), (0xe000, 0x102000)] {
            cursor
                .map(
                    VirtAddr::from_usize(vaddr),
                    PhysAddr::from_usize(paddr),
                    TestMeta::PAGE_SIZE,
                    MappingFlags::READ,
                )
                .unwrap();
        }
        cursor
            .map(
                VirtAddr::from_usize(0xc000),
                PhysAddr::from_usize(0x200000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ | MappingFlags::WRITE,
            )
            .unwrap();
    }

    for (vaddr, paddr) in [(0xc000, 0x200000), (0xd000, 0x201000), (0xe000, 0x202000)] {
        assert_mapping(
            &table,
            vaddr,
            paddr,
            MappingFlags::READ | MappingFlags::WRITE,
            TestMeta::LEVEL_PAGE_SIZE[1],
        );
    }
    drop(table);
    reset_test_state();
}

/// Verifies preservation of neighboring pages when splitting a huge mapping.
///
/// Replacing one base page retains the original physical offsets and flags for
/// other pages and requests a conservative full TLB flush.
#[test]
fn splitting_huge_page_preserves_unaffected_subpages_and_full_flushes() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor();
        cursor
            .map(
                VirtAddr::from_usize(0x0000),
                PhysAddr::from_usize(0x40000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ,
            )
            .unwrap();
        cursor.clean_up();
        clear_flush_log();

        cursor
            .map(
                VirtAddr::from_usize(0x2000),
                PhysAddr::from_usize(0x90000),
                TestMeta::PAGE_SIZE,
                MappingFlags::READ | MappingFlags::WRITE,
            )
            .unwrap();
    }

    assert_eq!(flush_log(), std::vec![None]);
    assert_mapping(
        &table,
        0x0000,
        0x40000,
        MappingFlags::READ,
        TestMeta::PAGE_SIZE,
    );
    assert_mapping(
        &table,
        0x1000,
        0x41000,
        MappingFlags::READ,
        TestMeta::PAGE_SIZE,
    );
    assert_mapping(
        &table,
        0x2000,
        0x90000,
        MappingFlags::READ | MappingFlags::WRITE,
        TestMeta::PAGE_SIZE,
    );
    assert_mapping(
        &table,
        0x3000,
        0x43000,
        MappingFlags::READ,
        TestMeta::PAGE_SIZE,
    );
    drop(table);
    reset_test_state();
}

/// Verifies that removing an entire huge leaf requests a full TLB flush.
///
/// The huge mapping must be absent once the cursor is dropped.
#[test]
fn unmapping_huge_leaf_records_full_flush() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor();
        cursor
            .map(
                VirtAddr::from_usize(0x4000),
                PhysAddr::from_usize(0x50000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ,
            )
            .unwrap();
        cursor.clean_up();
        clear_flush_log();

        cursor
            .unmap(VirtAddr::from_usize(0x4000), TestMeta::LEVEL_PAGE_SIZE[1])
            .unwrap();
    }

    assert_unmapped(&table, 0x4000);
    assert_eq!(flush_log(), std::vec![None]);
    drop(table);
    reset_test_state();
}

/// Verifies unmapping one base page from inside a huge mapping.
///
/// The huge leaf is split, neighboring base pages preserve their mappings, and
/// the cursor requests a full TLB flush.
#[test]
fn unmapping_base_page_inside_huge_page_splits_and_full_flushes() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor();
        cursor
            .map(
                VirtAddr::from_usize(0x8000),
                PhysAddr::from_usize(0xa0000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ,
            )
            .unwrap();
        cursor.clean_up();
        clear_flush_log();

        cursor
            .unmap(VirtAddr::from_usize(0xa000), TestMeta::PAGE_SIZE)
            .unwrap();
    }

    assert_eq!(flush_log(), std::vec![None]);
    assert_mapping(
        &table,
        0x9000,
        0xa1000,
        MappingFlags::READ,
        TestMeta::PAGE_SIZE,
    );
    assert_unmapped(&table, 0xa000);
    assert_mapping(
        &table,
        0xb000,
        0xa3000,
        MappingFlags::READ,
        TestMeta::PAGE_SIZE,
    );
    drop(table);
    reset_test_state();
}

/// Prepares existing leaf mappings for tests of leaf-only invalidation.
///
/// Table creation is flushed separately so it cannot make a leaf or threshold test pass via Full.
fn prepare_existing_leaf_tables(
    table: &mut PageTable<TestMeta, TestPte, TestPagingHandler>,
    addresses: &[usize],
) {
    {
        let mut cursor = table.cursor();
        for &address in addresses {
            cursor
                .map(
                    VirtAddr::from_usize(address),
                    PhysAddr::from_usize(0x100000 + address),
                    TestMeta::PAGE_SIZE,
                    MappingFlags::READ,
                )
                .unwrap();
        }
    }
    {
        let mut cursor = table.cursor();
        for &address in addresses {
            cursor
                .unmap(VirtAddr::from_usize(address), TestMeta::PAGE_SIZE)
                .unwrap();
        }
    }
    clear_flush_log();
}

/// Flushes all local translations after installing a previously absent non-leaf table.
///
/// Address-specific RISC-V invalidation does not cover cached non-leaf translations.
#[test]
fn new_non_leaf_table_requires_full_invalidation() {
    reset_test_state();
    let mut table = new_table();
    table
        .cursor()
        .map(
            VirtAddr::from_usize(0x1000),
            PhysAddr::from_usize(0x8000),
            TestMeta::PAGE_SIZE,
            MappingFlags::READ,
        )
        .unwrap();
    assert_eq!(flush_log(), std::vec![None]);
    drop(table);
    reset_test_state();
}

/// Verifies that dropping a cursor flushes one changed base page.
///
/// No invalidation occurs before the drop, and the installed mapping remains
/// visible afterward.
#[test]
fn cursor_drop_flushes_single_page() {
    reset_test_state();
    let mut table = new_table();
    prepare_existing_leaf_tables(&mut table, &[0x1000]);
    let vaddr = VirtAddr::from_usize(0x1000);
    let paddr = PhysAddr::from_usize(0x8000);

    {
        let mut cursor = table.cursor();
        cursor
            .map(
                vaddr,
                paddr,
                TestMeta::PAGE_SIZE,
                MappingFlags::READ | MappingFlags::WRITE,
            )
            .unwrap();
        assert!(flush_log().is_empty());
    }

    assert_eq!(flush_log(), std::vec![Some(0x1000)]);
    assert_eq!(
        lookup(&table, 0x1000),
        Some(MappingSnapshot {
            paddr: 0x8000,
            flags: MappingFlags::READ | MappingFlags::WRITE,
            page_size: TestMeta::PAGE_SIZE,
        })
    );
    drop(table);
    reset_test_state();
}

/// Verifies that an explicit cursor flush is not repeated on drop.
///
/// Draining the pending set leaves no invalidation for the cursor destructor.
#[test]
fn cursor_manual_flush_is_idempotent_with_drop() {
    reset_test_state();
    let mut table = new_table();
    prepare_existing_leaf_tables(&mut table, &[0x2000]);

    {
        let mut cursor = table.cursor();
        cursor
            .map(
                VirtAddr::from_usize(0x2000),
                PhysAddr::from_usize(0x9000),
                TestMeta::PAGE_SIZE,
                MappingFlags::READ,
            )
            .unwrap();
        cursor.clean_up();
        assert_eq!(flush_log(), std::vec![Some(0x2000)]);
    }

    assert_eq!(flush_log(), std::vec![Some(0x2000)]);
    drop(table);
    reset_test_state();
}

/// Verifies promotion from individual invalidations to a full flush.
///
/// Recording more pages than [`SMALL_FLUSH_THRESHOLD`] can hold must issue one
/// full local TLB invalidation.
#[test]
fn cursor_flush_threshold_falls_back_to_full_flush() {
    reset_test_state();
    let mut table = new_table();
    prepare_existing_leaf_tables(
        &mut table,
        &(0..=SMALL_FLUSH_THRESHOLD)
            .map(|page| page * TestMeta::PAGE_SIZE)
            .collect::<Vec<_>>(),
    );

    {
        let mut cursor = table.cursor();
        for page in 0..=SMALL_FLUSH_THRESHOLD {
            cursor
                .map(
                    VirtAddr::from_usize(page * TestMeta::PAGE_SIZE),
                    PhysAddr::from_usize(0x100000 + page * TestMeta::PAGE_SIZE),
                    TestMeta::PAGE_SIZE,
                    MappingFlags::READ | MappingFlags::WRITE,
                )
                .unwrap();
        }
        assert!(flush_log().is_empty());
    }

    assert_eq!(flush_log(), std::vec![None]);
    drop(table);
    reset_test_state();
}

/// Verifies drop-time flushing after a partially successful operation.
///
/// A later allocation failure leaves the earlier huge mapping installed, and
/// the cursor still invalidates that successful change when dropped.
#[test]
fn partial_success_still_flushes_on_drop_after_error() {
    reset_test_state();
    let mut table = new_table();
    fail_on_alloc(2);

    {
        let mut cursor = table.cursor();
        let result = cursor.map(
            VirtAddr::from_usize(0x0000),
            PhysAddr::from_usize(0x800000),
            TestMeta::LEVEL_PAGE_SIZE[2] + TestMeta::PAGE_SIZE,
            MappingFlags::READ | MappingFlags::WRITE,
        );

        assert!(matches!(result, Err(PagingError::AllocationFailed)));
        assert_mapping(
            &*cursor.table,
            0x0000,
            0x800000,
            MappingFlags::READ | MappingFlags::WRITE,
            TestMeta::LEVEL_PAGE_SIZE[2],
        );
        assert_unmapped(&*cursor.table, TestMeta::LEVEL_PAGE_SIZE[2]);
        assert!(flush_log().is_empty());
    }

    assert_eq!(flush_log(), std::vec![Some(0x0000)]);
    drop(table);
    reset_test_state();
}

/// Compares deterministic mapping pressure against a shadow model.
///
/// Repeated pseudo-random map and unmap operations must leave every base page in
/// the same state as the simple in-memory model.
#[test]
fn deterministic_pressure_matches_shadow_model() {
    /// The number of base pages represented by the shadow model.
    ///
    /// The compact address range permits exhaustive checking after every step.
    const BASE_PAGES: usize = 64;

    /// Advances the deterministic pseudo-random sequence.
    ///
    /// The linear congruential generator is stable and requires no external
    /// randomness source.
    fn next(seed: &mut u64) -> u64 {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *seed
    }

    reset_test_state();
    let mut table = new_table();
    let mut shadow = [None; BASE_PAGES];
    let mut seed = 0x6d2b_79f5_aa34_1d29;

    {
        let mut cursor = table.cursor();

        for step in 0..256 {
            let map_operation = next(&mut seed) & 1 == 0;
            let page_count = (next(&mut seed) as usize % 4) + 1;
            let start_page = next(&mut seed) as usize % (BASE_PAGES - page_count + 1);
            let vaddr = start_page * TestMeta::PAGE_SIZE;
            let size = page_count * TestMeta::PAGE_SIZE;

            if map_operation {
                let paddr = 0x400000 + step * 0x10000;
                cursor
                    .map(
                        VirtAddr::from_usize(vaddr),
                        PhysAddr::from_usize(paddr),
                        size,
                        MappingFlags::READ | MappingFlags::WRITE,
                    )
                    .unwrap();

                for offset in 0..page_count {
                    shadow[start_page + offset] = Some(paddr + offset * TestMeta::PAGE_SIZE);
                }
            } else {
                cursor.unmap(VirtAddr::from_usize(vaddr), size).unwrap();

                for offset in 0..page_count {
                    shadow[start_page + offset] = None;
                }
            }

            for (page, expected) in shadow.iter().copied().enumerate() {
                assert_eq!(
                    lookup(&*cursor.table, page * TestMeta::PAGE_SIZE).map(|mapping| mapping.paddr),
                    expected,
                    "step {step}, page {page}, range {start_page}..{}",
                    start_page + page_count
                );
            }
        }
    }

    drop(table);
    reset_test_state();
}
