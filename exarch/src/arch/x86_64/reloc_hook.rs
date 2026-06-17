use crate::reloc_hook::RelocHookIf;

pub struct RelocHookImpl;

#[crate_interface::impl_interface]
impl RelocHookIf for RelocHookImpl {
    fn before_reloc() {}

    fn after_reloc() {
        super::after_reloc();
    }
}
