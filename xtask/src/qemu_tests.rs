//! Guest diagnostic regression tests.
//!
//! Covers architecture fault messages and case-insensitive fatal detection.

use crate::qemu::fatal;

/// Recognizes each supported fatal message.
///
/// Panic detection must not depend on a particular guest architecture or letter case.
#[test]
fn detects_guest_fatal_messages() {
    for text in [
        "Kernel panic: test",
        "FATAL: error",
        "triple fault",
        "unhandled supervisor instruction access fault",
    ] {
        assert!(fatal(text.as_bytes()), "missed {text}");
    }
    assert!(!fatal(b"Hello, ecraOS App!"));
}
