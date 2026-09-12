use quote::{format_ident, quote};
use syn::{Ident, Type};

fn macos_unimplemented(item: proc_macro2::TokenStream) -> proc_macro2::TokenStream {
    quote! {
        // aarch64 macOS is supported.
        #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
        { #item }
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        { unimplemented!("per-CPU variables are not supported on x86_64 macOS") }
    }
}

/// Generate a code block that calculates the virtual memory address (VMA) of
/// a symbol in the `.percpu` section based on the inner symbol name.
pub fn gen_symbol_vma(symbol: &Ident) -> proc_macro2::TokenStream {
    quote! {
        unsafe {
            let value: usize;
            #[cfg(target_arch = "x86_64")]
            ::core::arch::asm!(
                "lea {0:r}, [rip + {VAR}]", // Requires offset <= 0xffff_ffff
                out(reg) value,
                VAR = sym #symbol,
            );
            #[cfg(target_arch = "riscv64")]
            ::core::arch::asm!(
                "lla {result}, {symbol}",
                result = out(reg) value,
                symbol = sym #symbol,
            );
            #[cfg(target_arch = "aarch64")]
            ::core::arch::asm!(
                "adrp {value}, {symbol}",
                "add {value}, {value}, :lo12:{symbol}",
                value = out(reg) value,
                symbol = sym #symbol,
            );
            #[cfg(not(any(target_arch = "x86_64", target_arch = "riscv64", target_arch = "aarch64")))]
            { unimplemented!() }
            // #[cfg(target_arch = "aarch64")]
            // ::core::arch::asm!(
            //     "movz {0}, #:abs_g0_nc:{VAR}", // Requires offset <= 0xffff
            //     out(reg) value,
            //     VAR = sym #symbol,
            // );
            // #[cfg(target_arch = "arm")]
            // {
            //     let mut offset: u32;
            //     ::core::arch::asm!(
            //         "movw {0}, #:lower16:{VAR}", // Requires offset <= 0xffff_ffff
            //         "movt {0}, #:upper16:{VAR}",
            //         out(reg) offset,
            //         VAR = sym #symbol,
            //     );
            //     value = offset as usize;
            // }
            // #[cfg(any(target_arch = "loongarch64"))]
            // ::core::arch::asm!(
            //     "lu12i.w {0}, %abs_hi20({VAR})",
            //     "ori {0}, {0}, %abs_lo12({VAR})", // Requires offset <= 0xffff_ffff
            //     out(reg) value,
            //     VAR = sym #symbol,
            // );
            value
        }
    }
}

/// Generate a code block that calculates the offset of the per-CPU variable
/// based on the inner symbol name.
pub fn gen_offset(symbol: &Ident) -> proc_macro2::TokenStream {
    let symbol_vma = gen_symbol_vma(symbol);
    let symbol_vma_percpu_start = gen_symbol_vma(&format_ident!("_percpu_start"));
    quote! {
        {
            unsafe extern "C" {
                static _percpu_start: u8;
            }
            (#symbol_vma) - (#symbol_vma_percpu_start)
        }
    }
}

/// Generate a code block that calculates the pointer to the per-CPU variable on the current CPU, based on the inner
/// symbol name and the type of the variable.
pub fn gen_current_ptr(symbol: &Ident, ty: &Type) -> proc_macro2::TokenStream {
    // let aarch64_tpidr = if cfg!(feature = "arm-el2") {
    //     "TPIDR_EL2"
    // } else {
    //     // For ARM architecture, we assume running in EL1 by default,
    //     // and use `TPIDR_EL1` to store the base address of the per-CPU data area.
    //     "TPIDR_EL1"
    // };
    // let aarch64_asm = format!("mrs {{}}, {aarch64_tpidr}");

    if cfg!(feature = "host-test") {
        let offset = gen_offset(symbol);
        return quote! {
            {
                let base = expercpu::__priv::host_current_base();
                (base.as_usize() + #offset) as *const #ty
            }
        };
    }

    macos_unimplemented(quote! {
        #[cfg(target_arch = "x86_64")]
        {
            let ptr: *const #ty;
            ::core::arch::asm!(
                "lea {ptr}, [rip + {VAR}]",
                "rdgsbase {gs_base}",
                "add {ptr}, {gs_base}",
                gs_base = out(reg) _,
                ptr = out(reg) ptr,
                VAR = sym #symbol,
                options(nostack, preserves_flags, readonly),
            );
            ptr
        }
        #[cfg(target_arch = "riscv64")]
        {
            let ptr: *const #ty;
            ::core::arch::asm!(
                "lla {ptr}, {VAR}",
                "add {ptr}, {ptr}, gp",
                ptr = out(reg) ptr,
                VAR = sym #symbol,
                options(nostack, readonly),
            );
            ptr
        }
        #[cfg(target_arch = "aarch64")]
        {
            let ptr: *const #ty;
            let base: usize;
            ::core::arch::asm!(
                "adrp {ptr}, {symbol}",
                "add {ptr}, {ptr}, :lo12:{symbol}",
                "mrs {base}, TPIDR_EL1",
                "add {ptr}, {ptr}, {base}",
                ptr = out(reg) ptr,
                base = out(reg) base,
                symbol = sym #symbol,
                options(nostack, readonly),
            );
            ptr
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "riscv64", target_arch = "aarch64")))]
        { unimplemented!() }
        // #[cfg(not(target_arch = "x86_64"))]
        // {
        //     #[cfg(target_arch = "aarch64")]
        //     ::core::arch::asm!(#aarch64_asm, out(reg) base);
        //     #[cfg(target_arch = "arm")]
        //     ::core::arch::asm!("mrc p15, 0, {}, c13, c0, 4", out(reg) base);
        //     #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
        //     ::core::arch::asm!("mv {}, gp", out(reg) base);
        //     #[cfg(any(target_arch = "loongarch64"))]
        //     ::core::arch::asm!("move {}, $r21", out(reg) base);
        //     (base + self.symbol_vma()) as *const #ty
        // }
    })
}

/// Generate a code block that reads the value of the per-CPU variable on the current CPU, based on the inner symbol
/// name and the type of the variable.
///
/// The type of the variable must be one of the following: `bool`, `u8`, `u16`, `u32`, `u64`, or `usize`.
pub fn gen_read_current_raw(symbol: &Ident, ty: &Type) -> proc_macro2::TokenStream {
    if cfg!(feature = "host-test") {
        return quote! {{ unsafe { *self.current_ptr() } }};
    }

    let ty_str = quote!(#ty).to_string();
    let rv64_op = match ty_str.as_str() {
        "u8" | "bool" => "lbu",
        "u16" => "lhu",
        "u32" => "lwu",
        "u64" | "usize" => "ld",
        _ => unreachable!(),
    };
    let rv64_convert = match ty_str.as_str() {
        "bool" => quote! { value != 0 },
        "u8" => quote! { value as u8 },
        "u16" => quote! { value as u16 },
        "u32" => quote! { value as u32 },
        "u64" => quote! { value as u64 },
        "usize" => quote! { value },
        _ => unreachable!(),
    };
    let rv64_code = quote! {
        {
            let addr: usize;
            let value: usize;
            ::core::arch::asm!(
                "lla {addr}, {VAR}",
                "add {addr}, {addr}, gp",
                concat!(#rv64_op, " {value}, 0({addr})"),
                addr = out(reg) addr,
                value = out(reg) value,
                VAR = sym #symbol,
                options(nostack, readonly),
            );
            #rv64_convert
        }
    };

    // // https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html#_ldx_buhuwud_stx_bhwd
    // let la64_op = match ty_str.as_str() {
    //     "u8" | "bool" => "ldx.bu",
    //     "u16" => "ldx.hu",
    //     "u32" => "ldx.wu",
    //     "u64" | "usize" => "ldx.d",
    //     _ => unreachable!(),
    // };
    // let la64_asm = quote! {
    //     ::core::arch::asm!(
    //         "lu12i.w {0}, %abs_hi20({VAR})",
    //         "ori {0}, {0}, %abs_lo12({VAR})",
    //         concat!(#la64_op, " {0}, {0}, $r21"),
    //         out(reg) value,
    //         VAR = sym #symbol,
    //     )
    // };

    let (x64_asm, x64_reg) = if ["bool", "u8"].contains(&ty_str.as_str()) {
        (
            "mov {0}, byte ptr gs:[{1}]".into(),
            format_ident!("reg_byte"),
        )
    } else {
        let (x64_mod, x64_ptr) = match ty_str.as_str() {
            "u16" => ("x", "word"),
            "u32" => ("e", "dword"),
            "u64" | "usize" => ("r", "qword"),
            _ => unreachable!(),
        };
        (
            format!("mov {{0:{x64_mod}}}, {x64_ptr} ptr gs:[{{1}}]"),
            format_ident!("reg"),
        )
    };
    let x64_asm = quote! {
        ::core::arch::asm!(
            "lea {1:r}, [rip + {VAR}]",
            #x64_asm,
            out(#x64_reg) value,
            out(reg) _,
            VAR = sym #symbol
        )
    };

    let gen_code = |asm_stmt| {
        if ty_str.as_str() == "bool" {
            quote! {
                let value: u8;
                #asm_stmt;
                value != 0
            }
        } else {
            quote! {
                let value: #ty;
                #asm_stmt;
                value
            }
        }
    };

    // let la64_code = gen_code(la64_asm);
    let x64_code = gen_code(x64_asm);
    macos_unimplemented(quote! {
        #[cfg(target_arch = "riscv64")]
        { #rv64_code }
        // #[cfg(target_arch = "loongarch64")]
        // { #la64_code }
        #[cfg(target_arch = "x86_64")]
        { #x64_code }
        #[cfg(target_arch = "aarch64")]
        { unsafe { *self.current_ptr() } }
        #[cfg(not(any(target_arch = "riscv64", target_arch = "x86_64", target_arch = "aarch64")))]
        { unimplemented!() }
        // #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64", target_arch = "x86_64")))]
        // { *self.current_ptr() }
    })
}

/// Generate a code block that writes the value of the per-CPU variable on the current CPU, based on the inner symbol
/// name, the identifier of the value to write, and the type of the variable.
///
/// The type of the variable must be one of the following: `bool`, `u8`, `u16`, `u32`, `u64`, or `usize`.
pub fn gen_write_current_raw(symbol: &Ident, val: &Ident, ty: &Type) -> proc_macro2::TokenStream {
    if cfg!(feature = "host-test") {
        return quote! {{ unsafe { *(self.current_ptr() as *mut #ty) = #val; } }};
    }

    let ty_str = quote!(#ty).to_string();
    let ty_fixup = if ty_str.as_str() == "bool" {
        format_ident!("u8")
    } else {
        format_ident!("{}", ty_str)
    };

    let rv64_op = match ty_str.as_str() {
        "u8" | "bool" => "sb",
        "u16" => "sh",
        "u32" => "sw",
        "u64" | "usize" => "sd",
        _ => unreachable!(),
    };
    let rv64_code = quote! {
        {
            let addr: usize;
            ::core::arch::asm!(
                "lla {addr}, {VAR}",
                "add {addr}, {addr}, gp",
                concat!(#rv64_op, " {value}, 0({addr})"),
                addr = out(reg) addr,
                value = in(reg) #val as #ty_fixup,
                VAR = sym #symbol,
                options(nostack),
            );
        }
    };

    // // https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html#common-memory-access-instructions
    // let la64_op = match ty_str.as_str() {
    //     "u8" | "bool" => "stx.b",
    //     "u16" => "stx.h",
    //     "u32" => "stx.w",
    //     "u64" | "usize" => "stx.d",
    //     _ => unreachable!(),
    // };
    // let la64_code = quote! {
    //     ::core::arch::asm!(
    //         "lu12i.w {0}, %abs_hi20({VAR})",
    //         "ori {0}, {0}, %abs_lo12({VAR})",
    //         concat!(#la64_op, " {1}, {0}, $r21"),
    //         out(reg) _,
    //         in(reg) #val as #ty_fixup,
    //         VAR = sym #symbol,
    //     );
    // };

    let (x64_asm, x64_reg) = if ["bool", "u8"].contains(&ty_str.as_str()) {
        (
            "mov byte ptr gs:[{1}], {0}".into(),
            format_ident!("reg_byte"),
        )
    } else {
        let (x64_mod, x64_ptr) = match ty_str.as_str() {
            "u16" => ("x", "word"),
            "u32" => ("e", "dword"),
            "u64" | "usize" => ("r", "qword"),
            _ => unreachable!(),
        };
        (
            format!("mov {x64_ptr} ptr gs:[{{1}}], {{0:{x64_mod}}}"),
            format_ident!("reg"),
        )
    };
    let x64_code = quote! {
        ::core::arch::asm!(
            "lea {1:r}, [rip + {VAR}]",
            #x64_asm,
            in(#x64_reg) #val as #ty_fixup,
            out(reg) _,
            VAR = sym #symbol
        )
    };

    macos_unimplemented(quote! {
        #[cfg(target_arch = "riscv64")]
        { #rv64_code }
        // #[cfg(target_arch = "loongarch64")]
        // { #la64_code }
        #[cfg(target_arch = "x86_64")]
        { #x64_code }
        #[cfg(target_arch = "aarch64")]
        { unsafe { *(self.current_ptr() as *mut #ty) = #val; } }
        #[cfg(not(any(target_arch = "riscv64", target_arch = "x86_64", target_arch = "aarch64")))]
        { unimplemented!() }
        // #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64", target_arch = "x86_64")))]
        // { *(self.current_ptr() as *mut #ty) = #val }
    })
}
