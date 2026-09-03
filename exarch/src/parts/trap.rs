//! Trap handling.

use crate_interface::def_interface;
use look_at::look_at;
use memory_addr::VirtAddr;
pub use page_table_entry::MappingFlags as PageFaultFlags;

mod handlers;
mod semantic;

pub use self::{handlers::*, semantic::*};

look_at! {
    @crate::arch::current::context:

    /// A target-specific saved trap frame.
    pub type TrapFrame: TrapFrameAccess;
}

/// The architecture-neutral accessors for a saved trap frame.
///
/// Kernel handlers use this trait rather than layout-specific fields so their control flow remains
/// independent of x86 and RISC-V frame representations.
pub trait TrapFrameAccess {
    /// Returns the saved instruction pointer.
    ///
    /// The returned address identifies the interrupted instruction or its architecture-adjusted
    /// continuation point.
    fn instruction_pointer(&self) -> VirtAddr;

    /// Replaces the saved instruction pointer.
    ///
    /// The new address becomes the continuation target when the architecture returns from the
    /// trap.
    fn set_instruction_pointer(&mut self, instruction_pointer: VirtAddr);

    /// Returns whether the trap interrupted user mode.
    ///
    /// This describes the saved privilege state rather than the kernel's current execution mode.
    fn is_user(&self) -> bool;
}

look_at! {
    @crate::arch::current::irq:

    /// A target-specific global interrupt source identifier.
    ///
    /// x86 aliases this type to a GSI and RISC-V aliases it to a PLIC source identifier. The
    /// distinct target-specific newtypes prevent local vectors and controller source identifiers
    /// from mixing.
    pub type GlobalIrq;
}

#[def_interface(gen_caller)]
pub trait KernelTrapIf {
    /// Dispatches one decoded trap to the kernel's handlers.
    ///
    /// When a trap happens, the architecture-specific code will decode the trap and call this
    /// function to dispatch it to the kernel, which is usually expected to use [`HandlerSlots`]
    /// to find the appropriate handler for the trap.
    ///
    /// The kernel may also choose to handle it in a different way, but it must return a
    /// [`TrapDisposition`] to indicate whether the trap was handled or not.
    fn handle(frame: &mut TrapFrame, trap: Trap) -> TrapDisposition {
        (_, _) = (frame, trap);
        TrapDisposition::Unhandled
    }
}

/// Semantic trap contract tests.
///
/// These tests describe the typed boundary before its architecture decoders are implemented.
#[cfg(test)]
mod tests {
    use super::{
        Exception, GlobalIrq, Interrupt, LocalInterrupt, RawTrap, Trap, TrapDisposition,
        should_mask_unhandled_local,
    };

    /// Verifies that semantic trap variants preserve their distinct domains.
    #[test]
    fn semantic_trap_preserves_distinct_domains() {
        let raw = RawTrap(0xfeed);
        let global = GlobalIrq::new(7);

        assert!(matches!(
            Trap::Exception(Exception::Unknown(raw)),
            Trap::Exception(Exception::Unknown(observed)) if observed == raw
        ));
        assert!(matches!(
            Trap::Interrupt(Interrupt::Local(LocalInterrupt::Timer)),
            Trap::Interrupt(Interrupt::Local(LocalInterrupt::Timer))
        ));
        assert!(matches!(
            Trap::Interrupt(Interrupt::Global(global)),
            Trap::Interrupt(Interrupt::Global(observed)) if observed == global
        ));
        assert!(matches!(
            Trap::Interrupt(Interrupt::Unknown(raw)),
            Trap::Interrupt(Interrupt::Unknown(observed)) if observed == raw
        ));
    }

    /// Verifies that trap disposition combination preserves an unhandled result.
    #[test]
    fn external_dispatch_preserves_unhandled_result_until_a_handler_runs() {
        assert_eq!(
            TrapDisposition::Unhandled + TrapDisposition::Unhandled,
            TrapDisposition::Unhandled
        );
        assert_eq!(
            TrapDisposition::Unhandled + TrapDisposition::Handled,
            TrapDisposition::Unhandled
        );
        assert_eq!(
            TrapDisposition::Handled + TrapDisposition::Handled,
            TrapDisposition::Handled
        );
    }

    /// Verifies that unhandled software interrupts require masking.
    #[test]
    fn unhandled_software_interrupt_requires_masking() {
        assert!(should_mask_unhandled_local(
            LocalInterrupt::Software,
            TrapDisposition::Unhandled
        ));
        assert!(!should_mask_unhandled_local(
            LocalInterrupt::Timer,
            TrapDisposition::Unhandled
        ));
        assert!(!should_mask_unhandled_local(
            LocalInterrupt::Software,
            TrapDisposition::Handled
        ));
    }
}
