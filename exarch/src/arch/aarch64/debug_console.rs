//! PL011 early console support for the QEMU `virt` platform.

use crate::kernel_if::phys_to_virt;

// TODO: This is a temporary value for the PL011 base address. Probing it from DTB should be implemented.
const PL011_BASE: usize = 0x0900_0000;
const UARTDR: usize = 0x00;
const UARTFR: usize = 0x18;
const FR_TXFF: u32 = 1 << 5;
const FR_RXFE: u32 = 1 << 4;

fn reg(offset: usize) -> *mut u32 {
    phys_to_virt(memory_addr::PhysAddr::from_usize(PL011_BASE + offset)).as_mut_ptr_of()
}

/// Writes bytes through the PL011 transmit FIFO.
pub fn write_bytes(bytes: &[u8]) {
    for &byte in bytes {
        while unsafe { reg(UARTFR).read_volatile() } & FR_TXFF != 0 {
            core::hint::spin_loop();
        }
        unsafe { reg(UARTDR).write_volatile(byte as u32) };
    }
}

/// Reads bytes immediately available in the PL011 receive FIFO.
pub fn read_bytes(bytes: &mut [u8]) -> usize {
    let mut count = 0;
    for byte in bytes {
        if unsafe { reg(UARTFR).read_volatile() } & FR_RXFE != 0 {
            break;
        }
        *byte = unsafe { reg(UARTDR).read_volatile() as u8 };
        count += 1;
    }
    count
}

/// Keeps the early console entry point compatible with other architectures.
pub fn init() {}
