//! Physical inter-processor notification mechanisms.
//!
//! A failed send guarantees that no notification was initiated, allowing common-layer retry.

use look_at::look_at;

use crate::power::PhysicalCpuId;

/// A physical IPI mechanism failure before submission.
///
/// Once the architecture submits a write it returns success and common acknowledgement decides
/// whether the request completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpiError {
    /// An unavailable platform transport.
    ///
    /// No physical notification was initiated.
    Unsupported,
    /// An unrepresentable physical destination.
    ///
    /// Destination validation precedes every hardware write.
    InvalidTarget,
    /// A transport rejection before submission.
    ///
    /// Firmware errors are retained as a diagnosable failure by the kernel protocol.
    Transport,
}

look_at! {
    @crate::arch::current::ipi:

    /// Enables the current CPU's reserved local IPI source.
    ///
    /// Common mailboxes and semantic handlers must be initialized before calling this function.
    pub fn init_percpu();

    /// Submits one notification to a firmware physical CPU identifier.
    ///
    /// Logical identifiers, pending reasons and acknowledgements remain in the kernel.
    pub fn send(target: PhysicalCpuId) -> Result<(), IpiError>;
}
