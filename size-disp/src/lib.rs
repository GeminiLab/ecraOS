#![no_std]

use core::fmt::{Display, Formatter, Result as FmtResult};

const KIB: u64 = 1024;
const MIB: u64 = KIB * 1024;
const GIB: u64 = MIB * 1024;
const TIB: u64 = GIB * 1024;
const PIB: u64 = TIB * 1024;
const EIB: u64 = PIB * 1024;

/// The wide version of the size display. It's a `000.00 MiB` format.
pub struct SizeDisplayWide(u64);

impl Display for SizeDisplayWide {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        let size = self.0;

        if size < 1000 {
            write!(f, "{size:>6.2}   B")
        } else if size < 1000 * KIB {
            let size_kib = size as f64 / KIB as f64;
            write!(f, "{size_kib:>6.2} KiB")
        } else if size < 1000 * MIB {
            let size_mib = size as f64 / MIB as f64;
            write!(f, "{size_mib:>6.2} MiB")
        } else if size < 1000 * GIB {
            let size_gib = size as f64 / GIB as f64;
            write!(f, "{size_gib:>6.2} GiB")
        } else if size < 1000 * TIB {
            let size_tib = size as f64 / TIB as f64;
            write!(f, "{size_tib:>6.2} TiB")
        } else if size < 1000 * PIB {
            let size_pib = size as f64 / PIB as f64;
            write!(f, "{size_pib:>6.2} PiB")
        } else {
            let size_eib = size as f64 / EIB as f64;
            write!(f, "{size_eib:>6.2} EiB")
        }
    }
}

pub trait SizeDisplay: Copy {
    fn to_u64(self) -> u64;

    fn size_display_wide(self) -> SizeDisplayWide {
        SizeDisplayWide(self.to_u64())
    }
}

macro_rules! impl_size_display {
    ($($t:ty),*) => {
        $(
            impl SizeDisplay for $t {
                fn to_u64(self) -> u64 {
                    self as u64
                }
            }
        )*
    };
}

impl_size_display!(usize, u64, u32, u16, u8);
