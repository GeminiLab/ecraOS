//! GICv3 distributor, redistributor and EL1 CPU-interface mechanisms.
//!
//! Controller topology remains physical mechanism state and never stores logical CPU membership.

use core::{arch::asm, time::Duration};

use aarch64_cpu::registers::{MPIDR_EL1, Readable};
use expercpu::def_percpu;
use kspin::SpinNoIrq;
use lazyinit::LazyInit;
use memory_addr::{PhysAddr, VirtAddr, va};

use crate::{ipi::IpiError, power::PhysicalCpuId, trap::irq::IrqError};

/// Pure controller topology and physical interrupt encoding.
///
/// Host tests use these helpers without mapped registers or privileged instructions.
mod encoding;

pub(crate) use encoding::{GicInterrupt, decode_intid};
use encoding::{IPI_SGI, select_redistributor, sgi1r};

/// A GIC global interrupt source identifier.
///
/// Valid global sources are SPIs rather than SGIs, PPIs or exception causes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ArchIrq(u32);

impl ArchIrq {
    /// Creates a source identifier from a validated GIC INTID.
    ///
    /// The controller rejects private and out-of-range sources at preparation time.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
    /// Returns the controller INTID at its register boundary.
    ///
    /// This value is never interpreted as a logical CPU or trap cause.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// The complete firmware-described GICv3 MMIO regions.
///
/// Redistributor discovery must retain the entire region to locate every physical PE's frame.
#[derive(Debug, Clone)]
pub struct Aarch64ExternalIrqConfig {
    /// The distributor physical base.
    ///
    /// The kernel direct map supplies the accessible device address.
    pub gicd_base: PhysAddr,
    /// The complete distributor region size.
    ///
    /// Register access is validated against this firmware boundary.
    pub gicd_size: usize,
    /// The complete redistributor region physical base.
    ///
    /// This is not a single BSP frame address.
    pub gicr_base: PhysAddr,
    /// The complete redistributor region size.
    ///
    /// The Last marker and VLPIS stride also bound frame discovery.
    pub gicr_size: usize,
    /// The platform's upper bound on usable SPI sources.
    ///
    /// GICD_TYPER further limits the actual controller capacity.
    pub spi_count: usize,
}

/// Immutable mapped controller topology.
///
/// Local delivery and completion never acquire a controller lock.
struct Gic {
    /// The distributor device mapping.
    ///
    /// SPI configuration alone uses the separate configuration lock.
    dist: VirtAddr,
    /// The complete redistributor region mapping.
    ///
    /// Each PE selects its own frame during initialization.
    redist: VirtAddr,
    /// The firmware redistributor region boundary.
    ///
    /// Topology scanning validates every complete frame stride.
    redist_size: usize,
    /// The usable SPI capacity.
    ///
    /// Private sources remain outside this namespace.
    spi_count: usize,
}

/// The controller state installed once during BSP initialization.
///
/// It contains only physical controller mappings and capacity.
static GIC: LazyInit<Gic> = LazyInit::new();

/// The short SPI read-modify-write serialization boundary.
///
/// Local SGI/PPI acknowledgement and IPI send paths never acquire this lock.
static SPI_CONFIG: SpinNoIrq<()> = SpinNoIrq::new(());

/// The current PE's selected redistributor frame at the per-CPU storage boundary.
///
/// Zero denotes early initialization before the complete controller mapping is available.
#[def_percpu]
static LOCAL_REDISTRIBUTOR: usize = 0;

/// The distributor control-register offset.
///
/// Its RWP bit bounds completion of configuration writes.
const GICD_CTLR: usize = 0;
/// The distributor capacity-register offset.
///
/// ITLinesNumber determines the number of implemented interrupt sources.
const GICD_TYPER: usize = 4;
/// The interrupt-group register array offset.
///
/// Non-secure kernel sources use Group one.
const IGROUPR: usize = 0x80;
/// The source-enable register array offset.
///
/// Each bit enables one controller source without a read-modify-write.
const ISENABLER: usize = 0x100;
/// The source-disable register array offset.
///
/// Each bit masks one controller source without disturbing adjacent sources.
const ICENABLER: usize = 0x180;
/// The byte priority-register array offset.
///
/// Byte writes preserve the priorities of adjacent sources.
const IPRIORITYR: usize = 0x400;
/// The trigger-configuration register array offset.
///
/// SPI preparation preserves unrelated two-bit source fields.
const ICFGR: usize = 0xc00;
/// The physical SPI affinity-routing register array offset.
///
/// Each implemented SPI has an eight-byte IROUTER entry.
const IROUTER: usize = 0x6000;
/// The redistributor frame affinity-register offset.
///
/// The register also supplies Last and VLPIS frame-discovery information.
const GICR_TYPER: usize = 8;
/// The redistributor wake-register offset.
///
/// ChildrenAsleep must clear before enabling local sources.
const GICR_WAKER: usize = 0x14;
/// The SGI/PPI register page offset within a redistributor frame.
///
/// Its registers affect only the PE owning that selected frame.
const SGI_BASE: usize = 0x10000;

/// Reads one device register through a semantic virtual address.
///
/// Callers validate the mapped controller region during initialization.
fn read32(base: VirtAddr, offset: usize) -> u32 {
    unsafe { (base + offset).as_ptr_of::<u32>().read_volatile() }
}
/// Writes one device register through a semantic virtual address.
///
/// Controller bit-set and bit-clear registers do not require a read-modify-write lock.
fn write32(base: VirtAddr, offset: usize, value: u32) {
    unsafe { (base + offset).as_mut_ptr_of::<u32>().write_volatile(value) }
}
/// Reads one aligned sixty-four-bit device register.
///
/// Redistributor affinity discovery reads GICR_TYPER with this width.
fn read64(base: VirtAddr, offset: usize) -> u64 {
    unsafe { (base + offset).as_ptr_of::<u64>().read_volatile() }
}
/// Writes one aligned sixty-four-bit device register.
///
/// SPI affinity routing uses the physical MPIDR encoding.
fn write64(base: VirtAddr, offset: usize, value: u64) {
    unsafe { (base + offset).as_mut_ptr_of::<u64>().write_volatile(value) }
}
/// Writes one source's priority byte without changing adjacent priorities.
///
/// Priority-register offsets are not necessarily word aligned.
fn write8(base: VirtAddr, offset: usize, value: u8) {
    unsafe { (base + offset).as_mut_ptr_of::<u8>().write_volatile(value) }
}

/// Waits for controller initialization with one absolute monotonic deadline.
///
/// Failure reports the physical CPU and observation rather than continuing with an unusable source.
fn wait_clear(base: VirtAddr, offset: usize, bits: u32, operation: &str) {
    let deadline = crate::time::monotonic_time().saturating_add(Duration::from_secs(1));
    while read32(base, offset) & bits != 0 {
        let observed = crate::time::monotonic_time();
        assert!(
            observed < deadline,
            "GIC {operation} timeout physical={:#x} generation=None deadline={deadline:?} observation={observed:?} register={:#x}",
            MPIDR_EL1.get(),
            read32(base, offset)
        );
        core::hint::spin_loop();
    }
}

/// Initializes the complete controller topology with all SPIs masked.
///
/// Firmware parsing remains with the existing platform owner until its separate convergence task.
pub fn init_external_controller(config: Aarch64ExternalIrqConfig) {
    assert!(
        config.gicd_size >= 0x10000,
        "GIC distributor region is incomplete"
    );
    let dist = crate::kernel_if::phys_to_virt(config.gicd_base);
    let redist = crate::kernel_if::phys_to_virt(config.gicr_base);
    let capacity = ((read32(dist, GICD_TYPER) & 31) as usize + 1) * 32;
    let spi_count = config.spi_count.min(capacity.saturating_sub(32)).min(988);
    write32(dist, GICD_CTLR, 0);
    wait_clear(dist, GICD_CTLR, 1 << 31, "distributor disable");
    // QEMU virt runs non-secure EL1. ARE_NS enables physical-affinity SGI and SPI routing.
    write32(dist, GICD_CTLR, 1 << 4);
    wait_clear(dist, GICD_CTLR, 1 << 31, "affinity routing enable");
    for id in (32..32 + spi_count).step_by(32) {
        write32(dist, IGROUPR + (id / 32) * 4, u32::MAX);
        write32(dist, ICENABLER + (id / 32) * 4, u32::MAX);
    }
    wait_clear(dist, GICD_CTLR, 1 << 31, "SPI mask");
    write32(dist, GICD_CTLR, (1 << 4) | 3);
    wait_clear(dist, GICD_CTLR, 1 << 31, "group one enable");
    GIC.init_once(Gic {
        dist,
        redist,
        redist_size: config.gicr_size,
        spi_count,
    });
    let mut valid = alloc::vec![false; 32 + spi_count];
    valid[32..].fill(true);
    crate::trap::irq::init_external_registry(&valid);
}

/// Initializes this PE's redistributor and EL1 CPU interface.
///
/// Early vector installation may precede mapped controller discovery, so selection waits until
/// the existing late platform initialization supplies the complete MMIO regions.
pub fn init_percpu() {
    if let Some(gic) = GIC.get() {
        let mpidr = MPIDR_EL1.get();
        let offset = select_redistributor(gic.redist_size, mpidr, |offset| {
            Some(read64(gic.redist, offset + GICR_TYPER))
        })
        .unwrap_or_else(|error| {
            panic!(
                "GIC redistributor physical={mpidr:#x} region={:#x} size={:#x}: {error:?}",
                gic.redist, gic.redist_size
            )
        });
        let redist = gic.redist + offset;
        LOCAL_REDISTRIBUTOR.write_current(redist.as_usize());
        write32(redist, GICR_WAKER, read32(redist, GICR_WAKER) & !(1 << 1));
        wait_clear(redist, GICR_WAKER, 1 << 2, "redistributor wake");
        write32(redist, SGI_BASE + IGROUPR, u32::MAX);
        write8(redist, SGI_BASE + IPRIORITYR + 27, 0x80);
        write32(redist, SGI_BASE + ISENABLER, 1 << 27);
    }
    // aarch64-cpu does not expose the ICC registers used by this EL1-only backend.
    unsafe {
        asm!("msr icc_sre_el1, {value}", "isb", value = in(reg) 7u64, options(nostack));
        let control: u64;
        asm!("mrs {value}, icc_ctlr_el1", value = out(reg) control, options(nomem, nostack));
        asm!("msr icc_ctlr_el1, {value}", value = in(reg) control & !2, options(nomem, nostack));
        asm!("msr icc_pmr_el1, {value}", value = in(reg) 0xffu64, options(nomem, nostack));
        asm!("msr icc_bpr1_el1, xzr", options(nomem, nostack));
        asm!("msr icc_igrpen1_el1, {value}", "isb", value = in(reg) 1u64, options(nostack));
    }
}

/// Enables the reserved SGI in this PE's selected redistributor.
///
/// Common boot calls this only after software semantic dispatch and mailboxes are installed.
pub(crate) fn enable_ipi() {
    let redist = va!(LOCAL_REDISTRIBUTOR.read_current());
    assert!(
        redist.as_usize() != 0,
        "IPI source requires a selected redistributor"
    );
    write8(redist, SGI_BASE + IPRIORITYR + IPI_SGI as usize, 0x80);
    write32(redist, SGI_BASE + ISENABLER, 1 << IPI_SGI);
    unsafe {
        asm!("dsb sy", "isb", options(nostack));
    }
}

/// Submits a Group-one SGI to one physical affinity.
///
/// Validation precedes the system-register write, which commits delivery without a controller lock.
pub(crate) fn send_ipi(target: PhysicalCpuId) -> Result<(), IpiError> {
    let value = sgi1r(target, IPI_SGI)?;
    if GIC.get().is_none() {
        return Err(IpiError::Unsupported);
    }
    unsafe {
        asm!("dsb ishst", "msr icc_sgi1r_el1, {value}", "isb", value = in(reg) value, options(nostack));
    }
    Ok(())
}

/// Acknowledges the highest-priority local Group-one delivery.
///
/// Special INTIDs denote no actual delivery and need no EOI.
pub(crate) fn acknowledge() -> u32 {
    let intid: u64;
    unsafe {
        // The acknowledgement must complete before shared mailbox or device accesses.
        asm!("mrs {value}, icc_iar1_el1", "dsb sy", value = out(reg) intid, options(nostack));
    }
    intid as u32
}

/// Completes one acknowledged local delivery.
///
/// EOImode is cleared during CPU-interface initialization, so this write also deactivates it.
pub(crate) fn end_of_interrupt(intid: u32) {
    unsafe {
        asm!("msr icc_eoir1_el1, {value}", "isb", value = in(reg) u64::from(intid), options(nostack));
    }
}

/// Enables or masks one valid SPI source.
///
/// The bit-set/bit-clear device registers require no shared read-modify-write operation.
pub fn set_irq_enabled(irq: ArchIrq, enabled: bool) -> Result<(), IrqError> {
    let gic = GIC.get().ok_or(IrqError::Unsupported)?;
    let id = irq.raw() as usize;
    if id < 32 || id >= 32 + gic.spi_count {
        return Err(IrqError::InvalidNumber);
    }
    write32(
        gic.dist,
        if enabled { ISENABLER } else { ICENABLER } + (id / 32) * 4,
        1 << (id % 32),
    );
    Ok(())
}

/// Prepares one masked SPI with physical affinity routing to the current PE.
///
/// The short configuration lock protects only shared trigger-register fields.
pub fn prepare_irq(irq: ArchIrq) -> Result<(), IrqError> {
    let gic = GIC.get().ok_or(IrqError::Unsupported)?;
    let id = irq.raw() as usize;
    if id < 32 || id >= 32 + gic.spi_count {
        return Err(IrqError::InvalidNumber);
    }
    let _guard = SPI_CONFIG.lock();
    write8(gic.dist, IPRIORITYR + id, 0x80);
    let offset = ICFGR + (id / 16) * 4;
    write32(
        gic.dist,
        offset,
        read32(gic.dist, offset) & !(3 << ((id % 16) * 2)),
    );
    write64(gic.dist, IROUTER + id * 8, MPIDR_EL1.get() & 0xff_00ff_ffff);
    Ok(())
}
