//! [`explat::init::InitIf`] implementation for this platform.

use explat::{crate_interface, init::InitIf};

/// Forwards portable init calls into [`crate::init_early`] and no-op later init.
pub struct InitImpl;

#[crate_interface::impl_interface]
impl InitIf for InitImpl {
    fn init_early(_arg: usize) {
        crate::init_early();
    }

    fn init_later() {}
}
