use explat::{crate_interface, init::InitIf};

pub struct InitImpl;

#[crate_interface::impl_interface]
impl InitIf for InitImpl {
    fn init_early() {
        crate::init_early();
    }

    fn init_later() {}
}
