#![no_std]
#![doc = include_str!("../README.md")]

#[cfg(test)]
#[macro_use]
extern crate std;

mod irq;
mod locks;

#[cfg(test)]
mod tests;

pub use self::irq::{NoIrqGuardPolicy, NoPreemptIrqSave};
pub use self::locks::{
    BaseRwLock, BaseRwLockReadGuard, BaseRwLockWriteGuard, RwLockNoIrq, RwLockNoIrqReadGuard,
    RwLockNoIrqWriteGuard, SpinNoIrq, SpinNoIrqGuard,
};
pub use kspin::{
    BaseSpinLock, BaseSpinLockGuard, SpinNoPreempt, SpinNoPreemptGuard, SpinRaw, SpinRawGuard,
};

/// An error in a CPU's nested IRQ guard state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NestedIrqError {
    /// The active guard count cannot be represented.
    DepthOverflow,
    /// An active guard count has no saved outer interrupt state.
    MissingOuterToken,
    /// An inactive state unexpectedly retains an outer interrupt token.
    UnexpectedOuterToken,
    /// A guard was released when no guard was active.
    UnbalancedExit,
}
