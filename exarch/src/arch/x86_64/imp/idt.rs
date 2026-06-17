//! Interrupt Descriptor Table (IDT) definitions.
//!
//! Original code from `axcpu` v0.3.1.

use core::cell::UnsafeCell;
use core::fmt;

use lazyinit::LazyInit;
use x86_64::addr::VirtAddr;
use x86_64::instructions::tables::lidt;
use x86_64::structures::DescriptorTablePointer;
use x86_64::structures::idt::{Entry, HandlerFunc, InterruptDescriptorTable};

const NUM_INT: usize = 256;

static IDT: LazyInit<IdtStruct> = LazyInit::new();

/// A wrapper of the Interrupt Descriptor Table (IDT).
#[repr(transparent)]
pub struct IdtStruct {
    table: UnsafeCell<InterruptDescriptorTable>,
}

unsafe impl Sync for IdtStruct {}

impl IdtStruct {
    /// Constructs a new IDT struct with no entries set.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let idt = Self {
            table: UnsafeCell::new(InterruptDescriptorTable::new()),
        };

        // We should not fill the IDT with the current virtual address of the trap handlers here,
        // because the local variable `idt` may be **NOT** properly aligned for some mysterious
        // reason, which has been observed in practice.

        idt
    }

    /// Fills the IDT with the current virtual address of the trap handlers.
    ///
    /// This function should be called after the relocation of the trap handlers.
    ///
    /// # Safety
    ///
    /// This function should never be called concurrently.
    pub unsafe fn fill_handlers(&self) {
        unsafe extern "C" {
            #[link_name = "trap_handler_offset_table"]
            static OFFSETS: [usize; NUM_INT];
            fn _trap_handlers();
        }
        // SAFETY: The caller promises it.
        let entries = unsafe {
            let ptr = self.table.get() as *mut Entry<HandlerFunc>;
            core::slice::from_raw_parts_mut(ptr, NUM_INT)
        };

        for i in 0..NUM_INT {
            let handler = unsafe { (_trap_handlers as *const fn()).byte_add(OFFSETS[i]) };
            #[allow(clippy::missing_transmute_annotations)]
            let opt = entries[i].set_handler_fn(unsafe { core::mem::transmute(handler) });
            if i == 0x3 || i == 0x80 {
                // enable user space breakpoints and legacy int 0x80 syscall
                opt.set_privilege_level(x86_64::PrivilegeLevel::Ring3);
            }
        }
    }

    /// Returns the IDT pointer (base and limit) that can be used in the `lidt`
    /// instruction.
    pub fn pointer(&self) -> DescriptorTablePointer {
        DescriptorTablePointer {
            // SAFETY: We use `VirtAddr::new_unsafe` because `x86_64` does not support 57-bit
            // virtual addresses which we want to use.
            //
            // TODO: Find a better solution, or submit a PR to `x86_64` crate.
            base: unsafe { VirtAddr::new_unsafe(self.table.get() as u64) },
            limit: (core::mem::size_of::<InterruptDescriptorTable>() - 1) as u16,
        }
    }

    /// Loads the IDT into the CPU (executes the `lidt` instruction).
    ///
    /// # Safety
    ///
    /// This function is unsafe because it manipulates the CPU's privileged
    /// states.
    pub unsafe fn load(&'static self) {
        unsafe { lidt(&self.pointer()) };
    }
}

impl fmt::Debug for IdtStruct {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("IdtStruct")
            .field("pointer", &self.pointer())
            .field("table", &self.table)
            .finish()
    }
}

/// Initializes the global IDT and loads it into the current CPU.
pub fn init_idt() {
    IDT.call_once(IdtStruct::new);
    // SAFETY: Initialization runs in early single-threaded context before the IDT is loaded.
    unsafe {
        IDT.fill_handlers();
        IDT.load()
    };
}

pub fn reload_idt() {
    // SAFETY: Reloading are called in a single-threaded context (BSP only).
    unsafe {
        IDT.fill_handlers();
        IDT.load()
    };
}
