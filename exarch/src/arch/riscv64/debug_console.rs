use memory_addr::VirtAddr;

use crate::{debug_console::DebugConsoleIf, kernel_if::virt_to_phys};

/// The maximum number of bytes that can be read at once.
const MAX_RW_SIZE: usize = 256;

/// Tries to write bytes to the console from input u8 slice.
/// Returns the number of bytes written.
fn try_write_bytes(bytes: &[u8]) -> usize {
    let ret = sbi_rt::console_write(sbi_rt::Physical::new(
        bytes.len().min(MAX_RW_SIZE),
        virt_to_phys(VirtAddr::from_ptr_of(bytes.as_ptr())).as_usize(),
        0,
    ));
    if ret.error == 0 { ret.value } else { 0 }
}

struct DebugConsoleImpl;

#[crate_interface::impl_interface]
impl DebugConsoleIf for DebugConsoleImpl {
    /// Writes bytes to the console from input u8 slice.
    fn write_bytes(bytes: &[u8]) {
        let mut write_len = 0;
        let mut buf = [0; MAX_RW_SIZE];
        while write_len < bytes.len() {
            let n = buf.len().min(bytes.len() - write_len);
            if n == 0 {
                break;
            }
            // `bytes` can be from user space, copy it into a kernel buffer
            // to correctly use `virt_to_phys`.
            buf[..n].copy_from_slice(&bytes[write_len..write_len + n]);
            let n = try_write_bytes(&buf[..n]);
            if n == 0 {
                break;
            }
            write_len += n;
        }
    }

    /// Reads bytes from the console into the given mutable slice.
    /// Returns the number of bytes read.
    fn read_bytes(bytes: &mut [u8]) -> usize {
        sbi_rt::console_read(sbi_rt::Physical::new(
            bytes.len().min(MAX_RW_SIZE),
            virt_to_phys(VirtAddr::from_mut_ptr_of(bytes.as_mut_ptr())).as_usize(),
            0,
        ))
        .value
    }
}
