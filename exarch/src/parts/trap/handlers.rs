use core::{
    mem,
    ops::{Add, AddAssign},
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

use super::{
    GlobalIrq, TrapFrame,
    semantic::{Exception, Interrupt, LocalInterrupt, Trap},
};

/// The kernel's non-fatal disposition for a decoded trap.
///
/// Fatal handlers panic directly, which diverges and therefore needs no separate disposition.
///
/// Two dispositions can be combined with `+` to produce a single outcome for a batch of traps. The
/// batch is handled only when every trap in it is handled.
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

impl Add for TrapDisposition {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        match (self, rhs) {
            (TrapDisposition::Handled, TrapDisposition::Handled) => TrapDisposition::Handled,
            _ => TrapDisposition::Unhandled,
        }
    }
}

impl AddAssign for TrapDisposition {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
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

/// A classification of kernel trap handlers.
///
/// Each variant identifies one atomic slot in [`HandlerSlots`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapHandlerKind {
    /// Selects the handler for [`Trap::Exception`]s.
    ///
    /// This variant maps to the exception slot.
    Exception,
    /// Selects the handler for [`Trap::Interrupt`]s with [`Interrupt::Local`] sources.
    ///
    /// This variant maps to the local-interrupt slot.
    LocalInterrupt,
    /// Selects the handler for [`Trap::Interrupt`]s with [`Interrupt::Global`] sources.
    ///
    /// This variant maps to the global-IRQ slot.
    GlobalIrq,
}

/// Private sealing support for [`TrapHandler`].
pub(crate) mod sealed {
    /// Prevents external implementations of [`super::TrapHandler`].
    ///
    /// Only the architecture-defined handler types implement this marker.
    pub trait Sealed {}
}

/// A kernel trap handler of a specific semantic class.
///
/// # Safety
///
/// Implementers must ensure that the signature of the handler matches the semantic class.
pub unsafe trait TrapHandler: sealed::Sealed + Copy {
    /// Converts the handler into a raw pointer and its semantic class.
    ///
    /// The returned class must identify the slot whose dispatch signature matches the pointer.
    fn into_raw(self) -> (TrapHandlerKind, *const ());
}

impl sealed::Sealed for ExceptionHandler {}
impl sealed::Sealed for LocalInterruptHandler {}
impl sealed::Sealed for GlobalIrqHandler {}

// SAFETY: This implementation returns the exception kind and its matching function-pointer type.
unsafe impl TrapHandler for ExceptionHandler {
    fn into_raw(self) -> (TrapHandlerKind, *const ()) {
        (TrapHandlerKind::Exception, self as *const ())
    }
}

// SAFETY: This returns the local-interrupt kind and its matching function-pointer type.
unsafe impl TrapHandler for LocalInterruptHandler {
    fn into_raw(self) -> (TrapHandlerKind, *const ()) {
        (TrapHandlerKind::LocalInterrupt, self as *const ())
    }
}

// SAFETY: This implementation returns the global-IRQ kind and its matching function-pointer type.
unsafe impl TrapHandler for GlobalIrqHandler {
    fn into_raw(self) -> (TrapHandlerKind, *const ()) {
        (TrapHandlerKind::GlobalIrq, self as *const ())
    }
}

/// Three semantic handler slots.
///
/// A kernel owns an instance of this table and exposes it to architecture trap entry through the
/// [`KernelTrapIf`](super::KernelTrapIf) crate interface. Dispatch is allocation-free and
/// lock-free.
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

    /// Locates the atomic pointer for the handler slot of a semantic class.
    ///
    /// The returned slot is shared by registration, unregistration, and dispatch for that class.
    fn kind_to_slot(&self, kind: TrapHandlerKind) -> &AtomicPtr<()> {
        match kind {
            TrapHandlerKind::Exception => &self.exception,
            TrapHandlerKind::LocalInterrupt => &self.local_interrupt,
            TrapHandlerKind::GlobalIrq => &self.global_irq,
        }
    }

    /// Registers or replaces one semantic handler.
    ///
    /// Registration publishes the function pointer with release ordering so a later architecture
    /// dispatch observes all handler initialization that precedes this call.
    ///
    /// # Synchronization
    ///
    /// Registration has weak synchronization with dispatch. A dispatch that loads the old pointer
    /// before this store may still call the old handler after registration returns. A dispatch that
    /// loads the new pointer observes the new handler and the writes sequenced before registration.
    /// Registration does not wait for in-flight dispatches to quiesce.
    pub fn register<H: TrapHandler>(&self, handler: H) {
        let (kind, pointer) = handler.into_raw();
        self.kind_to_slot(kind)
            .store(pointer as _, Ordering::Release);
    }

    /// Unregisters one semantic handler.
    ///
    /// Like [`Self::register`], this function uses release ordering. A dispatch that already loaded
    /// the handler pointer may still call it after this function returns, while later dispatches
    /// that observe the cleared slot return [`TrapDisposition::Unhandled`]. This function does not
    /// wait for in-flight dispatches to quiesce.
    pub fn unregister(&self, kind: TrapHandlerKind) {
        self.kind_to_slot(kind)
            .store(ptr::null_mut(), Ordering::Release);
    }

    /// Dispatches a decoded trap to its semantic handler.
    ///
    /// Missing handlers and unknown interrupts return [`TrapDisposition::Unhandled`]. Fatal
    /// semantic handlers panic directly and therefore never return a disposition. Like
    /// [`Self::register`] and [`Self::unregister`], dispatch uses weak synchronization: it may call
    /// a handler pointer loaded before a concurrent slot update, but an acquire load observes the
    /// initialization published before a handler registration.
    pub fn dispatch(&self, frame: &mut TrapFrame, trap: Trap) -> TrapDisposition {
        macro_rules! load_handler {
            ($slot:expr, $ty:ty) => {{
                let pointer = $slot.load(Ordering::Acquire);
                if pointer.is_null() {
                    return TrapDisposition::Unhandled;
                }
                // SAFETY: The corresponding `register` implementation stores only a `$ty` in
                // this slot, and the slot is cleared or replaced only with the same type.
                let handler = unsafe { mem::transmute::<*mut (), $ty>(pointer) };
                handler
            }};
        }

        match trap {
            Trap::Exception(exception) => {
                load_handler!(self.exception, ExceptionHandler)(frame, exception)
            }
            Trap::Interrupt(Interrupt::Local(interrupt)) => {
                load_handler!(self.local_interrupt, LocalInterruptHandler)(frame, interrupt)
            }
            Trap::Interrupt(Interrupt::Global(irq)) => {
                load_handler!(self.global_irq, GlobalIrqHandler)(frame, irq)
            }
            Trap::Interrupt(Interrupt::Unknown(_)) => TrapDisposition::Unhandled,
        }
    }
}

impl Default for HandlerSlots {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        super::{Exception, GlobalIrq, Interrupt, LocalInterrupt, RawTrap, TrapFrame},
        ExceptionHandler, GlobalIrqHandler, HandlerSlots, LocalInterruptHandler, Trap,
        TrapDisposition, TrapHandlerKind,
    };

    /// Counts registration-test exception-handler invocations.
    static REGISTRATION_EXCEPTION_CALLS: AtomicUsize = AtomicUsize::new(0);

    /// Counts registration-test local-interrupt handler invocations.
    static REGISTRATION_LOCAL_CALLS: AtomicUsize = AtomicUsize::new(0);

    /// Counts local-interrupt calls made by the replacement test's original handler.
    static ORIGINAL_LOCAL_CALLS: AtomicUsize = AtomicUsize::new(0);

    /// Counts replacement-handler invocations.
    static REPLACEMENT_CALLS: AtomicUsize = AtomicUsize::new(0);

    /// Counts registration-test global-IRQ handler invocations.
    static REGISTRATION_GLOBAL_CALLS: AtomicUsize = AtomicUsize::new(0);

    /// Counts lifecycle-test exception-handler invocations.
    static LIFECYCLE_EXCEPTION_CALLS: AtomicUsize = AtomicUsize::new(0);

    /// Counts empty-slot-test global-IRQ handler invocations.
    static EMPTY_SLOT_GLOBAL_CALLS: AtomicUsize = AtomicUsize::new(0);

    /// Records one exception-handler invocation.
    fn handle_exception(_frame: &mut TrapFrame, _exception: Exception) -> TrapDisposition {
        REGISTRATION_EXCEPTION_CALLS.fetch_add(1, Ordering::Relaxed);
        TrapDisposition::Handled
    }

    /// Records one local-interrupt handler invocation.
    fn handle_local_interrupt(
        _frame: &mut TrapFrame,
        _interrupt: LocalInterrupt,
    ) -> TrapDisposition {
        REGISTRATION_LOCAL_CALLS.fetch_add(1, Ordering::Relaxed);
        TrapDisposition::Handled
    }

    /// Records one replacement local-interrupt handler invocation.
    fn replacement_local_interrupt(
        _frame: &mut TrapFrame,
        _interrupt: LocalInterrupt,
    ) -> TrapDisposition {
        REPLACEMENT_CALLS.fetch_add(1, Ordering::Relaxed);
        TrapDisposition::Handled
    }

    /// Records one local-interrupt call from the replacement test's original handler.
    fn original_local_interrupt(
        _frame: &mut TrapFrame,
        _interrupt: LocalInterrupt,
    ) -> TrapDisposition {
        ORIGINAL_LOCAL_CALLS.fetch_add(1, Ordering::Relaxed);
        TrapDisposition::Handled
    }

    /// Records one global-IRQ handler invocation.
    fn handle_global_irq(_frame: &mut TrapFrame, _irq: GlobalIrq) -> TrapDisposition {
        REGISTRATION_GLOBAL_CALLS.fetch_add(1, Ordering::Relaxed);
        TrapDisposition::Handled
    }

    /// Records one lifecycle-test exception-handler invocation.
    fn lifecycle_exception(_frame: &mut TrapFrame, _exception: Exception) -> TrapDisposition {
        LIFECYCLE_EXCEPTION_CALLS.fetch_add(1, Ordering::Relaxed);
        TrapDisposition::Handled
    }

    /// Records one empty-slot-test global-IRQ handler invocation.
    fn empty_slot_global(_frame: &mut TrapFrame, _irq: GlobalIrq) -> TrapDisposition {
        EMPTY_SLOT_GLOBAL_CALLS.fetch_add(1, Ordering::Relaxed);
        TrapDisposition::Handled
    }

    /// Verifies registration and dispatch for each semantic handler class.
    #[test]
    fn registers_and_dispatches_each_semantic_class() {
        REGISTRATION_EXCEPTION_CALLS.store(0, Ordering::Relaxed);
        REGISTRATION_LOCAL_CALLS.store(0, Ordering::Relaxed);
        REGISTRATION_GLOBAL_CALLS.store(0, Ordering::Relaxed);
        let slots = HandlerSlots::new();
        let mut frame = TrapFrame::default();

        slots.register::<ExceptionHandler>(handle_exception);
        slots.register::<LocalInterruptHandler>(handle_local_interrupt);
        slots.register::<GlobalIrqHandler>(handle_global_irq);

        assert_eq!(
            slots.dispatch(&mut frame, Trap::Exception(Exception::Unknown(RawTrap(1)))),
            TrapDisposition::Handled
        );
        assert_eq!(
            slots.dispatch(
                &mut frame,
                Trap::Interrupt(Interrupt::Local(LocalInterrupt::Timer)),
            ),
            TrapDisposition::Handled
        );
        assert_eq!(
            slots.dispatch(
                &mut frame,
                Trap::Interrupt(Interrupt::Global(GlobalIrq::new(2))),
            ),
            TrapDisposition::Handled
        );
        assert_eq!(REGISTRATION_EXCEPTION_CALLS.load(Ordering::Relaxed), 1);
        assert_eq!(REGISTRATION_LOCAL_CALLS.load(Ordering::Relaxed), 1);
        assert_eq!(REGISTRATION_GLOBAL_CALLS.load(Ordering::Relaxed), 1);
    }

    /// Verifies that unregistering permits a later registration for the same class.
    #[test]
    fn unregister_clears_handler_and_allows_re_registration() {
        LIFECYCLE_EXCEPTION_CALLS.store(0, Ordering::Relaxed);
        let slots = HandlerSlots::new();
        let mut frame = TrapFrame::default();
        let trap = Trap::Exception(Exception::Unknown(RawTrap(1)));

        slots.register::<ExceptionHandler>(lifecycle_exception);
        assert_eq!(slots.dispatch(&mut frame, trap), TrapDisposition::Handled);
        slots.unregister(TrapHandlerKind::Exception);
        assert_eq!(slots.dispatch(&mut frame, trap), TrapDisposition::Unhandled);

        slots.register::<ExceptionHandler>(lifecycle_exception);
        assert_eq!(slots.dispatch(&mut frame, trap), TrapDisposition::Handled);
        assert_eq!(LIFECYCLE_EXCEPTION_CALLS.load(Ordering::Relaxed), 2);
    }

    /// Verifies that clearing an empty slot leaves other semantic classes unchanged.
    #[test]
    fn unregistering_empty_slot_does_not_affect_other_classes() {
        EMPTY_SLOT_GLOBAL_CALLS.store(0, Ordering::Relaxed);
        let slots = HandlerSlots::new();
        let mut frame = TrapFrame::default();

        slots.unregister(TrapHandlerKind::Exception);
        slots.register::<GlobalIrqHandler>(empty_slot_global);
        slots.unregister(TrapHandlerKind::Exception);
        assert_eq!(
            slots.dispatch(
                &mut frame,
                Trap::Interrupt(Interrupt::Global(GlobalIrq::new(2))),
            ),
            TrapDisposition::Handled
        );
        assert_eq!(EMPTY_SLOT_GLOBAL_CALLS.load(Ordering::Relaxed), 1);
    }

    /// Verifies that registration replaces the current handler for a class.
    #[test]
    fn re_registration_replaces_handler_for_same_class() {
        ORIGINAL_LOCAL_CALLS.store(0, Ordering::Relaxed);
        REPLACEMENT_CALLS.store(0, Ordering::Relaxed);
        let slots = HandlerSlots::new();
        let mut frame = TrapFrame::default();
        let trap = Trap::Interrupt(Interrupt::Local(LocalInterrupt::Timer));

        slots.register::<LocalInterruptHandler>(original_local_interrupt);
        slots.register::<LocalInterruptHandler>(replacement_local_interrupt);
        assert_eq!(slots.dispatch(&mut frame, trap), TrapDisposition::Handled);
        assert_eq!(REPLACEMENT_CALLS.load(Ordering::Relaxed), 1);
        assert_eq!(ORIGINAL_LOCAL_CALLS.load(Ordering::Relaxed), 0);
    }
}
