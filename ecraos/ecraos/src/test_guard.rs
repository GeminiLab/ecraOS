//! Host test compile guard.
//!
//! To allow running unit tests on host, the `host-test` feature is introduced to notify lower-level
//! modules that they are running in a host test environment (`#[cfg(test)]` does not work on
//! dependencies).
//!
//! Unit tests require the `host-test` feature to be enabled, and vice versa. This module enforces
//! this requirement at compile time.
//!
//! For best compatibility with `rust-analyzer`, `#[cfg(test)]` should be used on items that should
//! only be included in unit tests (instead of `#[cfg(feature = "host-test")]`), and
//! `#[cfg(not(feature = "host-test"))]` should be used on items that should only be included in the
//! real kernel build (instead of `#[cfg(not(test))]`).
//!
//! Such patterns ensure the maximum code coverage of `rust-analyzer`.
//!
//! When it causes conflicts, use `#[cfg(feature = "host-test")]` instead of `#[cfg(test)]`.
//!
//! This rule applies only to the `ecraos` crate, lower-level modules should always use the
//! `host-test` feature.

#[cfg(all(test, not(feature = "host-test"), not(rust_analyzer)))]
compile_error!("`host-test` feature must be enabled for unit tests on host");

#[cfg(all(not(test), feature = "host-test"))]
compile_error!("`host-test` feature must not be enabled for real kernel build");

#[cfg(all(feature = "host-test", not(target_os = "linux")))]
compile_error!("host-test feature is only supported on Linux currently");
