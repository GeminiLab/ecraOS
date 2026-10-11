//! AArch64 firmware device-information source discovery.

use alloc::vec::Vec;

use crate::device::DeviceInfoSource;

/// Returns the device-tree source supplied by the bootloader.
pub fn probe_device_info_sources(arg: ecraldr_base::PlatformBootArg) -> Vec<DeviceInfoSource> {
    match arg {
        ecraldr_base::PlatformBootArg::DeviceTree(address) => {
            alloc::vec![DeviceInfoSource::DeviceTree(address)]
        }
        _ => Vec::new(),
    }
}
