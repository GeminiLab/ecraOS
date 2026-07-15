pub mod debug_console;
pub mod device;
pub mod imp;
pub mod init;
pub mod mem;
pub mod power;
pub mod reloc_hook;
pub mod time;

/// Runs platform-local early initialization work.
pub fn init_early() {
    // Initialize the debug console
    debug_console::init();
    crate::dbcn_println!("\n\n");

    // Check if the required features are supported
    check_required_features();

    // Initialize the GDT and IDT
    imp::gdt::init_gdt();
    imp::idt::init_idt();

    // Initialize the time module
    time::init_early();
}

pub fn init_later() {
    imp::apic::init_primary();
}

pub fn init_early_ap() {
    imp::gdt::init_gdt();
    imp::idt::init_idt();
}

pub fn after_reloc() {
    imp::gdt::reload_gdt();
    imp::idt::reload_idt();
}

fn check_required_features() {
    use raw_cpuid::CpuId;

    let cpu_id = CpuId::new();
    let feature_info = cpu_id.get_feature_info().unwrap();

    if !feature_info.has_tsc() {
        panic!("TSC not supported");
    }

    if !feature_info.has_x2apic() {
        panic!("X2APIC not supported");
    }
}

// TODO: remove this
pub use imp::TrapFrame;
