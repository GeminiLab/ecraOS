use alloc::vec::Vec;

use look_at::look_at;
use memory_addr::PhysAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceInfoSource {
    DeviceTree(PhysAddr),
    ACPI(PhysAddr),
}

pub struct DeviceInfoToBeImplemented {}

look_at! {
    @crate::arch::current::device:

    /// Probes platform-provided device information sources.
    pub fn probe_device_info_sources(arg: ecraldr_base::PlatformBootArg) -> Vec<DeviceInfoSource>;
}
