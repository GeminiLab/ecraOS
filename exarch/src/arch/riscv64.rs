use ecraldr_base::PlatformBootArg;
use fdt_rs::base::DevTree;
use lazyinit::LazyInit;
use memory_addr::va;

mod context;

pub mod debug_console;
pub mod device;
pub mod init;
pub mod irq;
pub mod mem;
pub mod power;
pub mod reloc_hook;
pub mod time;
pub mod trap;

pub use context::TrapFrame;

static DTB: LazyInit<DevTree<'_>> = LazyInit::new();

pub fn init_early_bsp(arg: PlatformBootArg) {
    // TODO: re-initialize it in the high address space after relocat
    if let PlatformBootArg::DeviceTree(dtb) = arg {
        let dtb_va = va!(dtb.as_usize());
        let dtb = unsafe {
            DevTree::from_raw_pointer(dtb_va.as_ptr()).expect("failed to parse Device Tree")
        };
        DTB.init_once(dtb);
    }

    time::init();
    trap::init_percpu();
}

/// Runs early per-hart architecture initialization.
///
/// This installs trap handling while global interrupts remain disabled.
pub fn init_early_ap() {
    trap::init_percpu();
}
