//! Vmalloc reservations and acknowledged mapping transactions.
//!
//! Metadata preparation, PTE mutation, unlocked remote completion and resource release are distinct
//! phases. Failed completion retains both physical backing and the complete virtual reservation.

use alloc::{boxed::Box, vec, vec::Vec};
use core::range::Range;

use ecraldr_base::PhysAddrRange;
use expt::{opaque::PageTableAction, pte::MappingFlags};
use kspin::SpinNoIrq;
use lazyinit::LazyInit;
use log::{error, info};
use memory_addr::{AddrRangeBounds, MemoryAddr, PhysAddr, VirtAddr, VirtAddrRange, pa};
use memory_range_set::{RangeSet, TryInsertError};

use crate::{
    mem::{
        allocs::palloc,
        vmm::{self, MappingTransaction},
    },
    mp,
    tlb::{TlbInvalidation, TlbShootdownError},
};

/// Per-page mapping and completion state.
///
/// Pending and quarantined entries retain backing but never represent a live translation.
#[derive(Clone, Copy, Debug, PartialEq)]
enum VMAllocPageStatus {
    /// An available page slot.
    ///
    /// No backing frame is owned by this slot.
    NotMapped,
    /// A live mapping backed by an owned frame.
    ///
    /// The frame is released only after exact remote completion.
    Owned {
        /// The live PTE permissions.
        ///
        /// These are retained in a prepared token for a reversible pre-mutation failure.
        flags: MappingFlags,
    },
    /// A live mapping backed by external storage.
    ///
    /// Vmalloc never returns this frame to the physical allocator.
    External {
        /// The live PTE permissions.
        ///
        /// External ownership is independent of the mapping lifetime.
        flags: MappingFlags,
    },
    /// A reserved slot awaiting mapping or unmapping completion.
    ///
    /// The transaction's token owns the original status until finalization.
    Pending,
    /// A slot whose CPU views may differ after a failure.
    ///
    /// Backing storage and the complete reservation remain occupied permanently.
    Quarantined,
}

impl VMAllocPageStatus {
    /// Reports whether metadata describes a live mapping.
    ///
    /// Pending and quarantined states cannot be translated through virt_to_phys.
    const fn is_mapped(self) -> bool {
        matches!(self, Self::Owned { .. } | Self::External { .. })
    }

    /// Reports whether the recorded backing is owned.
    ///
    /// Release tokens preserve this before changing the page state.
    const fn is_owned(self) -> bool {
        matches!(self, Self::Owned { .. })
    }
}

/// Indexed physical backing and mapping states.
///
/// Indices are relative to the non-guard virtual range.
struct VMAllocPageList {
    /// The retained physical frame addresses.
    ///
    /// Pending and quarantine transitions preserve this array.
    phys_pages: Box<[PhysAddr]>,
    /// The live or reserved state of each page.
    ///
    /// Only NotMapped slots accept new mappings.
    statuses: Box<[VMAllocPageStatus]>,
}

impl VMAllocPageList {
    /// Creates an empty page list.
    ///
    /// Physical zero is a placeholder for slots with no backing.
    fn new(count: usize) -> Self {
        Self {
            phys_pages: vec![pa!(0); count].into_boxed_slice(),
            statuses: vec![VMAllocPageStatus::NotMapped; count].into_boxed_slice(),
        }
    }

    /// Returns a page's retained physical address.
    ///
    /// Callers must separately check whether its status is live.
    fn phys_addr(&self, index: usize) -> PhysAddr {
        self.phys_pages[index]
    }

    /// Records backing and its ownership state.
    ///
    /// This metadata operation performs no PTE mutation or remote wait.
    fn mark_mapped(&mut self, index: usize, address: PhysAddr, status: VMAllocPageStatus) {
        self.phys_pages[index] = address;
        self.statuses[index] = status;
    }

    /// Clears a slot after acknowledged completion.
    ///
    /// The token retains the frame address until physical release outside the metadata lock.
    fn mark_unmapped(&mut self, index: usize) {
        self.phys_pages[index] = pa!(0);
        self.statuses[index] = VMAllocPageStatus::NotMapped;
    }
}

/// The non-guard mapping records for one complete reservation.
///
/// The RangeSet key retains both guards throughout pending completion and quarantine.
struct VMAllocRange {
    /// The usable virtual range.
    ///
    /// Guard pages lie outside this range but inside the RangeSet reservation.
    non_guard_range: VirtAddrRange,
    /// The usable pages' backing records.
    ///
    /// A token carries ownership across the unlocked completion phase.
    pages: VMAllocPageList,
    /// Whether any uncertain mutation has quarantined this reservation.
    ///
    /// Quarantine prohibits all future mutation and reuse of the allocation.
    quarantined: bool,
    /// Additional backing retained by a failed diagnostic replacement.
    ///
    /// Both old and newly allocated frames remain occupied if replacement completion fails.
    #[cfg(feature = "foundation-test")]
    retained_backing: Vec<PhysAddr>,
}

impl VMAllocRange {
    /// Creates an unmapped allocation record.
    ///
    /// All page slots begin available for a serialized mapping transaction.
    fn new(range: VirtAddrRange, count: usize) -> Self {
        Self {
            non_guard_range: range,
            pages: VMAllocPageList::new(count),
            quarantined: false,
            #[cfg(feature = "foundation-test")]
            retained_backing: Vec::new(),
        }
    }

    /// Returns validated page indices for a subrange.
    ///
    /// Quarantine rejects even otherwise valid subrange operations.
    fn page_index_range(
        &self,
        range: VirtAddrRange,
        size: usize,
    ) -> Result<Range<usize>, VMAllocError> {
        VMAllocator::validate_range(range, size)?;
        if self.quarantined {
            return Err(VMAllocError::Quarantined);
        }
        if !self.non_guard_range.contains_range(range) {
            return Err(VMAllocError::OutOfOuterRange);
        }
        let start = range.start.sub_addr(self.non_guard_range.start) / size;
        Ok(Range {
            start,
            end: start + range.size() / size,
        })
    }

    /// Translates only live mapping metadata.
    ///
    /// A cleared or uncertain PTE is never reported as a usable mapping.
    fn virt_to_phys(&self, address: VirtAddr, size: usize) -> Option<PhysAddr> {
        if self.quarantined || !self.non_guard_range.contains(address) {
            return None;
        }
        let offset = address.sub_addr(self.non_guard_range.start);
        self.pages.statuses[offset / size]
            .is_mapped()
            .then(|| self.pages.phys_addr(offset / size) + offset % size)
    }
}

/// One retained frame and its original mapping ownership.
///
/// The semantic addresses remain available through completion or quarantine.
struct ReleasedPage {
    /// The virtual page being changed.
    ///
    /// This is always aligned to the allocator's runtime granule.
    virtual_address: VirtAddr,
    /// The retained physical backing.
    ///
    /// It is not released until every remote acknowledgement has arrived.
    physical_address: PhysAddr,
    /// Whether physical release belongs to vmalloc.
    ///
    /// External pages are never deallocated by finalization.
    owned: bool,
    /// The original live or unmapped state.
    ///
    /// Restoration is legal only before any PTE mutation was attempted.
    original: VMAllocPageStatus,
}

/// An owned operation spanning preparation and unlocked completion.
///
/// Dropping the token itself never frees backing or virtual reservations.
struct PreparedUnmap {
    /// The full selected virtual subrange.
    ///
    /// Invalidations cover this range even when some slots were already unmapped.
    range: VirtAddrRange,
    /// Physical backing retained across PTE mutation and remote completion.
    ///
    /// Allocation is completed before metadata becomes pending.
    pages: Vec<ReleasedPage>,
    /// Whether success also removes the complete guarded reservation.
    ///
    /// Failure always retains that reservation.
    release_reservation: bool,
}

/// A reservation, mutation or completion failure.
///
/// Completion failures preserve common IPI diagnostics and require resource quarantine.
#[derive(Debug, thiserror::Error)]
pub enum VMAllocError {
    /// An overlapping reservation.
    ///
    /// No existing allocation is changed.
    #[error("overlapping reservation")]
    Overlap,
    /// A reservation outside allocator limits.
    ///
    /// RangeSet validation rejects it before insertion.
    #[error("outside allocator limits")]
    OutOfRange,
    /// An exhausted virtual area.
    ///
    /// Pending and quarantined reservations remain occupied.
    #[error("no free virtual range")]
    NoFreeRange,
    /// An unaligned page boundary.
    ///
    /// Alignment uses the runtime granule.
    #[error("unaligned range")]
    RangeNotAligned,
    /// An empty or reversed range.
    ///
    /// Such a range cannot produce a mutation token.
    #[error("empty or reversed range")]
    EmptyRange,
    /// A subrange outside its containing reservation.
    ///
    /// Guard pages cannot be mapped through this API.
    #[error("outside usable reservation")]
    OutOfOuterRange,
    /// A live or pending page in a mapping request.
    ///
    /// Existing ownership is unchanged.
    #[error("page already mapped or pending")]
    AlreadyMapped,
    /// A physical or token allocation failure.
    ///
    /// No PTE mutation has begun.
    #[error("allocation failed")]
    AllocationError,
    /// A possibly partial PTE mutation.
    ///
    /// The entire operation is quarantined conservatively.
    #[error("page-table mutation failed")]
    PageTableError,
    /// An inconsistent virtual and physical span.
    ///
    /// Validation precedes metadata preparation.
    #[error("mapping sizes differ")]
    RangeSizeMismatch,
    /// An unknown allocation start.
    ///
    /// Complete deallocation requires the usable range's exact start.
    #[error("not an allocated range")]
    NotAllocated,
    /// An unavailable global allocator.
    ///
    /// Initialization must precede runtime operations.
    #[error("vmalloc is not initialized")]
    NotInitialized,
    /// A failed physical release after completed invalidation.
    ///
    /// The error is diagnosed as a retained physical resource.
    #[error("physical deallocation failed")]
    DeallocationError,
    /// A permanently retained allocation.
    ///
    /// No operation can reuse or claim its backing or reservation.
    #[error("quarantined reservation")]
    Quarantined,
    /// A rejected or incomplete common shootdown.
    ///
    /// The diagnostic preserves the remote target and exact generation.
    #[error("TLB completion failed: {0}")]
    Completion(CompletionDiagnostic),
}

/// A public completion diagnostic independent of private request APIs.
///
/// It preserves common protocol failures without exposing mutable mailbox state.
#[derive(Debug)]
pub struct CompletionDiagnostic(pub(crate) TlbShootdownError);

impl core::fmt::Display for CompletionDiagnostic {
    /// Formats the exact common failure context.
    ///
    /// Debug representation includes all available generation and target observations.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{:?}", self.0)
    }
}

impl From<TlbShootdownError> for VMAllocError {
    /// Preserves a shootdown rejection or timeout.
    ///
    /// Callers still distinguish pre-mutation rejection from post-mutation quarantine.
    fn from(error: TlbShootdownError) -> Self {
        Self::Completion(CompletionDiagnostic(error))
    }
}

impl<A: MemoryAddr, P> From<TryInsertError<A, P>> for VMAllocError {
    /// Converts RangeSet insertion failures.
    ///
    /// Existing reservations remain untouched.
    fn from(error: TryInsertError<A, P>) -> Self {
        match error {
            TryInsertError::Overlap { .. } => Self::Overlap,
            TryInsertError::OutOfLimit { .. } => Self::OutOfRange,
            TryInsertError::Empty { .. } => Self::EmptyRange,
        }
    }
}

/// Metadata for complete guarded vmalloc reservations.
///
/// This owner does not perform PTE operations, waits or physical release under its lock.
pub struct VMAllocator {
    /// The occupied complete reservations.
    ///
    /// Quarantine preserves the original RangeSet entry.
    ranges: RangeSet<VirtAddr, VMAllocRange>,
    /// The selected runtime page shift.
    ///
    /// Address validation does not assume a 4 KiB granule.
    page_size_shift: usize,
}

impl VMAllocator {
    /// Creates an allocator over a page-aligned virtual area.
    ///
    /// Reservations and mapping metadata begin empty.
    pub fn new(range: VirtAddrRange, shift: usize) -> Result<Self, VMAllocError> {
        Self::validate_range(range, 1 << shift)?;
        Ok(Self {
            ranges: RangeSet::new(range),
            page_size_shift: shift,
        })
    }

    /// Reserves usable pages and guards without installing PTEs.
    ///
    /// The returned span excludes both guard areas.
    pub fn alloc_range(
        &mut self,
        count: usize,
        guards: usize,
    ) -> Result<VirtAddrRange, VMAllocError> {
        if count == 0 {
            return Err(VMAllocError::EmptyRange);
        }
        let size = self.page_size();
        let bytes = guards
            .checked_mul(2)
            .and_then(|g| count.checked_add(g))
            .and_then(|n| n.checked_mul(size))
            .ok_or(VMAllocError::NoFreeRange)?;
        let free = self
            .ranges
            .find_free_range(bytes)
            .ok_or(VMAllocError::NoFreeRange)?;
        let full = VirtAddrRange::from_start_size(free.start, bytes);
        let range = VirtAddrRange::new(full.start + guards * size, full.end - guards * size);
        self.ranges
            .try_insert(full, VMAllocRange::new(range, count))?;
        Ok(range)
    }

    /// Validates that every selected page is available for a new mapping.
    ///
    /// Pending and quarantine states reject competing operations.
    fn ensure_unmapped(&self, range: VirtAddrRange) -> Result<(), VMAllocError> {
        let (_, allocation) = self
            .ranges
            .find(range.start)
            .ok_or(VMAllocError::NotAllocated)?;
        for index in allocation.page_index_range(range, self.page_size())? {
            if allocation.pages.statuses[index] != VMAllocPageStatus::NotMapped {
                return Err(VMAllocError::AlreadyMapped);
            }
        }
        Ok(())
    }

    /// Returns the usable range identified by its exact start.
    ///
    /// Complete deallocation cannot start within a guard or interior page.
    fn allocation_range(&self, address: VirtAddr) -> Result<VirtAddrRange, VMAllocError> {
        let (_, allocation) = self
            .ranges
            .find(address)
            .ok_or(VMAllocError::NotAllocated)?;
        if allocation.non_guard_range.start != address {
            return Err(VMAllocError::NotAllocated);
        }
        Ok(allocation.non_guard_range)
    }

    /// Preallocates a release token and marks live pages pending.
    ///
    /// No PTE or physical allocator operation occurs in this phase.
    fn prepare_unmap(
        &mut self,
        range: VirtAddrRange,
        release: bool,
    ) -> Result<PreparedUnmap, VMAllocError> {
        let size = self.page_size();
        let (_, allocation) = self
            .ranges
            .find_mut(range.start)
            .ok_or(VMAllocError::NotAllocated)?;
        let indices = allocation.page_index_range(range, size)?;
        if release && range != allocation.non_guard_range {
            return Err(VMAllocError::OutOfOuterRange);
        }
        let mut pages = Vec::new();
        pages
            .try_reserve_exact(indices.end - indices.start)
            .map_err(|_| VMAllocError::AllocationError)?;
        for index in indices {
            let original = allocation.pages.statuses[index];
            if matches!(
                original,
                VMAllocPageStatus::Pending | VMAllocPageStatus::Quarantined
            ) {
                return Err(VMAllocError::AlreadyMapped);
            }
            if original.is_mapped() {
                pages.push(ReleasedPage {
                    virtual_address: allocation.non_guard_range.start + index * size,
                    physical_address: allocation.pages.phys_addr(index),
                    owned: original.is_owned(),
                    original,
                });
            }
        }
        for page in &pages {
            let index = page
                .virtual_address
                .sub_addr(allocation.non_guard_range.start)
                / size;
            allocation.pages.statuses[index] = VMAllocPageStatus::Pending;
        }
        Ok(PreparedUnmap {
            range,
            pages,
            release_reservation: release,
        })
    }

    /// Restores pending metadata before any PTE mutation was attempted.
    ///
    /// This function must never be used to recover an uncertain or partial mutation.
    fn restore_unmutated(&mut self, token: &PreparedUnmap) -> Result<(), VMAllocError> {
        self.set_token_states(token, |page| page.original)
    }

    /// Clears completed page metadata and optionally releases the guarded reservation.
    ///
    /// The caller releases owned frames separately after dropping the metadata lock.
    fn finalize_unmap(&mut self, token: &PreparedUnmap) -> Result<(), VMAllocError> {
        let size = self.page_size();
        let (full, allocation) = self
            .ranges
            .find_mut(token.range.start)
            .ok_or(VMAllocError::NotAllocated)?;
        for page in &token.pages {
            let index = page
                .virtual_address
                .sub_addr(allocation.non_guard_range.start)
                / size;
            assert_eq!(allocation.pages.statuses[index], VMAllocPageStatus::Pending);
            allocation.pages.mark_unmapped(index);
        }
        let start = full.start;
        if token.release_reservation {
            self.ranges
                .remove(start)
                .expect("prepared reservation vanished");
        }
        Ok(())
    }

    /// Retains the complete reservation and every uncertain physical backing.
    ///
    /// The old mapping metadata is never restored after mutation or timeout.
    fn quarantine_unmap(&mut self, token: &PreparedUnmap) -> Result<(), VMAllocError> {
        self.set_token_states(token, |_| VMAllocPageStatus::Quarantined)?;
        self.ranges
            .find_mut(token.range.start)
            .ok_or(VMAllocError::NotAllocated)?
            .1
            .quarantined = true;
        Ok(())
    }

    /// Applies a token's metadata transition without touching PTEs.
    ///
    /// Serialization guarantees that the retained reservation cannot disappear.
    fn set_token_states(
        &mut self,
        token: &PreparedUnmap,
        mut state: impl FnMut(&ReleasedPage) -> VMAllocPageStatus,
    ) -> Result<(), VMAllocError> {
        let size = self.page_size();
        let (_, allocation) = self
            .ranges
            .find_mut(token.range.start)
            .ok_or(VMAllocError::NotAllocated)?;
        for page in &token.pages {
            let index = page
                .virtual_address
                .sub_addr(allocation.non_guard_range.start)
                / size;
            assert_eq!(allocation.pages.statuses[index], VMAllocPageStatus::Pending);
            allocation.pages.statuses[index] = state(page);
        }
        Ok(())
    }

    /// Registers a preexisting external contiguous mapping.
    ///
    /// This is used to adopt boot stacks without changing their PTEs.
    pub fn register_external_range(
        &mut self,
        full: VirtAddrRange,
        range: VirtAddrRange,
        physical: PhysAddrRange,
        flags: MappingFlags,
    ) -> Result<(), VMAllocError> {
        Self::validate_phys_range(physical, self.page_size())?;
        if physical.size() != range.size() {
            return Err(VMAllocError::RangeSizeMismatch);
        }
        let pages = physical
            .iter(self.page_size())
            .expect("validated external range")
            .collect::<Vec<_>>()
            .into_boxed_slice();
        self.register_external_pages(full, range, pages, flags)
    }

    /// Registers preexisting external pages and their full reservation.
    ///
    /// Vmalloc owns only the virtual reservation and never the physical pages.
    pub fn register_external_pages(
        &mut self,
        full: VirtAddrRange,
        range: VirtAddrRange,
        pages: Box<[PhysAddr]>,
        flags: MappingFlags,
    ) -> Result<(), VMAllocError> {
        let size = self.page_size();
        Self::validate_range(full, size)?;
        Self::validate_range(range, size)?;
        if !full.contains_range(range) {
            return Err(VMAllocError::OutOfOuterRange);
        }
        if pages.len() != range.size() / size {
            return Err(VMAllocError::RangeSizeMismatch);
        }
        validate_phys_pages(&pages, size)?;
        let mut allocation = VMAllocRange::new(range, pages.len());
        allocation.pages.phys_pages = pages;
        allocation
            .pages
            .statuses
            .fill(VMAllocPageStatus::External { flags });
        self.ranges.try_insert(full, allocation)?;
        Ok(())
    }

    /// Translates only live usable pages.
    ///
    /// Guards, cleared PTEs and quarantined allocations return None.
    pub fn virt_to_phys(&self, address: VirtAddr) -> Option<PhysAddr> {
        self.ranges
            .find(address)?
            .1
            .virt_to_phys(address, self.page_size())
    }

    /// Returns the runtime granule.
    ///
    /// The shift is fixed at allocator initialization.
    fn page_size(&self) -> usize {
        1 << self.page_size_shift
    }

    /// Validates a nonempty aligned virtual span.
    ///
    /// Reversed spans are rejected before subtraction or iteration.
    fn validate_range(range: VirtAddrRange, size: usize) -> Result<(), VMAllocError> {
        if range.start >= range.end {
            return Err(VMAllocError::EmptyRange);
        }
        if !range.start.is_aligned(size) || !range.end.is_aligned(size) {
            return Err(VMAllocError::RangeNotAligned);
        }
        Ok(())
    }

    /// Validates a nonempty aligned physical span.
    ///
    /// External mapping requests use the same runtime granule as owned pages.
    fn validate_phys_range(range: PhysAddrRange, size: usize) -> Result<(), VMAllocError> {
        if range.start >= range.end {
            return Err(VMAllocError::EmptyRange);
        }
        if !range.start.is_aligned(size) || !range.end.is_aligned(size) {
            return Err(VMAllocError::RangeNotAligned);
        }
        Ok(())
    }
}

/// The one global reservation owner.
///
/// Its IRQ-saving lock is held only during metadata phases.
static VMALLOC: LazyInit<SpinNoIrq<VMAllocator>> = LazyInit::new();

/// Initializes the runtime reservation owner.
///
/// The virtual area and granule match the installed kernel page tables.
pub fn init_vmalloc(range: VirtAddrRange, shift: usize) {
    info!("Initializing VMAllocator...");
    VMALLOC.init_once(SpinNoIrq::new(VMAllocator::new(range, shift).unwrap()));
}

/// Runs only a metadata operation with the reservation owner locked.
///
/// PTE mutation, shootdown and physical release must take place outside this closure.
pub fn with_allocator<R>(function: impl FnOnce(&mut VMAllocator) -> R) -> Result<R, VMAllocError> {
    let mut allocator = VMALLOC.get().ok_or(VMAllocError::NotInitialized)?.lock();
    Ok(function(&mut allocator))
}

/// Validates an external physical page list.
///
/// Empty lists and unaligned frames cannot reach a mapping transaction.
fn validate_phys_pages(pages: &[PhysAddr], size: usize) -> Result<(), VMAllocError> {
    if pages.is_empty() {
        return Err(VMAllocError::EmptyRange);
    }
    if pages.iter().any(|page| !page.is_aligned(size)) {
        return Err(VMAllocError::RangeNotAligned);
    }
    Ok(())
}

/// Reserves an unmapped global virtual range.
///
/// The returned usable span excludes the guards.
pub fn alloc_range(count: usize, guards: usize) -> Result<VirtAddrRange, VMAllocError> {
    with_allocator(|allocator| allocator.alloc_range(count, guards))?
}

/// Allocates and maps owned pages into a guarded reservation.
///
/// Failure before any mutation releases the empty reservation. Uncertain mutations quarantine it.
pub fn alloc_range_and_map_alloc(
    count: usize,
    guards: usize,
    flags: MappingFlags,
) -> Result<VirtAddrRange, VMAllocError> {
    let range = alloc_range(count, guards)?;
    if let Err(error) = map_alloc(range, flags) {
        release_empty_reservation(range);
        return Err(error);
    }
    Ok(range)
}

/// Removes an untouched empty reservation after failed preparation.
///
/// Pending, live or quarantined records are always retained.
fn release_empty_reservation(range: VirtAddrRange) {
    let _ = with_allocator(|allocator| {
        if allocator.ensure_unmapped(range).is_ok() {
            let full_start = allocator.ranges.find(range.start).unwrap().0.start;
            allocator.ranges.remove(full_start);
        }
    });
}

/// Installs a prepared list of physical frames without waiting under metadata or PTE locks.
///
/// New boot mappings on a not-ready caller need no remote translation removal. Every runtime
/// publication and all rollback removal use the common completion protocol.
fn map_pages_inner(
    transaction: &MappingTransaction,
    range: VirtAddrRange,
    pages: &[PhysAddr],
    owned: bool,
    flags: MappingFlags,
) -> Result<(), VMAllocError> {
    let size = vmm::page_size();
    VMAllocator::validate_range(range, size)?;
    validate_phys_pages(pages, size)?;
    if range.size() / size != pages.len() {
        return Err(VMAllocError::RangeSizeMismatch);
    }
    let mut token_pages = Vec::new();
    token_pages
        .try_reserve_exact(pages.len())
        .map_err(|_| VMAllocError::AllocationError)?;
    for (index, &physical_address) in pages.iter().enumerate() {
        token_pages.push(ReleasedPage {
            virtual_address: range.start + index * size,
            physical_address,
            owned,
            original: VMAllocPageStatus::NotMapped,
        });
    }
    let token = PreparedUnmap {
        range,
        pages: token_pages,
        release_reservation: false,
    };
    with_allocator(|allocator| {
        allocator.ensure_unmapped(range)?;
        let (_, allocation) = allocator.ranges.find_mut(range.start).unwrap();
        for page in &token.pages {
            let index = page
                .virtual_address
                .sub_addr(allocation.non_guard_range.start)
                / size;
            allocation
                .pages
                .mark_mapped(index, page.physical_address, VMAllocPageStatus::Pending);
        }
        Ok::<_, VMAllocError>(())
    })??;
    // VMALLOC has been released before the short PTE mutation phase.
    let mutation = vmm::with_page_table(|table| {
        table.apply_actions_with_cleanup(token.pages.iter().map(|page| PageTableAction::Map {
            vaddr: page.virtual_address,
            paddr: page.physical_address,
            size,
            flags,
        }))
    });
    let cleanup = match mutation {
        Ok(cleanup) => cleanup,
        Err(error) => {
            error!("vmalloc map partial mutation range={range:#x}: {error:?}");
            quarantine(&token)?;
            return Err(VMAllocError::PageTableError);
        }
    };
    let request = vmm::finish_local_cleanup(cleanup, TlbInvalidation::All);
    // No metadata, page-table, physical allocator, queue or IRQ-saving guard remains here.
    if mp::is_ipi_ready(mp::current_cpu_id()) {
        if let Err(error) = transaction.complete(request) {
            quarantine(&token)?;
            return Err(error.into());
        }
    }
    let status = if owned {
        VMAllocPageStatus::Owned { flags }
    } else {
        VMAllocPageStatus::External { flags }
    };
    with_allocator(|allocator| allocator.set_token_states(&token, |_| status))??;
    Ok(())
}

/// Maps newly allocated owned physical pages into an empty subrange.
///
/// Physical acquisition finishes before PTE mutation. Any uncertain mutation retains all frames.
pub fn map_alloc(range: VirtAddrRange, flags: MappingFlags) -> Result<(), VMAllocError> {
    let transaction = MappingTransaction::begin()?;
    VMAllocator::validate_range(range, vmm::page_size())?;
    with_allocator(|allocator| allocator.ensure_unmapped(range))??;
    let mut pages = Vec::new();
    pages
        .try_reserve_exact(range.size() / vmm::page_size())
        .map_err(|_| VMAllocError::AllocationError)?;
    for _ in 0..range.size() / vmm::page_size() {
        match palloc::alloc_frame() {
            Ok(page) => pages.push(page),
            Err(_) => {
                release_frames(&pages)?;
                return Err(VMAllocError::AllocationError);
            }
        }
    }
    let result = map_pages_inner(&transaction, range, &pages, true, flags);
    if result.is_err() {
        // Before-PTE failures leave the metadata empty. After-PTE failures mark quarantine and
        // retain every frame, including pages whose individual mapping was never attempted.
        if with_allocator(|allocator| allocator.ensure_unmapped(range))?.is_ok() {
            release_frames(&pages)?;
        }
    }
    result
}

/// Maps external physical pages into an empty global subrange.
///
/// Vmalloc retains their addresses on quarantine but never claims physical ownership.
#[expect(unused)]
pub fn map_phys_pages(
    range: VirtAddrRange,
    pages: &[PhysAddr],
    flags: MappingFlags,
) -> Result<(), VMAllocError> {
    let transaction = MappingTransaction::begin()?;
    map_pages_inner(&transaction, range, pages, false, flags)
}

/// Maps an external contiguous physical span into an empty subrange.
///
/// The physical and virtual sizes must match exactly.
#[expect(unused)]
pub fn map_phys_range(
    range: VirtAddrRange,
    physical: PhysAddrRange,
    flags: MappingFlags,
) -> Result<(), VMAllocError> {
    VMAllocator::validate_phys_range(physical, vmm::page_size())?;
    if physical.size() != range.size() {
        return Err(VMAllocError::RangeSizeMismatch);
    }
    let pages = physical.iter(vmm::page_size()).unwrap().collect::<Vec<_>>();
    map_phys_pages(range, &pages, flags)
}

/// Allocates a guarded reservation mapped to external pages.
///
/// Untouched preparation failures release the empty virtual reservation.
#[expect(unused)]
pub fn alloc_range_and_map_phys_pages(
    guards: usize,
    pages: &[PhysAddr],
    flags: MappingFlags,
) -> Result<VirtAddrRange, VMAllocError> {
    validate_phys_pages(pages, vmm::page_size())?;
    let range = alloc_range(pages.len(), guards)?;
    if let Err(error) = map_phys_pages(range, pages, flags) {
        release_empty_reservation(range);
        return Err(error);
    }
    Ok(range)
}

/// Allocates a guarded reservation mapped to an external contiguous span.
///
/// External storage remains owned by its caller throughout the mapping lifetime.
#[expect(unused)]
pub fn alloc_range_and_map_phys_range(
    guards: usize,
    physical: PhysAddrRange,
    flags: MappingFlags,
) -> Result<VirtAddrRange, VMAllocError> {
    VMAllocator::validate_phys_range(physical, vmm::page_size())?;
    let pages = physical.iter(vmm::page_size()).unwrap().collect::<Vec<_>>();
    alloc_range_and_map_phys_pages(guards, &pages, flags)
}

/// Registers an already-mapped external contiguous allocation.
///
/// Boot uses this metadata-only operation to adopt existing stack reservations.
pub fn register_external_range(
    full: VirtAddrRange,
    range: VirtAddrRange,
    physical: PhysAddrRange,
    flags: MappingFlags,
) -> Result<(), VMAllocError> {
    with_allocator(|allocator| allocator.register_external_range(full, range, physical, flags))?
}

/// Registers an already-mapped external page list.
///
/// No local mutation or remote completion is needed for adoption.
#[expect(unused)]
pub fn register_external_pages(
    full: VirtAddrRange,
    range: VirtAddrRange,
    pages: Box<[PhysAddr]>,
    flags: MappingFlags,
) -> Result<(), VMAllocError> {
    with_allocator(|allocator| allocator.register_external_pages(full, range, pages, flags))?
}

/// Retains uncertain backing and diagnoses the complete operation.
///
/// Quarantine records never report a live mapping or permit reservation reuse.
fn quarantine(token: &PreparedUnmap) -> Result<(), VMAllocError> {
    with_allocator(|allocator| allocator.quarantine_unmap(token))??;
    error!(
        "vmalloc quarantine range={:#x} pages={} reservation=retained frames=retained",
        token.range,
        token.pages.len()
    );
    Ok(())
}

/// Returns owned frames outside all metadata and page-table locks.
///
/// This helper is used only before mutation or after completed remote invalidation.
fn release_frames(pages: &[PhysAddr]) -> Result<(), VMAllocError> {
    for &page in pages {
        palloc::dealloc_frame(page).map_err(|error| {
            error!("physical release failed frame={page:#x}: {error:?}");
            VMAllocError::DeallocationError
        })?;
    }
    Ok(())
}

/// Completes a prepared unmap while retaining only the mapping transaction guard.
///
/// Any attempted mutation error is conservatively partial, so the entire token is quarantined.
fn complete_unmap(
    transaction: &MappingTransaction,
    token: PreparedUnmap,
) -> Result<(), VMAllocError> {
    if !transaction.targets().is_empty() && !exarch::trap::local_enabled() {
        with_allocator(|allocator| allocator.restore_unmutated(&token))??;
        return Err(TlbShootdownError::Ipi(crate::ipi::IpiError::InterruptsDisabled).into());
    }
    let mutation = vmm::with_page_table(|table| {
        table.apply_actions_with_cleanup(token.pages.iter().map(|page| PageTableAction::Unmap {
            vaddr: page.virtual_address,
            size: vmm::page_size(),
        }))
    });
    let cleanup = match mutation {
        Ok(cleanup) => cleanup,
        Err(error) => {
            error!(
                "vmalloc unmap partial mutation range={:#x}: {error:?}",
                token.range
            );
            quarantine(&token)?;
            return Err(VMAllocError::PageTableError);
        }
    };
    // Both VMALLOC and page-table guards are released before cleanup and remote completion.
    let request = if token.range.size() == vmm::page_size() {
        TlbInvalidation::Page(token.range.start)
    } else {
        TlbInvalidation::Range(token.range)
    };
    let request = vmm::finish_local_cleanup(cleanup, request);
    if let Err(error) = transaction.complete(request) {
        error!(
            "vmalloc completion range={:#x} targets={:?}: {error:?}",
            token.range,
            transaction.targets()
        );
        quarantine(&token)?;
        return Err(error.into());
    }
    with_allocator(|allocator| allocator.finalize_unmap(&token))??;
    for page in &token.pages {
        if page.owned {
            release_frames(core::slice::from_ref(&page.physical_address))?;
        }
    }
    Ok(())
}

/// Unmaps a usable subrange with exact remote completion.
///
/// Success retains the virtual reservation while releasing owned backing pages.
#[expect(unused)]
pub fn unmap(range: VirtAddrRange) -> Result<(), VMAllocError> {
    let transaction = MappingTransaction::begin()?;
    let token = with_allocator(|allocator| allocator.prepare_unmap(range, false))??;
    complete_unmap(&transaction, token)
}

/// Deallocates a complete guarded reservation after exact remote completion.
///
/// Timeout or possible partial mutation retains every frame and the complete virtual reservation.
pub fn dealloc_range(address: VirtAddr) -> Result<(), VMAllocError> {
    let transaction = MappingTransaction::begin()?;
    let token = with_allocator(|allocator| {
        let range = allocator.allocation_range(address)?;
        allocator.prepare_unmap(range, true)
    })??;
    complete_unmap(&transaction, token)
}

/// Translates only acknowledged live mapping records.
///
/// Pending or quarantined metadata cannot masquerade as the old mapping.
pub fn virt_to_phys(address: VirtAddr) -> Option<PhysAddr> {
    VMALLOC.get()?.lock().virt_to_phys(address)
}

/// Replaces owned diagnostic pages under one acknowledged mapping transaction.
///
/// Reader barriers prevent access during break-before-make. New frames are allocated while old
/// backing is still live, ensuring the smoke test exercises distinct physical translations.
#[cfg(feature = "foundation-test")]
pub(crate) fn replace_for_foundation(
    range: VirtAddrRange,
    values: &[u64],
    request: TlbInvalidation,
) -> Result<u64, VMAllocError> {
    let transaction = MappingTransaction::begin()?;
    let size = vmm::page_size();
    crate::tlb::validate(request, size)?;
    VMAllocator::validate_range(range, size)?;
    if values.len() != range.size() / size
        || !match request {
            TlbInvalidation::Page(address) => address == range.start && range.size() == size,
            TlbInvalidation::Range(selected) => selected == range,
            TlbInvalidation::All => true,
        }
    {
        return Err(VMAllocError::RangeSizeMismatch);
    }
    let mut new_pages = Vec::new();
    new_pages
        .try_reserve_exact(values.len())
        .map_err(|_| VMAllocError::AllocationError)?;
    for &value in values {
        let address = match palloc::alloc_frame() {
            Ok(address) => address,
            Err(_) => {
                release_frames(&new_pages)?;
                return Err(VMAllocError::AllocationError);
            }
        };
        let direct = vmm::direct_mapping_phys_to_virt(address).expect("allocated frame direct map");
        // SAFETY: This newly allocated frame is uniquely owned and not yet published to readers.
        unsafe {
            direct.as_mut_ptr_of::<u64>().write_volatile(value);
        }
        new_pages.push(address);
    }
    let token = match with_allocator(|allocator| allocator.prepare_unmap(range, false))? {
        Ok(token) => token,
        Err(error) => {
            release_frames(&new_pages)?;
            return Err(error);
        }
    };
    if token.pages.len() != new_pages.len() || token.pages.iter().any(|page| !page.owned) {
        with_allocator(|allocator| allocator.restore_unmutated(&token))??;
        release_frames(&new_pages)?;
        return Err(VMAllocError::RangeSizeMismatch);
    }
    let mutation = vmm::with_page_table(|table| {
        table.apply_actions_with_cleanup(token.pages.iter().map(|old| PageTableAction::Unmap {
            vaddr: old.virtual_address,
            size,
        }))
    });
    let result = mutation
        .map_err(|_| VMAllocError::PageTableError)
        .and_then(|cleanup| {
            let request = vmm::finish_local_cleanup(cleanup, request);
            transaction.complete(request).map_err(VMAllocError::from)
        })
        .and_then(|()| {
            // Complete break-before-make on every PE before publishing replacement descriptors.
            // Old and new frames both remain owned throughout both completion phases.
            let cleanup = vmm::with_page_table(|table| {
                table.apply_actions_with_cleanup(token.pages.iter().zip(&new_pages).map(
                    |(old, &new)| PageTableAction::Map {
                        vaddr: old.virtual_address,
                        paddr: new,
                        size,
                        flags: MappingFlags::READ.union(MappingFlags::WRITE),
                    },
                ))
            })
            .map_err(|_| VMAllocError::PageTableError)?;
            let request = vmm::finish_local_cleanup(cleanup, request);
            transaction.complete(request).map_err(VMAllocError::from)
        });
    if let Err(error) = result {
        quarantine(&token)?;
        error!(
            "replacement quarantine old={:?} new={new_pages:?}: {error:?}",
            token
                .pages
                .iter()
                .map(|page| page.physical_address)
                .collect::<Vec<_>>()
        );
        with_allocator(|allocator| {
            allocator
                .ranges
                .find_mut(range.start)
                .expect("quarantined reservation")
                .1
                .retained_backing = new_pages;
        })?;
        return Err(error);
    }
    let generation = crate::tlb::published_generation();
    with_allocator(|allocator| {
        let (_, allocation) = allocator
            .ranges
            .find_mut(range.start)
            .expect("prepared replacement");
        for (old, &new) in token.pages.iter().zip(&new_pages) {
            let index = old
                .virtual_address
                .sub_addr(allocation.non_guard_range.start)
                / size;
            assert_eq!(allocation.pages.statuses[index], VMAllocPageStatus::Pending);
            allocation.pages.mark_mapped(
                index,
                new,
                VMAllocPageStatus::Owned {
                    flags: MappingFlags::READ.union(MappingFlags::WRITE),
                },
            );
        }
    })?;
    for old in &token.pages {
        release_frames(core::slice::from_ref(&old.physical_address))?;
    }
    Ok(generation)
}

/// Reports retained reservation and quarantine metadata for deterministic diagnostics.
///
/// Guard addresses are included because the complete RangeSet key survives timeout.
#[cfg(feature = "foundation-test")]
pub(crate) fn reservation_state(address: VirtAddr) -> (bool, bool) {
    let Some(allocator) = VMALLOC.get() else {
        return (false, false);
    };
    let allocator = allocator.lock();
    allocator
        .ranges
        .find(address)
        .map_or((false, false), |(_, allocation)| {
            (true, allocation.quarantined)
        })
}

/// Delayed-reuse metadata state-machine tests.
///
/// Semantic fake addresses exercise real ownership transitions without privileged PTE access.
#[cfg(test)]
mod tests {
    use alloc::vec;

    use expt::pte::MappingFlags;
    use memory_addr::{VirtAddrRange, pa, va};

    use super::{VMAllocPageStatus, VMAllocator};

    /// Creates an isolated guarded allocation with two recorded pages.
    ///
    /// One frame is owned and one belongs to external code so release tokens must distinguish them.
    fn allocator() -> (VMAllocator, VirtAddrRange) {
        let mut allocator =
            VMAllocator::new(VirtAddrRange::new(va!(0x10000), va!(0x20000)), 12).unwrap();
        let range = allocator.alloc_range(2, 1).unwrap();
        let (_, allocation) = allocator.ranges.find_mut(range.start).unwrap();
        allocation.pages.mark_mapped(
            0,
            pa!(0x1000),
            VMAllocPageStatus::Owned {
                flags: MappingFlags::READ,
            },
        );
        allocation.pages.mark_mapped(
            1,
            pa!(0x2000),
            VMAllocPageStatus::External {
                flags: MappingFlags::READ,
            },
        );
        (allocator, range)
    }

    /// Retains resources during prepare and releases the complete reservation only at finalize.
    ///
    /// Cleared metadata never reports a live mapping while remote completion is pending.
    #[test]
    fn success_releases_only_after_completion() {
        let (mut allocator, range) = allocator();
        let token = allocator.prepare_unmap(range, true).unwrap();
        assert_eq!(
            token
                .pages
                .iter()
                .map(|page| page.owned)
                .collect::<alloc::vec::Vec<_>>(),
            vec![true, false]
        );
        assert_eq!(allocator.virt_to_phys(range.start), None);
        assert!(allocator.prepare_unmap(range, false).is_err());
        assert!(allocator.alloc_range(2, 1).unwrap().start > range.start);
        allocator.finalize_unmap(&token).unwrap();
        assert_eq!(allocator.alloc_range(2, 1).unwrap(), range);
    }

    /// Quarantines every token page and its guarded reservation after uncertain mutation.
    ///
    /// Neither remapping, retrying deallocation nor allocating a new range can reuse that token.
    #[test]
    fn timeout_and_partial_mutation_retain_resources() {
        let (mut allocator, range) = allocator();
        let token = allocator.prepare_unmap(range, true).unwrap();
        allocator.quarantine_unmap(&token).unwrap();
        assert_eq!(allocator.virt_to_phys(range.start), None);
        assert!(allocator.prepare_unmap(range, true).is_err());
        assert!(allocator.ensure_unmapped(range).is_err());
        assert_ne!(allocator.alloc_range(2, 1).unwrap(), range);
        let (_, allocation) = allocator.ranges.find(range.start).unwrap();
        assert_eq!(allocation.pages.phys_addr(0), pa!(0x1000));
        assert_eq!(allocation.pages.phys_addr(1), pa!(0x2000));
    }

    /// Restores ownership only when no PTE mutation was attempted.
    ///
    /// Subrange completion leaves the surrounding guarded reservation allocated.
    #[test]
    fn no_mutation_restores_and_subrange_keeps_reservation() {
        let (mut allocator, range) = allocator();
        let first = VirtAddrRange::from_start_size(range.start, 4096);
        let token = allocator.prepare_unmap(first, false).unwrap();
        allocator.restore_unmutated(&token).unwrap();
        assert_eq!(allocator.virt_to_phys(first.start), Some(pa!(0x1000)));
        let token = allocator.prepare_unmap(first, false).unwrap();
        allocator.finalize_unmap(&token).unwrap();
        assert_eq!(allocator.virt_to_phys(first.start), None);
        assert_eq!(
            allocator.virt_to_phys(range.start + 4096),
            Some(pa!(0x2000))
        );
        assert_ne!(allocator.alloc_range(2, 1).unwrap(), range);
    }
}
