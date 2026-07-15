//! Global Descriptor Table (GDT) definitions.
//!
//! Original code from `axcpu` v0.3.1.

use core::fmt;

use lazyinit::LazyInit;
use x86_64::instructions::tables::{lgdt, load_tss};
use x86_64::registers::segmentation::{CS, Segment, SegmentSelector};
use x86_64::structures::gdt::{Descriptor, DescriptorFlags};
use x86_64::structures::{DescriptorTablePointer, tss::TaskStateSegment};
use x86_64::{PrivilegeLevel, addr::VirtAddr};

#[unsafe(no_mangle)]
#[expercpu::def_percpu]
static TSS: TaskStateSegment = TaskStateSegment::new();

#[expercpu::def_percpu]
pub(super) static GDT: LazyInit<GdtStruct> = LazyInit::new();

/// The number of entries in the [GDT struct](GdtStruct).
pub const GDT_STRUCT_ENTRY_COUNT: usize = 16;

/// A wrapper of the Global Descriptor Table (GDT) with maximum 16 entries.
#[repr(align(16))]
#[derive(Clone)]
pub struct GdtStruct {
    table: [u64; GDT_STRUCT_ENTRY_COUNT],
}

impl GdtStruct {
    /// The index of the kernel code segment for 32-bit mode in the GDT.
    pub const KCODE32_INDEX: u16 = 1;
    /// The index of the kernel code segment for 64-bit mode in the GDT.
    pub const KCODE64_INDEX: u16 = 2;
    /// The index of the kernel data segment in the GDT.
    pub const KDATA_INDEX: u16 = 3;
    /// The index of the user code segment for 32-bit mode in the GDT.
    pub const UCODE32_INDEX: u16 = 4;
    /// The index of the user data segment in the GDT.
    pub const UDATA_INDEX: u16 = 5;
    /// The index of the user code segment for 64-bit mode in the GDT.
    pub const UCODE64_INDEX: u16 = 6;
    /// The index of the TSS segment (low) in the GDT.
    pub const TSS_INDEX_LOW: u16 = 7;
    /// The index of the TSS segment (high) in the GDT.
    pub const TSS_INDEX_HIGH: u16 = 8;

    /// Kernel code segment for 32-bit mode.
    pub const KCODE32_SELECTOR: SegmentSelector =
        SegmentSelector::new(Self::KCODE32_INDEX, PrivilegeLevel::Ring0);
    /// Kernel code segment for 64-bit mode.
    pub const KCODE64_SELECTOR: SegmentSelector =
        SegmentSelector::new(Self::KCODE64_INDEX, PrivilegeLevel::Ring0);
    /// Kernel data segment.
    pub const KDATA_SELECTOR: SegmentSelector =
        SegmentSelector::new(Self::KDATA_INDEX, PrivilegeLevel::Ring0);
    /// User code segment for 32-bit mode.
    #[expect(unused)]
    pub const UCODE32_SELECTOR: SegmentSelector =
        SegmentSelector::new(Self::UCODE32_INDEX, PrivilegeLevel::Ring3);
    /// User data segment.
    #[expect(unused)]
    pub const UDATA_SELECTOR: SegmentSelector =
        SegmentSelector::new(Self::UDATA_INDEX, PrivilegeLevel::Ring3);
    /// User code segment for 64-bit mode.
    #[expect(unused)]
    pub const UCODE64_SELECTOR: SegmentSelector =
        SegmentSelector::new(Self::UCODE64_INDEX, PrivilegeLevel::Ring3);
    /// TSS segment.
    pub const TSS_SELECTOR: SegmentSelector =
        SegmentSelector::new(Self::TSS_INDEX_LOW, PrivilegeLevel::Ring0);

    /// Constructs a new GDT struct that filled with the default segment
    /// descriptors, including the given TSS segment.
    pub fn new(tss: &'static TaskStateSegment) -> Self {
        let mut table = [0; GDT_STRUCT_ENTRY_COUNT];
        // first 3 entries are the same as in multiboot.S
        table[Self::KCODE32_INDEX as usize] = DescriptorFlags::KERNEL_CODE32.bits(); // 0x00cf9b000000ffff
        table[Self::KCODE64_INDEX as usize] = DescriptorFlags::KERNEL_CODE64.bits(); // 0x00af9b000000ffff
        table[Self::KDATA_INDEX as usize] = DescriptorFlags::KERNEL_DATA.bits(); // 0x00cf93000000ffff
        table[Self::UCODE32_INDEX as usize] = DescriptorFlags::USER_CODE32.bits(); // 0x00cffb000000ffff
        table[Self::UDATA_INDEX as usize] = DescriptorFlags::USER_DATA.bits(); // 0x00cff3000000ffff
        table[Self::UCODE64_INDEX as usize] = DescriptorFlags::USER_CODE64.bits(); // 0x00affb000000ffff
        if let Descriptor::SystemSegment(low, high) = Descriptor::tss_segment(tss) {
            table[Self::TSS_INDEX_LOW as usize] = low;
            table[Self::TSS_INDEX_HIGH as usize] = high;
        }
        Self { table }
    }

    /// Returns the GDT pointer (base and limit) that can be used in `lgdt`
    /// instruction.
    pub fn pointer(&self) -> DescriptorTablePointer {
        DescriptorTablePointer {
            // SAFETY: We use `VirtAddr::new_unsafe` because `x86_64` does not support 57-bit
            // virtual addresses which we want to use.
            //
            // TODO: Find a better solution, or submit a PR to `x86_64` crate.
            base: unsafe { VirtAddr::new_unsafe(self.table.as_ptr() as u64) },
            limit: (core::mem::size_of_val(&self.table) - 1) as u16,
        }
    }

    /// Loads the GDT into the CPU (executes the `lgdt` instruction), and
    /// updates the code segment register (`CS`).
    ///
    /// # Safety
    ///
    /// This function is unsafe because it manipulates the CPU's privileged
    /// states.
    pub unsafe fn load(&'static self) {
        unsafe {
            lgdt(&self.pointer());
            CS::set_reg(Self::KCODE64_SELECTOR);
        }
    }

    /// Loads the TSS into the CPU (executes the `ltr` instruction).
    ///
    /// # Safety
    ///
    /// This function is unsafe because it manipulates the CPU's privileged
    /// states.
    pub unsafe fn load_tss(&'static self) {
        unsafe {
            load_tss(Self::TSS_SELECTOR);
        }
    }
}

impl fmt::Debug for GdtStruct {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("GdtStruct")
            .field("pointer", &self.pointer())
            .field("table", &self.table)
            .finish()
    }
}

/// Initializes the per-CPU TSS and GDT structures and loads them into the current CPU.
pub fn init_gdt() {
    unsafe {
        let gdt = GDT.current_ref_raw();
        gdt.init_once(GdtStruct::new(TSS.current_ref_raw()));
        gdt.load();
        gdt.load_tss();
    }
}

/// Reloads the GDT into the current CPU.
///
/// This function should be called after a relocation.
pub fn reload_gdt() {
    unsafe {
        let gdt = GDT.current_ref_raw();
        gdt.load();
    }
}
