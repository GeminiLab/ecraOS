//! RISC-V supervisor interrupt support.
//!
//! This implementation currently registers only the supervisor timer interrupt.

use alloc::boxed::Box;
use kspin::SpinNoIrq;
use lazyinit::LazyInit;
use memory_addr::PhysAddr;
use riscv::{
    interrupt::supervisor::{self, Interrupt as RiscInterrupt},
    register::sstatus,
};

use crate::{
    irq::IrqError,
    trap::{Interrupt, Trap, TrapDisposition, TrapFrame},
};

/// A RISC-V PLIC source identifier.
///
/// This identifier is assigned by the interrupt controller and is distinct from `scause` values
/// and local interrupt causes. Source zero is reserved by the PLIC and is never valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PlicSourceId(u32);

impl PlicSourceId {
    /// Creates a PLIC source identifier from a claimed or device-tree value.
    ///
    /// Callers must reject source zero and values outside the configured PLIC source range.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the controller-assigned source value.
    ///
    /// This conversion stays at PLIC register access and device-tree parsing boundaries.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// The RISC-V global interrupt identifier.
///
/// This target alias lets common trap code name a global source without conflating it with x86
/// GSIs or raw `scause` values.
pub type GlobalIrq = PlicSourceId;

/// Describes the RISC-V PLIC external-controller topology.
#[derive(Debug, Clone)]
pub struct RiscvExternalIrqConfig {
    /// Stores the PLIC physical MMIO base.
    pub physical_base: PhysAddr,
    /// Stores the PLIC MMIO size.
    pub size: usize,
    /// Stores the highest valid source ID.
    pub source_count: usize,
    /// Stores the supervisor context for each online hart.
    pub contexts: Box<[RiscvPlicContext]>,
}

/// Identifies one hart's supervisor PLIC context.
#[derive(Debug, Clone, Copy)]
pub struct RiscvPlicContext {
    /// Stores the hart identifier.
    pub hart_id: usize,
    /// Stores the PLIC context index.
    pub context: usize,
}

const ENABLE_BASE: usize = 0x2000;
const CONTEXT_BASE: usize = 0x200000;
const CONTEXT_STRIDE: usize = 0x1000;
const ENABLE_STRIDE: usize = 0x80;
const PRIORITY_STRIDE: usize = 4;

struct Plic {
    base: usize,
    source_count: usize,
    contexts: Box<[RiscvPlicContext]>,
}

static PLIC: LazyInit<SpinNoIrq<Plic>> = LazyInit::new();

/// Initializes the PLIC and disables all discovered supervisor sources.
pub fn init_external(config: RiscvExternalIrqConfig) {
    let maximum_context = config
        .contexts
        .iter()
        .map(|context| context.context)
        .max()
        .expect("PLIC has no supervisor contexts");
    let required_size = (CONTEXT_BASE + maximum_context * CONTEXT_STRIDE + 8)
        .max((config.source_count + 1) * PRIORITY_STRIDE);
    assert!(
        config.size >= required_size,
        "PLIC MMIO range is too small: size={:#x}, required={:#x}",
        config.size,
        required_size
    );
    let base = crate::kernel_if::phys_to_virt(config.physical_base).as_usize();
    let plic = Plic {
        base,
        source_count: config.source_count,
        contexts: config.contexts,
    };
    for source in 1..=plic.source_count {
        plic.write32(source * PRIORITY_STRIDE, 1);
    }
    for context in &plic.contexts {
        for word in 0..plic.enable_words() {
            plic.write32(ENABLE_BASE + context.context * ENABLE_STRIDE + word * 4, 0);
        }
        plic.write32(CONTEXT_BASE + context.context * CONTEXT_STRIDE, 0);
    }
    PLIC.init_once(SpinNoIrq::new(plic));
    let mut valid = alloc::vec![true; config.source_count + 1];
    valid[0] = false;
    crate::irq::init_external_registry(&valid);
}

/// Claims, dispatches, and completes every pending source for the current hart.
pub fn handle_external(frame: &mut TrapFrame) -> TrapDisposition {
    let Some(plic) = PLIC.get() else {
        return TrapDisposition::Unhandled;
    };
    let mut disposition = TrapDisposition::Handled;
    let mut claimed = false;
    loop {
        let source = {
            let plic = plic.lock();
            plic.claim()
        };
        let Some(source) = source else {
            break;
        };
        claimed = true;
        let Ok(source_id) = u32::try_from(source) else {
            disposition = TrapDisposition::Unhandled;
            plic.lock().complete(source);
            continue;
        };
        let irq = GlobalIrq::new(source_id);
        if source == 0 {
            disposition = TrapDisposition::Unhandled;
            plic.lock().complete(source);
            continue;
        }
        let source_disposition =
            crate::trap::handle(frame, Trap::Interrupt(Interrupt::Global(irq)));
        if source_disposition == TrapDisposition::Unhandled {
            plic.lock().mask(irq);
        }
        disposition += source_disposition;
        // Complete only after semantic dispatch and any required masking.
        plic.lock().complete(source);
    }
    if claimed {
        disposition
    } else {
        TrapDisposition::Unhandled
    }
}

impl Plic {
    fn enable_words(&self) -> usize {
        (self.source_count + 1).div_ceil(32)
    }

    fn write32(&self, offset: usize, value: u32) {
        // SAFETY: PLIC MMIO is mapped as device memory during VMM setup.
        unsafe { ((self.base + offset) as *mut u32).write_volatile(value) }
    }

    fn read32(&self, offset: usize) -> u32 {
        // SAFETY: PLIC MMIO is mapped as device memory during VMM setup.
        unsafe { ((self.base + offset) as *const u32).read_volatile() }
    }

    fn context(&self) -> Option<RiscvPlicContext> {
        let hart = crate::kernel_if::current_cpu_phys_id();
        self.contexts
            .iter()
            .copied()
            .find(|context| context.hart_id == hart)
    }

    fn set_enabled(&self, irq: GlobalIrq, enabled: bool) -> Result<(), IrqError> {
        let source = irq.raw() as usize;
        if source == 0 || source > self.source_count {
            return Err(IrqError::InvalidNumber);
        }
        let context = self.context().ok_or(IrqError::Unsupported)?;
        let offset = ENABLE_BASE + context.context * ENABLE_STRIDE + ((source / 32) * 4);
        let bit = 1u32 << (source % 32);
        let value = self.read32(offset);
        self.write32(offset, if enabled { value | bit } else { value & !bit });
        Ok(())
    }

    fn mask(&self, irq: GlobalIrq) {
        let _ = self.set_enabled(irq, false);
    }

    fn unmask(&self, irq: GlobalIrq) {
        let _ = self.set_enabled(irq, true);
    }

    fn claim(&self) -> Option<usize> {
        let context = self.context()?;
        let source = self.read32(CONTEXT_BASE + context.context * CONTEXT_STRIDE + 4) as usize;
        (source != 0).then_some(source)
    }

    fn complete(&self, source: usize) {
        if let Some(context) = self.context() {
            self.write32(
                CONTEXT_BASE + context.context * CONTEXT_STRIDE + 4,
                source as u32,
            );
        }
    }
}

/// The interrupt marker bit in `scause`.
///
/// RISC-V places this marker in the most significant XLEN bit.
pub const INTC_IRQ_BASE: usize = 1 << (usize::BITS - 1);

/// The supervisor software interrupt cause.
///
/// This cause remains masked until an IPI implementation is added.
pub const SOFTWARE_IRQ_NUM: usize = INTC_IRQ_BASE | RiscInterrupt::SupervisorSoft as usize;

/// The supervisor timer interrupt cause.
///
/// This is the only IRQ accepted by the current registration implementation.
pub const TIMER_IRQ_NUM: usize = INTC_IRQ_BASE | RiscInterrupt::SupervisorTimer as usize;

/// The supervisor external interrupt cause.
///
/// This cause remains masked until a PLIC implementation is added.
pub const EXTERNAL_IRQ_NUM: usize = INTC_IRQ_BASE | RiscInterrupt::SupervisorExternal as usize;

/// Enables interrupts globally on the current hart.
pub fn enable_local() {
    // SAFETY: callers enable interrupts only after installing handlers.
    unsafe { supervisor::enable() };
}

/// Disables interrupts globally on the current hart.
pub fn disable_local() {
    supervisor::disable();
}

/// Returns whether interrupts are globally enabled on the current hart.
pub fn local_enabled() -> bool {
    sstatus::read().sie()
}

/// Prepares an external PLIC source while keeping it masked.
pub fn prepare(irq: GlobalIrq) -> Result<(), IrqError> {
    let plic = PLIC.get().ok_or(IrqError::Unsupported)?;
    if irq.raw() == 0 || irq.raw() as usize > plic.lock().source_count {
        return Err(IrqError::InvalidNumber);
    }
    Ok(())
}

/// Enables or disables a prepared PLIC source.
pub fn set_enabled(irq: GlobalIrq, enabled: bool) -> Result<(), IrqError> {
    let plic = PLIC.get().ok_or(IrqError::Unsupported)?;
    let plic = plic.lock();
    if enabled {
        plic.unmask(irq);
    } else {
        plic.mask(irq);
    }
    Ok(())
}

/// Enables the supervisor timer source on the current hart.
///
/// Global interrupt delivery remains controlled separately through [`crate::irq::enable_local`].
pub(super) fn init_percpu() {
    // SAFETY: global interrupts remain disabled until the kernel installs and arms the timer.
    unsafe {
        supervisor::enable_interrupt(RiscInterrupt::SupervisorTimer);
        supervisor::enable_interrupt(RiscInterrupt::SupervisorExternal);
    }
}
