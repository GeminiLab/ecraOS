//! Tests for architecture-specific per-CPU code generation.

#[cfg(not(feature = "host-test"))]
use quote::format_ident;

#[cfg(not(feature = "host-test"))]
use super::gen_current_ptr;

/// Verifies that EL2 uses the EL2 per-CPU register.
#[cfg(all(feature = "arm-el2", not(feature = "host-test")))]
#[test]
fn current_pointer_uses_el2_percpu_register() {
    let generated = gen_current_ptr(&format_ident!("__PERCPU_VALUE"), &syn::parse_quote!(usize));

    assert!(
        generated.to_string().contains("TPIDR_EL2"),
        "generated code did not select TPIDR_EL2: {generated}"
    );
}

/// Verifies that the default AArch64 mode uses the EL1 per-CPU register.
#[cfg(all(not(feature = "arm-el2"), not(feature = "host-test")))]
#[test]
fn current_pointer_uses_el1_percpu_register() {
    let generated = gen_current_ptr(&format_ident!("__PERCPU_VALUE"), &syn::parse_quote!(usize));

    assert!(
        generated.to_string().contains("TPIDR_EL1"),
        "generated code did not select TPIDR_EL1: {generated}"
    );
}
