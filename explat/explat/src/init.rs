use crate::crate_interface::def_interface;

#[def_interface(gen_caller)]
pub trait InitIf {
    fn init_early();
    fn init_later();
}
