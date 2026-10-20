//! Tests for the public size display API.
//!
//! The tests cover format output and unit transition boundaries.

use std::format;

use size_disp::{SizeDisplay, SizeDisplayCompact, SizeDisplayNatural, SizeDisplayWide};

/// Displays wide sizes with aligned values and units.
///
/// The example checks the padded output for one kibibyte.
#[test]
fn displays_wide_size() {
    let size: SizeDisplayWide = 1024usize.size_display_wide();

    assert_eq!(format!("{size}"), "  1.00 KiB");
}

/// Switches compact sizes at a value of 1000.
///
/// The test checks byte output below the threshold and kibibyte output at it.
#[test]
fn displays_compact_size() {
    let below_threshold: SizeDisplayCompact = 999usize.size_display_compact();
    let at_threshold: SizeDisplayCompact = 1000usize.size_display_compact();

    assert_eq!(format!("{below_threshold}"), "999.00B");
    assert_eq!(format!("{at_threshold}"), "0.98KiB");
}

/// Switches natural sizes at powers of 1024.
///
/// The test checks byte output below one kibibyte and kibibyte output at the boundary.
#[test]
fn displays_natural_size() {
    let below_threshold: SizeDisplayNatural = 1023usize.size_display_natural();
    let at_threshold: SizeDisplayNatural = 1024usize.size_display_natural();

    assert_eq!(format!("{below_threshold}"), "1023.00B");
    assert_eq!(format!("{at_threshold}"), "1.00KiB");
}
