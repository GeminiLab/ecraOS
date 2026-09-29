use std::{
    alloc::{Layout, alloc, dealloc},
    cell::RefCell,
    vec::Vec,
};

use expalloc_trait::PageAllocator;
use memory_addr::{AddrRange, PhysAddr, VirtAddr};
use page_table_entry::{GenericPTE, MappingFlags};

use crate::{
    error::PagingError,
    meta::{LowerCoverage, PageTableCoverage, PageTableMeta, SymmetricCoverage, UpperCoverage},
    opaque::{OpaquePageTableRoot, OpaquePageTableType, PageTableAction},
    pt::{PageTable, PageTableCursorLike, single::checked_page_end},
};

/// The tag for an unused test entry.
///
/// Zero makes freshly cleared table memory represent unused entries.
const PTE_UNUSED: u8 = 0;
/// The tag for a child-table test entry.
///
/// Entries with this tag point to the next lower page-table level.
const PTE_TABLE: u8 = 1;
/// The tag for a base-page test entry.
///
/// Entries with this tag map one level-zero page.
const PTE_PAGE: u8 = 2;
/// The tag for a huge-page test entry.
///
/// Entries with this tag terminate traversal above level zero.
const PTE_HUGE: u8 = 3;

/// A compact page-table entry used by the unit tests.
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
    /// Child-table and unused entries keep this value empty.
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
    /// Creates a child-table test entry.
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
    /// Unused and child-table entries return an empty flag set.
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

/// Metadata for the compact page-table geometry used in tests.
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
    type Coverage = LowerCoverage<Self::VirtAddr>;
    /// The highest level permitted to contain a test leaf.
    ///
    /// Every level in the compact format supports page mappings.
    const MAX_PAGE_LEVEL: usize = 2;
    /// Records a requested test TLB invalidation.
    ///
    /// Addresses are stored as integers and [`None`] represents a full flush.
    fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
        FLUSH_LOG.with(|log| {
            log.borrow_mut().push(vaddr.map(|vaddr| vaddr.as_usize()));
        });
    }
}

/// Metadata for the upper half of the dual-root opaque-table test.
#[cfg(target_arch = "aarch64")]
struct TestUpperMeta;

#[cfg(target_arch = "aarch64")]
impl PageTableMeta for TestUpperMeta {
    /// The virtual-address type used by the test format.
    type VirtAddr = VirtAddr;
    /// The three levels used by the dual-root test format.
    const LEVELS: usize = 3;
    /// The twelve-bit offset of a 4 KiB base page.
    const PAGE_OFFSET_BITS: usize = 12;
    /// The two virtual-address bits handled at each test level.
    const LEVEL_BITS: [usize; Self::LEVELS] = [2, 2, 2];
    /// The compact test format covers the upper virtual-address range.
    type Coverage = UpperCoverage<Self::VirtAddr>;
    /// Every level in the compact format supports page mappings.
    const MAX_PAGE_LEVEL: usize = 2;

    /// Records a requested test TLB invalidation.
    fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
        FLUSH_LOG.with(|log| {
            log.borrow_mut().push(vaddr.map(|vaddr| vaddr.as_usize()));
        });
    }
}

/// Metadata for testing symmetric canonical-address validation.
///
/// The compact geometry keeps the canonical-boundary tests independent from a
/// target architecture's page-table implementation.
struct SymmetricTestMeta;

impl PageTableMeta for SymmetricTestMeta {
    /// The virtual-address type used by the symmetric validation tests.
    type VirtAddr = VirtAddr;
    /// The three levels used by the validation tests.
    const LEVELS: usize = 3;
    /// The twelve-bit offset of a 4 KiB base page.
    const PAGE_OFFSET_BITS: usize = 12;
    /// The two virtual-address bits handled at each test level.
    const LEVEL_BITS: [usize; Self::LEVELS] = [2, 2, 2];
    /// The format covers canonical lower and upper ranges symmetrically.
    type Coverage = SymmetricCoverage<Self::VirtAddr>;

    /// The validation tests do not issue TLB invalidations.
    fn flush_tlb(_vaddr: Option<Self::VirtAddr>) {}
}

/// The resolved mapping observed by the test page-table walker.
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
    /// Releases allocations left in the test state.
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

/// A host-backed [`PageAllocator`] used by the page-table tests.
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
            unsafe { dealloc(record.ptr as *mut u8, record.layout) };
        });
    }
    /// Converts a simulated physical address to its host virtual address.
    ///
    /// The test allocator uses an identity mapping between the two domains.
    fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
        VirtAddr::from_usize(addr.as_usize())
    }
}

/// Restores the allocator and TLB log to their initial state.
///
/// Live allocations are released and failure injection is disabled.
fn reset_test_state() {
    ALLOC_STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.dealloc_all();
        state.alloc_count = 0;
        state.fail_on_alloc = None;
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
/// Returns a snapshot of recorded TLB invalidations.
///
/// Cloning the log allows assertions without retaining a thread-local borrow.
fn flush_log() -> Vec<Option<usize>> {
    FLUSH_LOG.with(|log| log.borrow().clone())
}
/// Clears all recorded TLB invalidations.
///
/// Allocator state and page-table contents are left unchanged.
fn clear_flush_log() {
    FLUSH_LOG.with(|log| log.borrow_mut().clear());
}

/// Computes the test-table index for a virtual address and level.
///
/// The calculation uses the bit ranges derived from [`TestMeta`].
fn index_for(level: usize, vaddr: usize) -> usize {
    let (start, end) = TestMeta::LEVEL_BIT_RANGES[level];
    (vaddr >> start) & ((1 << (end - start)) - 1)
}

/// Borrows the test table entries stored at a physical address.
///
/// The caller-provided page-table handle ties the returned slice to the logical
/// table under inspection, while the test allocator performs identity mapping.
fn table_slice(_table: &PageTable<TestMeta, TestPte>, paddr: PhysAddr, level: usize) -> &[TestPte] {
    let entry_count = TestMeta::LEVEL_TABLE_SIZE[level];
    let ptr = TestPagingHandler::phys_to_virt(paddr).as_ptr() as *const TestPte;
    unsafe { core::slice::from_raw_parts(ptr, entry_count) }
}

/// Resolves one virtual address through the compact test page table.
///
/// The walk stops at an unused entry or at the first base-page or huge-page leaf.
fn lookup(table: &PageTable<TestMeta, TestPte>, vaddr: usize) -> Option<MappingSnapshot> {
    let mut table_paddr = table.root_paddr();
    let mut level = TestMeta::LEVELS - 1;
    loop {
        let entries = table_slice(table, table_paddr, level);
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

/// Allocates a fresh compact test page table.
///
/// Allocation failure is unexpected unless a test configures it explicitly.
fn new_table() -> PageTable<TestMeta, TestPte> {
    PageTable::<TestMeta, TestPte>::new_alloc::<TestPagingHandler>().unwrap()
}

/// Asserts that a virtual address resolves to an expected mapping.
///
/// The assertion compares the translated physical address, mapping flags, and
/// leaf page size.
fn assert_mapping(
    table: &PageTable<TestMeta, TestPte>,
    vaddr: usize,
    paddr: usize,
    flags: MappingFlags,
    page_size: usize,
) {
    assert_eq!(
        lookup(table, vaddr),
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
fn assert_unmapped(table: &PageTable<TestMeta, TestPte>, vaddr: usize) {
    assert_eq!(lookup(table, vaddr), None);
}

/// Asserts recursively that a subtree contains no leaf mappings.
///
/// Every live entry encountered above level zero must point to another table.
fn assert_subtree_has_no_leaf_mappings(
    table: &PageTable<TestMeta, TestPte>,
    paddr: PhysAddr,
    level: usize,
) {
    for entry in table_slice(table, paddr, level) {
        if entry.is_unused() {
            continue;
        }

        assert!(level > 0);
        assert_eq!(entry.kind, PTE_TABLE);
        assert_subtree_has_no_leaf_mappings(table, entry.paddr(), level - 1);
    }
}

/// Verifies root allocation, clearing, and allocation failure.
///
/// A new root must occupy one frame, contain only unused entries, and propagate
/// a failure from its first allocator request.
#[test]
fn new_alloc_initializes_zeroed_root_table() {
    reset_test_state();

    let table = PageTable::<TestMeta, TestPte>::new_alloc::<TestPagingHandler>().unwrap();
    let root_paddr = table.root_paddr();
    assert_ne!(root_paddr.as_usize(), 0);
    assert!(allocation_contains(
        root_paddr,
        1 << TestPagingHandler::page_size_shift()
    ));
    for entry in table_slice(&table, root_paddr, TestMeta::LEVELS - 1) {
        assert!(entry.is_unused());
        assert_eq!(entry.paddr().as_usize(), 0);
        assert!(entry.flags().is_empty());
        assert_eq!(entry.bits(), 0);
    }

    reset_test_state();
    fail_on_alloc(1);
    assert!(matches!(
        PageTable::<TestMeta, TestPte>::new_alloc::<TestPagingHandler>(),
        Err(PagingError::AllocationFailed)
    ));
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
            .map::<TestPagingHandler>(
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

        cursor.flush();
        cursor
            .unmap::<TestPagingHandler>(VirtAddr::from_usize(0x3000), TestMeta::PAGE_SIZE)
            .unwrap();
    }

    assert_unmapped(&table, 0x3000);
    assert_eq!(flush_log(), std::vec![Some(0x3000), Some(0x3000)]);
    reset_test_state();
}

/// Verifies that unmapping an absent page records no TLB invalidation.
///
/// Intermediate tables created during traversal remain allocated but contain no
/// leaf mappings.
#[test]
fn cursor_unmaps_empty_range_without_flush_or_mapping() {
    reset_test_state();
    let mut table = new_table();
    let root_index = index_for(TestMeta::LEVELS - 1, 0x4000);
    assert!(table_slice(&table, table.root_paddr(), TestMeta::LEVELS - 1)[root_index].is_unused());

    {
        let mut cursor = table.cursor();
        cursor
            .unmap::<TestPagingHandler>(VirtAddr::from_usize(0x4000), TestMeta::PAGE_SIZE)
            .unwrap();
        assert_unmapped(&*cursor.table, 0x4000);
    }

    assert_eq!(
        table_slice(&table, table.root_paddr(), TestMeta::LEVELS - 1)[root_index].kind,
        PTE_TABLE
    );
    assert_subtree_has_no_leaf_mappings(&table, table.root_paddr(), TestMeta::LEVELS - 1);
    assert_unmapped(&table, 0x4000);
    assert!(flush_log().is_empty());
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
            .map::<TestPagingHandler>(
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
            .map::<TestPagingHandler>(
                VirtAddr::from_usize(0x10000),
                PhysAddr::from_usize(0x20000),
                TestMeta::LEVEL_PAGE_SIZE[2],
                MappingFlags::READ,
            )
            .unwrap();
        cursor
            .map::<TestPagingHandler>(
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

/// Verifies canonical validation for symmetric virtual-address coverage.
#[test]
fn symmetric_vaddr_validation_rejects_canonical_hole() {
    type Table = PageTable<SymmetricTestMeta, TestPte>;
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
#[test]
fn opaque_page_table_applies_action_batch() {
    reset_test_state();
    let page_table_type = OpaquePageTableType::<VirtAddr>::new::<TestMeta, TestPte>();
    let mut opaque_page_table = page_table_type
        .new_pagetable_alloc::<TestPagingHandler>()
        .unwrap();

    opaque_page_table
        .apply_actions::<TestPagingHandler, _>([
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

    #[cfg(target_arch = "aarch64")]
    let OpaquePageTableRoot::Single(root) = opaque_page_table.root() else {
        panic!("single-root opaque table returned a dual root");
    };
    #[cfg(not(target_arch = "aarch64"))]
    let OpaquePageTableRoot::Single(root) = opaque_page_table.root();
    let table = unsafe { PageTable::<TestMeta, TestPte>::new_at(root) };
    assert_mapping(
        &table,
        0x3000,
        0xa000,
        MappingFlags::READ,
        TestMeta::PAGE_SIZE,
    );
    assert_mapping(
        &table,
        0x4000,
        0xb000,
        MappingFlags::READ | MappingFlags::WRITE,
        TestMeta::PAGE_SIZE,
    );
    assert_eq!(flush_log(), std::vec![Some(0x3000), Some(0x4000)]);
    reset_test_state();
}

/// Verifies that an opaque dual table dispatches actions to both roots.
#[cfg(target_arch = "aarch64")]
#[test]
fn opaque_dual_page_table_applies_actions_to_both_roots() {
    reset_test_state();
    let page_table_type =
        OpaquePageTableType::<VirtAddr>::new_dual::<TestMeta, TestPte, TestUpperMeta, TestPte>();
    let mut opaque_page_table = page_table_type
        .new_pagetable_alloc::<TestPagingHandler>()
        .unwrap();
    let upper_base = !((1usize << (TestUpperMeta::VA_BITS - 1)) - 1);

    opaque_page_table
        .apply_actions::<TestPagingHandler, _>([
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
    assert_eq!(flush_log(), vec![Some(0x3000), Some(upper_base + 0x3000)]);
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
            .map::<TestPagingHandler>(
                VirtAddr::from_usize(0x8000),
                PhysAddr::from_usize(0x18000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ,
            )
            .unwrap();
        cursor
            .map::<TestPagingHandler>(
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
                .map::<TestPagingHandler>(
                    VirtAddr::from_usize(vaddr),
                    PhysAddr::from_usize(paddr),
                    TestMeta::PAGE_SIZE,
                    MappingFlags::READ,
                )
                .unwrap();
        }
        cursor
            .map::<TestPagingHandler>(
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
            .map::<TestPagingHandler>(
                VirtAddr::from_usize(0x0000),
                PhysAddr::from_usize(0x40000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ,
            )
            .unwrap();
        cursor.flush();
        clear_flush_log();

        cursor
            .map::<TestPagingHandler>(
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
            .map::<TestPagingHandler>(
                VirtAddr::from_usize(0x4000),
                PhysAddr::from_usize(0x50000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ,
            )
            .unwrap();
        cursor.flush();
        clear_flush_log();

        cursor
            .unmap::<TestPagingHandler>(VirtAddr::from_usize(0x4000), TestMeta::LEVEL_PAGE_SIZE[1])
            .unwrap();
    }

    assert_unmapped(&table, 0x4000);
    assert_eq!(flush_log(), std::vec![None]);
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
            .map::<TestPagingHandler>(
                VirtAddr::from_usize(0x8000),
                PhysAddr::from_usize(0xa0000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ,
            )
            .unwrap();
        cursor.flush();
        clear_flush_log();

        cursor
            .unmap::<TestPagingHandler>(VirtAddr::from_usize(0xa000), TestMeta::PAGE_SIZE)
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
    let vaddr = VirtAddr::from_usize(0x1000);
    let paddr = PhysAddr::from_usize(0x8000);

    {
        let mut cursor = table.cursor();
        cursor
            .map::<TestPagingHandler>(
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
    reset_test_state();
}

/// Verifies that an explicit cursor flush is not repeated on drop.
///
/// Draining the pending set leaves no invalidation for the cursor destructor.
#[test]
fn cursor_manual_flush_is_idempotent_with_drop() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor();
        cursor
            .map::<TestPagingHandler>(
                VirtAddr::from_usize(0x2000),
                PhysAddr::from_usize(0x9000),
                TestMeta::PAGE_SIZE,
                MappingFlags::READ,
            )
            .unwrap();
        cursor.flush();
        assert_eq!(flush_log(), std::vec![Some(0x2000)]);
    }

    assert_eq!(flush_log(), std::vec![Some(0x2000)]);
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

    {
        let mut cursor = table.cursor();
        for page in 0..=super::flush::SMALL_FLUSH_THRESHOLD {
            cursor
                .map::<TestPagingHandler>(
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
        let result = cursor.map::<TestPagingHandler>(
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
                    .map::<TestPagingHandler>(
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
                cursor
                    .unmap::<TestPagingHandler>(VirtAddr::from_usize(vaddr), size)
                    .unwrap();

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

    reset_test_state();
}
