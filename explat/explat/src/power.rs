//! Power management.

use crate::crate_interface::def_interface;

/// Power operations implemented by the platform.
#[def_interface(gen_caller)]
pub trait PowerIf {
    /// Turns the system off or halts it in a power-off state.
    fn poweroff() -> !;
}
