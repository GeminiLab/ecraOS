use core::{
    arch::global_asm,
    sync::atomic::{AtomicU64, Ordering},
};

use x86_64::registers::control::{Cr0Flags, Cr4Flags, EferFlags};

const CR0: u64 = Cr0Flags::PROTECTED_MODE_ENABLE.bits()
    | Cr0Flags::MONITOR_COPROCESSOR.bits()
    | Cr0Flags::NUMERIC_ERROR.bits()
    | Cr0Flags::WRITE_PROTECT.bits()
    | Cr0Flags::PAGING.bits();
const CR4: u64 = Cr4Flags::PHYSICAL_ADDRESS_EXTENSION.bits()
    | Cr4Flags::PAGE_GLOBAL.bits()
    | Cr4Flags::OSFXSR.bits()
    | Cr4Flags::OSXMMEXCPT_ENABLE.bits();
const EFER: u64 = EferFlags::LONG_MODE_ENABLE.bits() | EferFlags::NO_EXECUTE_ENABLE.bits();

global_asm!(
    include_str!("multiboot.S"),
    options(att_syntax),
    cr0 = const CR0,
    cr4 = const CR4,
    efer = const EFER,
);

#[unsafe(link_section = ".data")]
static MULTIBOOT_ARGS: AtomicU64 = AtomicU64::new(0);

fn save_multiboot_args(arg0: u32, arg1: u32) {
    MULTIBOOT_ARGS.store(arg0 as u64 | ((arg1 as u64) << 32), Ordering::Relaxed);
}

pub fn read_multiboot_args() -> (u32, u32) {
    let args = MULTIBOOT_ARGS.load(Ordering::Relaxed);
    (args as u32, (args >> 32) as u32)
}

#[unsafe(no_mangle)]
fn rust_entry64_bsp(arg0: u32, arg1: u32) -> ! {
    unsafe extern "C" {
        unsafe fn kernel_main() -> !;
    }

    save_multiboot_args(arg0, arg1);

    unsafe {
        kernel_main();
    }
}
