//! Virtual memory management.

use core::{
    mem,
    ops::DerefMut,
    sync::atomic::{AtomicBool, Ordering},
};

use exarch::mem::{VirtAddrSpaceMode, VirtAddrSpaceModes};
use expalloc_trait::PageAllocator;
use expt::opaque::{OpaquePageTable, OpaquePageTableRoot, OpaquePageTableType};
use kspin::SpinNoIrq;
use lazyinit::LazyInit;
use log::info;
use memory_addr::{AddrRangeBounds, PhysAddr, VirtAddr, VirtAddrRange, va};
use size_disp::SizeDisplay;

use crate::{
    kprintln,
    mem::{MemoryRegionFlags, allocs, early, pmm::MemoryRegions, region_flags_to_mapping},
};

/// The layout of the virtual address space.
pub struct VirtualAddressSpaceLayout {
    /// The selected architecture translation mode.
    mode: VirtAddrSpaceMode,
    /// The logarithm of the base-page size in bytes.
    page_shift: u8,
    /// The virtual range reserved for the physical-memory direct map.
    direct_mapping_range: VirtAddrRange,
    /// The virtual range reserved for independent kernel allocations.
    vmalloc_range: VirtAddrRange,
}

/// The virtual address space.
pub struct VirtualAddressSpace {
    /// The selected kernel virtual-address layout.
    layout: VirtualAddressSpaceLayout,
    /// The descriptor for the currently selected page-table allocator.
    ///
    /// Early initialization uses the early allocator and post-relocation handoff replaces it with
    /// the formal allocator's descriptor.
    page_table_type_mutex: SpinNoIrq<OpaquePageTableType<VirtAddr>>,
    /// The owner of the kernel page-table pages.
    page_table_mutex: SpinNoIrq<OpaquePageTable<VirtAddr>>,
}

/// The kernel virtual address space.
static VIRTUAL_ADDRESS_SPACE: LazyInit<VirtualAddressSpace> = LazyInit::new();

/// Indicates that the final page-table roots are active.
///
/// Address conversion uses the direct map only after the processor has installed these roots.
static PAGE_TABLE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Returns whether the virtual address space has been initialized.
///
/// Layout availability alone does not imply that the final page-table roots are active.
pub fn is_initialized() -> bool {
    VIRTUAL_ADDRESS_SPACE.get().is_some()
}

/// Returns the selected architecture translation mode.
///
/// The virtual address space must have been initialized before this accessor is called.
#[inline]
#[expect(unused)]
pub fn mode() -> VirtAddrSpaceMode {
    VIRTUAL_ADDRESS_SPACE.layout.mode
}

/// Returns the logarithm of the selected base-page size in bytes.
///
/// Both halves of the kernel address space use the returned page-size shift.
#[inline]
pub fn page_size_shift() -> usize {
    VIRTUAL_ADDRESS_SPACE.layout.page_shift as usize
}

/// Returns the selected base-page size in bytes.
///
/// Physical allocation and virtual mapping operations share this granularity.
#[inline]
pub fn page_size() -> usize {
    1usize << page_size_shift()
}

/// Returns the number of base pages needed for a byte count.
///
/// A partial final page is included in the returned count.
#[inline]
pub fn page_count_for_bytes(bytes: usize) -> usize {
    bytes.div_ceil(page_size())
}

/// Returns the offset between physical addresses and their direct-map virtual addresses.
///
/// The integer represents a translation offset used during relocation and physical-memory access.
#[inline]
pub fn direct_mapping_offset() -> usize {
    VIRTUAL_ADDRESS_SPACE
        .layout
        .direct_mapping_range
        .start
        .as_usize()
}

/// Returns the first virtual address reserved for vmalloc.
///
/// Early boot uses this boundary when reserving the BSP stack.
#[inline]
pub fn vmalloc_base() -> VirtAddr {
    vmalloc_range().start
}

/// Returns the virtual-address range reserved for vmalloc.
///
/// The formal virtual allocator uses this reservation after early boot completes.
#[inline]
pub fn vmalloc_range() -> VirtAddrRange {
    VIRTUAL_ADDRESS_SPACE.layout.vmalloc_range
}

/// Translates a direct-map virtual address into a physical address.
///
/// Returns [`None`] when the layout is unavailable or the address lies outside the direct map.
pub fn direct_mapping_virt_to_phys(addr: VirtAddr) -> Option<PhysAddr> {
    let addr_space = VIRTUAL_ADDRESS_SPACE.get()?;
    if !addr_space.layout.direct_mapping_range.contains(addr) {
        return None;
    }

    Some(PhysAddr::from_usize(
        addr.as_usize() - addr_space.layout.direct_mapping_range.start.as_usize(),
    ))
}

/// Translates a physical address into an active direct-map virtual address.
///
/// Returns [`None`] before the final roots are active or when the offset exceeds the reservation.
pub fn direct_mapping_phys_to_virt(addr: PhysAddr) -> Option<VirtAddr> {
    if !PAGE_TABLE_ACTIVE.load(Ordering::Acquire) {
        return None;
    }

    let addr_space = VIRTUAL_ADDRESS_SPACE.get()?;
    let offset = addr.as_usize();
    let start = addr_space.layout.direct_mapping_range.start.as_usize();
    let virt = VirtAddr::from_usize(start.checked_add(offset)?);
    addr_space
        .layout
        .direct_mapping_range
        .contains(virt)
        .then_some(virt)
}

/// Runs a closure with exclusive access to the kernel page-table owner.
///
/// Operations use the allocator captured by the owner's descriptor. The page-table slot must
/// already be initialized and cannot be observed while allocator handoff holds the same lock.
#[inline]
pub fn with_page_table<F, T>(f: F) -> T
where
    F: FnOnce(&mut OpaquePageTable<VirtAddr>) -> T,
{
    let mut pt = VIRTUAL_ADDRESS_SPACE.page_table_mutex.lock();
    f(&mut pt)
}

/// Marks the final page-table roots as active.
///
/// Called after installing these roots so physical-address translation can use the direct map.
pub(crate) fn mark_page_table_active() {
    PAGE_TABLE_ACTIVE.store(true, Ordering::Release);
}

/// Returns the active page-table root.
///
/// The borrowed identifier does not transfer ownership of the installed table pages.
#[inline]
pub fn page_table_root() -> OpaquePageTableRoot {
    with_page_table(|pt| pt.root())
}

/// Initializes the virtual address space.
///
/// Detects the translation mode and calculates the kernel layout without allocating table pages.
/// The initial opaque page table type captures `AL`, which early boot supplies as its page allocator.
pub(super) fn init_vmm_layout<AL: PageAllocator>() {
    kprintln!("Virtual address space:");

    let mode = select_va_mode(exarch::mem::virt_addr_space_modes());
    exarch::mem::set_virt_addr_space_mode(mode);
    let layout = calculate_vmm_layout(mode);

    VIRTUAL_ADDRESS_SPACE.init_once(VirtualAddressSpace {
        layout,
        page_table_type_mutex: SpinNoIrq::new(exarch::mem::get_page_table_type::<AL>(mode)),
        page_table_mutex: SpinNoIrq::new(OpaquePageTable::dummy()),
    });
}

/// Selects the virtual address space mode to use.
///
/// Currently, we require the virtual address space mode to cover both halves
/// symmetrically, and we prefer those with the largest upper virtual address
/// bits, and among which we prefer those with the largest page size.
fn select_va_mode(va_modes: VirtAddrSpaceModes) -> VirtAddrSpaceMode {
    kprintln!("  Platform virtual address space modes:");
    for mode in &va_modes.modes {
        kprintln!("    {}", *mode);
    }

    kprintln!(
        "  Current virtual address space mode:\n    {}",
        va_modes.modes[va_modes.current_index]
    );

    // Prefer an address-space mode that covers both halves. AArch64 exposes
    // these as independent TTBR0 and TTBR1 regimes, while x86-64 and RISC-V
    // expose a unified root.
    let mut chosen_index = None::<usize>;
    for (index, mode) in va_modes.modes.iter().enumerate() {
        if matches!(
            mode,
            VirtAddrSpaceMode::Unified(..) | VirtAddrSpaceMode::Independent { .. }
        ) && mode.upper_va_bits() == mode.lower_va_bits()
            && mode.upper_page_shift() == mode.lower_page_shift()
            && chosen_index
                .map(|index| {
                    let old_mode = va_modes.modes[index];

                    old_mode.upper_va_bits() < mode.upper_va_bits()
                        || (old_mode.upper_va_bits() == mode.upper_va_bits()
                            && old_mode.upper_page_shift() < mode.upper_page_shift())
                })
                .unwrap_or(true)
        {
            chosen_index = Some(index);
        }
    }

    let chosen_index = chosen_index.expect("No good virtual address space mode found");
    let chosen_mode = va_modes.modes[chosen_index];

    kprintln!(
        "  Selected virtual address space mode:\n    {}",
        chosen_mode
    );

    chosen_mode
}

/// Calculates the layout of the virtual address space based on the given mode.
///
/// Reserves the first half of the upper address range for the direct map and the next quarter for
/// vmalloc, leaving the final quarter unused.
fn calculate_vmm_layout(mode: VirtAddrSpaceMode) -> VirtualAddressSpaceLayout {
    let upper_bits = mode.upper_va_bits().unwrap().get();
    let page_shift = mode.upper_page_shift().unwrap().get();
    let upper_start = va!((1usize << upper_bits).wrapping_neg());

    kprintln!("  Upper half start    : {:#x}", upper_start);

    // Top level memory areas:
    // - 1st half: direct mapping area
    // - 2nd half:
    //   - 3rd quarter: vmalloc area
    //   - 4th quarter: not used
    // This scheme gives us at least the same size of these areas as Linux does.
    let quarter_size = 1usize << (upper_bits - 2);
    let direct_mapping_range = VirtAddrRange::from_start_size(upper_start, quarter_size * 2);
    let vmalloc_range =
        VirtAddrRange::from_start_size(upper_start + quarter_size * 2, quarter_size);
    kprintln!(
        "  Direct mapping area : {:x}, {}",
        direct_mapping_range,
        direct_mapping_range.size().size_display_wide()
    );
    kprintln!(
        "  Vmalloc area        : {:x}, {}",
        vmalloc_range,
        vmalloc_range.size().size_display_wide()
    );

    VirtualAddressSpaceLayout {
        mode,
        page_shift,
        direct_mapping_range,
        vmalloc_range,
    }
}

/// Initializes the page table with identity mappings and the physical-memory direct map.
pub(super) fn init_vmm_mapping_early(phys_mem_regions: &MemoryRegions) {
    let virt_phys_offset = direct_mapping_offset();
    let mut pt = VIRTUAL_ADDRESS_SPACE
        .page_table_type_mutex
        .lock()
        .new_pagetable_alloc()
        .unwrap();

    // TODO: the physical memory regions are not guaranteed to be page-aligned,
    // that may cause issues when mapping them. Luckily, it's not a serious or
    // urgent issue now, because free memory regions and kernel sections are
    // almost guaranteed to be page-aligned. We should fix this in the future.
    for region in phys_mem_regions {
        let mapping_flags = region_flags_to_mapping(region.flags);
        if mapping_flags.is_empty() {
            continue;
        }

        let paddr = region.range.start;
        let vaddr_low = VirtAddr::from_usize(paddr.as_usize());
        let vaddr_high = vaddr_low + virt_phys_offset;
        let size = region.range.size();

        // The high direct map is needed for all physical memory. The low
        // identity map only needs to cover code/data used before the jump and
        // pages allocated for the bootstrap page tables.
        let early_range = early::phys_addr_range();
        let needs_full_identity = region.flags.intersects(
            MemoryRegionFlags::KERNEL
                | MemoryRegionFlags::BOOT_SERVICE
                | MemoryRegionFlags::EXECUTE,
        );
        if needs_full_identity {
            pt.map(vaddr_low, paddr, size, mapping_flags).unwrap();
        } else if region.range.start < early_range.end && early_range.start < region.range.end {
            let identity_start = region.range.start.max(early_range.start);
            let identity_end = region.range.end.min(early_range.end);
            let identity_size = identity_end.as_usize() - identity_start.as_usize();
            pt.map(
                VirtAddr::from_usize(identity_start.as_usize()),
                identity_start,
                identity_size,
                mapping_flags,
            )
            .unwrap();
        }
        pt.map(vaddr_high, paddr, size, mapping_flags).unwrap();
    }

    *VIRTUAL_ADDRESS_SPACE.page_table_mutex.lock() = pt;
}

/// Transfers the active page-table owner to the formal allocator.
///
/// Refreshes relocated function pointers and transfers the early owner's roots to `AL` without
/// freeing them or calling its old method pointers. Before this handoff, the formal allocator
/// must have registered all early page-table allocations as allocated pages so it can release them.
pub(super) fn use_formal_page_table_allocator() {
    // Fix function addresses in the opaque page table type first.
    let page_table_type = exarch::mem::get_page_table_type::<FormalPageAllocator>(VIRTUAL_ADDRESS_SPACE.layout.mode);
    *VIRTUAL_ADDRESS_SPACE.page_table_type_mutex.lock() = page_table_type.clone();

    let mut page_table_guard = VIRTUAL_ADDRESS_SPACE.page_table_mutex.lock();

    let root = mem::replace(page_table_guard.deref_mut(), OpaquePageTable::dummy()).leak();

    // SAFETY: leak transfers the existing roots and all their table pages out of the only owner.
    // The refreshed descriptor has the same geometry, and AL can access and release those pages.
    *page_table_guard = unsafe { page_table_type.new_pagetable_at(root) };
}

/// Removes temporary low-half identity mappings from the active page table.
///
/// Uses the formal allocator already captured by the active owner. The low mappings remain
/// available until secondary processors have finished their early startup.
pub(super) fn remove_identical_mapping(phys_mem_regions: &MemoryRegions) {
    info!("Removing identical mappings...");

    with_page_table(|pt| {
        for region in phys_mem_regions {
            let paddr = region.range.start;
            let vaddr_low = VirtAddr::from_usize(paddr.as_usize());
            let size = region.range.size();

            pt.unmap(vaddr_low, size).unwrap();
        }
    });
}

/// The kernel's formal page allocator for page-table storage.
///
/// Uses the initialized buddy allocator and the high physical-memory direct map. The buddy
/// allocator adopts early allocations before this type becomes the active descriptor's allocator.
pub struct FormalPageAllocator;

impl PageAllocator for FormalPageAllocator {
    /// Returns the logarithm of the kernel base-page size in bytes.
    ///
    /// Page-table allocations use the same granularity as the physical allocator.
    fn page_size_shift() -> usize {
        page_size_shift()
    }

    /// Allocates one physical frame for page-table storage.
    ///
    /// Returns [`None`] when the buddy allocator cannot satisfy the request.
    fn alloc_frame() -> Option<PhysAddr> {
        allocs::palloc::alloc_frame().ok()
    }

    /// Allocates contiguous physical frames for page-table storage.
    ///
    /// The allocation is aligned to at least one kernel base page.
    fn alloc_frames(page_count: usize) -> Option<PhysAddr> {
        allocs::palloc::alloc_frames(page_count, 1 << page_size_shift()).ok()
    }

    /// Releases one physical page-table frame to the buddy allocator.
    ///
    /// The frame must belong to this allocator, including registered early allocations.
    fn dealloc_frame(addr: PhysAddr) {
        allocs::palloc::dealloc_frame(addr).unwrap();
    }

    /// Releases contiguous physical page-table frames to the buddy allocator.
    ///
    /// The address and count must match an allocation adopted or created by this allocator.
    fn dealloc_frames(addr: PhysAddr, page_count: usize) {
        allocs::palloc::dealloc_frames(addr, page_count).unwrap();
    }

    /// Translates a physical page-table address through the kernel direct map.
    ///
    /// The final page-table roots must be active before this allocator accesses table storage.
    fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
        VirtAddr::from_usize(addr.as_usize() + direct_mapping_offset())
    }
}
