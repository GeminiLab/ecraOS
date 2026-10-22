//! Architecture-specific instructions, types, and utilities.

#![no_std]
#![feature(linkage)]
#![feature(doc_cfg)]

extern crate alloc;

pub mod arch;
pub mod kernel_if;
mod parts;

pub use parts::*;
