//! Infomations about the sections of the kernel binary.

macro_rules! sections {
    ($(
        $name:ident ($name_aligned:ident) => $start_symbol:ident .. $end_symbol:ident .. $aligned_end_symbol:ident
    ),* $(,)?) => {
        unsafe extern "C" {
            $(
                fn $start_symbol();
                fn $end_symbol();
                fn $aligned_end_symbol();
            )*
        }

        $(
            #[doc = concat!("Returns the address range actually used by the ", stringify!($name), " section.")]
            #[doc = ""]
            #[doc = concat!("Compared to [`", stringify!($name_aligned), "`], the returned range is smaller because it does not include the padding bytes at the end of the section.")]
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

            #[doc = concat!("Returns the address range occupied by the ", stringify!($name), " section.")]
            #[doc = ""]
            #[doc = concat!("Compared to [`", stringify!($name), "`], the returned range is larger because it includes the padding bytes at the end of the section.")]
            #[doc = ""]
            #[doc = "The returned range is constant and does not change after relocation."]
            pub fn $name_aligned() -> memory_addr::VirtAddrRange {
                unsafe {
                    memory_addr::VirtAddrRange::new_unchecked(
                        $crate::get_symbol_addr!($start_symbol).into(),
                        $crate::get_symbol_addr!($aligned_end_symbol).into(),
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
        pub fn all_sections() -> [(&'static str, memory_addr::VirtAddrRange, memory_addr::VirtAddrRange); SECTION_COUNT] {
            [$((stringify!($name), $name(), $name_aligned())),*]
        }
    };
}

sections![
    text (text_aligned) => _stext .. _etext .. _ftext,
    rodata (rodata_aligned) => _srodata .. _erodata .. _frodata,
    data (data_aligned) => _sdata .. _edata .. _fdata,
    rela_dyn (rela_dyn_aligned) => _srela_dyn .. _erela_dyn .. _frela_dyn,
    got (got_aligned) => _sgot .. _egot .. _fgot,
    bss (bss_aligned) => _sbss .. _ebss .. _fbss,
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
