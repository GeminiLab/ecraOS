use crate::crate_interface::def_interface;

#[def_interface(gen_caller)]
pub trait PowerIf {
    fn poweroff() -> !;
}
