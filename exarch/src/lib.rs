//! Architecture-specific instructions, types, and utilities.

#![no_std]
#![feature(linkage)]

extern crate alloc;

mod arch;
pub mod debug_console;
pub mod device;
pub mod init;
pub mod mem;
pub mod power;
pub mod reloc_hook;
pub mod time;
pub mod trap;

// TODO: remove this
pub use arch::current::TrapFrame;
