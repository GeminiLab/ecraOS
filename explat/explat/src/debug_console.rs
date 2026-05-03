use crate::crate_interface::def_interface;

#[def_interface(gen_caller)]
pub trait DebugConsoleIf {
    fn write_bytes(bytes: &[u8]);
    fn read_bytes(bytes: &mut [u8]) -> usize;
}

pub fn write_str<S: AsRef<str>>(s: S) {
    let s = s.as_ref();
    let b = s.as_bytes();
    write_bytes(b);
}
