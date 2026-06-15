use crate_interface::def_interface;
use memory_addr::PhysAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceInfoSource {
    DeviceTree(PhysAddr),
    ACPI(PhysAddr),
}

#[def_interface(gen_caller)]
pub trait DeviceIf {}
