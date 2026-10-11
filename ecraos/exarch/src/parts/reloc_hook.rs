//! Relocation hook.

use look_at::look_at;

look_at! {
    @crate::arch::current::reloc_hook:
    /// The hook function to be called before relocation.
    pub fn before_reloc();

    /// The hook function to be called after relocation.
    pub fn after_reloc();
}
