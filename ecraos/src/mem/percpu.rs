use expercpu::def_percpu;
use memory_addr::VirtAddr;

use crate::{kprintln, mem};

pub fn section_size() -> usize {
    mem::sections::percpu_aligned().size()
}

#[def_percpu]
pub static FOO: usize = 21867;

#[def_percpu]
pub static BAR: (usize, usize) = (123, 456);

#[def_percpu]
pub static CPU_ID: usize = 0;

/// Initializes the per-CPU data area using the early slot.
pub fn init_early() {
    unsafe { expercpu::init_in_early_slot() };
}

/// Initializes the per-CPU data area for the BSP.
pub fn init_bsp(base: VirtAddr) {
    unsafe extern "C" {
        static _percpu_early_slot_start: u8;
        static _percpu_early_slot_end: u8;
    }
    unsafe {
        kprintln!(
            "early slot start: {:#x}, end: {:#x}",
            &_percpu_early_slot_start as *const u8 as usize,
            &_percpu_early_slot_end as *const u8 as usize
        );
    }
    unsafe { expercpu::init_from_early_slot(base) };
}
