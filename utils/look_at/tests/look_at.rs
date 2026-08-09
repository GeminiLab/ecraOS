//! Integration tests for the `look_at` attribute macro.
//!
//! The tests compile and invoke generated wrappers for every supported signature category and
//! module layout.

use core::ops::Add;

use look_at::look_at;

/// Architecture-specific implementations used by the forwarding wrappers.
///
/// Each function has a corresponding declaration transformed by `look_at`.
mod arch {
    use core::ops::Add;

    /// Adds two values.
    ///
    /// This implementation exercises type-generic forwarding and a where clause.
    pub(crate) fn generic<T>(left: T, right: T) -> T
    where
        T: Add<Output = T>,
    {
        left + right
    }

    /// Creates a fixed-size array.
    ///
    /// This implementation exercises const-generic forwarding.
    pub(crate) fn array<const N: usize>(value: u8) -> [u8; N] {
        [value; N]
    }

    /// Returns a reference with its input lifetime.
    ///
    /// This implementation exercises lifetime-generic forwarding.
    #[allow(clippy::needless_lifetimes)]
    pub(crate) fn borrow<'a, T: ?Sized>(value: &'a T) -> &'a T {
        value
    }

    /// Reads a value through a raw pointer.
    ///
    /// The generated wrapper must preserve unsafety and call this function in an unsafe block.
    pub(crate) unsafe fn read(pointer: *const u32) -> u32 {
        // SAFETY: The caller provides the validity guarantee.
        unsafe { *pointer }
    }

    /// Doubles a value through the C ABI.
    ///
    /// The generated wrapper and signature check must preserve the ABI.
    pub(crate) extern "C" fn abi(value: u32) -> u32 {
        value * 2
    }

    /// Increments a value through a module wrapper.
    ///
    /// This implementation is reached through a retained wrapper module.
    pub(crate) fn nested(value: u32) -> u32 {
        value + 1
    }

    /// Decrements a value through a flattened module wrapper.
    ///
    /// This implementation is reached through a wrapper emitted into the parent module.
    pub(crate) fn flattened(value: u32) -> u32 {
        value - 1
    }
}

/// Adds two generic values through the selected implementation.
///
/// The declaration is replaced with a forwarding body by `look_at`.
#[look_at(crate::arch)]
fn generic<T>(left: T, right: T) -> T
where
    T: Add<Output = T>;

/// Creates a const-generic array through the selected implementation.
///
/// The const argument must be forwarded in the target function's turbofish.
#[look_at(crate::arch)]
fn array<const N: usize>(value: u8) -> [u8; N];

/// Borrows a generic value through the selected implementation.
///
/// The lifetime is inferred when calling the target function.
#[allow(clippy::needless_lifetimes)]
#[look_at(crate::arch)]
fn borrow<'a, T: ?Sized>(value: &'a T) -> &'a T;

/// Reads a pointer through the selected unsafe implementation.
///
/// The generated declaration remains unsafe for callers.
#[look_at(crate::arch)]
unsafe fn read(pointer: *const u32) -> u32;

/// Doubles a value through the selected C ABI implementation.
///
/// The generated declaration retains its ABI.
#[look_at(crate::arch)]
extern "C" fn abi(value: u32) -> u32;

/// Wrappers retained inside their declared module.
///
/// The generated module keeps its name and contains forwarding function bodies.
#[look_at(crate::arch)]
mod wrappers {
    /// Increments a value through the selected implementation.
    ///
    /// The declaration's visibility allows its parent test module to invoke it.
    pub(super) fn nested(value: u32) -> u32;
}

/// Wrappers emitted directly into the parent module.
///
/// The generated output omits this module because `flatten` is present.
#[look_at(crate::arch, flatten)]
mod flattened_wrappers {
    /// Decrements a value through the selected implementation.
    ///
    /// This declaration becomes a function in the parent test module.
    fn flattened(value: u32) -> u32;
}

/// Verifies forwarding for supported standalone function signatures.
///
/// The assertions cover type, const and lifetime generics, unsafety, and the C ABI.
#[test]
fn forwards_supported_function_signatures() {
    assert_eq!(generic(2_u64, 3), 5);
    assert_eq!(array::<3>(4), [4, 4, 4]);

    let text = String::from("borrowed");
    assert_eq!(borrow(&text), "borrowed");

    let value = 7;
    // SAFETY: `value` remains alive for the duration of the call.
    assert_eq!(unsafe { read(&raw const value) }, 7);

    assert_eq!(abi(4), 8);
}

/// Verifies retained and flattened module expansion.
///
/// Both module layouts must call the matching target function.
#[test]
fn forwards_module_declarations() {
    assert_eq!(wrappers::nested(4), 5);
    assert_eq!(flattened(4), 3);
}
