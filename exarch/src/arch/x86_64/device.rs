use alloc::vec::Vec;

use crate::device::{DeviceIf, DeviceInfoSource};

struct DeviceImpl;

#[crate_interface::impl_interface]
impl DeviceIf for DeviceImpl {
    fn probe_device_info_sources(_arg: ecraldr_base::PlatformBootArg) -> Vec<DeviceInfoSource> {
        unimplemented!();
    }
}
