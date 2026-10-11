//! AArch64 exception and GICv3 interrupt support.

use core::arch::{asm, global_asm};

use aarch64_cpu::registers::{DAIF, ReadWriteable, Readable, VBAR_EL1, Writeable};
use kspin::SpinNoIrq;
use lazyinit::LazyInit;
use memory_addr::PhysAddr;

use crate::trap::{
    Exception, Interrupt, LocalInterrupt, PageFaultFlags, RawTrap, Trap, TrapDisposition,
    TrapFrame, TrapFrameAccess,
};

global_asm!(include_str!("trap.S"));

/// A GICv3 interrupt source identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ArchIrq(u32);

impl ArchIrq {
    /// Creates a source identifier from a validated GIC INTID.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
    /// Returns the raw GIC INTID.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// Describes the GICv3 distributor used by the platform.
#[derive(Debug, Clone)]
pub struct Aarch64ExternalIrqConfig {
    /// Distributor physical base address.
    pub gicd_base: PhysAddr,
    /// Redistributor physical base address for the boot CPU.
    pub gicr_base: PhysAddr,
    /// Distributor MMIO size.
    pub size: usize,
    /// Number of valid SPI source IDs.
    pub spi_count: usize,
}

struct Gic {
    dist: usize,
    redist: usize,
    spi_count: usize,
}

/// Provides the GIC CPU-interface system-register boundary.
struct GicCpuInterface;

impl GicCpuInterface {
    /// Enables the EL1 system-register interface and unmasks group-one interrupts.
    fn enable() {
        // `aarch64-cpu` 11.2.0 does not define the ICC_* EL1 registers.
        unsafe {
            asm!("msr icc_sre_el1, {value}", value = in(reg) 7u64, options(nomem, nostack));
            asm!("msr icc_pmr_el1, {value}", value = in(reg) 0xffu64, options(nomem, nostack));
            asm!("msr icc_igrpen1_el1, {value}", value = in(reg) 1u64, options(nomem, nostack));
        }
    }

    /// Acknowledges the highest-priority pending interrupt.
    fn acknowledge() -> u64 {
        let intid: u64;
        unsafe {
            asm!("mrs {value}, icc_iar1_el1", value = out(reg) intid, options(nomem, nostack));
        }
        intid
    }

    /// Signals completion of an acknowledged interrupt.
    fn end_of_interrupt(intid: u64) {
        unsafe {
            asm!("msr icc_eoir1_el1, {value}", value = in(reg) intid, options(nomem, nostack));
        }
    }
}

static GIC: LazyInit<SpinNoIrq<Gic>> = LazyInit::new();

const GICD_CTLR: usize = 0x000;
const GICD_ISENABLER: usize = 0x100;
const GICD_ICENABLER: usize = 0x180;
const GICD_IPRIORITYR: usize = 0x400;
const GICD_ICFGR: usize = 0xC00;
const GICD_IGROUPR: usize = 0x080;
const GICR_WAKER: usize = 0x14;
const GICR_SGI_BASE: usize = 0x10000;
const GICR_ISENABLER0: usize = GICR_SGI_BASE + 0x100;
const GICR_IGROUPR0: usize = GICR_SGI_BASE + 0x080;
const GICR_IPRIORITYR: usize = GICR_SGI_BASE + 0x400;

/// Returns whether an INTID belongs to the GIC SPI namespace.
fn is_spi(intid: u32) -> bool {
    (32..1020).contains(&intid)
}

unsafe extern "C" {
    /// The 2048-byte-aligned EL1 exception vector table.
    fn aarch64_vector_base();
}

impl Gic {
    fn write32(&self, base: usize, offset: usize, value: u32) {
        unsafe { ((base + offset) as *mut u32).write_volatile(value) }
    }
    fn read32(&self, base: usize, offset: usize) -> u32 {
        unsafe { ((base + offset) as *const u32).read_volatile() }
    }
    fn set_enabled(&self, irq: ArchIrq, enabled: bool) -> Result<(), crate::trap::irq::IrqError> {
        let id = irq.raw() as usize;
        if id < 32 || id >= 32 + self.spi_count {
            return Err(crate::trap::irq::IrqError::InvalidNumber);
        }
        let offset = (id / 32) * 4;
        self.write32(
            self.dist,
            if enabled {
                GICD_ISENABLER
            } else {
                GICD_ICENABLER
            } + offset,
            1 << (id % 32),
        );
        Ok(())
    }
}

/// Initializes the GIC distributor and CPU interface with all SPIs masked.
pub fn init_external_controller(config: Aarch64ExternalIrqConfig) {
    let dist = crate::kernel_if::phys_to_virt(config.gicd_base).as_usize();
    let redist = crate::kernel_if::phys_to_virt(config.gicr_base).as_usize();
    assert!(config.size >= GICD_ICFGR + (config.spi_count.div_ceil(16) * 4));
    let gic = Gic {
        dist,
        redist,
        spi_count: config.spi_count,
    };
    gic.write32(dist, GICD_CTLR, 0x3);
    for id in (32..32 + config.spi_count).step_by(32) {
        gic.write32(dist, GICD_IGROUPR + (id / 32) * 4, u32::MAX);
        gic.write32(dist, GICD_ICENABLER + (id / 32) * 4, u32::MAX);
    }
    GIC.init_once(SpinNoIrq::new(gic));
    let mut valid = alloc::vec![false; 32 + config.spi_count];
    for source in &mut valid[32..] {
        *source = true;
    }
    crate::trap::irq::init_external_registry(&valid);
}

/// Enables or disables one prepared SPI source.
pub fn set_irq_enabled(irq: ArchIrq, enabled: bool) -> Result<(), crate::trap::irq::IrqError> {
    GIC.get()
        .ok_or(crate::trap::irq::IrqError::Unsupported)?
        .lock()
        .set_enabled(irq, enabled)
}

/// Prepares an SPI source while keeping it masked.
pub fn prepare_irq(irq: ArchIrq) -> Result<(), crate::trap::irq::IrqError> {
    let gic = GIC
        .get()
        .ok_or(crate::trap::irq::IrqError::Unsupported)?
        .lock();
    if irq.raw() < 32 || irq.raw() as usize >= 32 + gic.spi_count {
        return Err(crate::trap::irq::IrqError::InvalidNumber);
    }
    gic.write32(gic.dist, GICD_IPRIORITYR + irq.raw() as usize, 0x80);
    gic.write32(gic.dist, GICD_ICFGR + (irq.raw() as usize / 16) * 4, 0);
    Ok(())
}

/// Enables the EL1 GIC system-register interface and unmasks priorities.
pub fn init_percpu() {
    if let Some(gic) = GIC.get() {
        let gic = gic.lock();
        let waker = gic.read32(gic.redist, GICR_WAKER);
        gic.write32(gic.redist, GICR_WAKER, waker & !(1 << 1));
        // Run all SGIs and PPIs as non-secure Group 1 interrupts.  This keeps
        // local timer and software interrupt routing consistent with the SPI
        // configuration in the distributor.
        gic.write32(gic.redist, GICR_IGROUPR0, u32::MAX);
        gic.write32(gic.redist, GICR_ISENABLER0, 1 << 27);
        unsafe {
            ((gic.redist + GICR_IPRIORITYR + 27) as *mut u8).write_volatile(0x80);
        }
    }
    VBAR_EL1.set(aarch64_vector_base as *const () as usize as u64);
    GicCpuInterface::enable();
    aarch64_cpu::asm::barrier::isb(aarch64_cpu::asm::barrier::SY);
}

/// Enables maskable interrupts at EL1.
pub fn enable_local() {
    DAIF.modify(DAIF::I::CLEAR);
}
/// Disables maskable interrupts at EL1.
pub fn disable_local() {
    DAIF.modify(DAIF::I::SET);
}
/// Returns whether maskable interrupts are enabled at EL1.
pub fn local_enabled() -> bool {
    let value = DAIF.get();
    value & (1 << 7) == 0
}

/// Acknowledges and dispatches one GIC interrupt, then signals end of interrupt.
pub fn handle_external(frame: &mut TrapFrame) -> TrapDisposition {
    let intid = GicCpuInterface::acknowledge();
    let disposition = if intid == 27 {
        crate::trap::handle(
            frame,
            Trap::Interrupt(Interrupt::Local(LocalInterrupt::Timer)),
        )
    } else if is_spi(intid as u32) {
        crate::trap::handle(
            frame,
            Trap::Interrupt(Interrupt::Global(ArchIrq::new(intid as u32))),
        )
    } else {
        TrapDisposition::Unhandled
    };
    GicCpuInterface::end_of_interrupt(intid);
    disposition
}

#[cfg(test)]
mod tests {
    use super::is_spi;

    /// Verifies that SGIs and PPIs do not enter the global SPI registry.
    #[test]
    fn distinguishes_global_spi_intids() {
        assert!(!is_spi(0));
        assert!(!is_spi(27));
        assert!(is_spi(32));
        assert!(is_spi(1019));
        assert!(!is_spi(1020));
    }
}

/// Decodes one synchronous AArch64 exception and dispatches it to common handlers.
#[unsafe(no_mangle)]
extern "C" fn aarch64_trap_handler(frame: &mut TrapFrame) {
    let ec = (frame.esr_el1 >> 26) & 0x3f;
    let is_irq = frame.vector % 4 == 1;
    let disposition = if is_irq {
        handle_external(frame)
    } else {
        let trap = match ec {
            0x20 | 0x21 => Trap::Exception(Exception::Unknown(RawTrap(frame.esr_el1))),
            0x24 | 0x25 => Trap::Exception(Exception::PageFault {
                address: memory_addr::va!(frame.far_el1),
                flags: PageFaultFlags::READ,
                is_user: frame.is_user(),
            }),
            0x3c => Trap::Exception(Exception::Breakpoint),
            _ => Trap::Exception(Exception::Unknown(RawTrap(frame.esr_el1))),
        };
        crate::trap::handle(frame, trap)
    };
    if disposition == TrapDisposition::Unhandled {
        log::warn!("Unhandled AArch64 trap: esr={:#x}", frame.esr_el1);
    }
}
