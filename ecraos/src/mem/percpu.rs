use expercpu::def_percpu;

use crate::mem;

pub fn section_size() -> usize {
    mem::sections::percpu_aligned().size()
}

#[def_percpu]
pub static FOO: usize = 21867;

#[def_percpu]
pub static BAR: (usize, usize) = (123, 456);
