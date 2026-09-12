//! AArch64 EL1 platform support.
//!
//! This module owns the small architecture boundary used by the common kernel runtime. Platform
//! discovery supplies controller and firmware descriptions through the typed configuration APIs.

pub mod context;
pub mod debug_console;
pub mod device;
pub mod init;
pub mod mem;
pub mod power;
pub mod reloc_hook;
pub mod time;
pub mod trap;

/// Runs bootstrap-CPU architecture initialization after device-tree ownership is established.
pub fn init_early_bsp(arg: ecraldr_base::PlatformBootArg) {
    debug_console::init();
    init::probe_platform(arg);
    time::init();
    trap::init_percpu();
}

/// Runs application-CPU architecture initialization.
pub fn init_early_ap() {
    trap::init_percpu();
}
