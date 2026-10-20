# `size_disp`

`size_disp` formats unsigned byte counts as human-readable IEC binary units. The crate is `no_std`
and has no dependencies.

## Unit selection

All formats scale values with powers of 1024 and use IEC unit symbols such as `KiB` and `MiB`.
Values are rounded to two decimal places, so a value just below a unit transition can round to the
next displayed number, such as 1000.00 before a compact transition or 1024.00 before a natural one.

| Bytes | Wide | Compact | Natural |
| ---: | --- | --- | --- |
| 999 | `999.00   B` | `999.00B` | `999.00B` |
| 1000 | `  0.98 KiB` | `0.98KiB` | `1000.00B` |
| 1024 | `  1.00 KiB` | `1.00KiB` | `1.00KiB` |
| 1,023,000 | `999.02 KiB` | `999.02KiB` | `999.02KiB` |
| 1,024,000 | `  0.98 MiB` | `0.98MiB` | `1000.00KiB` |
| 1,048,576 | `  1.00 MiB` | `1.00MiB` | `1.00MiB` |

- `size_display_wide` aligns the number and unit into columns. It advances to the next unit when the
  unrounded size value reaches 1000.
- `size_display_compact` omits alignment padding and uses the same 1000 transition threshold.
- `size_display_natural` omits alignment padding and advances at powers of 1024, such as 1024
  bytes per KiB.

## Usage

Import the `SizeDisplay` trait to use the formatting methods on unsigned integer byte counts:

```rust
use size_disp::SizeDisplay;

let sizes = [999u64, 1000, 1024, 1_023_000, 1_024_000, 1_048_576];
let wide = [
    "999.00   B",
    "  0.98 KiB",
    "  1.00 KiB",
    "999.02 KiB",
    "  0.98 MiB",
    "  1.00 MiB",
];
let compact = ["999.00B", "0.98KiB", "1.00KiB", "999.02KiB", "0.98MiB", "1.00MiB"];
let natural = ["999.00B", "1000.00B", "1.00KiB", "999.02KiB", "1000.00KiB", "1.00MiB"];

for (bytes, expected) in sizes.into_iter().zip(wide) {
    assert_eq!(format!("{}", bytes.size_display_wide()), expected);
}
for (bytes, expected) in sizes.into_iter().zip(compact) {
    assert_eq!(format!("{}", bytes.size_display_compact()), expected);
}
for (bytes, expected) in sizes.into_iter().zip(natural) {
    assert_eq!(format!("{}", bytes.size_display_natural()), expected);
}
```

The trait is implemented for `u8`, `u16`, `u32`, `u64`, and `usize`.
