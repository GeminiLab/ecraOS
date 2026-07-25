use alloc::vec::Vec;

use crate_interface::def_interface;
use memory_addr::PhysAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceInfoSource {
    DeviceTree(PhysAddr),
    ACPI(PhysAddr),
}

pub struct DeviceInfoToBeImplemented {}

#[def_interface(gen_caller)]
pub trait DeviceIf {
    fn probe_device_info_sources(arg: ecraos_boot::PlatformBootArg) -> Vec<DeviceInfoSource>;
}
