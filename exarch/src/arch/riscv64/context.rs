//! RISC-V trap context frames.
//!
//! Field order is part of the assembly ABI used by `trap.S`.

use riscv::register::sstatus;

/// The RISC-V integer register file saved during a trap.
///
/// The zero slot keeps field offsets equal to architectural register numbers.
#[allow(missing_docs)]
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct GeneralRegisters {
    pub zero: usize,
    pub ra: usize,
    pub sp: usize,
    pub gp: usize,
    pub tp: usize,
    pub t0: usize,
    pub t1: usize,
    pub t2: usize,
    pub s0: usize,
    pub s1: usize,
    pub a0: usize,
    pub a1: usize,
    pub a2: usize,
    pub a3: usize,
    pub a4: usize,
    pub a5: usize,
    pub a6: usize,
    pub a7: usize,
    pub s2: usize,
    pub s3: usize,
    pub s4: usize,
    pub s5: usize,
    pub s6: usize,
    pub s7: usize,
    pub s8: usize,
    pub s9: usize,
    pub s10: usize,
    pub s11: usize,
    pub t3: usize,
    pub t4: usize,
    pub t5: usize,
    pub t6: usize,
}

/// The saved RISC-V supervisor trap frame.
///
/// Assembly restores `sepc` and `sstatus` from this frame before `sret`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapFrame {
    /// The saved integer registers.
    pub regs: GeneralRegisters,
    /// The saved supervisor exception program counter.
    pub sepc: usize,
    /// The saved supervisor status register.
    pub sstatus: sstatus::Sstatus,
}

impl Default for TrapFrame {
    fn default() -> Self {
        Self {
            regs: GeneralRegisters::default(),
            sepc: 0,
            sstatus: sstatus::Sstatus::from_bits(0),
        }
    }
}

const _: () =
    assert!(core::mem::size_of::<GeneralRegisters>() == 32 * core::mem::size_of::<usize>());
const _: () = assert!(core::mem::size_of::<TrapFrame>() == 34 * core::mem::size_of::<usize>());
