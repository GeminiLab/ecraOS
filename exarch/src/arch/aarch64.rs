//! AArch64 EL1 platform support.
//!
//! This module owns the small architecture boundary used by the common kernel runtime. Platform
//! discovery supplies controller and firmware descriptions through the typed configuration APIs.

pub mod context;
pub mod debug_console;
pub mod device;
pub mod gicv3;
pub mod init;
pub mod ipi;
pub mod mem;
pub mod power;
pub mod reloc_hook;
pub mod time;
pub mod trap;

/// Runs bootstrap-CPU architecture initialization before virtual-memory setup.
pub fn init_early_bsp(arg: ecraldr_base::PlatformBootArg) {
    if !debug_console::init(arg) {
        power::shutdown(crate::power::ShutdownReason::Panicked);
    }
    trap::init_exception_vector();
    time::init();
}

/// Runs application-CPU architecture initialization.
pub fn init_early_ap() {
    trap::init_exception_vector();
}
