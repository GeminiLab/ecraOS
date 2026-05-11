//! Handles the relocation of the kernel.

use crate::kernel_entry;

/// Gets the runtime (and current) address of a symbol, with
/// architecture-specific PC-relative addressing instructions.
///
/// It's safe to use this macro even when relocation is not performed.
#[macro_export]
macro_rules! get_symbol_addr {
    ($symbol:expr) => {{
        let addr: usize;
        #[cfg(target_arch = "x86_64")]
        core::arch::asm!(
            "lea {result}, [rip + {symbol}]",
            result = out(reg) addr,
            symbol = sym $symbol,
            options(nostack, preserves_flags, readonly)
        );

        #[cfg(target_arch = "aarch64")]
        core::arch::asm!(
            "adrp {result}, {symbol}",
            "add {result}, {result}, :lo12:{symbol}",
            result = out(reg) addr,
            symbol = sym $symbol,
            options(nostack, preserves_flags, readonly)
        );

        #[cfg(target_arch = "riscv64")]
        core::arch::asm!(
            "la {result}, {symbol}",
            result = out(reg) addr,
            symbol = sym $symbol,
            options(nostack, preserves_flags, readonly)
        );

        #[cfg(target_arch = "loongarch64")]
        core::arch::asm!(
            "la.pcrel {result}, {symbol}",
            result = out(reg) addr,
            symbol = sym $symbol,
            options(nostack, preserves_flags, readonly)
        );

        addr
    }};
}

/// A relocation entry in the `.rela.dyn` section in an ELF64 file.
#[repr(C)]
struct Elf64Rela {
    /// The virtual address (VMA, during linking) of the place to be patched.
    offset: u64,
    /// The symbol table index and the relocation type.
    info: u64,
    /// The constant addend to calculate the value to be patched.
    addend: i64,
}

/// The value of R_<current_arch>_RELATIVE relocation type.
const RELATIVE_TYPE: u32 = {
    #[cfg(target_arch = "x86_64")]
    {
        8
    } // R_X86_64_RELATIVE
    #[cfg(target_arch = "aarch64")]
    {
        1027
    } // R_AARCH64_RELATIVE
    #[cfg(target_arch = "riscv64")]
    {
        3
    } // R_RISCV_RELATIVE
    #[cfg(target_arch = "loongarch64")]
    {
        3
    } // R_LARCH_RELATIVE
    #[cfg(not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "loongarch64",
    )))]
    panic!("Unsupported target architecture");
};

/// Handles the relocation of the kernel, by patching the kernel according to
/// the `R_<current_arch>_RELATIVE` entries from the `.rela.dyn` section.
///
/// # Notes
///
/// ## VMA Assumption
///
/// This function assumes that during linking, [`kernel_entry`] is placed at the
/// very beginning of the kernel image, and at VMA 0.
///
/// The linker script of this crate should handle this correctly. Take care when
/// updating it.
///
/// ## GOT Prevention
///
/// This function must be **naturally place-independent**, i.e. it must not
/// access the GOT or PLT indirections. Updates to this function must be
/// carefully scrutinized to ensure that this invariant is maintained.
///
/// Even the most trivial calls to core library functions can break this. For
/// example, the following is a wrong implementation:
///
/// ```
/// pub unsafe fn relocate_me() {
///     unsafe extern "C" {
///         fn _srela_dyn();
///         fn _erela_dyn();
///     }
///
///     // The kernel entry is at VMA 0, so the load bias is the actual address of
///     // the kernel entry.
///     let load_bias = unsafe { get_symbol_addr!(crate::kernel_entry) };
///     let srela_dyn_addr = unsafe { get_symbol_addr!(_srela_dyn) };
///     let erela_dyn_addr = unsafe { get_symbol_addr!(_erela_dyn) };
///
///     let rela_base = srela_dyn_addr as *const Elf64Rela;
///     let rela_cnt = (erela_dyn_addr - srela_dyn_addr) / core::mem::size_of::<Elf64Rela>();
///
///     for i in 0..rela_cnt {
///         let rela = unsafe { rela_base.add(i).as_ref_unchecked() };
///         if rela.info as u32 == RELATIVE_TYPE {
///             let target = (load_bias + rela.addend as usize) as *mut u64;
///             unsafe {
///                 target.write((load_bias as u64).wrapping_add_signed(rela.addend));
///             }
///         }
///     }
/// }
/// ```
#[inline(never)]
pub unsafe fn relocate_me() {
    unsafe extern "C" {
        fn _srela_dyn();
        fn _erela_dyn();
    }

    unsafe {
        // `kernel_entry` is linked at VMA 0, the runtime base is where that
        // address landed in memory.
        let load_bias = get_symbol_addr!(kernel_entry);
        let srela = get_symbol_addr!(_srela_dyn);
        let erela = get_symbol_addr!(_erela_dyn);
        let rela_stride = core::mem::size_of::<Elf64Rela>();

        let mut p = srela;
        while p < erela {
            let rela_ptr = p as *const Elf64Rela;
            let offset = (*rela_ptr).offset;
            let info = (*rela_ptr).info;
            let addend = (*rela_ptr).addend;

            // ELF64 r_type is the low 32 bits of r_info.
            if info as u32 == RELATIVE_TYPE {
                let target = (load_bias + offset as usize) as *mut u64;
                let value = (load_bias as i64).wrapping_add(addend) as u64;
                target.write(value);
            }

            p += rela_stride;
        }
    }
}

pub mod sections;
