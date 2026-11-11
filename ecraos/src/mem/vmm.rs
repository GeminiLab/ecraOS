//! Virtual memory management.
//!
//! This module manages the kernel's [virtual address space layout](#virtual-address-space-layout)
//! and [page table](#page-table).
//!
//! # Virtual address space layout
//!
//! ecraOS currently uses a single shared virtual address space across all CPUs and tasks. Only the
//! upper half of the virtual address space is used (after the
//! [temporary identity mappings](#identity-mapping) are removed). It is divided into three areas:
//!
//! | Area | Portion of the upper half | Purpose |
//! | --- | --- | --- |
//! | [Direct mapping](#direct-mapping-area) | Lower half | Physical memory access. |
//! | [Vmalloc](#vmalloc-area) | Next quarter | [`vmalloc`](super::allocs::vmalloc) allocations. |
//! | Reserved | Final quarter | Not used. |
//!
//! The kernel
//! [selects the virtual address space mode](#selection-of-the-virtual-address-space-mode)
//! and calculates the layout during early boot.
//!
//! ## Direct mapping area
//!
//! The direct mapping area provides access to physical memory at a fixed virtual address offset,
//! returned by [`direct_mapping_offset`]. Adding this offset to a physical address gives its
//! direct mapping address, provided the physical address fits in the area.
//!
//! Use [`direct_mapping_phys_to_virt`] and [`direct_mapping_virt_to_phys`] to convert addresses
//! within the direct mapping area once [`is_page_table_active`] returns `true`. These functions
//! check address bounds, but do not look up page-table entries. A successful conversion does not
//! guarantee that the resulting address is mapped.
//!
//! ### Physical region mappings
//!
//! [`super::pmm`] maintains physical memory regions with byte-precise boundaries. Direct mappings
//! round these regions outwards to base-page boundaries (the original physical memory regions table
//! from [`super::pmm`] is not modified). Regions that share a page must satisfy the following
//! rules:
//!
//! - Regions without any `READ`, `WRITE`, or `EXECUTE` bits are ignored unless marked `NO_MAP`.
//! - A `NO_MAP` region excludes its entire rounded pages from mapping. A region marked `KERNEL`,
//!   `BOOT_SERVICE`, `DEVICE`, or `EXECUTE` must not share a page with a `NO_MAP` region.
//! - Other mapped regions sharing a page must agree on their `READ`, `WRITE`, `EXECUTE`, `DEVICE`,
//!   and `UNCACHED` bits. Compatible regions share one mapping for each overlapping page.
//!
//! Use [`add_and_map_phys_regions`] to add or overwrite physical regions and update their mappings.
//! It also updates identity mappings while they remain active. Adding regions is expensive, so
//! prefer adding them in batches.
//!
//! ## Vmalloc area
//!
//! The vmalloc area is used for kernel allocations whose virtual addresses are chosen
//! independently of their physical backing. Neighboring virtual pages can map non-contiguous
//! physical pages. Use [`vmalloc`](super::allocs::vmalloc) to allocate and map memory in this area.
//!
//! [`vmalloc_base`] and [`vmalloc_area`] describe the area reserved for these allocations.
//! Querying this reservation does not allocate memory or guarantee that an address is mapped.
//!
//! # Page table
//!
//! The kernel page table defines the mappings and access permissions for the shared address
//! space. Its format depends on the selected [`VirtAddrSpaceMode`]. Depending on the architecture,
//! it may have a single root or separate roots for the lower and upper halves. All CPUs use the
//! same kernel page table.
//!
//! ## Mapping transactions
//!
//! [Mapping transactions](MappingTransaction) provide exclusive access to the VMM state, allowing
//! page-table changes, TLB invalidation, and mapping record updates to be performed safely and
//! serially.
//!
//! Transactions do not roll back page-table changes if an operation fails. See the documentation
//! for each method of [`MappingTransaction`] for details.
//!
//! See [`add_and_map_phys_regions`] and the [`vmalloc`](super::allocs::vmalloc) interfaces for
//! their calling requirements and failure behavior.
//!
//! # Identity mapping
//!
//! An identity mapping maps a virtual address to the physical address with the same numerical
//! value. It allows startup code to execute before switching to the kernel's upper-half addresses.
//! Temporary identity mappings are needed by the bootstrap processor (BSP) during early boot and
//! by application processors (APs) during secondary CPU startup. Identity mappings are removed
//! after APs have finished early startup and no longer need them.
//!
//! # Lifecycle of the virtual address space and page table
//!
//! ## When the kernel starts
//!
//! The bootloader must provide a valid identity mapping for kernel entry, and that mapping must
//! cover all physical addresses that the kernel will need to access before the early page table
//! can be installed. The kernel initially executes through this mapping. Its own virtual address
//! layout and page table have not yet been initialized. Both [`is_layout_initialized`] and
//! [`is_page_table_active`] return `false`.
//!
//! ## Selection of the virtual address space mode
//!
//! The platform reports supported virtual address space modes as [`VirtAddrSpaceMode`] values.
//! A mode describes which halves are available, their address widths and page sizes, and whether
//! they use separate page-table roots. The kernel currently selects its preferred mode as follows:
//!
//! 1. Remove all modes that do not cover both halves of the virtual address space.
//! 2. Remove modes whose two halves have different address widths or base-page sizes.
//! 3. Choose the mode with the largest address width per half.
//! 4. If multiple modes remain, choose the one with the largest base-page size.
//!
//! The platform then activates the selected mode and must ensure that a valid identity mapping
//! remains available. It may preserve the existing identity mapping or install temporary page
//! tables. The kernel then calculates its layout. The selected mode and layout remain unchanged
//! throughout the rest of the kernel's lifetime.
//!
//! From this point, [`is_layout_initialized`] returns `true` and layout queries are available.
//! [`is_page_table_active`] remains `false`, and both direct-map address conversion functions
//! return `None` until the kernel page table is installed.
//!
//! ## Early page table
//!
//! The kernel initializes its early page allocator and uses it to construct the early page table.
//! Physical regions are mapped into the direct map according to the
//! [physical-region mapping rules](#physical-region-mappings), with low-address identity mappings.
//! The kernel also allocates and maps a BSP stack in the vmalloc area.
//!
//! Constructing this table does not activate it. Once the mappings are ready, the BSP installs
//! the table, after which [`is_page_table_active`] returns `true` and direct-map address conversion
//! is available in both directions. Execution then moves to the kernel's direct-map code address
//! and the new stack. The identity mappings remain available.
//!
//! ## Formal page table
//!
//! After entering the upper half, the kernel initializes the buddy page allocator and the heap.
//! The buddy allocator takes over allocation of frames already in use, including early page-table
//! frames.
//!
//! This begins the formal page-table phase. The existing page-table tree and its mappings are
//! preserved. Runtime mapping transactions are now available, including updates through
//! [`add_and_map_phys_regions`]. The vmalloc allocator is then initialized over its reserved area
//! and records the existing BSP stack mapping. Identity mappings remain for AP startup.
//!
//! ## Removal of the identity mapping
//!
//! Once APs have finished early startup and no longer need identity mappings, the kernel removes
//! those mappings and completes the required local and remote TLB invalidation. Direct mappings and
//! vmalloc mappings remain in place. Later physical-region updates maintain direct mappings
//! without recreating identity mappings.

use core::sync::atomic::{AtomicBool, Ordering};

use exarch::mem::{LocalTlbFlusher, VirtAddrSpaceMode, VirtAddrSpaceModes};
use expalloc_trait::PageAllocator;
use expt::{
    opaque::OpaquePageTableRoot,
    pt::cleanup::{PendingCleanup, TlbFlushes},
    pte::MappingFlags,
};
use exspin::RwLockNoIrq;
use lazyinit::LazyInit;
use memory_addr::{AddrRangeBounds, PhysAddr, VirtAddr, VirtAddrRange, va};
use size_disp::SizeDisplay;

use crate::{kprintln, mem::allocs};

mod direct_mapping;
mod state;
mod transaction;

#[cfg(test)]
mod tests;

pub use direct_mapping::add_and_map_phys_regions;
pub(super) use direct_mapping::{init_vmm_mapping_early, remove_identity_mapping};
pub use state::{EarlyPhase, FormalPhase, StableFormalPhase, VmmState};
pub use transaction::MappingTransaction;

/// The layout of the virtual address space.
struct VirtualAddressSpaceLayout {
    /// The selected architecture translation mode.
    mode: VirtAddrSpaceMode,
    /// The logarithm of the base-page size in bytes.
    page_shift: u8,
    /// The virtual address range reserved for the physical-memory direct map.
    direct_mapping_area: VirtAddrRange,
    /// The virtual address range reserved for independent kernel allocations.
    vmalloc_area: VirtAddrRange,
}

/// The virtual address space.
struct VirtualAddressSpace {
    /// The selected kernel virtual-address layout.
    layout: VirtualAddressSpaceLayout,
    /// The VMM lifecycle and current page-table owner.
    state: RwLockNoIrq<VmmState>,
}

/// The kernel virtual address space.
static VIRTUAL_ADDRESS_SPACE: LazyInit<VirtualAddressSpace> = LazyInit::new();

/// Publication flag for kernel page-table activation.
///
/// This flag exists to avoid taking the VMM state lock when checking if the page table is active.
/// It is basically equivalent to the following function:
///
/// ```rust,ignore
/// fn is_page_table_active() -> bool {
///     let Some(addr_space) = VIRTUAL_ADDRESS_SPACE.get() else { return false };
///     matches!(
///         &*addr_space.state.read(),
///         VmmState::Early { phase: EarlyPhase::Active, .. } | VmmState::Formal { .. }
///     )
/// }
/// ```
///
/// Set after the BSP installs the early page table. [`is_page_table_active`] reads this flag
/// without taking the VMM state lock, including from address conversion paths. It remains set
/// during later lifecycle transitions and does not track individual mappings.
static PAGE_TABLE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Returns whether the kernel virtual-address layout has been initialized.
///
/// A ready layout does not imply that the kernel page table has been created or installed.
#[inline]
pub fn is_layout_initialized() -> bool {
    VIRTUAL_ADDRESS_SPACE.get().is_some()
}

/// Returns whether the BSP has activated the kernel page table.
///
/// Becomes true after the early page table is installed and remains true through the formal
/// allocator handoff and later mapping updates.
#[inline]
pub fn is_page_table_active() -> bool {
    PAGE_TABLE_ACTIVE.load(Ordering::Acquire)
}

/// Returns the selected architecture translation mode.
///
/// # Panics
///
/// Panics if the virtual-address layout has not been initialized.
#[inline]
pub fn mode() -> VirtAddrSpaceMode {
    VIRTUAL_ADDRESS_SPACE.layout.mode
}

/// Returns the base-2 logarithm of the selected base page size in bytes.
///
/// The base page size is `1 << page_size_shift()`. Both halves of the kernel address space use this
/// size.
///
/// # Panics
///
/// Panics if the virtual-address layout has not been initialized.
#[inline]
pub fn page_size_shift() -> usize {
    VIRTUAL_ADDRESS_SPACE.layout.page_shift as usize
}

/// Returns the selected base page size in bytes.
///
/// Both halves of the kernel address space use this size. This is the granularity of physical
/// allocation and virtual mapping operations.
///
/// # Panics
///
/// Panics if the virtual-address layout has not been initialized.
#[inline]
pub fn page_size() -> usize {
    1usize << page_size_shift()
}

/// Returns the number of base pages needed for a byte count.
///
/// Rounds up to include a partial final page. Returns zero for zero bytes.
///
/// # Panics
///
/// Panics if the virtual-address layout has not been initialized.
#[inline]
pub fn page_count_for_bytes(bytes: usize) -> usize {
    bytes.div_ceil(page_size())
}

/// Returns the offset between physical addresses and their direct mapping virtual addresses.
///
/// Adding this offset to a physical address gives its direct mapping address, provided it fits in
/// the reserved direct mapping range. This calculation does not check whether the address is
/// mapped.
///
/// # Panics
///
/// Panics if the virtual-address layout has not been initialized.
#[inline]
pub fn direct_mapping_offset() -> usize {
    VIRTUAL_ADDRESS_SPACE
        .layout
        .direct_mapping_area
        .start
        .as_usize()
}

/// Returns the first virtual address reserved for vmalloc.
///
/// Use [`vmalloc_area`] to obtain both boundaries of the reservation.
///
/// # Panics
///
/// Panics if the virtual-address layout has not been initialized.
#[inline]
pub fn vmalloc_base() -> VirtAddr {
    vmalloc_area().start
}

/// Returns the virtual address range reserved for vmalloc.
///
/// Use [`super::allocs::vmalloc`] to allocate and map memory within this range.
///
/// # Panics
///
/// Panics if the virtual-address layout has not been initialized.
#[inline]
pub fn vmalloc_area() -> VirtAddrRange {
    VIRTUAL_ADDRESS_SPACE.layout.vmalloc_area
}

/// Computes the physical address corresponding to a direct mapping virtual address.
///
/// Available once [`is_page_table_active`] returns `true`. Checks whether the address lies in the
/// configured direct mapping area, but does not check whether the page table maps the address.
///
/// Returns [`None`] before the kernel page table is active or when the address lies outside the
/// direct mapping area.
pub fn direct_mapping_virt_to_phys(addr: VirtAddr) -> Option<PhysAddr> {
    if !is_page_table_active() {
        return None;
    }

    let addr_space = VIRTUAL_ADDRESS_SPACE.get()?;
    if !addr_space.layout.direct_mapping_area.contains(addr) {
        return None;
    }

    Some(PhysAddr::from_usize(
        addr.as_usize() - addr_space.layout.direct_mapping_area.start.as_usize(),
    ))
}

/// Computes the direct mapping virtual address corresponding to a physical address.
///
/// Available once [`is_page_table_active`] returns `true`. Checks that the address fits in the
/// reserved direct mapping area, but does not check whether it is mapped. In particular, `NO_MAP`
/// regions have no direct mappings.
///
/// Returns [`None`] before the early page table is installed or when the address does not fit.
pub fn direct_mapping_phys_to_virt(addr: PhysAddr) -> Option<VirtAddr> {
    if !is_page_table_active() {
        return None;
    }

    let addr_space = VIRTUAL_ADDRESS_SPACE.get()?;
    let offset = addr.as_usize();
    let start = addr_space.layout.direct_mapping_area.start.as_usize();
    let virt = VirtAddr::from_usize(start.checked_add(offset)?);
    addr_space
        .layout
        .direct_mapping_area
        .contains(virt)
        .then_some(virt)
}

/// Maps a range needed during early boot.
///
/// The early page table must be created but not yet installed. This is used to map the BSP stack.
///
/// # Panics
///
/// Panics unless the early table is ready but inactive, or if mapping fails.
pub(super) fn map_early_range(
    virtual_range: VirtAddrRange,
    physical_start: PhysAddr,
    flags: MappingFlags,
) {
    let mut state = VIRTUAL_ADDRESS_SPACE.state.write();
    let VmmState::Early {
        page_table,
        phase: EarlyPhase::TableReady,
    } = &mut *state
    else {
        panic!("early mapping requires an inactive early table");
    };
    page_table
        .map(
            virtual_range.start,
            physical_start,
            virtual_range.size(),
            flags,
        )
        .expect("failed to map early boot range");
}

/// Performs local TLB invalidations and appends them to a list of pending remote invalidations.
///
/// It's the caller's responsibility to complete the remote invalidations before publishing the new
/// mapping or reusing affected frames.
fn finish_local_cleanup(
    cleanup: PendingCleanup<VirtAddr>,
    remote_flushes: &mut TlbFlushes<VirtAddr>,
) {
    // SAFETY: Preserve every request for remote invalidation and process it locally through the
    // captured strategy. The transaction retains responsibility for remote completion.
    let (mut flushes, flusher) = unsafe { cleanup.into_inner() };
    remote_flushes.append_ref(&flushes);
    flushes.flush_with(&flusher);
}

/// Installs the early page table.
///
/// Call on the BSP with interrupts disabled, before starting APs. This function locks the VMM state
/// temporarily.
///
/// Direct mapping conversions are enabled after this function returns.
///
/// # Panics
///
/// Panics unless an inactive early table has been constructed.
pub(super) fn activate_early_page_table() {
    let root = {
        let mut state = VIRTUAL_ADDRESS_SPACE.state.write();
        let VmmState::Early { page_table, phase } = &mut *state else {
            panic!("activation requires an early page table");
        };
        assert!(phase.activate(), "early page table already active");
        page_table.root()
    };
    // On x86, the low per-CPU area becomes read-only after this switch. The state lock must
    // already be released so dropping its execution guard does not write to that area.
    exarch::mem::set_page_table_root(root);
    PAGE_TABLE_ACTIVE.store(true, Ordering::Release);
}

/// Returns the kernel page-table root addresses.
///
/// Does not transfer ownership of page-table frames. Secondary CPU startup uses these addresses
/// to install the same page table.
///
/// # Panics
///
/// Panics before the initial table has been constructed.
#[inline]
pub(crate) fn page_table_root() -> OpaquePageTableRoot {
    let state = VIRTUAL_ADDRESS_SPACE.state.read();
    match &*state {
        VmmState::Early { page_table, .. } | VmmState::Formal { page_table, .. } => {
            page_table.root()
        }
        VmmState::LayoutReady { .. } => panic!("page table has not been created"),
    }
}

/// Selects the kernel address space mode and calculates the layout.
///
/// `AL` is the early allocator used when the first page table is created. This function does not
/// allocate, so `AL` can be initialized after this function and before the first page table is
/// created.
///
/// # Panics
///
/// Panics if the layout is already initialized or no supported mode provides matching address
/// widths and page sizes for both halves.
pub(super) fn init_vmm_mode_and_layout<AL: PageAllocator>() {
    kprintln!("Virtual address space:");

    let mode = select_va_mode(exarch::mem::virt_addr_space_modes());
    exarch::mem::set_virt_addr_space_mode(mode);
    let layout = calculate_vmm_layout(mode);

    VIRTUAL_ADDRESS_SPACE.init_once(VirtualAddressSpace {
        layout,
        state: RwLockNoIrq::new(VmmState::LayoutReady {
            page_table_type: exarch::mem::get_page_table_type::<AL, LocalTlbFlusher>(mode),
        }),
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
    let direct_mapping_area = VirtAddrRange::from_start_size(upper_start, quarter_size * 2);
    let vmalloc_area = VirtAddrRange::from_start_size(upper_start + quarter_size * 2, quarter_size);
    kprintln!(
        "  Direct mapping area : {:x}, {}",
        direct_mapping_area,
        direct_mapping_area.size().size_display_wide()
    );
    kprintln!(
        "  Vmalloc area        : {:x}, {}",
        vmalloc_area,
        vmalloc_area.size().size_display_wide()
    );

    VirtualAddressSpaceLayout {
        mode,
        page_shift,
        direct_mapping_area,
        vmalloc_area,
    }
}

/// Switches page-table frame allocation and deallocation to the buddy allocator.
///
/// Keeps the existing page-table tree and rebuilds its direct mapping records from the physical
/// memory table. Call after the heap and final PMM storage are ready, but before MMIO discovery
/// adds regions. The buddy allocator must already own all frames used by the early page table.
///
/// # Panics
///
/// Panics if the early page table is not active, PMM is not using final storage, or the mapping
/// records cannot be built.
pub(super) fn use_formal_page_table_allocator() {
    let regions = super::pmm::clone_final_regions();
    let mappings = direct_mapping::plan_formal_mappings(regions.as_slice(), page_size(), true)
        .expect("failed to reconstruct formal mapping records");
    let page_table_type = exarch::mem::get_page_table_type::<FormalPageAllocator, LocalTlbFlusher>(
        VIRTUAL_ADDRESS_SPACE.layout.mode,
    );
    // SAFETY: The unchanged mode selects the same page-table format. The buddy allocator has
    // registered all early table frames, and the active direct map provides access to them.
    unsafe {
        VIRTUAL_ADDRESS_SPACE
            .state
            .write()
            .switch_to_formal_page_table(page_table_type, mappings);
    }
}

/// The buddy-backed allocator for kernel page-table frames.
struct FormalPageAllocator;

/// Page-table frame allocation, deallocation, and direct mapping access.
impl PageAllocator for FormalPageAllocator {
    /// Returns the logarithm of the kernel base page size in bytes.
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
    /// The early page table must already provide direct-map access to the frame.
    fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
        VirtAddr::from_usize(addr.as_usize() + direct_mapping_offset())
    }
}
