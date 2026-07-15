//! [`exarch::init::InitIf`] implementation for this platform.

use crate::init::{InitIf, PlatformBootArg};

/// The implementation of the [`InitIf`] trait.
pub struct InitImpl;

#[crate_interface::impl_interface]
impl InitIf for InitImpl {
    fn init_early(_arg: PlatformBootArg) {
        super::init_early();
    }

    fn init_later(_arg: PlatformBootArg) {
        super::init_later();
    }

    fn init_early_ap() {
        super::init_early_ap();
    }
}
