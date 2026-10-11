#![no_std]
#![no_main]

use ecraos::*;

#[app_main_bsp]
fn main() {
    kprintln!("Hello, ecraOS App!");
}
