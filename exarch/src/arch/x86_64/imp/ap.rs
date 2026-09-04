//! Secondary CPU initialization.

use core::mem::{MaybeUninit, offset_of};

use memory_addr::{PhysAddr, PhysAddrRange, VirtAddr};
use x86::msr::{IA32_EFER, rdmsr};
use x86_64::{
    registers::control::{Cr0, Cr4},
    structures::DescriptorTablePointer,
};

use crate::{arch::x86_64::imp::gdt::GdtStruct, power::PhysicalCpuId};

/// The index of the AP start page.
pub const AP_START_PAGE_INDEX: u8 = 6;
/// The default page size for x86_64.
const PAGE_SIZE: usize = 0x1000;
/// The physical address of the AP start page.
const AP_START_PAGE_ADDR: PhysAddr = PhysAddr::from_usize(AP_START_PAGE_INDEX as usize * PAGE_SIZE);
/// The physical address range of the AP start page.
pub const AP_START_PAGE_RANGE: PhysAddrRange = unsafe {
    PhysAddrRange::new_unchecked(
        AP_START_PAGE_ADDR,
        PhysAddr::from_usize(AP_START_PAGE_ADDR.as_usize() + PAGE_SIZE),
    )
};
/// The physical address of the AP start arguments, inside the AP start page.
///
/// The region is located at the higher half of AP start page, and is 2KiB in size. Though only a
/// small amount of data is passed to the APs.
const AP_START_ARGS_ADDR: PhysAddr =
    PhysAddr::from_usize(AP_START_PAGE_INDEX as usize * PAGE_SIZE + PAGE_SIZE / 2);

/// Arguments and temporary GDT placed in the secondary CPU start page.
struct APStartArgs {
    /// The physical CPU ID of the AP.
    pub phys_cpu_id: PhysicalCpuId,
    /// The physical address of the page table root of the AP.
    pub page_table_root: PhysAddr,
    /// The physical address of the 5-level identical page table.
    pub id_pt_pml5: PhysAddr,
    /// The physical address of the 4-level identical page table.
    pub id_pt_pml4: PhysAddr,
    /// The virtual address of the stack top of the AP.
    pub stack_top: VirtAddr,
    /// The virtual address of the entry address of the AP.
    pub entry_addr: VirtAddr,
    /// The temporary GDT descriptor.
    pub gdt_desc: DescriptorTablePointer,
    /// The temporary GDT.
    pub gdt: GdtStruct,
    /// The CR0 register of the AP.
    pub cr0: u64,
    /// The CR4 register of the AP.
    pub cr4: u64,
    /// The EFER register of the AP.
    pub efer: u64,
}

const AP_START_ARGS_OFFSET_PHYS_CPU_ID: usize = offset_of!(APStartArgs, phys_cpu_id);
const AP_START_ARGS_OFFSET_PAGE_TABLE_ROOT: usize = offset_of!(APStartArgs, page_table_root);
const AP_START_ARGS_OFFSET_ID_PT_PML5: usize = offset_of!(APStartArgs, id_pt_pml5);
const AP_START_ARGS_OFFSET_ID_PT_PML4: usize = offset_of!(APStartArgs, id_pt_pml4);
const AP_START_ARGS_OFFSET_STACK_TOP: usize = offset_of!(APStartArgs, stack_top);
const AP_START_ARGS_OFFSET_ENTRY_ADDR: usize = offset_of!(APStartArgs, entry_addr);
const AP_START_ARGS_OFFSET_GDT_DESC: usize = offset_of!(APStartArgs, gdt_desc);
#[expect(unused)]
const AP_START_ARGS_OFFSET_GDT: usize = offset_of!(APStartArgs, gdt);
const AP_START_ARGS_OFFSET_CR0: usize = offset_of!(APStartArgs, cr0);
const AP_START_ARGS_OFFSET_CR4: usize = offset_of!(APStartArgs, cr4);
const AP_START_ARGS_OFFSET_EFER: usize = offset_of!(APStartArgs, efer);

core::arch::global_asm!(
    include_str!("ap_start_page.S"),
    phys_cpu_id = const (AP_START_ARGS_ADDR.as_usize() + AP_START_ARGS_OFFSET_PHYS_CPU_ID),
    page_table_root = const (AP_START_ARGS_ADDR.as_usize() + AP_START_ARGS_OFFSET_PAGE_TABLE_ROOT),
    id_pt_pml5 = const (AP_START_ARGS_ADDR.as_usize() + AP_START_ARGS_OFFSET_ID_PT_PML5),
    id_pt_pml4 = const (AP_START_ARGS_ADDR.as_usize() + AP_START_ARGS_OFFSET_ID_PT_PML4),
    stack_top = const (AP_START_ARGS_ADDR.as_usize() + AP_START_ARGS_OFFSET_STACK_TOP),
    entry_addr = const (AP_START_ARGS_ADDR.as_usize() + AP_START_ARGS_OFFSET_ENTRY_ADDR),
    gdt_desc = const (AP_START_ARGS_ADDR.as_usize() + AP_START_ARGS_OFFSET_GDT_DESC),
    cr0 = const (AP_START_ARGS_ADDR.as_usize() + AP_START_ARGS_OFFSET_CR0),
    cr4 = const (AP_START_ARGS_ADDR.as_usize() + AP_START_ARGS_OFFSET_CR4),
    efer = const (AP_START_ARGS_ADDR.as_usize() + AP_START_ARGS_OFFSET_EFER),
    page_index = const AP_START_PAGE_INDEX,

    code32_selector = const super::gdt::GdtStruct::KCODE32_INDEX * 8,
    code64_selector = const super::gdt::GdtStruct::KCODE64_INDEX * 8,
    data_selector = const super::gdt::GdtStruct::KDATA_INDEX * 8,

    efer_msr = const IA32_EFER as u32,
    options(att_syntax),
);

/// Sets up the secondary CPU start page with the given arguments, assuming that the page is mapped
/// identically.
pub fn setup_ap_start_page(
    phys_cpu_id: PhysicalCpuId,
    page_table_root: PhysAddr,
    stack_top: VirtAddr,
    entry_addr: VirtAddr,
) {
    let ap_start_page_va = VirtAddr::from_usize(AP_START_PAGE_ADDR.as_usize());
    let ap_start_args_va = VirtAddr::from_usize(AP_START_ARGS_ADDR.as_usize());
    let ap_start_code_ptr = ap_start_page_va.as_mut_ptr();
    let ap_start_args_ptr: *mut MaybeUninit<APStartArgs> = ap_start_args_va.as_mut_ptr_of();

    // Copy the code from `ap.S` to the secondary CPU start page.
    unsafe {
        unsafe extern "C" {
            static ap_start_code_start: u8;
            static ap_start_code_end: u8;
        }

        let ap_start_code_start_ptr = &ap_start_code_start as *const u8;
        let ap_start_code_end_ptr = &ap_start_code_end as *const u8;
        let ap_start_code_size =
            ap_start_code_end_ptr.offset_from_unsigned(ap_start_code_start_ptr);

        core::slice::from_raw_parts_mut(ap_start_code_ptr, ap_start_code_size).copy_from_slice(
            core::slice::from_raw_parts(ap_start_code_start_ptr, ap_start_code_size),
        );
    }

    // Write the args.
    unsafe {
        let ap_start_args = ap_start_args_ptr
            .as_mut()
            .expect("cannot access ap_start_args");
        let ap_start_args_ptr = ap_start_args.as_mut_ptr();

        unsafe extern "C" {
            static exarch_pt_pml5: u8;
            static exarch_pt_pml4: u8;
        }

        let exarch_pt_pml5_ptr = &exarch_pt_pml5 as *const u8;
        let exarch_pt_pml4_ptr = &exarch_pt_pml4 as *const u8;

        (&raw mut (*ap_start_args_ptr).phys_cpu_id).write(phys_cpu_id);
        (&raw mut (*ap_start_args_ptr).page_table_root).write(page_table_root);
        (&raw mut (*ap_start_args_ptr).id_pt_pml5).write(PhysAddr::from_usize(
            exarch_pt_pml5_ptr as usize & u32::MAX as usize,
        ));
        (&raw mut (*ap_start_args_ptr).id_pt_pml4).write(PhysAddr::from_usize(
            exarch_pt_pml4_ptr as usize & u32::MAX as usize,
        ));
        (&raw mut (*ap_start_args_ptr).stack_top).write(stack_top);
        (&raw mut (*ap_start_args_ptr).entry_addr).write(entry_addr);

        let gdt = super::gdt::GDT
            .current_ref_raw()
            .get()
            .expect("GDT should be inited when starting up aps");
        let mut pointer = gdt.pointer();

        (&raw mut (*ap_start_args_ptr).gdt).write(gdt.clone());
        pointer.base =
            x86_64::VirtAddr::new_unsafe((&raw const (*ap_start_args_ptr).gdt).addr() as _);
        (&raw mut (*ap_start_args_ptr).gdt_desc).write(pointer);

        (&raw mut (*ap_start_args_ptr).cr0).write(Cr0::read_raw());
        (&raw mut (*ap_start_args_ptr).cr4).write(Cr4::read_raw());
        (&raw mut (*ap_start_args_ptr).efer).write(rdmsr(IA32_EFER));
    }
}
