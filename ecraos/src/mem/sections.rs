//! Kernel image section information.
//!
//! # Sections and symbols
//!
//! This module provides information about sections in the kernel image that are used by the kernel
//! itself during runtime. These sections do not exactly correspond to ELF sections. For example,
//! the `rodata` section also covers the `.init_array` section. Currently there are 7 sections:
//!
//! | Section | Description |
//! | ------- | ----------- |
//! | text | The compiled kernel code. |
//! | rodata | The read-only data. |
//! | percpu | The initial data for per-CPU variables. |
//! | data | The initialized writable global data. |
//! | rela_dyn | The dynamic relocation entries. |
//! | got | The global offset table. |
//! | bss | The zero-initialized global data. |
//!
//! This module uses symbols defined in the linker script (`ecraos/link.ld`) to locate these
//! sections. Each section has three linker symbols marking its start, true end, and aligned end.
//! The alignment is specified by the linker script, currently 64 KiB, and does not depend on the
//! runtime page size.
//!
//! # Relocation compatibility
//!
//! This module resolves symbol addresses with PC-relative addressing. Queries return the current
//! runtime ranges, which will change after relocation to a new address. Previously returned ranges
//! and [`Section`] values do not update automatically.
//!
//! # Usage inside the kernel crate
//!
//! Initialization code uses this module to map the kernel image properly and clear the BSS section.

use exarch::mem::MemoryRegionFlags;
use memory_addr::{AddrRangeBounds, VirtAddrRange};

/// Defines sections from symbols and access flags.
macro_rules! sections {
    ($(
        $name:ident ($name_aligned:ident) =>
        $start_symbol:ident .. $end_symbol:ident .. $aligned_end_symbol:ident,
        $flags_fn:ident: $flag0:ident $(| $flags:ident)*
    ),* $(,)?) => {
        unsafe extern "C" {
            $(
                #[doc = concat!("Marks the start of the ", stringify!($name), " section.")]
                fn $start_symbol();
                #[doc = concat!("Marks the content end of the ", stringify!($name), " section.")]
                fn $end_symbol();
                #[doc = concat!("Marks the padded end of the ", stringify!($name), " section.")]
                fn $aligned_end_symbol();
            )*
        }

        $(
            #[doc = concat!(
                "Returns the address range actually used by the ", stringify!($name), " section."
            )]
            #[doc = ""]
            #[doc = concat!(
                "Compared to [`", stringify!($name_aligned), "`], the returned range is usually ",
                "smaller because it excludes trailing alignment padding. The ranges are equal ",
                "when the content end is already aligned."
            )]
            pub fn $name() -> VirtAddrRange {
                // SAFETY: The linker script guarantees `$end_symbol >= $start_symbol`.
                // Relocation does not change their relative order.
                unsafe {
                    VirtAddrRange::new_unchecked(
                        $crate::get_symbol_addr!($start_symbol).into(),
                        $crate::get_symbol_addr!($end_symbol).into(),
                    )
                }
            }

            #[doc = concat!(
                "Returns the address range occupied by the ", stringify!($name), " section."
            )]
            #[doc = ""]
            #[doc = concat!(
                "Compared to [`", stringify!($name), "`], the returned range is usually larger ",
                "because it includes trailing alignment padding. The ranges are equal when the ",
                "content end is already aligned."
            )]
            pub fn $name_aligned() -> VirtAddrRange {
                // SAFETY: The linker script guarantees `$aligned_end_symbol >= $start_symbol`.
                // Relocation does not change their relative order.
                unsafe {
                    VirtAddrRange::new_unchecked(
                        $crate::get_symbol_addr!($start_symbol).into(),
                        $crate::get_symbol_addr!($aligned_end_symbol).into(),
                    )
                }
            }

            #[doc = concat!(
                "Returns the mapping flags for the ", stringify!($name), " section."
            )]
            #[doc = ""]
            #[doc = concat!(
                "The flags are constant and have the semantics described by ",
                "[`Section::flags`]."
            )]
            pub const fn $flags_fn() -> MemoryRegionFlags {
                MemoryRegionFlags::$flag0
                $(
                    .union(MemoryRegionFlags::$flags)
                )*
            }
        )*

        #[doc = "The number of sections returned by [`all_sections`]."]
        pub const SECTION_COUNT: usize = {
            [$(stringify!($name)),*].len()
        };

        /// A snapshot of one kernel image section.
        #[derive(Debug, Clone, Copy)]
        pub struct Section {
            /// The name of the section.
            pub name: &'static str,
            /// The virtual address range excluding trailing alignment padding.
            pub range: VirtAddrRange,
            /// The virtual address range including trailing linker alignment padding.
            pub aligned_range: VirtAddrRange,
            /// The access flags used to map the section.
            ///
            /// It describes the intended attributes rather than querying current page-table
            /// permissions. It is determined by the kernel itself as a constant. [`super::pmm`]
            /// adds [`MemoryRegionFlags::KERNEL`] when processing the sections.
            pub flags: MemoryRegionFlags,
        }

        #[doc = "Returns a snapshot of all declared sections in linker order."]
        #[doc = ""]
        #[doc = "The array includes each section even when it is empty."]
        pub fn all_sections() -> [Section; SECTION_COUNT] {
            [$(Section {
                name: stringify!($name),
                range: $name(),
                aligned_range: $name_aligned(),
                flags: $flags_fn(),
            }),*]
        }
    };
}

sections![
    text (text_aligned) => _stext .. _etext .. _ftext, text_flags: READ | EXECUTE,
    rodata (rodata_aligned) => _srodata .. _erodata .. _frodata, rodata_flags: READ,
    percpu (percpu_aligned) => _spercpu .. _epercpu .. _fpercpu, percpu_flags: READ,
    data (data_aligned) => _sdata .. _edata .. _fdata, data_flags: READ | WRITE,
    rela_dyn (rela_dyn_aligned) => _srela_dyn .. _erela_dyn .. _frela_dyn, rela_dyn_flags: READ | WRITE,
    got (got_aligned) => _sgot .. _egot .. _fgot, got_flags: READ | WRITE,
    bss (bss_aligned) => _sbss .. _ebss .. _fbss, bss_flags: READ | WRITE,
];

/// Returns the entire kernel image range, including alignment padding.
///
/// Spans `_skernel.._ekernel`, covering all sections returned by [`all_sections`].
pub fn kernel_range() -> VirtAddrRange {
    unsafe extern "C" {
        /// Marks the start of the linked kernel image.
        fn _skernel();
        /// Marks the padded end of the linked kernel image.
        fn _ekernel();
    }

    // SAFETY: The linker script guarantees `_ekernel >= _skernel`.
    // Relocation does not change their relative order.
    unsafe {
        VirtAddrRange::new_unchecked(
            crate::get_symbol_addr!(_skernel).into(),
            crate::get_symbol_addr!(_ekernel).into(),
        )
    }
}

/// Clears BSS contents, including the reserved BSP per-CPU early slot.
///
/// Zeros [`bss`] without clearing trailing alignment padding.
///
/// # Safety
///
/// The BSP must call this once after initial relocation, before initializing per-CPU storage,
/// using BSS-backed globals, or starting APs. Interrupts must be disabled. The BSS range must be
/// mapped writable, with no live references or concurrent accesses to it.
pub(crate) unsafe fn clear_bss() {
    let bss_range = bss();
    // SAFETY: The caller guarantees writable memory and exclusive access. The linker script
    // guarantees the BSS range is valid.
    let bss_slice =
        unsafe { core::slice::from_raw_parts_mut(bss_range.start.as_mut_ptr(), bss_range.size()) };

    bss_slice.fill(0);
}
