//! Architecture-specific instructions, types, and utilities.

#![no_std]
#![feature(linkage)]

extern crate alloc;

mod arch;
pub mod kernel_if;
mod parts;
pub mod trap;

pub use arch::current::TrapFrame;
pub use parts::*;
