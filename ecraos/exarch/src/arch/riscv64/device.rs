use alloc::vec::Vec;

use crate::device::DeviceInfoSource;

/// Probes RISC-V platform device information sources.
pub fn probe_device_info_sources(_arg: ecraldr_base::PlatformBootArg) -> Vec<DeviceInfoSource> {
    unimplemented!();
}
