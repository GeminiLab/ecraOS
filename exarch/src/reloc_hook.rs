//! Relocation hook.

use look_at::look_at;

/// Relocation hook operations implemented by the platform.
///
/// The wrapper forwards calls to the selected architecture implementation.
#[look_at(crate::arch::current::reloc_hook, flatten)]
mod _wrapper {
    /// The hook function to be called before relocation.
    pub fn before_reloc();

    /// The hook function to be called after relocation.
    pub fn after_reloc();
}
