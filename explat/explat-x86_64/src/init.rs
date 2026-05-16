//! [`explat::init::InitIf`] implementation for this platform.

use explat::{
    init::{InitIf, PlatformBootArg},
    reexport::crate_interface,
};

/// The implementation of the [`explat::init::InitIf`] trait.
pub struct InitImpl;

#[crate_interface::impl_interface]
impl InitIf for InitImpl {
    fn init_early(_arg: PlatformBootArg) {
        crate::init_early();
    }

    fn init_later() {}
}
