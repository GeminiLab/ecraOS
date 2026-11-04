//! AArch64 relocation hooks.

/// Runs the pre-relocation hook.
pub fn before_reloc() {}

/// Runs the post-relocation hook.
pub fn after_reloc() {
    super::trap::init_exception_vector();
}
