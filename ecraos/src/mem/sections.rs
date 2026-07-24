//! Infomations about the sections of the kernel binary.

macro_rules! sections {
    ($(
        $name:ident ($name_aligned:ident) => $start_symbol:ident .. $end_symbol:ident .. $aligned_end_symbol:ident, $flag_fn:ident: $flag0:ident $(| $flags:ident)*
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

            #[doc = concat!("Returns the flags of the ", stringify!($name), " section.")]
            #[doc = ""]
            #[doc = "The flags are constant and do not change after relocation."]
            pub const fn $flag_fn() -> exarch::mem::MemoryRegionFlags {
                exarch::mem::MemoryRegionFlags::$flag0
                $(
                    .union(exarch::mem::MemoryRegionFlags::$flags)
                )*
            }
        )*

        #[doc = "The number of sections in the kernel binary."]
        pub const SECTION_COUNT: usize = {
            [$(stringify!($name)),*].len()
        };

        /// A section of the kernel binary.
        #[derive(Debug, Clone, Copy)]
        pub struct Section {
            /// The name of the section.
            pub name: &'static str,
            /// The address range of the section.
            pub range: memory_addr::VirtAddrRange,
            /// The address range of the section with padding bytes at the end.
            pub aligned_range: memory_addr::VirtAddrRange,
            /// The flags of the section.
            pub flags: exarch::mem::MemoryRegionFlags,
        }

        #[doc = "Returns a list of all sections with their names and ranges."]
        #[doc = ""]
        #[doc = "The returned list is not constant and may change after relocation."]
        pub fn all_sections() -> [Section; SECTION_COUNT] {
            [$((Section { name: stringify!($name), range: $name(), aligned_range: $name_aligned(), flags: $flag_fn() })),*]
        }
    };
}

sections![
    text (text_aligned) => _stext .. _etext .. _ftext, text_flags: READ | EXECUTE,
    rodata (rodata_aligned) => _srodata .. _erodata .. _frodata, rodata_flag: READ,
    percpu (percpu_aligned) => _spercpu .. _epercpu .. _fpercpu, percpu_flag: READ,
    data (data_aligned) => _sdata .. _edata .. _fdata, data_flag: READ | WRITE,
    rela_dyn (rela_dyn_aligned) => _srela_dyn .. _erela_dyn .. _frela_dyn, rela_dyn_flag: READ | WRITE,
    got (got_aligned) => _sgot .. _egot .. _fgot, got_flag: READ | WRITE,
    bss (bss_aligned) => _sbss .. _ebss .. _fbss, bss_flag: READ | WRITE,
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

/// Clears the BSS section.
///
/// This function should be called after the relocation of the kernel image.
pub fn clear_bss() {
    let bss_range = bss();
    let bss_slice =
        unsafe { core::slice::from_raw_parts_mut(bss_range.start.as_mut_ptr(), bss_range.size()) };

    bss_slice.fill(0);
}
