#![doc = include_str!("../README.md")]

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::{Error, ItemStatic};

mod arch;

/// Converts a syntax error into compiler-error tokens.
fn compiler_error(err: Error) -> TokenStream {
    err.to_compile_error().into()
}

/// Defines a per-CPU static variable.
///
/// It should be used on a `static` variable definition.
///
/// See the documentation of the [expercpu](https://docs.rs/expercpu) crate for more details.
#[proc_macro_attribute]
pub fn def_percpu(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return compiler_error(Error::new(
            Span::call_site(),
            "expect an empty attribute: `#[def_percpu]`",
        ));
    }

    let ast = syn::parse_macro_input!(item as ItemStatic);

    let attrs = &ast.attrs;
    let vis = &ast.vis;
    let name = &ast.ident;
    let ty = &ast.ty;
    let init_expr = &ast.expr;

    let inner_symbol_name = &format_ident!("__PERCPU_{}", name);
    let struct_name = &format_ident!("{}_WRAPPER", name);

    let ty_str = quote!(#ty).to_string();
    let is_primitive_int = ["bool", "u8", "u16", "u32", "u64", "usize"].contains(&ty_str.as_str());

    let no_preempt_guard = if cfg!(feature = "preempt") {
        quote! { let _guard = expercpu :: __priv::NoPreemptGuard::new(); }
    } else {
        quote! {}
    };
    let current_access_note = if cfg!(feature = "preempt") {
        "Disables preemption for the duration of the call."
    } else {
        "The caller must keep execution on the same CPU for the call."
    };
    let current_raw_safety_note = if cfg!(feature = "host-test") {
        "The current host thread must remain associated with a live per-CPU area."
    } else {
        "Caller must ensure that preemption is disabled on the current CPU."
    };

    // Do not generate `fn read_current()`, `fn write_current()`, etc for non primitive types.
    let read_write_methods = if is_primitive_int {
        let read_current_raw = arch::gen_read_current_raw(inner_symbol_name, ty);
        let write_current_raw =
            arch::gen_write_current_raw(inner_symbol_name, &format_ident!("val"), ty);

        quote! {
            /// Returns the value of the per-CPU static variable on the current CPU.
            ///
            /// # Safety
            ///
            #[doc = #current_raw_safety_note]
            #[inline]
            pub unsafe fn read_current_raw(&self) -> #ty {
                #read_current_raw
            }

            /// Writes the value of the per-CPU static variable on the current CPU.
            ///
            /// # Safety
            ///
            #[doc = #current_raw_safety_note]
            #[inline]
            pub unsafe fn write_current_raw(&self, val: #ty) {
                #write_current_raw
            }

            #[doc = "Returns the value of the per-CPU static variable on the current CPU."]
            #[doc = ""]
            #[doc = #current_access_note]
            pub fn read_current(&self) -> #ty {
                #no_preempt_guard
                unsafe { self.read_current_raw() }
            }

            #[doc = "Writes the value of the per-CPU static variable on the current CPU."]
            #[doc = ""]
            #[doc = #current_access_note]
            pub fn write_current(&self, val: #ty) {
                #no_preempt_guard
                unsafe { self.write_current_raw(val) }
            }
        }

        // TODO: Add remote read and write helpers when they are needed.
    } else {
        quote! {}
    };

    let symbol_vma = arch::gen_symbol_vma(inner_symbol_name);
    let offset = arch::gen_offset(inner_symbol_name);
    let current_ptr = arch::gen_current_ptr(inner_symbol_name, ty);
    let remote_methods = if cfg!(feature = "remote-access") {
        quote! {
            /// Returns the raw pointer of this per-CPU static variable on the given CPU.
            ///
            /// # Safety
            ///
            /// Caller must ensure that the CPU ID is valid, the returned remote
            /// area is accessible, and data races will not happen.
            #[inline]
            pub unsafe fn remote_ptr(&self, cpu_id: usize) -> *const #ty {
                let base = expercpu :: percpu_area_base_for(cpu_id);
                let offset = #offset;
                (base.as_usize() + offset) as *const #ty
            }

            /// Returns a reference to the per-CPU static variable on the given CPU.
            ///
            /// # Safety
            ///
            /// Caller must satisfy the safety requirements of [`Self::remote_ptr`].
            #[inline]
            pub unsafe fn remote_ref_raw(&self, cpu_id: usize) -> &#ty {
                unsafe { &*self.remote_ptr(cpu_id) }
            }

            /// Returns a mutable reference to the per-CPU static variable on the given CPU.
            ///
            /// # Safety
            ///
            /// Caller must satisfy the safety requirements of [`Self::remote_ptr`]
            /// and ensure unique mutable access.
            #[inline]
            #[allow(clippy::mut_from_ref)]
            pub unsafe fn remote_ref_mut_raw(&self, cpu_id: usize) -> &mut #ty {
                unsafe { &mut *(self.remote_ptr(cpu_id) as *mut #ty) }
            }
        }
    } else {
        quote! {}
    };
    quote! {
        #[doc(hidden)]
        #[cfg_attr(not(target_os = "macos"), unsafe(link_section = ".percpu"))] // unimplemented on macos
        #(#attrs)*
        static mut #inner_symbol_name: #ty = #init_expr;

        #[doc = concat!("The per-CPU data wrapper for ", stringify!(#name), ".")]
        #[allow(non_camel_case_types)]
        #vis struct #struct_name {}

        #[doc = concat!("The per-CPU accessor for ", stringify!(#name), ".")]
        #(#attrs)*
        #vis static #name: #struct_name = #struct_name {};

        impl #struct_name {
            /// Returns the virtual memory address of this per-CPU static
            /// variable in the `.percpu` section.
            ///
            /// The address is the same across all CPUs.
            #[inline]
            pub fn symbol_vma(&self) -> usize {
                #symbol_vma
            }

            /// Returns the offset relative to the per-CPU data area base.
            #[inline]
            pub fn offset(&self) -> usize {
                #offset
            }

            /// Returns the raw pointer of this per-CPU static variable on the current CPU.
            ///
            /// # Safety
            ///
            #[doc = #current_raw_safety_note]
            #[inline]
            pub unsafe fn current_ptr(&self) -> *const #ty {
                #current_ptr
            }

            /// Returns the reference of the per-CPU static variable on the current CPU.
            ///
            /// # Safety
            ///
            #[doc = #current_raw_safety_note]
            #[inline]
            pub unsafe fn current_ref_raw(&self) -> &#ty {
                &*self.current_ptr()
            }

            /// Returns the mutable reference of the per-CPU static variable on the current CPU.
            ///
            /// # Safety
            ///
            #[doc = #current_raw_safety_note]
            #[inline]
            #[allow(clippy::mut_from_ref)]
            pub unsafe fn current_ref_mut_raw(&self) -> &mut #ty {
                &mut *(self.current_ptr() as *mut #ty)
            }

            /// Provides mutable access to the per-CPU data on the current CPU.
            ///
            #[doc = #current_access_note]
            pub fn with_current<F, T>(&self, f: F) -> T
            where
                F: FnOnce(&mut #ty) -> T,
            {
                #no_preempt_guard
                f(unsafe { self.current_ref_mut_raw() })
            }

            /// Resets the per-CPU data on the current CPU to its initial value.
            ///
            #[doc = #current_access_note]
            ///
            /// If the initial expression has side effects, they may be executed.
            pub fn reset_to_init(&self) {
                self.with_current(|val| unsafe {
                    *val = #init_expr;
                });
            }

            #remote_methods

            #read_write_methods
        }
    }
    .into()
}

/// Expands a linker symbol virtual address expression.
#[doc(hidden)]
#[proc_macro]
pub fn percpu_symbol_vma(item: TokenStream) -> TokenStream {
    let symbol = &format_ident!("{}", item.to_string());
    let offset = arch::gen_symbol_vma(symbol);
    quote!({ #offset }).into()
}
