use expercpu::def_percpu;

use crate::mem;

#[expect(unused)]
pub fn section_size() -> usize {
    mem::sections::percpu_aligned().size()
}

#[def_percpu]
pub static CPU_ID: usize = 0;

/// Initializes the per-CPU data area using the early slot.
pub fn init_early() {
    unsafe { expercpu::init_in_early_slot() };
}

/// Re-initializes the per-CPU data area pointer after relocation.
pub fn init_bsp_after_reloc() {
    unsafe {
        expercpu::write_percpu_reg(expercpu::early_slot_start());
    };
}
