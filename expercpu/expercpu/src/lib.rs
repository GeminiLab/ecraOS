#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "host-test"), no_std)]

#[cfg(all(feature = "host-test", not(target_os = "linux")))]
compile_error!("the expercpu `host-test` backend currently supports Linux only");

#[cfg(feature = "host-test")]
extern crate std;

use core::alloc::Layout;

#[cfg(feature = "remote-access")]
use crate_interface::def_interface;
/// Defines a per-CPU static variable and its accessors.
pub use expercpu_macros::def_percpu;
use memory_addr::VirtAddr;

#[cfg(feature = "host-test")]
pub mod host_test;

/// Internal items used by macro expansions.
#[doc(hidden)]
pub mod __priv {
    /// Resolves the current host thread per-CPU area base.
    #[cfg(feature = "host-test")]
    pub use crate::host_test::current_base as host_current_base;

    /// The guard used by generated current-CPU accessors.
    #[cfg(feature = "preempt")]
    pub use kernel_guard::NoPreempt as NoPreemptGuard;
}

/// The alignment used for every per-CPU data area.
const SIZE_64BIT: usize = 64;

/// The required alignment of every per-CPU data area.
const PERCPU_AREA_ALIGN: usize = SIZE_64BIT;

// Linker-provided bounds and optional early-slot bounds for the per-CPU section.
unsafe extern "C" {
    /// The virtual address of the first byte in the per-CPU section.
    static _percpu_start: u8;

    /// The virtual address immediately after the per-CPU section.
    static _percpu_end: u8;

    /// The virtual address of the first byte in the early per-CPU slot.
    #[cfg(feature = "early-slot")]
    static _percpu_early_slot_start: u8;

    /// The virtual address immediately after the early per-CPU slot.
    #[cfg(feature = "early-slot")]
    static _percpu_early_slot_end: u8;
}

/// Resolves per-CPU area bases for remote access.
#[cfg(feature = "remote-access")]
#[def_interface(gen_caller)]
pub trait PerCPUAreaIf {
    /// Returns the per-CPU area base virtual address for `cpu_id`.
    fn percpu_area_base_for(cpu_id: usize) -> VirtAddr;
}

/// Rounds a size up to the per-CPU area alignment.
const fn align_up_64(val: usize) -> usize {
    (val + SIZE_64BIT - 1) & !(SIZE_64BIT - 1)
}

/// Returns the per-CPU data area size for one CPU.
pub fn percpu_area_size() -> usize {
    expercpu_macros::percpu_symbol_vma!(_percpu_end)
        - expercpu_macros::percpu_symbol_vma!(_percpu_start)
}

/// Returns the allocation layout for one per-CPU data area.
pub fn percpu_area_layout() -> Layout {
    Layout::from_size_align(align_up_64(percpu_area_size()), PERCPU_AREA_ALIGN).unwrap()
}

/// Validates a per-CPU area base address.
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
/// - `base` is the current CPU's writable per-CPU area base,
/// - the area is at least `percpu_area_layout().size()` bytes and 64-byte
///   aligned,
/// - the destination does not overlap the `.percpu` initial image source, and
/// - setting the current CPU's per-CPU register to this base is valid for the
///   current execution context.
pub unsafe fn init(base: VirtAddr) {
    let percpu_start = VirtAddr::from_usize(expercpu_macros::percpu_symbol_vma!(_percpu_start));
    unsafe {
        init_from(base, percpu_start);
    }
}

/// Initializes the current CPU's per-CPU data area using the early slot.
///
/// Early slot is a reserved area in the binary for initializing the per-CPU data area without
/// relying on dynamic memory allocation. Early slot is bounded by the symbols
/// `_percpu_early_slot_start` and `_percpu_early_slot_end`.
///
/// # Panics
///
/// Panics at runtime if the linker-provided early slot is smaller than the per-CPU initial image.
///
/// # Safety
///
/// The caller must ensure that the early slot is included in the binary, and is not used by other
/// cores.
#[cfg(feature = "early-slot")]
pub unsafe fn init_in_early_slot() {
    let early_slot_start = expercpu_macros::percpu_symbol_vma!(_percpu_early_slot_start);
    let early_slot_end = expercpu_macros::percpu_symbol_vma!(_percpu_early_slot_end);

    assert!(
        early_slot_end - early_slot_start >= percpu_area_size(),
        "early slot is too small"
    );

    unsafe {
        init(VirtAddr::from_usize(early_slot_start));
    }
}

/// Initializes the current CPU's per-CPU data area, copying from the early slot.
///
/// # Safety
///
/// See the safety requirements of [`init`] for more details.
#[cfg(feature = "early-slot")]
pub unsafe fn init_from_early_slot(base: VirtAddr) {
    unsafe { init_from(base, early_slot_start()) };
}

/// Returns the start address of the early slot.
#[cfg(feature = "early-slot")]
#[inline]
pub fn early_slot_start() -> VirtAddr {
    VirtAddr::from_usize(expercpu_macros::percpu_symbol_vma!(
        _percpu_early_slot_start
    ))
}

/// Copies a per-CPU image and writes the current CPU register.
///
/// # Safety
///
/// `base` must identify a valid writable per-CPU area and `from` must identify
/// a readable per-CPU image. The regions must be valid for `percpu_area_size()`
/// bytes and must not overlap in a way that invalidates the copy.
unsafe fn init_from(base: VirtAddr, from: VirtAddr) {
    validate_percpu_base(base);
    let size = percpu_area_size();

    unsafe {
        core::ptr::copy(from.as_mut_ptr(), base.as_mut_ptr(), size);
        write_percpu_reg(base);
    }
}

/// Reads the architecture-specific per-CPU data register.
#[cfg(not(feature = "host-test"))]
pub fn read_percpu_reg() -> VirtAddr {
    let tp: usize;
    unsafe {
        cfg_if::cfg_if! {
            if #[cfg(target_arch = "x86_64")] {
                core::arch::asm!("rdgsbase {}", out(reg) tp);
            } else if #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))] {
                // RISC-V `tp` may be a better long-term per-CPU base, but keep `gp`
                // for compatibility with the current design.
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
    VirtAddr::from_usize(tp + expercpu_macros::percpu_symbol_vma!(_percpu_start))
}

/// Reads the host thread current per-CPU area base.
#[cfg(feature = "host-test")]
pub fn read_percpu_reg() -> VirtAddr {
    host_test::current_base()
}

/// Writes the architecture-specific per-CPU data register.
///
/// # Safety
///
/// `tp` must be the valid per-CPU area base for the current CPU, and setting
/// the architecture-specific per-CPU register to that base must be valid for
/// the current execution context.
#[cfg(not(feature = "host-test"))]
pub unsafe fn write_percpu_reg(tp: VirtAddr) {
    let tp = tp.as_usize() - expercpu_macros::percpu_symbol_vma!(_percpu_start);

    unsafe {
        cfg_if::cfg_if! {
            if #[cfg(target_arch = "x86_64")] {
                core::arch::asm!("wrgsbase {}", in(reg) tp);
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

/// Writes the host thread current per-CPU area base.
///
/// # Safety
///
/// `tp` must be a valid per-CPU area base for the current host thread.
#[cfg(feature = "host-test")]
pub unsafe fn write_percpu_reg(tp: VirtAddr) {
    host_test::set_current_base(tp);
}

// #[allow(unused_imports)]
// use crate as expercpu;
