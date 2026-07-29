//! RISC-V supervisor interrupt support.
//!
//! This implementation currently registers only the supervisor timer interrupt.

use core::{
    mem, ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

use riscv::{
    interrupt::supervisor::{self, Interrupt},
    register::sstatus,
};

use crate::irq::{IrqHandler, IrqIf};

/// The interrupt marker bit in `scause`.
///
/// RISC-V places this marker in the most significant XLEN bit.
pub const INTC_IRQ_BASE: usize = 1 << (usize::BITS - 1);

/// The supervisor software interrupt cause.
///
/// This cause remains masked until an IPI implementation is added.
pub const SOFTWARE_IRQ_NUM: usize = INTC_IRQ_BASE | Interrupt::SupervisorSoft as usize;

/// The supervisor timer interrupt cause.
///
/// This is the only IRQ accepted by the current registration implementation.
pub const TIMER_IRQ_NUM: usize = INTC_IRQ_BASE | Interrupt::SupervisorTimer as usize;

/// The supervisor external interrupt cause.
///
/// This cause remains masked until a PLIC implementation is added.
pub const EXTERNAL_IRQ_NUM: usize = INTC_IRQ_BASE | Interrupt::SupervisorExternal as usize;

/// The registered supervisor timer handler.
///
/// A null pointer means the handler has not been installed.
static TIMER_HANDLER: AtomicPtr<()> = AtomicPtr::new(ptr::null_mut());

/// The RISC-V implementation of interrupt operations.
///
/// Registration and dispatch are limited to [`TIMER_IRQ_NUM`].
pub struct IrqImpl;

#[crate_interface::impl_interface]
impl IrqIf for IrqImpl {
    fn register(irq: usize, handler: IrqHandler) -> bool {
        if irq != TIMER_IRQ_NUM {
            return false;
        }

        TIMER_HANDLER
            .compare_exchange(
                ptr::null_mut(),
                handler as *mut (),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn unregister(irq: usize) -> Option<IrqHandler> {
        if irq != TIMER_IRQ_NUM {
            return None;
        }

        let handler = TIMER_HANDLER.swap(ptr::null_mut(), Ordering::AcqRel);
        if handler.is_null() {
            None
        } else {
            // SAFETY: `register` stores only pointers converted from `IrqHandler`.
            Some(unsafe { mem::transmute::<*mut (), IrqHandler>(handler) })
        }
    }

    fn handle(irq: usize) -> bool {
        if irq != TIMER_IRQ_NUM {
            return false;
        }

        let handler = TIMER_HANDLER.load(Ordering::Acquire);
        if handler.is_null() {
            return false;
        }

        // SAFETY: `register` stores only pointers converted from `IrqHandler`.
        unsafe { mem::transmute::<*mut (), IrqHandler>(handler)() };
        true
    }

    fn enable_local() {
        // SAFETY: callers enable interrupts only after installing handlers.
        unsafe { supervisor::enable() };
    }

    fn disable_local() {
        supervisor::disable();
    }

    fn local_enabled() -> bool {
        sstatus::read().sie()
    }
}

/// Enables the supervisor timer source on the current hart.
///
/// Global interrupt delivery remains controlled separately through [`crate::irq::enable_local`].
pub(super) fn init_percpu() {
    // SAFETY: global interrupts remain disabled until the kernel installs and arms the timer.
    unsafe { supervisor::enable_interrupt(Interrupt::SupervisorTimer) };
}
