use crate::device::DeviceIf;

#[expect(dead_code)]
struct DeviceImpl;

#[crate_interface::impl_interface]
impl DeviceIf for DeviceImpl {}
