/// Runs the x86-64 hook before kernel relocation.
pub fn before_reloc() {}

/// Runs the x86-64 hook after kernel relocation.
pub fn after_reloc() {
    super::after_reloc();
}
