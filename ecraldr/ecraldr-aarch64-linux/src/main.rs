//! Linux arm64 Image boot module for AArch64 QEMU `virt`.
//!
//! The assembly entry follows the Linux arm64 boot protocol and forwards the
//! device-tree pointer with a CPU identifier read from `MPIDR_EL1`.

#![no_main]
#![no_std]
#![cfg(target_arch = "aarch64")]

use core::arch::global_asm;

use ecraldr_base::{BootArg, PhysAddr, PhysAddrRange, PlatformBootArg};

unsafe extern "C" {
    static _ebootstack: u8;
}

global_asm!(
    include_str!("boot.S"),
    boot_stack_top = sym _ebootstack,
    entry = sym rust_entry64_bsp,
);

/// Enters the portable kernel path on the bootstrap processor.
///
/// The CPU identifier and device-tree address are forwarded as the standard
/// [`BootArg`] value.
#[unsafe(no_mangle)]
extern "C" fn rust_entry64_bsp(cpu_id: u64, dtb: u64) -> ! {
    unsafe extern "C" {
        fn _sloader();
        fn _skernel();
    }

    let sloader = _sloader as *const () as usize;
    let skernel = _skernel as *const () as usize;
    let boot_arg = BootArg {
        // SAFETY: The loader linker script places these symbols around the
        // loader image and kernel payload respectively.
        loader_range: unsafe { PhysAddrRange::new_unchecked(sloader.into(), skernel.into()) },
        plat_arg: PlatformBootArg::DeviceTree(PhysAddr::from_usize(dtb as usize)),
    };

    ecraldr_base::call_kernel_entry!(cpu_id as usize, &boot_arg)
}

ecraldr::ecraldr_panic_handler!();
