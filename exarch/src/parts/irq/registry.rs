use alloc::{boxed::Box, vec::Vec};
use core::{
    mem,
    sync::atomic::{AtomicUsize, Ordering},
};
use kspin::SpinNoIrq;

use super::{GlobalIrq, IrqError, IrqHandler};

const ENABLED: usize = 1;
const ACTIVE_STEP: usize = 2;
const ACTIVE_MASK: usize = !ENABLED;

/// Explains why a common external dispatch did not invoke a handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnhandledReason {
    /// Reports delivery with no registered handler.
    Unregistered,
    /// Reports delivery while the source is logically disabled.
    Disabled,
}

struct Slot {
    valid: bool,
    handler: AtomicUsize,
    state: AtomicUsize,
    unhandled: AtomicUsize,
    disabled: AtomicUsize,
}

impl Slot {
    fn new(valid: bool) -> Self {
        Self {
            valid,
            handler: AtomicUsize::new(0),
            state: AtomicUsize::new(0),
            unhandled: AtomicUsize::new(0),
            disabled: AtomicUsize::new(0),
        }
    }
}

/// Stores the architecture-independent external IRQ lifecycle state.
///
/// The slot array is allocated once from the discovered source domain. Dispatch performs only
/// atomic operations and never allocates or takes the control-path lock.
pub struct Registry {
    slots: Box<[Slot]>,
    control_lock: SpinNoIrq<()>,
}

impl Registry {
    /// Creates a registry with immutable source-validity metadata.
    pub fn new(valid: &[bool]) -> Self {
        let mut slots = Vec::with_capacity(valid.len());
        slots.extend(valid.iter().copied().map(Slot::new));
        Self {
            slots: slots.into_boxed_slice(),
            control_lock: SpinNoIrq::new(()),
        }
    }

    /// Registers a handler while leaving its source disabled.
    pub fn register(&self, irq: GlobalIrq, handler: IrqHandler) -> Result<(), IrqError> {
        let _guard = self.control_lock.lock();
        let slot = self.slot(irq)?;
        slot.handler
            .compare_exchange(0, handler as usize, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| IrqError::AlreadyRegistered)
    }

    /// Unregisters a handler after masking and draining in-flight calls.
    pub fn unregister(&self, irq: GlobalIrq) -> Result<IrqHandler, IrqError> {
        let _guard = self.control_lock.lock();
        let slot = self.slot(irq)?;
        slot.state.fetch_and(!ENABLED, Ordering::AcqRel);
        while slot.state.load(Ordering::Acquire) & ACTIVE_MASK != 0 {
            core::hint::spin_loop();
        }
        let handler = slot.handler.swap(0, Ordering::AcqRel);
        if handler == 0 {
            return Err(IrqError::NotRegistered);
        }

        // SAFETY: only function pointers converted by `register` are stored in this atomic.
        Ok(unsafe { mem::transmute::<usize, IrqHandler>(handler) })
    }

    /// Changes logical delivery state without touching architecture hardware.
    pub fn set_enabled(&self, irq: GlobalIrq, enabled: bool) -> Result<(), IrqError> {
        let _guard = self.control_lock.lock();
        let slot = self.slot(irq)?;
        if slot.handler.load(Ordering::Acquire) == 0 {
            return Err(IrqError::NotRegistered);
        }
        if enabled {
            slot.state.fetch_or(ENABLED, Ordering::AcqRel);
        } else {
            slot.state.fetch_and(!ENABLED, Ordering::AcqRel);
            while slot.state.load(Ordering::Acquire) & ACTIVE_MASK != 0 {
                core::hint::spin_loop();
            }
        }
        Ok(())
    }

    /// Dispatches a source and reports whether a handler ran.
    pub fn dispatch(&self, irq: GlobalIrq) -> Option<UnhandledReason> {
        let Ok(slot) = self.slot(irq) else {
            return Some(UnhandledReason::Unregistered);
        };

        let mut state = slot.state.load(Ordering::Acquire);
        loop {
            if state & ENABLED == 0 {
                let reason = if slot.handler.load(Ordering::Acquire) == 0 {
                    slot.unhandled.fetch_add(1, Ordering::Relaxed);
                    UnhandledReason::Unregistered
                } else {
                    slot.disabled.fetch_add(1, Ordering::Relaxed);
                    UnhandledReason::Disabled
                };
                return Some(reason);
            }
            let next = state.checked_add(ACTIVE_STEP)?;
            match slot.state.compare_exchange_weak(
                state,
                next,
                Ordering::Acquire,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => state = observed,
            }
        }

        let handler = slot.handler.load(Ordering::Acquire);
        if handler == 0 {
            slot.state.fetch_sub(ACTIVE_STEP, Ordering::Release);
            slot.unhandled.fetch_add(1, Ordering::Relaxed);
            return Some(UnhandledReason::Unregistered);
        }

        // SAFETY: the active count prevents `unregister` from clearing this handler while it runs.
        unsafe { mem::transmute::<usize, IrqHandler>(handler)() };
        slot.state.fetch_sub(ACTIVE_STEP, Ordering::Release);
        None
    }

    /// Returns the number of unregistered deliveries for a source.
    pub fn unhandled_count(&self, irq: GlobalIrq) -> usize {
        self.slot(irq)
            .map(|slot| slot.unhandled.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    fn slot(&self, irq: GlobalIrq) -> Result<&Slot, IrqError> {
        self.slots
            .get(irq.raw() as usize)
            .filter(|slot| slot.valid)
            .ok_or(IrqError::InvalidNumber)
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        super::{IrqError, IrqHandler},
        GlobalIrq, Registry, UnhandledReason,
    };

    static HANDLER_CALLS: AtomicUsize = AtomicUsize::new(0);

    fn handler() {
        HANDLER_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[test]
    fn registration_and_enabled_dispatch_are_observable() {
        HANDLER_CALLS.store(0, Ordering::Relaxed);
        let registry = Registry::new(&[true]);

        assert_eq!(registry.register(GlobalIrq::new(0), handler), Ok(()));
        assert_eq!(
            registry.dispatch(GlobalIrq::new(0)),
            Some(UnhandledReason::Disabled)
        );
        assert_eq!(HANDLER_CALLS.load(Ordering::Relaxed), 0);
        assert_eq!(registry.set_enabled(GlobalIrq::new(0), true), Ok(()));
        assert_eq!(registry.dispatch(GlobalIrq::new(0)), None);
        assert_eq!(HANDLER_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn duplicate_and_invalid_registration_return_typed_errors() {
        let registry = Registry::new(&[true, false]);

        assert_eq!(registry.register(GlobalIrq::new(0), handler), Ok(()));
        assert_eq!(
            registry.register(GlobalIrq::new(0), handler),
            Err(IrqError::AlreadyRegistered)
        );
        assert_eq!(
            registry.register(GlobalIrq::new(1), handler),
            Err(IrqError::InvalidNumber)
        );
        assert_eq!(
            registry.register(GlobalIrq::new(2), handler),
            Err(IrqError::InvalidNumber)
        );
    }

    #[test]
    fn unregister_returns_handler_and_counts_later_unhandled_delivery() {
        let registry = Registry::new(&[true]);

        registry.register(GlobalIrq::new(0), handler).unwrap();
        registry.set_enabled(GlobalIrq::new(0), true).unwrap();
        assert!(matches!(
            registry.unregister(GlobalIrq::new(0)),
            Ok(previous) if core::ptr::fn_addr_eq(previous, handler as IrqHandler)
        ));
        assert_eq!(
            registry.unregister(GlobalIrq::new(0)),
            Err(IrqError::NotRegistered)
        );
        assert_eq!(
            registry.dispatch(GlobalIrq::new(0)),
            Some(UnhandledReason::Unregistered)
        );
        assert_eq!(registry.unhandled_count(GlobalIrq::new(0)), 1);
    }
}
