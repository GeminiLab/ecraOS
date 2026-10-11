//! x86-64 interrupt controller and local interrupt support.

use alloc::{boxed::Box, vec};
use core::sync::atomic::{AtomicUsize, Ordering};

use kspin::SpinNoIrq;
use lazyinit::LazyInit;
use x86_64::instructions::interrupts;

use crate::{
    arch::x86_64::imp::ioapic::{IoApicConfig, IoApicSet, Polarity, TriggerMode},
    trap::irq::IrqError,
};

/// An x86 global system interrupt identifier.
///
/// A GSI is assigned by firmware and resolves to one IOAPIC pin. It is not an IDT vector or an
/// ISA source number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Gsi(u32);

impl Gsi {
    /// Creates a GSI identifier from its firmware-assigned numeric value.
    ///
    /// Callers must obtain this value from validated ACPI routing information or an existing
    /// controller route.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the firmware-assigned numeric GSI value.
    ///
    /// This conversion stays at the controller boundary where an IOAPIC pin is calculated.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// The x86 global interrupt identifier.
///
/// This target alias lets common trap code name a global source without erasing its GSI semantics.
pub type ArchIrq = Gsi;

/// Describes an ACPI ISA interrupt source override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct X86IrqOverride {
    /// Stores the legacy ISA source number.
    pub isa_source: u8,
    /// Stores the target global system interrupt.
    pub gsi: u32,
    /// Stores the electrical polarity.
    pub polarity: Polarity,
    /// Stores the trigger mode.
    pub trigger: TriggerMode,
}

/// Describes the x86 external interrupt controller topology.
#[derive(Debug, Clone)]
pub struct X86ExternalIrqConfig {
    /// Stores all IOAPIC descriptors.
    pub io_apics: Box<[IoApicConfig]>,
    /// Stores all ISA source overrides.
    pub overrides: Box<[X86IrqOverride]>,
    /// Stores the BSP local APIC destination ID.
    pub bsp_apic_id: u32,
}

static EXTERNAL_CONTROLLER: LazyInit<SpinNoIrq<ExternalController>> = LazyInit::new();
static VECTOR_TO_GSI: [AtomicUsize; 256] = [const { AtomicUsize::new(0) }; 256];

struct ExternalController {
    ioapics: IoApicSet,
    overrides: Box<[X86IrqOverride]>,
    bsp_apic_id: u8,
    routes: Box<[bool]>,
    next_vector: u16,
}

impl ExternalController {
    unsafe fn new(config: X86ExternalIrqConfig) -> Self {
        let bsp_apic_id = u8::try_from(config.bsp_apic_id)
            .expect("x86 IOAPIC BSP destination must fit in an APIC RTE");
        let ioapics = unsafe { IoApicSet::new(config.io_apics.into_vec()) }
            .expect("invalid ACPI IOAPIC topology");
        let max_gsi = ioapics.gsi_limit();
        Self {
            ioapics,
            overrides: config.overrides,
            bsp_apic_id,
            routes: vec![false; max_gsi].into_boxed_slice(),
            next_vector: 0x30,
        }
    }

    fn prepare_route(&mut self, irq: ArchIrq) -> Result<(), IrqError> {
        let gsi = irq.raw();
        if self
            .routes
            .get(irq.raw() as usize)
            .copied()
            .ok_or(IrqError::InvalidNumber)?
        {
            return Ok(());
        }
        let (polarity, trigger) = self
            .overrides
            .iter()
            .find(|override_entry| override_entry.gsi == gsi)
            .map(|override_entry| (override_entry.polarity, override_entry.trigger))
            .unwrap_or((Polarity::ActiveHigh, TriggerMode::Edge));
        let vector = self.allocate_vector().ok_or(IrqError::Unsupported)?;
        self.ioapics
            .prepare_source(gsi, vector, self.bsp_apic_id, polarity, trigger)
            .map_err(|_| IrqError::InvalidNumber)?;
        *self
            .routes
            .get_mut(irq.raw() as usize)
            .ok_or(IrqError::InvalidNumber)? = true;
        VECTOR_TO_GSI[vector as usize].store(irq.raw() as usize + 1, Ordering::Release);
        Ok(())
    }

    fn allocate_vector(&mut self) -> Option<u8> {
        for _ in 0..(0xf0 - 0x30) {
            let candidate = self.next_vector as u8;
            self.next_vector = if self.next_vector + 1 >= 0xf0 {
                0x30
            } else {
                self.next_vector + 1
            };
            if VECTOR_TO_GSI[candidate as usize].load(Ordering::Acquire) == 0 {
                return Some(candidate);
            }
        }
        None
    }
}

/// Initializes the x86 external controller from ACPI data.
pub fn init_external_controller(config: X86ExternalIrqConfig) {
    // SAFETY: VMM initialization has established device mappings before ACPI discovery reaches
    // this function.
    let controller = unsafe { ExternalController::new(config) };
    let max_gsi = controller.ioapics.gsi_limit();
    let mut valid = vec![false; max_gsi];
    for gsi in 0..max_gsi {
        valid[gsi] = controller.ioapics.contains_gsi(gsi as u32);
    }
    EXTERNAL_CONTROLLER.init_once(SpinNoIrq::new(controller));
    crate::trap::irq::init_external_registry(&valid);
}

/// Returns the GSI routed to an x86 external vector on the BSP.
pub fn gsi_for_vector(vector: u8) -> Option<ArchIrq> {
    VECTOR_TO_GSI[vector as usize]
        .load(Ordering::Acquire)
        .checked_sub(1)
        .and_then(|gsi| u32::try_from(gsi).ok())
        .map(ArchIrq::new)
}

/// Masks a routed x86 global source before local APIC completion.
///
/// The trap adapter calls this for a source whose kernel handler returned `Unhandled`.
pub fn mask_source(irq: ArchIrq) {
    if let Some(controller) = EXTERNAL_CONTROLLER.get() {
        controller.lock().ioapics.mask(irq.raw());
    }
}

/// Enables interrupts globally on the current CPU.
pub fn enable_local() {
    interrupts::enable();
}

/// Disables interrupts globally on the current CPU.
pub fn disable_local() {
    interrupts::disable();
}

/// Returns whether interrupts are globally enabled on the current CPU.
pub fn local_enabled() -> bool {
    interrupts::are_enabled()
}

/// Prepares an external source while keeping it masked.
pub fn prepare_irq(irq: ArchIrq) -> Result<(), IrqError> {
    let controller = EXTERNAL_CONTROLLER.get().ok_or(IrqError::Unsupported)?;
    let mut controller = controller.lock();
    controller.prepare_route(irq)?;
    Ok(())
}

/// Enables or disables a prepared external source.
pub fn set_irq_enabled(irq: ArchIrq, enabled: bool) -> Result<(), IrqError> {
    let controller = EXTERNAL_CONTROLLER.get().ok_or(IrqError::Unsupported)?;
    let controller = controller.lock();
    if !controller.ioapics.contains_gsi(irq.raw()) {
        return Err(IrqError::InvalidNumber);
    }
    if enabled {
        controller.ioapics.unmask(irq.raw());
    } else {
        controller.ioapics.mask(irq.raw());
    }
    Ok(())
}
