//! Trap handling.

use core::{
    mem, ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

use crate_interface::def_interface;
use memory_addr::VirtAddr;

pub use crate::TrapFrame;
#[cfg(target_arch = "riscv64")]
pub use crate::arch::riscv64::irq::ArchGlobalIrq;
#[cfg(target_arch = "x86_64")]
pub use crate::arch::x86_64::irq::ArchGlobalIrq;
pub use page_table_entry::MappingFlags as PageFaultFlags;

/// A raw architecture trap identifier.
///
/// x86 stores an IDT vector and RISC-V stores the complete `scause` bit pattern. This type is
/// internal to an architecture decoder and is never a global interrupt source identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawTrap(pub usize);

/// A target-specific global interrupt source identifier.
///
/// x86 aliases this type to a GSI and RISC-V aliases it to a PLIC source identifier. The distinct
/// target-specific newtypes prevent local vectors and controller source identifiers from mixing.
pub type GlobalIrq = ArchGlobalIrq;

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

/// A decoded trap delivered to the kernel.
///
/// All variants except `UnknownInterrupt` have exactly one registered kernel handler. Architecture
/// code completes unknown interrupt paths after rate-limited logging without calling the kernel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SemanticTrap {
    /// A synchronous processor exception.
    ///
    /// This is dispatched to the exception handler.
    Exception(Exception),
    /// A CPU-local interrupt.
    ///
    /// This is dispatched to the local-interrupt handler.
    LocalInterrupt(LocalInterrupt),
    /// A controller-backed global interrupt source.
    ///
    /// This is dispatched to the kernel's GlobalIrq source table.
    GlobalIrq(GlobalIrq),
    /// An unknown asynchronous interrupt.
    ///
    /// Architecture code logs, masks when possible, and acknowledges this interrupt without a
    /// kernel callback.
    UnknownInterrupt(RawTrap),
}

/// The kernel's non-fatal disposition for a decoded trap.
///
/// Fatal handlers panic directly, which diverges and therefore needs no separate disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapDisposition {
    /// The kernel handled the semantic trap.
    ///
    /// The architecture adapter may complete the corresponding hardware delivery normally.
    Handled,
    /// The kernel did not handle the semantic trap.
    ///
    /// An architecture adapter masks a global source before hardware completion when possible.
    Unhandled,
}

/// Combines two semantic dispatch outcomes.
///
/// A batch is handled only when every source in it is handled.
#[cfg_attr(not(target_arch = "riscv64"), allow(dead_code))]
pub(crate) const fn merge_dispositions(
    current: TrapDisposition,
    next: TrapDisposition,
) -> TrapDisposition {
    match (current, next) {
        (TrapDisposition::Handled, TrapDisposition::Handled) => TrapDisposition::Handled,
        _ => TrapDisposition::Unhandled,
    }
}

/// Reports whether an unhandled local source requires architecture masking.
///
/// Software interrupts have no completion operation, so an unhandled one must be disabled before
/// returning from the trap handler.
#[cfg_attr(not(target_arch = "riscv64"), allow(dead_code))]
pub(crate) const fn should_mask_unhandled_local(
    interrupt: LocalInterrupt,
    disposition: TrapDisposition,
) -> bool {
    match (interrupt, disposition) {
        (LocalInterrupt::Software, TrapDisposition::Unhandled) => true,
        _ => false,
    }
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

/// A kernel exception handler.
///
/// The handler may update the saved frame before returning and panics directly for fatal
/// exceptions.
pub type ExceptionHandler = fn(&mut TrapFrame, Exception) -> TrapDisposition;

/// A kernel CPU-local interrupt handler.
///
/// The handler dispatches timer and future software interrupts without controller source lookup.
pub type LocalInterruptHandler = fn(&mut TrapFrame, LocalInterrupt) -> TrapDisposition;

/// A kernel global interrupt handler.
///
/// The handler maps a typed controller source to the kernel-owned device handler table.
pub type GlobalIrqHandler = fn(&mut TrapFrame, GlobalIrq) -> TrapDisposition;

/// A semantic kernel handler registration.
///
/// Each variant selects exactly one process-wide handler slot for its semantic trap class.
#[derive(Debug, Clone, Copy)]
pub enum Handler {
    /// The exception handler registration.
    ///
    /// This slot handles every [`Exception`] value.
    Exception(ExceptionHandler),
    /// The CPU-local interrupt handler registration.
    ///
    /// This slot handles every [`LocalInterrupt`] value.
    LocalInterrupt(LocalInterruptHandler),
    /// The global interrupt handler registration.
    ///
    /// This slot handles every target-specific [`GlobalIrq`] value.
    GlobalIrq(GlobalIrqHandler),
}

/// A semantic handler registration failure.
///
/// Handler registration is immutable after the first successful installation for each class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterError {
    /// The semantic class already has a registered handler.
    ///
    /// The original handler remains installed.
    AlreadyRegistered,
}

/// Three immutable semantic handler slots.
///
/// A kernel owns an instance of this table and exposes it to architecture trap entry through the
/// [`TrapHandler`] crate interface. Dispatch is allocation-free and lock-free.
pub struct HandlerSlots {
    exception: AtomicPtr<()>,
    local_interrupt: AtomicPtr<()>,
    global_irq: AtomicPtr<()>,
}

impl HandlerSlots {
    /// Creates empty semantic handler slots.
    ///
    /// The kernel must install all three handlers before enabling interrupt delivery.
    pub const fn new() -> Self {
        Self {
            exception: AtomicPtr::new(ptr::null_mut()),
            local_interrupt: AtomicPtr::new(ptr::null_mut()),
            global_irq: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// Registers one semantic handler exactly once.
    ///
    /// Registration publishes the function pointer with release ordering so a later architecture
    /// dispatch observes all handler initialization that precedes this call.
    pub fn register(&self, handler: Handler) -> Result<(), RegisterError> {
        // TODO: Support unregistering and re-registering semantic trap handlers.
        let (slot, pointer) = match handler {
            Handler::Exception(handler) => (&self.exception, handler as *mut ()),
            Handler::LocalInterrupt(handler) => (&self.local_interrupt, handler as *mut ()),
            Handler::GlobalIrq(handler) => (&self.global_irq, handler as *mut ()),
        };
        slot.compare_exchange(
            ptr::null_mut(),
            pointer,
            Ordering::Release,
            Ordering::Acquire,
        )
        .map(|_| ())
        .map_err(|_| RegisterError::AlreadyRegistered)
    }

    /// Dispatches a decoded trap to its semantic handler.
    ///
    /// Missing handlers and unknown interrupts return [`TrapDisposition::Unhandled`]. Fatal
    /// semantic handlers panic directly and therefore never return a disposition.
    pub fn dispatch(&self, frame: &mut TrapFrame, trap: SemanticTrap) -> TrapDisposition {
        match trap {
            SemanticTrap::Exception(exception) => {
                let pointer = self.exception.load(Ordering::Acquire);
                if pointer.is_null() {
                    return TrapDisposition::Unhandled;
                }
                // SAFETY: `register` stores only an `ExceptionHandler` in this slot and never
                // removes or replaces it.
                let handler = unsafe { mem::transmute::<*mut (), ExceptionHandler>(pointer) };
                handler(frame, exception)
            }
            SemanticTrap::LocalInterrupt(interrupt) => {
                let pointer = self.local_interrupt.load(Ordering::Acquire);
                if pointer.is_null() {
                    return TrapDisposition::Unhandled;
                }
                // SAFETY: `register` stores only a `LocalInterruptHandler` in this slot and never
                // removes or replaces it.
                let handler = unsafe { mem::transmute::<*mut (), LocalInterruptHandler>(pointer) };
                handler(frame, interrupt)
            }
            SemanticTrap::GlobalIrq(irq) => {
                let pointer = self.global_irq.load(Ordering::Acquire);
                if pointer.is_null() {
                    return TrapDisposition::Unhandled;
                }
                // SAFETY: `register` stores only a `GlobalIrqHandler` in this slot and never removes
                // or replaces it.
                let handler = unsafe { mem::transmute::<*mut (), GlobalIrqHandler>(pointer) };
                handler(frame, irq)
            }
            SemanticTrap::UnknownInterrupt(_) => TrapDisposition::Unhandled,
        }
    }
}

impl Default for HandlerSlots {
    fn default() -> Self {
        Self::new()
    }
}

#[def_interface(gen_caller)]
pub trait TrapHandler {
    /// Dispatches one architecture-decoded semantic trap.
    ///
    /// The architecture adapter owns raw decoding and hardware completion. A fatal kernel policy
    /// panics from its registered handler instead of returning a separate disposition.
    fn handle(frame: &mut TrapFrame, trap: SemanticTrap) -> TrapDisposition {
        (_, _) = (frame, trap);
        TrapDisposition::Unhandled
    }
}

/// Semantic trap contract tests.
///
/// These tests describe the typed boundary before its architecture decoders are implemented.
#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        Exception, GlobalIrq, Handler, HandlerSlots, LocalInterrupt, RawTrap, RegisterError,
        SemanticTrap, TrapDisposition, TrapFrame, merge_dispositions, should_mask_unhandled_local,
    };

    /// The number of exception handler calls made by the slot tests.
    static EXCEPTION_CALLS: AtomicUsize = AtomicUsize::new(0);

    /// The number of local-interrupt handler calls made by the slot tests.
    static LOCAL_INTERRUPT_CALLS: AtomicUsize = AtomicUsize::new(0);

    /// The number of global-IRQ handler calls made by the slot tests.
    static GLOBAL_IRQ_CALLS: AtomicUsize = AtomicUsize::new(0);

    /// Records one exception-handler invocation.
    fn handle_exception(_frame: &mut TrapFrame, _exception: Exception) -> TrapDisposition {
        EXCEPTION_CALLS.fetch_add(1, Ordering::Relaxed);
        TrapDisposition::Handled
    }

    /// Records one local-interrupt-handler invocation.
    fn handle_local_interrupt(
        _frame: &mut TrapFrame,
        _interrupt: LocalInterrupt,
    ) -> TrapDisposition {
        LOCAL_INTERRUPT_CALLS.fetch_add(1, Ordering::Relaxed);
        TrapDisposition::Handled
    }

    /// Records one global-IRQ-handler invocation.
    fn handle_global_irq(_frame: &mut TrapFrame, _irq: GlobalIrq) -> TrapDisposition {
        GLOBAL_IRQ_CALLS.fetch_add(1, Ordering::Relaxed);
        TrapDisposition::Handled
    }

    /// Verifies that semantic trap variants preserve their distinct domains.
    #[test]
    fn semantic_trap_preserves_distinct_domains() {
        let raw = RawTrap(0xfeed);
        let global = GlobalIrq::new(7);

        assert!(matches!(
            SemanticTrap::Exception(Exception::Unknown(raw)),
            SemanticTrap::Exception(Exception::Unknown(observed)) if observed == raw
        ));
        assert!(matches!(
            SemanticTrap::LocalInterrupt(LocalInterrupt::Timer),
            SemanticTrap::LocalInterrupt(LocalInterrupt::Timer)
        ));
        assert!(matches!(
            SemanticTrap::GlobalIrq(global),
            SemanticTrap::GlobalIrq(observed) if observed == global
        ));
        assert!(matches!(
            SemanticTrap::UnknownInterrupt(raw),
            SemanticTrap::UnknownInterrupt(observed) if observed == raw
        ));
    }

    #[test]
    fn external_dispatch_preserves_unhandled_result_until_a_handler_runs() {
        assert_eq!(
            merge_dispositions(TrapDisposition::Unhandled, TrapDisposition::Unhandled),
            TrapDisposition::Unhandled
        );
        assert_eq!(
            merge_dispositions(TrapDisposition::Unhandled, TrapDisposition::Handled),
            TrapDisposition::Unhandled
        );
        assert_eq!(
            merge_dispositions(TrapDisposition::Handled, TrapDisposition::Handled),
            TrapDisposition::Handled
        );
    }

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

    /// Verifies that each semantic class owns exactly one immutable handler slot.
    #[test]
    fn handler_slots_register_once_and_dispatch_by_semantic_class() {
        EXCEPTION_CALLS.store(0, Ordering::Relaxed);
        LOCAL_INTERRUPT_CALLS.store(0, Ordering::Relaxed);
        GLOBAL_IRQ_CALLS.store(0, Ordering::Relaxed);
        let slots = HandlerSlots::new();
        let mut frame = TrapFrame::default();

        assert_eq!(slots.register(Handler::Exception(handle_exception)), Ok(()));
        assert_eq!(
            slots.register(Handler::Exception(handle_exception)),
            Err(RegisterError::AlreadyRegistered)
        );
        assert_eq!(
            slots.register(Handler::LocalInterrupt(handle_local_interrupt)),
            Ok(())
        );
        assert_eq!(
            slots.register(Handler::GlobalIrq(handle_global_irq)),
            Ok(())
        );

        assert_eq!(
            slots.dispatch(
                &mut frame,
                SemanticTrap::Exception(Exception::Unknown(RawTrap(1)))
            ),
            TrapDisposition::Handled
        );
        assert_eq!(
            slots.dispatch(
                &mut frame,
                SemanticTrap::LocalInterrupt(LocalInterrupt::Timer)
            ),
            TrapDisposition::Handled
        );
        assert_eq!(
            slots.dispatch(&mut frame, SemanticTrap::GlobalIrq(GlobalIrq::new(2))),
            TrapDisposition::Handled
        );
        assert_eq!(EXCEPTION_CALLS.load(Ordering::Relaxed), 1);
        assert_eq!(LOCAL_INTERRUPT_CALLS.load(Ordering::Relaxed), 1);
        assert_eq!(GLOBAL_IRQ_CALLS.load(Ordering::Relaxed), 1);
    }
}
