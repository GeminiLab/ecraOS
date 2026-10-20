#![doc = include_str!("../README.md")]
#![no_std]

use core::fmt::{Display, Formatter, Result as FmtResult};

/// The number of bytes in a kibibyte.
///
/// This crate uses powers of 1024 when converting between byte-size units.
const KIB: u64 = 1024;

/// The number of bytes in a mebibyte.
///
/// This value is one kibibyte multiplied by 1024.
const MIB: u64 = KIB * 1024;

/// The number of bytes in a gibibyte.
///
/// This value is one mebibyte multiplied by 1024.
const GIB: u64 = MIB * 1024;

/// The number of bytes in a tebibyte.
///
/// This value is one gibibyte multiplied by 1024.
const TIB: u64 = GIB * 1024;

/// The number of bytes in a pebibyte.
///
/// This value is one tebibyte multiplied by 1024.
const PIB: u64 = TIB * 1024;

/// The number of bytes in an exbibyte.
///
/// This value is one pebibyte multiplied by 1024.
const EIB: u64 = PIB * 1024;

/// The unrounded value at which compact displays advance to the next unit.
///
/// Unit sizes remain powers of 1024. Two-decimal rounding can show 1000.00 just before
/// a transition.
const COMPACT_UNIT_THRESHOLD: u64 = 1000;

/// A binary size unit.
///
/// Values are formatted with the corresponding IEC unit symbols.
#[derive(Copy, Clone)]
enum Unit {
    /// A byte.
    ///
    /// This is the base unit for all size values.
    Byte,
    /// A kibibyte.
    ///
    /// One kibibyte contains 1024 bytes.
    KiB,
    /// A mebibyte.
    ///
    /// One mebibyte contains 1024 kibibytes.
    MiB,
    /// A gibibyte.
    ///
    /// One gibibyte contains 1024 mebibytes.
    GiB,
    /// A tebibyte.
    ///
    /// One tebibyte contains 1024 gibibytes.
    TiB,
    /// A pebibyte.
    ///
    /// One pebibyte contains 1024 tebibytes.
    PiB,
    /// An exbibyte.
    ///
    /// One exbibyte contains 1024 pebibytes.
    EiB,
}

/// Provides symbol information for binary size units.
///
/// Each symbol follows IEC notation for binary units.
impl Unit {
    /// Returns the abbreviated symbol for this unit.
    ///
    /// The symbols follow IEC notation for binary units.
    const fn symbol(self) -> &'static str {
        match self {
            Unit::Byte => "B",
            Unit::KiB => "KiB",
            Unit::MiB => "MiB",
            Unit::GiB => "GiB",
            Unit::TiB => "TiB",
            Unit::PiB => "PiB",
            Unit::EiB => "EiB",
        }
    }
}

/// A byte count expressed in a binary unit.
///
/// The value stores the scaled amount and the unit used to display it.
struct SizeInUnit(f64, Unit);

/// Converts byte counts to binary size units.
///
/// The conversion methods use either natural 1024 boundaries or compact 1000 thresholds.
impl SizeInUnit {
    /// Converts a byte count using the largest unit whose value is at least one.
    ///
    /// Unit transitions occur at powers of 1024, such as 1024 bytes per kibibyte.
    const fn from_bytes_natural(bytes: u64) -> Self {
        if bytes < KIB {
            Self(bytes as f64, Unit::Byte)
        } else if bytes < MIB {
            Self(bytes as f64 / KIB as f64, Unit::KiB)
        } else if bytes < GIB {
            Self(bytes as f64 / MIB as f64, Unit::MiB)
        } else if bytes < TIB {
            Self(bytes as f64 / GIB as f64, Unit::GiB)
        } else if bytes < PIB {
            Self(bytes as f64 / TIB as f64, Unit::TiB)
        } else if bytes < EIB {
            Self(bytes as f64 / PIB as f64, Unit::PiB)
        } else {
            Self(bytes as f64 / EIB as f64, Unit::EiB)
        }
    }

    /// Converts a byte count using a threshold of 1000 for each unit transition.
    ///
    /// Each unit still represents a power of 1024 bytes. The threshold keeps values compact.
    const fn from_bytes_compact(bytes: u64) -> Self {
        if bytes < COMPACT_UNIT_THRESHOLD {
            Self(bytes as f64, Unit::Byte)
        } else if bytes < COMPACT_UNIT_THRESHOLD * KIB {
            Self(bytes as f64 / KIB as f64, Unit::KiB)
        } else if bytes < COMPACT_UNIT_THRESHOLD * MIB {
            Self(bytes as f64 / MIB as f64, Unit::MiB)
        } else if bytes < COMPACT_UNIT_THRESHOLD * GIB {
            Self(bytes as f64 / GIB as f64, Unit::GiB)
        } else if bytes < COMPACT_UNIT_THRESHOLD * TIB {
            Self(bytes as f64 / TIB as f64, Unit::TiB)
        } else if bytes < COMPACT_UNIT_THRESHOLD * PIB {
            Self(bytes as f64 / PIB as f64, Unit::PiB)
        } else {
            Self(bytes as f64 / EIB as f64, Unit::EiB)
        }
    }
}

/// An aligned size display.
///
/// The value is right-aligned in a six-character field, followed by a spaced unit symbol.
///
/// # Examples
///
/// ```rust
/// use size_disp::{SizeDisplay, SizeDisplayWide};
///
/// let sizes = [999u64, 1000, 1024, 1_023_000, 1_024_000, 1_048_576];
/// let displays: [SizeDisplayWide; 6] =
///     sizes.map(|bytes| bytes.size_display_wide());
/// let expected = [
///     "999.00   B",
///     "  0.98 KiB",
///     "  1.00 KiB",
///     "999.02 KiB",
///     "  0.98 MiB",
///     "  1.00 MiB",
/// ];
/// for (display, expected) in displays.into_iter().zip(expected) {
///     assert_eq!(format!("{display}"), expected);
/// }
/// ```
pub struct SizeDisplayWide(u64);

/// Formats an aligned size display.
///
/// The value uses compact unit selection and separates the number from its unit symbol.
impl Display for SizeDisplayWide {
    /// Formats the byte count with aligned numeric and unit columns.
    ///
    /// Unit transitions use the compact 1000 threshold and binary unit sizes.
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        let SizeInUnit(size, unit) = SizeInUnit::from_bytes_compact(self.0);
        let symbol = unit.symbol();
        write!(f, "{size:>6.2} {symbol:>3}")
    }
}

/// A compact size display.
///
/// The value has no alignment padding and advances to the next unit at a threshold of 1000.
///
/// # Examples
///
/// ```rust
/// use size_disp::{SizeDisplay, SizeDisplayCompact};
///
/// let sizes = [999u64, 1000, 1024, 1_023_000, 1_024_000, 1_048_576];
/// let displays: [SizeDisplayCompact; 6] =
///     sizes.map(|bytes| bytes.size_display_compact());
/// let expected = [
///     "999.00B",
///     "0.98KiB",
///     "1.00KiB",
///     "999.02KiB",
///     "0.98MiB",
///     "1.00MiB",
/// ];
/// for (display, expected) in displays.into_iter().zip(expected) {
///     assert_eq!(format!("{display}"), expected);
/// }
/// ```
pub struct SizeDisplayCompact(u64);

/// Formats a compact size display.
///
/// The value uses compact unit selection and has no alignment padding.
impl Display for SizeDisplayCompact {
    /// Formats the byte count without alignment padding.
    ///
    /// Unit transitions use a threshold of 1000 and binary unit sizes.
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        let SizeInUnit(size, unit) = SizeInUnit::from_bytes_compact(self.0);
        let symbol = unit.symbol();
        write!(f, "{size:.2}{symbol}")
    }
}

/// A naturally scaled size display.
///
/// The value has no alignment padding and advances to the next unit at powers of 1024.
///
/// # Examples
///
/// ```rust
/// use size_disp::{SizeDisplay, SizeDisplayNatural};
///
/// let sizes = [999u64, 1000, 1024, 1_023_000, 1_024_000, 1_048_576];
/// let displays: [SizeDisplayNatural; 6] =
///     sizes.map(|bytes| bytes.size_display_natural());
/// let expected = [
///     "999.00B",
///     "1000.00B",
///     "1.00KiB",
///     "999.02KiB",
///     "1000.00KiB",
///     "1.00MiB",
/// ];
/// for (display, expected) in displays.into_iter().zip(expected) {
///     assert_eq!(format!("{display}"), expected);
/// }
/// ```
pub struct SizeDisplayNatural(u64);

/// Formats a naturally scaled size display.
///
/// The value uses powers of 1024 for unit transitions and has no alignment padding.
impl Display for SizeDisplayNatural {
    /// Formats the byte count without alignment padding.
    ///
    /// Unit transitions occur at powers of 1024 and use IEC binary unit symbols.
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        let SizeInUnit(size, unit) = SizeInUnit::from_bytes_natural(self.0);
        let symbol = unit.symbol();
        write!(f, "{size:.2}{symbol}")
    }
}

/// Size display formatting.
///
/// This trait adds formatting adapters to unsigned integers that represent byte counts.
///
/// # Examples
///
/// A generic function can apply a formatting rule to each supported byte count.
///
/// ```rust
/// use size_disp::SizeDisplay;
///
/// fn compact_sizes<T: SizeDisplay>(sizes: [T; 6]) -> [String; 6] {
///     sizes.map(|bytes| format!("{}", bytes.size_display_compact()))
/// }
///
/// let actual = compact_sizes([999u64, 1000, 1024, 1_023_000, 1_024_000, 1_048_576]);
/// let expected = ["999.00B", "0.98KiB", "1.00KiB", "999.02KiB", "0.98MiB", "1.00MiB"];
/// for (actual, expected) in actual.into_iter().zip(expected) {
///     assert_eq!(actual, expected);
/// }
/// ```
pub trait SizeDisplay: Copy {
    /// Converts the byte count to u64.
    ///
    /// The returned integer represents a number of bytes.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use size_disp::SizeDisplay;
    ///
    /// assert_eq!(1024u16.to_u64(), 1024);
    /// ```
    fn to_u64(self) -> u64;

    /// Returns an aligned display adapter for the byte count.
    ///
    /// The adapter uses a 1000 threshold for unit transitions and binary unit sizes.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use size_disp::SizeDisplay;
    ///
    /// let sizes = [999u64, 1000, 1024, 1_023_000, 1_024_000, 1_048_576];
    /// let expected = [
    ///     "999.00   B",
    ///     "  0.98 KiB",
    ///     "  1.00 KiB",
    ///     "999.02 KiB",
    ///     "  0.98 MiB",
    ///     "  1.00 MiB",
    /// ];
    /// for (bytes, expected) in sizes.into_iter().zip(expected) {
    ///     assert_eq!(format!("{}", bytes.size_display_wide()), expected);
    /// }
    /// ```
    fn size_display_wide(self) -> SizeDisplayWide {
        SizeDisplayWide(self.to_u64())
    }

    /// Returns a compact display adapter for the byte count.
    ///
    /// The adapter uses a 1000 threshold for unit transitions and binary unit sizes.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use size_disp::SizeDisplay;
    ///
    /// let sizes = [999u64, 1000, 1024, 1_023_000, 1_024_000, 1_048_576];
    /// let expected = ["999.00B", "0.98KiB", "1.00KiB", "999.02KiB", "0.98MiB", "1.00MiB"];
    /// for (bytes, expected) in sizes.into_iter().zip(expected) {
    ///     assert_eq!(format!("{}", bytes.size_display_compact()), expected);
    /// }
    /// ```
    fn size_display_compact(self) -> SizeDisplayCompact {
        SizeDisplayCompact(self.to_u64())
    }

    /// Returns a naturally scaled display adapter for the byte count.
    ///
    /// The adapter advances units at powers of 1024 and uses IEC binary unit symbols.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use size_disp::SizeDisplay;
    ///
    /// let sizes = [999u64, 1000, 1024, 1_023_000, 1_024_000, 1_048_576];
    /// let expected = ["999.00B", "1000.00B", "1.00KiB", "999.02KiB", "1000.00KiB", "1.00MiB"];
    /// for (bytes, expected) in sizes.into_iter().zip(expected) {
    ///     assert_eq!(format!("{}", bytes.size_display_natural()), expected);
    /// }
    /// ```
    fn size_display_natural(self) -> SizeDisplayNatural {
        SizeDisplayNatural(self.to_u64())
    }
}

/// Implements size display formatting for unsigned integer types.
///
/// The implementations convert each supported byte count to u64.
macro_rules! impl_size_display {
    ($($t:ty),*) => {
        $(
            impl SizeDisplay for $t {
                /// Converts the byte count to u64.
                ///
                /// The returned integer represents a number of bytes.
                fn to_u64(self) -> u64 {
                    self as u64
                }
            }
        )*
    };
}

impl_size_display!(usize, u64, u32, u16, u8);
