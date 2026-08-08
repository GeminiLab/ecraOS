pub mod ap;
pub mod apic;
pub mod context;
pub mod gdt;
pub mod idt;
pub mod ioapic;
pub mod trap;

// TODO: remove this
pub use context::TrapFrame;
