//! SBI physical IPI transport and local supervisor software source.
//!
//! Software notifications never use a PLIC source or an external-interrupt route.

use riscv::{
    interrupt::supervisor::{self, Interrupt},
    register::sip,
};

use crate::{ipi::IpiError, power::PhysicalCpuId};

/// Pure physical-target and firmware-error encoding.
///
/// These helpers also compile directly in host tests.
mod encoding;

use encoding::{hart_mask_parts, map_sbi_error};

/// Enables supervisor software delivery after installing common mailboxes.
///
/// Global interrupt enabling remains part of common per-CPU runtime initialization.
pub fn init_percpu() {
    // SAFETY: The common software handler is installed before this source is enabled.
    unsafe {
        sip::clear_ssoft();
        supervisor::enable_interrupt(Interrupt::SupervisorSoft);
    }
}

/// Sends one SBI notification to a physical hart identifier.
///
/// Rejected calls initiate no delivery and leave retry cleanup to the common publisher.
pub fn send(target: PhysicalCpuId) -> Result<(), IpiError> {
    let (mask, base) = hart_mask_parts(target)?;
    let result = sbi_rt::send_ipi(sbi_rt::HartMask::from_mask_base(mask, base));
    map_sbi_error(result.error)
}
