use explat::{crate_interface, debug_console::DebugConsoleIf};
use kspin::SpinNoIrq;
use uart_16550::SerialPort;

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
