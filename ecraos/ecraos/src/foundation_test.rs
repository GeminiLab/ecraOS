//! Deterministic hardware foundation test operations.
//!
//! Dedicated smoke applications use semantic operations and counters without controller access.

use alloc::vec::Vec;
use core::time::Duration;

use crate::{ipi, mp::LogicalCpuId};

pub use ipi::{IpiDiagnostic, IpiError};

/// Returns the stable online and IPI-ready logical CPU snapshot.
///
/// CPU inventory ownership remains with MP and no test maintains a second target list.
pub fn ready_cpus() -> Vec<LogicalCpuId> {
    crate::mp::ipi_ready_cpu_ids()
}

/// Performs an exact-target Test round trip and returns its completed generation.
///
/// Self delivery executes directly while remote delivery uses the physical IPI mechanism.
pub fn test_roundtrip(target: LogicalCpuId, timeout: Duration) -> Result<u64, IpiError> {
    ipi::test_roundtrip(target, false, timeout)
}

/// Publishes Test and Reschedule together before one physical notification.
///
/// Both reasons survive the same mailbox drain and retain their independent semantics.
pub fn combined_roundtrip(target: LogicalCpuId, timeout: Duration) -> Result<u64, IpiError> {
    ipi::test_roundtrip(target, true, timeout)
}

/// Reads executed Test, consumed Reschedule and acknowledged Test generation counters.
///
/// Acquire observations allow applications to use counters as deterministic completion evidence.
pub fn counters(target: LogicalCpuId) -> (u64, u64, u64) {
    ipi::test_counters(target)
}

/// Suppresses exactly the next Test acknowledgement on one CPU.
///
/// The Test handler executes normally, but the sender must observe a bounded timeout.
pub fn suppress_test_ack(target: LogicalCpuId) {
    ipi::suppress_next_ack(target, ipi::IpiReason::Test, true);
}

/// Clears any unused Test acknowledgement injection.
///
/// The following generation uses the normal production completion path.
pub fn clear_test_injection(target: LogicalCpuId) {
    ipi::suppress_next_ack(target, ipi::IpiReason::Test, false);
}
