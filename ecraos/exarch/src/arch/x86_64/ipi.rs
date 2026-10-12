//! x2APIC fixed-vector physical IPI delivery.
//!
//! Destination validation completes before the notification write is submitted.

use crate::{ipi::IpiError, power::PhysicalCpuId};

use super::imp::apic::{self, vectors::APIC_IPI_VECTOR};

/// Initializes the local IPI source after LAPIC and trap setup.
///
/// Fixed-delivery vectors require no separate local source-enable register.
pub fn init_percpu() {}

/// Encodes a firmware APIC ID into the x2APIC destination field.
///
/// Wider identifiers and the all-ones broadcast destination are rejected before submission.
fn destination(target: PhysicalCpuId) -> Result<u32, IpiError> {
    let target = u32::try_from(target).map_err(|_| IpiError::InvalidTarget)?;
    if target == u32::MAX {
        return Err(IpiError::InvalidTarget);
    }
    Ok(target)
}

/// Sends the reserved fixed-delivery vector to one physical APIC ID.
///
/// After writing the ICR this returns success and the common deadline determines completion.
pub fn send(target: PhysicalCpuId) -> Result<(), IpiError> {
    let destination = destination(target)?;
    // Local IRQ exclusion prevents migration and a nested LAPIC borrow during the ICR write.
    // The exclusion ends before any common-layer acknowledgement wait.
    x86_64::instructions::interrupts::without_interrupts(|| {
        // x2APIC WRMSR is nonserializing. MFENCE makes mailbox stores visible and LFENCE
        // prevents the notification write from overtaking that publication. This assembly
        // also supplies a compiler memory barrier and must not use `nomem`.
        unsafe {
            core::arch::asm!("mfence", "lfence", options(nostack));
        }
        apic::with_local_apic(|lapic| unsafe { lapic.send_ipi(APIC_IPI_VECTOR, destination) });
    });
    Ok(())
}

/// Physical APIC destination validation tests.
///
/// These checks execute without accessing the local APIC or privileged registers.
#[cfg(test)]
mod tests {
    use crate::ipi::IpiError;

    use super::destination;

    /// Rejects destinations wider than the hardware field without truncation.
    ///
    /// Valid extended x2APIC IDs retain all thirty-two destination bits.
    #[test]
    fn rejects_unrepresentable_destination() {
        assert_eq!(destination(0x1234), Ok(0x1234));
        assert_eq!(destination(u32::MAX as usize - 1), Ok(u32::MAX - 1));
        assert_eq!(destination(u32::MAX as usize), Err(IpiError::InvalidTarget));
        assert_eq!(
            destination(u32::MAX as usize + 1),
            Err(IpiError::InvalidTarget)
        );
    }
}
