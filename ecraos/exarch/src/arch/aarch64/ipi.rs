//! Physical-affinity SGI delivery for the EL1 GICv3 backend.
//!
//! The controller mechanism selects local redistributors while common reasons remain in ecraOS.

use crate::{ipi::IpiError, power::PhysicalCpuId};

use super::gicv3;

/// Enables the common software source for the current PE.
///
/// The local redistributor and kernel mailbox consumer must already be initialized.
pub fn init_percpu() {
    gicv3::enable_ipi();
}

/// Submits one physical-affinity SGI notification.
///
/// Unsupported Aff0 values fail before any system-register write.
pub fn send(target: PhysicalCpuId) -> Result<(), IpiError> {
    gicv3::send_ipi(target)
}
