//! Relocation hook.

use crate_interface::def_interface;

/// Relocation hook operations implemented by the platform.
#[def_interface(gen_caller)]
pub trait RelocHookIf {
    /// The hook function to be called before relocation.
    fn before_reloc();

    /// The hook function to be called after relocation.
    fn after_reloc();
}
