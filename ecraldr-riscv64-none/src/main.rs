//! Direct boot module for RISC-V 64-bit.
//!
//! Some code is copied from the `axplat-riscv64-qemu-virt` crate.

#![no_std]
#![no_main]
#![cfg(target_arch = "riscv64")]

use ecraos_boot::{PhysAddr, PhysAddrRange, PlatformBootArg};

unsafe extern "C" {
    static _ebootstack: u8;
}

core::arch::global_asm!(
    include_str!("boot.S"),
    boot_stack_top = sym _ebootstack,
    entry = sym rust_entry64_bsp,
);

/// First Rust code on the bootstrap processor after the `global_asm!` boot path: jumps to the
/// portable kernel entry with `hart_id == 0`.
fn rust_entry64_bsp(arg0: u64, arg1: u64) -> ! {
    unsafe extern "C" {
        fn _sloader();
        fn _skernel();
    }

    let sloader = _sloader as *const () as usize;
    let skernel = _skernel as *const () as usize;

    let boot_arg = ecraos_boot::BootArg {
        loader_range: unsafe { PhysAddrRange::new_unchecked(sloader.into(), skernel.into()) },
        plat_arg: PlatformBootArg::DeviceTree(PhysAddr::from_usize(arg1 as _)),
    };

    ecraos_boot::call_kernel_entry!(arg0 as _, &boot_arg);
}

ecraldr::ecraldr_panic_handler!();
