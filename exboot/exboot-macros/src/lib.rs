//! Proc macros for marking and calling the explat kernel entry point from platform code.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Ident, ItemFn, parse_macro_input};

/// Exported symbol name for the kernel entry (`export_name` / `extern "Rust"`).
const fn kernel_entry_export_name() -> &'static str {
    "__exboot_kernel_entry"
}

/// Builds the `syn` identifier for the exported kernel entry symbol (`__exboot_kernel_entry`).
fn kernel_entry_export_ident() -> Ident {
    format_ident!("{}", kernel_entry_export_name())
}

/// Marks a function as the kernel entry function, which will be called by the
/// platform crate by [`call_kernel_entry`] once the very early initialization
/// is complete.
///
/// The kernel entry function should then further initialize the platform by
/// methods defined in `InitIf` trait.
#[proc_macro_attribute]
pub fn kernel_entry(attr: TokenStream, input: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return quote! {
            compile_error!("#[kernel_entry] does not accept attributes");
        }
        .into();
    }

    let kernel_entry = parse_macro_input!(input as ItemFn);
    let kernel_entry_name = kernel_entry.sig.ident.clone();
    let kernel_entry_export_name = kernel_entry_export_name();

    quote! {
        #[unsafe(export_name = #kernel_entry_export_name)]
        #[unsafe(link_section = ".text.kernel_entry")]
        #kernel_entry

        #[doc(hidden)]
        #[allow(clippy::unused_unit)]
        const KERNEL_ENTRY_SIGNATURE_GUARD: () = {
            let _kernel_entry_must_match_signature: ::exboot::KernelEntryType = #kernel_entry_name;
            ()
        };
    }
    .into()
}

/// Calls the kernel entry function marked by [`kernel_entry`]. Two arguments
/// should be passed to the function: `hart_id` (Hardware Thread ID of the
/// bootstrap processor) and `arg` (the architecture-and-platform-specific
/// argument that will later be passed to the `InitIf::init_early` method).
///
/// The kernel entry function should be called by the platform crate with the
/// following conditions met:
/// - A pagetable providing identical mapping and covering the whole kernel
///   image must be enabled on the bootstrap processor.
/// - Interrupts must be globally disabled on the bootstrap processor.
///
/// The platform crate must call the kernel entry function as early as possible
/// before any other initialization code is executed.
#[proc_macro]
pub fn call_kernel_entry(input: TokenStream) -> TokenStream {
    let input = TokenStream2::from(input);
    let kernel_entry_ident = kernel_entry_export_ident();

    quote! {
        {
            unsafe extern "Rust" {
                fn #kernel_entry_ident(hart_id: usize, arg: ::exboot::BootArg) -> !;
            }

            unsafe {
                #kernel_entry_ident(#input)
            }
        }
    }
    .into()
}
