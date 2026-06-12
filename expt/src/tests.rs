use super::*;
use core::alloc::Layout;
use core::cell::RefCell;
use memory_addr::{PhysAddr, VirtAddr};
use page_table_entry::{GenericPTE, MappingFlags};
use std::alloc::{alloc, dealloc};
use std::vec::Vec;

const PTE_UNUSED: u8 = 0;
const PTE_TABLE: u8 = 1;
const PTE_PAGE: u8 = 2;
const PTE_HUGE: u8 = 3;

#[derive(Clone, Copy, Debug)]
struct TestPte {
    paddr: usize,
    flags: MappingFlags,
    kind: u8,
    _reserved: [u8; 7],
}

impl TestPte {
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
    fn default() -> Self {
        Self::empty()
    }
}

impl GenericPTE for TestPte {
    fn new_page(paddr: PhysAddr, flags: MappingFlags, is_huge: bool) -> Self {
        Self {
            paddr: paddr.as_usize(),
            flags,
            kind: if is_huge { PTE_HUGE } else { PTE_PAGE },
            _reserved: [0; 7],
        }
    }
    fn new_table(paddr: PhysAddr) -> Self {
        Self {
            paddr: paddr.as_usize(),
            flags: MappingFlags::empty(),
            kind: PTE_TABLE,
            _reserved: [0; 7],
        }
    }
    fn paddr(&self) -> PhysAddr {
        PhysAddr::from_usize(self.paddr)
    }
    fn flags(&self) -> MappingFlags {
        self.flags
    }
    fn set_paddr(&mut self, paddr: PhysAddr) {
        self.paddr = paddr.as_usize();
    }
    fn set_flags(&mut self, flags: MappingFlags, is_huge: bool) {
        self.flags = flags;
        self.kind = if is_huge { PTE_HUGE } else { PTE_PAGE };
    }
    fn bits(self) -> usize {
        self.paddr | self.kind as usize
    }
    fn is_unused(&self) -> bool {
        self.kind == PTE_UNUSED
    }
    fn is_present(&self) -> bool {
        matches!(self.kind, PTE_TABLE | PTE_PAGE | PTE_HUGE)
    }
    fn is_huge(&self) -> bool {
        self.kind == PTE_HUGE
    }
    fn clear(&mut self) {
        *self = Self::empty();
    }
}

struct TestMeta;

impl PageTableMeta for TestMeta {
    type VirtAddr = VirtAddr;
    const LEVELS: usize = 3;
    const PAGE_OFFSET_BITS: usize = 12;
    const LEVEL_BITS: [usize; Self::LEVELS] = [2, 2, 2];
    const MAX_PAGE_LEVEL: usize = 2;
    fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
        FLUSH_LOG.with(|log| {
            log.borrow_mut().push(vaddr.map(|vaddr| vaddr.as_usize()));
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct MappingSnapshot {
    paddr: usize,
    flags: MappingFlags,
    page_size: usize,
}

#[derive(Clone, Copy)]
struct AllocationRecord {
    ptr: usize,
    layout: Layout,
}

#[derive(Default)]
struct AllocState {
    allocations: Vec<AllocationRecord>,
    alloc_count: usize,
    fail_on_alloc: Option<usize>,
}

impl AllocState {
    fn dealloc_all(&mut self) {
        for record in self.allocations.drain(..) {
            unsafe { dealloc(record.ptr as *mut u8, record.layout) };
        }
    }
}

impl Drop for AllocState {
    fn drop(&mut self) {
        self.dealloc_all();
    }
}

std::thread_local! {
    static ALLOC_STATE: RefCell<AllocState> = RefCell::new(AllocState::default());
    static FLUSH_LOG: RefCell<Vec<Option<usize>>> = const { RefCell::new(Vec::new()) };
}

struct TestPagingHandler;

impl PagingHandler for TestPagingHandler {
    fn alloc_page_aligned(bytes_required: usize) -> Option<PhysAddr> {
        ALLOC_STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.alloc_count += 1;
            if state.fail_on_alloc == Some(state.alloc_count) {
                return None;
            }
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
    fn dealloc_page_aligned(addr: PhysAddr, bytes_deallocated: usize) {
        ALLOC_STATE.with(|state| {
            let mut state = state.borrow_mut();
            let index = state
                .allocations
                .iter()
                .position(|record| record.ptr == addr.as_usize())
                .expect("deallocating an unknown test page-table allocation");
            let record = state.allocations[index];
            assert_eq!(record.layout.size(), bytes_deallocated);
            let record = state.allocations.remove(index);
            unsafe { dealloc(record.ptr as *mut u8, record.layout) };
        });
    }
    fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
        VirtAddr::from_usize(addr.as_usize())
    }
}

fn reset_test_state() {
    ALLOC_STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.dealloc_all();
        state.alloc_count = 0;
        state.fail_on_alloc = None;
    });
    FLUSH_LOG.with(|log| log.borrow_mut().clear());
}

fn fail_on_alloc(n: usize) {
    ALLOC_STATE.with(|state| state.borrow_mut().fail_on_alloc = Some(n));
}
fn allocation_contains(paddr: PhysAddr, bytes: usize) -> bool {
    ALLOC_STATE.with(|state| {
        state
            .borrow()
            .allocations
            .iter()
            .any(|record| record.ptr == paddr.as_usize() && record.layout.size() == bytes)
    })
}
fn flush_log() -> Vec<Option<usize>> {
    FLUSH_LOG.with(|log| log.borrow().clone())
}
fn clear_flush_log() {
    FLUSH_LOG.with(|log| log.borrow_mut().clear());
}

fn index_for(level: usize, vaddr: usize) -> usize {
    let (start, end) = TestMeta::LEVEL_BIT_RANGES[level];
    (vaddr >> start) & ((1 << (end - start)) - 1)
}

fn table_slice(_table: &PageTable<TestMeta, TestPte>, paddr: PhysAddr, level: usize) -> &[TestPte] {
    let entry_count = TestMeta::LEVEL_TABLE_SIZE[level];
    let ptr = TestPagingHandler::phys_to_virt(paddr).as_ptr() as *const TestPte;
    unsafe { core::slice::from_raw_parts(ptr, entry_count) }
}

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

fn new_table() -> PageTable<TestMeta, TestPte> {
    PageTable::<TestMeta, TestPte>::new_alloc::<TestPagingHandler>().unwrap()
}

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

fn assert_unmapped(table: &PageTable<TestMeta, TestPte>, vaddr: usize) {
    assert_eq!(lookup(table, vaddr), None);
}

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

#[test]
fn new_alloc_initializes_zeroed_root_table() {
    reset_test_state();

    let table = PageTable::<TestMeta, TestPte>::new_alloc::<TestPagingHandler>().unwrap();
    let root_paddr = table.root_paddr();
    assert_ne!(root_paddr.as_usize(), 0);
    assert!(allocation_contains(
        root_paddr,
        core::mem::size_of::<TestPte>() * TestMeta::LEVEL_TABLE_SIZE[TestMeta::LEVELS - 1]
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

#[test]
fn cursor_maps_and_unmaps_single_base_page() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
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

        cursor.flush();
        cursor
            .unmap(VirtAddr::from_usize(0x3000), TestMeta::PAGE_SIZE)
            .unwrap();
    }

    assert_unmapped(&table, 0x3000);
    assert_eq!(flush_log(), std::vec![Some(0x3000), Some(0x3000)]);
    reset_test_state();
}

#[test]
fn cursor_unmaps_empty_range_without_flush_or_mapping() {
    reset_test_state();
    let mut table = new_table();
    let root_index = index_for(TestMeta::LEVELS - 1, 0x4000);
    assert!(table_slice(&table, table.root_paddr(), TestMeta::LEVELS - 1)[root_index].is_unused());

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
        cursor
            .unmap(VirtAddr::from_usize(0x4000), TestMeta::PAGE_SIZE)
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

#[test]
fn cursor_maps_unaligned_range_with_virtual_physical_offset() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
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
    reset_test_state();
}

#[test]
fn cursor_uses_largest_possible_page_levels() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
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
    reset_test_state();
}

#[test]
fn cursor_overwrites_existing_mappings() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
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
    reset_test_state();
}

#[test]
fn cursor_overwrites_lower_level_table_with_huge_mapping() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
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
    reset_test_state();
}

#[test]
fn splitting_huge_page_preserves_unaffected_subpages_and_full_flushes() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
        cursor
            .map(
                VirtAddr::from_usize(0x0000),
                PhysAddr::from_usize(0x40000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ,
            )
            .unwrap();
        cursor.flush();
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
    reset_test_state();
}

#[test]
fn unmapping_huge_leaf_records_full_flush() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
        cursor
            .map(
                VirtAddr::from_usize(0x4000),
                PhysAddr::from_usize(0x50000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ,
            )
            .unwrap();
        cursor.flush();
        clear_flush_log();

        cursor
            .unmap(VirtAddr::from_usize(0x4000), TestMeta::LEVEL_PAGE_SIZE[1])
            .unwrap();
    }

    assert_unmapped(&table, 0x4000);
    assert_eq!(flush_log(), std::vec![None]);
    reset_test_state();
}

#[test]
fn unmapping_base_page_inside_huge_page_splits_and_full_flushes() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
        cursor
            .map(
                VirtAddr::from_usize(0x8000),
                PhysAddr::from_usize(0xa0000),
                TestMeta::LEVEL_PAGE_SIZE[1],
                MappingFlags::READ,
            )
            .unwrap();
        cursor.flush();
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
    reset_test_state();
}

#[test]
fn cursor_drop_flushes_single_page() {
    reset_test_state();
    let mut table = new_table();
    let vaddr = VirtAddr::from_usize(0x1000);
    let paddr = PhysAddr::from_usize(0x8000);

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
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
    reset_test_state();
}

#[test]
fn cursor_manual_flush_is_idempotent_with_drop() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
        cursor
            .map(
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

#[test]
fn cursor_flush_threshold_falls_back_to_full_flush() {
    reset_test_state();
    let mut table = new_table();

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
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
    reset_test_state();
}

#[test]
fn partial_success_still_flushes_on_drop_after_error() {
    reset_test_state();
    let mut table = new_table();
    fail_on_alloc(2);

    {
        let mut cursor = table.cursor::<TestPagingHandler>();
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
    reset_test_state();
}

#[test]
fn deterministic_pressure_matches_shadow_model() {
    const BASE_PAGES: usize = 64;

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
        let mut cursor = table.cursor::<TestPagingHandler>();

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

    reset_test_state();
}
