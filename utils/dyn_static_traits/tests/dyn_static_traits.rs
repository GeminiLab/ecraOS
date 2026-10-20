//! Integration tests for the `dyn_static_traits` attribute macro.

use dyn_static_traits::dyn_static_traits;

/// A value type that does not implement `Clone`.
///
/// Tables over this type must still be cloneable because they store only function pointers.
#[derive(Debug, PartialEq, Eq)]
struct NotClone(u32);

/// A static trait whose type parameter uses the preferred constructor parameter name.
///
/// The generated constructor must choose another name for its implementation parameter.
#[dyn_static_traits(DynWrap)]
trait Wrap<T> {
    /// Wraps a value.
    ///
    /// The signature mentions the trait parameter named `T`.
    fn wrap(value: T) -> Option<T>;
}

/// An implementation of [`Wrap`] for every value type.
///
/// It always returns the value unchanged inside `Some`.
struct SomeWrapper;

/// Wraps values without changing them.
///
/// The implementation is generic so that tables can be built for any value type.
impl<T> Wrap<T> for SomeWrapper {
    /// Returns the value inside `Some`.
    ///
    /// The value is moved without being cloned.
    fn wrap(value: T) -> Option<T> {
        Some(value)
    }
}

/// A static trait whose name and parameters occupy `T`, `TA`, and `TB`.
///
/// The generated constructor must skip every occupied name and choose `TC`.
#[dyn_static_traits(DynTa)]
trait TA<T, const TB: usize> {
    /// Returns the value unchanged.
    ///
    /// The signature mentions the trait type parameter named `T`.
    fn identity(value: T) -> T;

    /// Returns the const parameter.
    ///
    /// The default body reads the trait const parameter named `TB`.
    fn capacity() -> usize {
        TB
    }
}

/// An implementation of [`TA`] for every value type.
///
/// It overrides only the required function and keeps the default `capacity`.
struct Identity;

/// Returns values unchanged.
///
/// The implementation covers every const argument.
impl<T, const TB: usize> TA<T, TB> for Identity {
    /// Returns the value unchanged.
    ///
    /// The value is moved without being cloned.
    fn identity(value: T) -> T {
        value
    }
}

/// Verifies that a trait parameter named `T` does not conflict with the constructor.
///
/// The table also stays cloneable although its type argument is not `Clone`.
#[test]
fn avoids_trait_parameter_named_t() {
    let table = DynWrap::<NotClone>::new::<SomeWrapper>();
    let copy = table.clone();
    assert_eq!((table.wrap)(NotClone(1)), Some(NotClone(1)));
    assert_eq!((copy.wrap)(NotClone(2)), Some(NotClone(2)));
}

/// Verifies that the trait name and its type and const parameters are all avoided.
///
/// The default body of `capacity` still reads the trait const parameter.
#[test]
fn avoids_trait_name_and_all_parameter_names() {
    let table = DynTa::<NotClone, 4>::new::<Identity>();
    assert_eq!((table.identity)(NotClone(3)), NotClone(3));
    assert_eq!((table.capacity)(), 4);
}
