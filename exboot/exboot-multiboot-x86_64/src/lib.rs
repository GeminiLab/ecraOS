//! Multiboot 1 boot module for x86-64.

#![no_std]

use core::arch::global_asm;

use x86_64::registers::control::{Cr0Flags, Cr4Flags, EferFlags};

use exboot::{BootArg, PhysAddr, PhysAddrRange, PlatformBootArg};

/// `CR0` value applied by boot code: protected mode, paging, write protect, FP-related bits.
const CR0: u64 = Cr0Flags::PROTECTED_MODE_ENABLE.bits()
    | Cr0Flags::MONITOR_COPROCESSOR.bits()
    | Cr0Flags::NUMERIC_ERROR.bits()
    | Cr0Flags::WRITE_PROTECT.bits()
    | Cr0Flags::PAGING.bits();
/// `CR4` value: PAE, global pages, SSE-related enables.
const CR4: u64 = Cr4Flags::PHYSICAL_ADDRESS_EXTENSION.bits()
    | Cr4Flags::PAGE_GLOBAL.bits()
    | Cr4Flags::OSFXSR.bits()
    | Cr4Flags::OSXMMEXCPT_ENABLE.bits();
/// `EFER` value: long mode and NX enable.
const EFER: u64 = EferFlags::LONG_MODE_ENABLE.bits() | EferFlags::NO_EXECUTE_ENABLE.bits();

global_asm!(
    include_str!("multiboot.S"),
    options(att_syntax),
    cr0 = const CR0,
    cr4 = const CR4,
    efer = const EFER,
);

use exboot::call_kernel_entry;

/// First Rust code on the bootstrap processor after the `global_asm!` boot path: jumps to the
/// portable kernel entry with `hart_id == 0`.
///
/// `_arg0` is the Multiboot magic value originally in `EAX` (currently unused). `arg1` is the
/// physical address of the Multiboot information structure from `EBX`, forwarded as `usize` to
/// [`explat::init::InitIf::init_early`].
#[unsafe(no_mangle)]
fn rust_entry64_bsp(_arg0: u32, arg1: u32) -> ! {
    unsafe extern "C" {
        fn boot_stack();
        fn boot_stack_top();
    }

    let boot_stack = boot_stack as *const () as usize;
    let boot_stack_top = boot_stack_top as *const () as usize;

    let boot_arg = BootArg {
        // SAFETY: We know it's valid because the linker script ensures it.
        boot_stack: unsafe {
            PhysAddrRange::new_unchecked(boot_stack.into(), boot_stack_top.into())
        },
        plat_arg: PlatformBootArg::Multiboot(PhysAddr::from_usize(arg1 as _)),
    };

    call_kernel_entry!(0, &boot_arg)
}
