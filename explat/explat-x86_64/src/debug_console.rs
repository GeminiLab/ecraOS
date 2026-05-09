//! COM1 (`0x3F8`) UART 16550 debug console with a spinlock for exclusive access.

use explat::{crate_interface, debug_console::DebugConsoleIf};
use kspin::SpinNoIrq;
use uart_16550::SerialPort;

/// Standard PC COM1 I/O base port.
const COM1_BASE: u16 = 0x3f8;

/// Shared UART instance guarded by a no-IRQ spinlock.
static COM1: SpinNoIrq<SerialPort> = unsafe { SpinNoIrq::new(SerialPort::new(COM1_BASE)) };

/// Initializes COM1 for polled transmit/receive.
pub fn init() {
    COM1.lock().init();
}

/// Type tag for [`crate_interface::impl_interface`] wiring to [`explat::debug_console::DebugConsoleIf`].
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

        for (index, byte) in bytes.iter_mut().enumerate() {
            let Some(n) = com.try_receive().ok() else {
                return index;
            };

            *byte = n;
        }

        bytes.len()
    }
}
