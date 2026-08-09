/// Runs the RISC-V hook before kernel relocation.
pub fn before_reloc() {}

/// Runs the RISC-V hook after kernel relocation.
pub fn after_reloc() {
    super::trap::after_reloc();
}
