//! Infomations about the sections of the kernel binary.

macro_rules! sections {
    ($(
        $name:ident => $start_symbol:ident .. $end_symbol:ident
    ),* $(,)?) => {
        unsafe extern "C" {
            $(
                fn $start_symbol();
                fn $end_symbol();
            )*
        }

        $(
            #[doc = concat!("Returns the address range of the ", stringify!($name), " section.")]
            #[doc = ""]
            #[doc = "The returned range is not constant and may change after relocation."]
            pub fn $name() -> memory_addr::VirtAddrRange {
                unsafe {
                    memory_addr::VirtAddrRange::new_unchecked(
                        $crate::get_symbol_addr!($start_symbol).into(),
                        $crate::get_symbol_addr!($end_symbol).into(),
                    )
                }
            }
        )*

        #[doc = "The number of sections in the kernel binary."]
        pub const SECTION_COUNT: usize = {
            [$(stringify!($name)),*].len()
        };

        #[doc = "Returns a list of all sections with their names and ranges."]
        #[doc = ""]
        #[doc = "The returned list is not constant and may change after relocation."]
        pub fn all_sections() -> [(&'static str, memory_addr::VirtAddrRange); SECTION_COUNT] {
            [$((stringify!($name), $name())),*]
        }
    };
}

sections![
    text => _stext .. _etext,
    rodata => _srodata .. _erodata,
    data => _sdata .. _edata,
    rela_dyn => _srela_dyn .. _erela_dyn,
    got => _sgot .. _egot,
    bss => _sbss .. _ebss,
];

/// Returns the address range of the kernel binary.
///
/// The returned range is not constant and may change after relocation.
pub fn kernel_range() -> memory_addr::VirtAddrRange {
    unsafe extern "C" {
        fn _skernel();
        fn _ekernel();
    }

    unsafe {
        memory_addr::VirtAddrRange::new_unchecked(
            crate::get_symbol_addr!(_skernel).into(),
            crate::get_symbol_addr!(_ekernel).into(),
        )
    }
}
