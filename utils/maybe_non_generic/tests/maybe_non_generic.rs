//! Integration tests for the `maybe_non_generic` attribute macro.

use dyn_static_traits::dyn_static_traits;
use maybe_non_generic::maybe_non_generic;

/// A static trait that reads a clock.
///
/// Its function-pointer table replaces the clock type parameter in the generated copies.
#[dyn_static_traits(DynClock)]
trait Clock {
    /// Returns the current time.
    ///
    /// The test clocks return fixed values.
    fn now() -> u64;
}

/// A clock that always reads 100.
///
/// It is used both as a generic argument and as a table implementation.
struct BootClock;

/// Reads the boot clock.
///
/// The returned time never changes.
impl Clock for BootClock {
    /// Returns 100.
    ///
    /// The value is fixed so that the tests can compare exact results.
    fn now() -> u64 {
        100
    }
}

/// A clock that always reads 250.
///
/// It is selected at runtime next to [`BootClock`].
struct LateClock;

/// Reads the late clock.
///
/// The returned time never changes.
impl Clock for LateClock {
    /// Returns 250.
    ///
    /// The value is fixed so that the tests can compare exact results.
    fn now() -> u64 {
        250
    }
}

/// Returns the time elapsed since `start`.
///
/// The copy `elapsed_dyn` reads the clock through a borrowed table.
#[maybe_non_generic(elapsed_dyn, type(C => clock: &DynClock))]
fn elapsed<C: Clock>(start: u64) -> u64 {
    C::now() - start
}

/// Returns the elapsed time multiplied by `N`.
///
/// The copy `scaled_elapsed_dyn` passes its borrowed table on to `elapsed_dyn`.
#[maybe_non_generic(
    scaled_elapsed_dyn,
    const(N => factor: u64),
    type(C => clock: &DynClock),
    fn(elapsed => elapsed_dyn),
)]
fn scaled_elapsed<const N: u64, C: Clock>(start: u64) -> u64 {
    elapsed::<C>(start) * N
}

/// Returns the time elapsed since `start`.
///
/// The copy `elapsed_owned_dyn` takes its table by value.
#[maybe_non_generic(elapsed_owned_dyn, type(C => clock: DynClock))]
fn elapsed_owned<C: Clock>(start: u64) -> u64 {
    C::now() - start
}

/// Returns twice the time elapsed since `start`.
///
/// The copy `twice_elapsed_owned_dyn` passes a clone of its table to each call.
#[maybe_non_generic(
    twice_elapsed_owned_dyn,
    type(C => clock: DynClock),
    fn(elapsed_owned => elapsed_owned_dyn),
)]
fn twice_elapsed_owned<C: Clock>(start: u64) -> u64 {
    elapsed_owned::<C>(start) + elapsed_owned::<C>(start)
}

/// Returns the sum of `N` and a nested function result.
///
/// The nested function declares its own `N`, which must not be replaced in the copy.
#[maybe_non_generic(outer_dyn, const(N => n: usize))]
fn outer<const N: usize>() -> usize {
    /// Returns its own const parameter.
    ///
    /// It shares the name `N` with the outer function and binds the generated name `n`.
    fn inner<const N: usize>(n: usize) -> usize {
        N * n
    }

    inner::<3>(2) + N
}

/// Functions that run while another item named `Clone` is in scope.
///
/// Generated clone calls must still resolve to the standard `Clone` trait.
mod shadowed_clone {
    use super::*;

    /// A local trait that shadows the prelude `Clone`.
    ///
    /// It is never implemented.
    #[allow(dead_code)]
    trait Clone {}

    /// Returns twice the time elapsed since `start`.
    ///
    /// The copy passes clones of its borrowed table to `elapsed_dyn`.
    #[maybe_non_generic(
        twice_elapsed_dyn,
        type(C => clock: &DynClock),
        fn(elapsed => elapsed_dyn),
    )]
    pub(super) fn twice_elapsed<C: Clock>(start: u64) -> u64 {
        elapsed::<C>(start) + elapsed::<C>(start)
    }
}

/// A trait with a generic default method.
///
/// The attribute generates a second default method next to it.
trait Sizes {
    /// Returns twice `N`.
    ///
    /// The copy `size_dyn` takes `N` as its argument.
    #[maybe_non_generic(size_dyn, const(N => n: usize))]
    fn size<const N: usize>() -> usize {
        N * 2
    }
}

/// A type that uses the default methods of [`Sizes`].
///
/// It does not override either method.
struct DefaultSizes;

/// Uses the default methods.
///
/// Both the original and the generated method are inherited.
impl Sizes for DefaultSizes {}

/// Verifies that generated copies select `dyn_static_traits` tables at runtime.
///
/// The original function selects the clock at compile time, and the copy receives a table.
#[test]
fn selects_tables_at_runtime() {
    let clocks = [DynClock::new::<BootClock>(), DynClock::new::<LateClock>()];

    assert_eq!(elapsed::<BootClock>(40), 60);
    assert_eq!(elapsed_dyn(40, &clocks[0]), 60);
    assert_eq!(elapsed_dyn(40, &clocks[1]), 210);
}

/// Verifies that renamed calls receive the replaced const and type arguments.
///
/// Borrowed and owned tables are both passed on to the renamed callee.
#[test]
fn passes_replacements_to_renamed_calls() {
    let clock = DynClock::new::<BootClock>();

    assert_eq!(scaled_elapsed::<2, BootClock>(40), 120);
    assert_eq!(scaled_elapsed_dyn(40, 2, &clock), 120);
    assert_eq!(twice_elapsed_owned::<BootClock>(40), 120);
    assert_eq!(twice_elapsed_owned_dyn(40, clock), 120);
}

/// Verifies that nested items keep their own generic parameters.
///
/// The nested `N` is still a const parameter of the nested function in the copy.
#[test]
fn keeps_nested_items_unchanged() {
    assert_eq!(outer::<4>(), 10);
    assert_eq!(outer_dyn(4), 10);
}

/// Verifies that generated clone calls ignore a shadowing `Clone` item.
///
/// The copy compiles only when the clone call uses an absolute path.
#[test]
fn clones_tables_with_shadowed_clone() {
    let clock = DynClock::new::<LateClock>();

    assert_eq!(shadowed_clone::twice_elapsed::<LateClock>(50), 400);
    assert_eq!(shadowed_clone::twice_elapsed_dyn(50, &clock), 400);
}

/// Verifies that the attribute works on trait default methods.
///
/// The generated default method is available on every implementation.
#[test]
fn copies_trait_default_methods() {
    assert_eq!(DefaultSizes::size::<3>(), 6);
    assert_eq!(DefaultSizes::size_dyn(3), 6);
}
