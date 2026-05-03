#![no_std]

use core::arch::global_asm;

use explat::{crate_interface, debug_console::DebugConsoleIf, init::InitIf, power::PowerIf};
use kspin::SpinNoIrq;
use uart_16550::SerialPort;

global_asm!(include_str!("multiboot.S"), options(att_syntax));

#[unsafe(no_mangle)]
pub fn rust_entry64_bsp(_arg0: u32, _arg1: u32) -> ! {
    unsafe extern "C" {
        unsafe fn kernel_main() -> !;
    }

    unsafe {
        kernel_main();
    }
}

pub fn poweroff() -> ! {
    unsafe {
        const POWEROFF_PORT: u16 = 0x604;
        const POWEROFF_VALUE: u16 = 0x2000;
        core::arch::asm!(
            "outw %ax, (%dx)",
            in("ax") POWEROFF_VALUE,
            in("dx") POWEROFF_PORT,
            options(att_syntax),
        );

        loop {
            core::arch::asm!("hlt", options(nomem, nostack));
        }
    }
}

const COM1_BASE: u16 = 0x3f8;

static COM1: SpinNoIrq<SerialPort> = unsafe { SpinNoIrq::new(SerialPort::new(COM1_BASE)) };

pub fn init() {
    COM1.lock().init();
}

pub struct DebugConsoleImpl;

#[crate_interface::impl_interface]
impl DebugConsoleIf for DebugConsoleImpl {
    fn write_bytes(bytes: &[u8]) {
        let mut com = COM1.lock();
        for &b in bytes {
            com.send(b);
        }
    }

    fn read_bytes(bytes: &mut [u8]) -> usize {
        let mut com = COM1.lock();

        for index in 0..bytes.len() {
            let Some(n) = com.try_receive().ok() else {
                return index;
            };

            bytes[index] = n;
        }

        bytes.len()
    }
}

pub struct InitImpl;

#[crate_interface::impl_interface]
impl InitIf for InitImpl {
    fn init_early() {
        init();
    }

    fn init_later() {}
}

pub struct PowerImpl;

#[crate_interface::impl_interface]
impl PowerIf for PowerImpl {
    fn poweroff() -> ! {
        poweroff()
    }
}
