//! Power-off via the QEMU ACPI PM control port (`0x604`).

use explat::{crate_interface, power::PowerIf};

/// Type tag for [`crate_interface::impl_interface`] wiring to [`explat::power::PowerIf`].
pub struct PowerImpl;

#[crate_interface::impl_interface]
impl PowerIf for PowerImpl {
    fn poweroff() -> ! {
        unsafe {
            const POWEROFF_PORT: u16 = 0x604;
            const POWEROFF_VALUE: u16 = 0x2000;
            core::arch::asm!(
                "outw %ax, (%dx)",
                in("ax") POWEROFF_VALUE,
                in("dx") POWEROFF_PORT,
                options(att_syntax),
            );

            loop {
                core::arch::asm!("hlt", options(nomem, nostack));
            }
        }
    }
}
