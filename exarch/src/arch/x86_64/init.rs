//! Platform initialization functions for x86_64.

use crate::init::PlatformBootArg;

pub fn init_early(_arg: PlatformBootArg) {
    super::init_early();
}

pub fn init_later() {
    super::init_later();
}

pub fn init_early_ap() {
    super::init_early_ap();
}

pub fn init_later_ap() {
    super::init_later_ap();
}
