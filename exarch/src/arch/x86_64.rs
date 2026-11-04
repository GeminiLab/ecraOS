pub mod context;
pub mod debug_console;
pub mod device;
pub mod imp;
pub mod init;
pub mod ipi;
pub mod mem;
pub mod power;
pub mod reloc_hook;
pub mod time;
pub mod trap;

/// Runs platform-local early initialization work.
pub fn init_early() {
    // Initialize the debug console
    debug_console::init();
    crate::dbcn_println!("\n\n");

    // Initialize the GDT and IDT
    imp::gdt::init_gdt();
    imp::idt::init_idt();

    // Check if the required features are supported after exception handling is available.
    check_required_features();

    // Initialize the time module
    time::init_early();
}

/// Initializes the discovered global controller and this CPU's local APIC.
///
/// Consumes the controller configuration published after discovery and MMIO mapping.
///
/// # Panics
///
/// Panics if platform discovery did not publish the controller configuration.
pub fn init_later() {
    let config = crate::platform::take_interrupt_controller_config();
    trap::init_external_controller(config);
    imp::apic::init_bsp();
}

pub fn init_early_ap() {
    imp::gdt::init_gdt();
    imp::idt::init_idt();
}

pub fn init_later_ap() {
    imp::apic::init_ap();
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
