//! Semantic trap (interrupts and exceptions) types.

use memory_addr::VirtAddr;
pub use page_table_entry::MappingFlags as PageFaultFlags;

use super::GlobalIrq;

/// A raw architecture trap identifier.
///
/// x86 stores an IDT vector and RISC-V stores the complete `scause` bit pattern. This type is
/// internal to an architecture decoder and is never a global interrupt source identifier (which
/// is represented by [`GlobalIrq`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawTrap(pub usize);

/// A decoded trap delivered to the kernel.
///
/// All variants except `UnknownInterrupt` have exactly one registered kernel handler. Architecture
/// code completes unknown interrupt paths after rate-limited logging without calling the kernel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trap {
    /// A synchronous processor exception.
    ///
    /// This is dispatched to the exception handler.
    Exception(Exception),
    /// An asynchronous processor interrupt.
    ///
    /// This is dispatched to the interrupt handler.
    Interrupt(Interrupt),
}

/// A synchronous processor exception.
///
/// Architecture decoders provide all information the kernel needs to decide whether the exception
/// is recoverable, so kernel handlers do not inspect raw registers such as CR2 or `stval`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Exception {
    /// A page fault with normalized access information.
    ///
    /// The address, requested access flags, and saved privilege state describe the faulting memory
    /// access independently of the architecture's raw error representation.
    PageFault {
        /// The virtual address whose access faulted.
        ///
        /// This is CR2 on x86 and `stval` on RISC-V.
        address: VirtAddr,
        /// The normalized requested mapping permissions.
        ///
        /// The flags include user access when the trap interrupted user mode.
        flags: PageFaultFlags,
        /// Whether the trap interrupted user mode.
        ///
        /// This remains explicit for fault policies that distinguish kernel and user recovery.
        is_user: bool,
    },
    /// A breakpoint exception.
    ///
    /// The architecture decoder has already advanced the saved instruction pointer when required.
    Breakpoint,
    /// An invalid-instruction exception.
    ///
    /// The kernel normally treats this as fatal until it implements emulation or userspace signal
    /// delivery.
    InvalidInstruction,
    /// A general-protection exception.
    ///
    /// The raw architecture error code is retained because its bit interpretation is x86-specific.
    GeneralProtection {
        /// The architecture-provided protection-fault error code.
        ///
        /// This bit pattern remains available for kernel diagnostics.
        error_code: usize,
    },
    /// An otherwise unclassified synchronous exception.
    ///
    /// This variant remains in the exception domain so the registered exception handler can make
    /// its fatal policy explicit rather than returning to the same unhandled fault.
    Unknown(RawTrap),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Interrupt {
    /// A CPU-local interrupt.
    ///
    /// This is dispatched to the local-interrupt handler.
    Local(LocalInterrupt),
    /// A controller-backed global interrupt source.
    ///
    /// This is dispatched to the kernel's GlobalIrq source table.
    Global(GlobalIrq),
    /// An otherwise unclassified asynchronous interrupt.
    ///
    /// Architecture code logs, masks when possible, and acknowledges this interrupt without a
    /// kernel callback.
    Unknown(RawTrap),
}

/// A CPU-local interrupt class.
///
/// These interrupts do not name a device-controller source and are delivered directly to a CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalInterrupt {
    /// The local timer interrupt.
    ///
    /// This interrupt advances the per-CPU kernel timer schedule.
    Timer,
    /// The local software interrupt.
    ///
    /// This interrupt is reserved for an architecture's future IPI implementation.
    Software,
}
