//! Initialization hooks: early and later platform bring-up.

use crate::crate_interface::def_interface;

/// Platform initialization contract invoked from portable kernel code.
#[def_interface(gen_caller)]
pub trait InitIf {
    /// Early platform initialization.
    ///
    /// The kernel code calls this method as soon as the kernel entry runs, with
    /// `arg` provided when the kernel entry was called.
    fn init_early(arg: usize);
    /// Later platform initialization.
    fn init_later();
}
