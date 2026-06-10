//! Per-CPU data support.
//!
//! Defines and initializes per-CPU data areas for ecraOS.

#![no_std]

extern crate expercpu_macros;

use core::{alloc::Layout, ptr::addr_of};

use memory_addr::VirtAddr;

pub use expercpu_macros::def_percpu;

#[cfg(feature = "remote-access")]
use crate_interface::def_interface;

const SIZE_64BIT: usize = 64;
const PERCPU_AREA_ALIGN: usize = SIZE_64BIT;

unsafe extern "C" {
    static _percpu_start: u8;
    static _percpu_load_start: u8;
    static _percpu_load_end: u8;
}

#[doc(hidden)]
pub mod __priv {
    #[cfg(feature = "preempt")]
    pub use kernel_guard::NoPreempt as NoPreemptGuard;
}

#[cfg(feature = "remote-access")]
#[def_interface(gen_caller)]
pub trait PerCPUAreaIf {
    /// Returns the per-CPU area base virtual address for `cpu_id`.
    fn percpu_area_base_for(cpu_id: usize) -> VirtAddr;
}

const fn align_up_64(val: usize) -> usize {
    (val + SIZE_64BIT - 1) & !(SIZE_64BIT - 1)
}

/// Returns the per-CPU data area size for one CPU.
pub fn percpu_area_size() -> usize {
    expercpu_macros::percpu_symbol_vma!(_percpu_load_end)
        - expercpu_macros::percpu_symbol_vma!(_percpu_load_start)
}

/// Returns the allocation layout for one per-CPU data area.
pub fn percpu_area_layout() -> Layout {
    Layout::from_size_align(align_up_64(percpu_area_size()), PERCPU_AREA_ALIGN).unwrap()
}

fn validate_percpu_base(base: VirtAddr) {
    assert!(
        base.as_usize() != 0,
        "per-CPU area base address must be non-null"
    );
    assert_eq!(
        base.as_usize() % PERCPU_AREA_ALIGN,
        0,
        "per-CPU area base address must be 64-byte aligned"
    );
}

/// Initializes the current CPU's per-CPU data area.
///
/// The caller supplies the base virtual address of the current CPU's per-CPU
/// area. The function copies the `.percpu` initial image to that area and
/// writes the architecture-specific per-CPU register for the current CPU.
///
/// # Safety
///
/// The caller must ensure that:
///
/// - `base` is the current CPU's writable per-CPU area base;
/// - the area is at least `percpu_area_layout().size()` bytes and 64-byte
///   aligned;
/// - the destination does not overlap the `.percpu` initial image source; and
/// - setting the current CPU's per-CPU register to this base is valid for the
///   current execution context.
pub unsafe fn init(base: VirtAddr) {
    validate_percpu_base(base);
    let size = percpu_area_size();

    unsafe {
        core::ptr::copy_nonoverlapping(addr_of!(_percpu_start), base.as_mut_ptr(), size);
        write_percpu_reg(base);
    }
}

/// Reads the architecture-specific per-CPU data register.
pub fn read_percpu_reg() -> VirtAddr {
    let tp: usize;
    unsafe {
        cfg_if::cfg_if! {
            if #[cfg(target_arch = "x86_64")] {
                tp = if cfg!(target_os = "linux") {
                    SELF_PTR.read_current_raw()
                } else if cfg!(target_os = "none") {
                    x86::msr::rdmsr(x86::msr::IA32_GS_BASE) as usize
                } else {
                    unimplemented!()
                };
            } else if #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))] {
                core::arch::asm!("mv {}, gp", out(reg) tp)
            } else if #[cfg(all(target_arch = "aarch64", not(feature = "arm-el2")))] {
                core::arch::asm!("mrs {}, TPIDR_EL1", out(reg) tp)
            } else if #[cfg(all(target_arch = "aarch64", feature = "arm-el2"))] {
                core::arch::asm!("mrs {}, TPIDR_EL2", out(reg) tp)
            } else if #[cfg(target_arch = "loongarch64")] {
                core::arch::asm!("move {}, $r21", out(reg) tp)
            } else if #[cfg(target_arch = "arm")] {
                core::arch::asm!("mrc p15, 0, {}, c13, c0, 4", out(reg) tp)
            } else {
                compile_error!("unsupported architecture for expercpu");
            }
        }
    }
    VirtAddr::from_usize(tp + expercpu_macros::percpu_symbol_vma!(_percpu_load_start))
}

/// Writes the architecture-specific per-CPU data register.
///
/// # Safety
///
/// `tp` must be the valid per-CPU area base for the current CPU, and setting
/// the architecture-specific per-CPU register to that base must be valid for
/// the current execution context.
pub unsafe fn write_percpu_reg(tp: VirtAddr) {
    let tp = tp.as_usize() - expercpu_macros::percpu_symbol_vma!(_percpu_load_start);

    unsafe {
        cfg_if::cfg_if! {
            if #[cfg(target_arch = "x86_64")] {
                if cfg!(target_os = "linux") {
                    const ARCH_SET_GS: usize = 0x1001;
                    const SYS_ARCH_PRCTL: isize = 158;
                    let ret: isize;
                    core::arch::asm!(
                        "syscall",
                        inlateout("rax") SYS_ARCH_PRCTL => ret,
                        in("rdi") ARCH_SET_GS,
                        in("rsi") tp,
                        lateout("rcx") _,
                        lateout("r11") _,
                    );
                    assert_eq!(ret, 0, "arch_prctl(ARCH_SET_GS) failed");
                } else if cfg!(target_os = "none") {
                    x86::msr::wrmsr(x86::msr::IA32_GS_BASE, tp as u64);
                } else {
                    unimplemented!()
                }
                SELF_PTR.write_current_raw(tp);
            } else if #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))] {
                core::arch::asm!("mv gp, {}", in(reg) tp)
            } else if #[cfg(all(target_arch = "aarch64", not(feature = "arm-el2")))] {
                core::arch::asm!("msr TPIDR_EL1, {}", in(reg) tp)
            } else if #[cfg(all(target_arch = "aarch64", feature = "arm-el2"))] {
                core::arch::asm!("msr TPIDR_EL2, {}", in(reg) tp)
            } else if #[cfg(target_arch = "loongarch64")] {
                core::arch::asm!("move $r21, {}", in(reg) tp)
            } else if #[cfg(target_arch = "arm")] {
                core::arch::asm!("mcr p15, 0, {}, c13, c0, 4", in(reg) tp)
            } else {
                compile_error!("unsupported architecture for expercpu");
            }
        }
    }
}

#[allow(unused_imports)]
use crate as expercpu;

#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
#[expercpu_macros::def_percpu]
static SELF_PTR: usize = 0;
