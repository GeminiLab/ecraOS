use alloc::alloc::{alloc, dealloc};
use core::{
    alloc::Layout,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use log::{info, warn};

const REMOTE_SLAB_SMOKE_CPU: usize = 1;
const REMOTE_SLAB_SMOKE_SIZE: usize = 64;
const REMOTE_SLAB_SMOKE_ALIGN: usize = 8;

static REMOTE_SLAB_PTR: AtomicUsize = AtomicUsize::new(0);
static REMOTE_SLAB_OWNER: AtomicUsize = AtomicUsize::new(usize::MAX);
static REMOTE_SLAB_PUBLISHED: AtomicBool = AtomicBool::new(false);
static REMOTE_SLAB_FREED: AtomicBool = AtomicBool::new(false);
static REMOTE_SLAB_DONE: AtomicBool = AtomicBool::new(false);

fn remote_slab_layout() -> Layout {
    Layout::from_size_align(REMOTE_SLAB_SMOKE_SIZE, REMOTE_SLAB_SMOKE_ALIGN)
        .expect("valid remote slab smoke layout")
}

pub fn remote_slab_free_bsp() {
    if crate::mp::CPU_LIST.len() <= REMOTE_SLAB_SMOKE_CPU {
        info!("Remote slab free smoke test skipped: no AP CPU");
        return;
    }

    info!("Remote slab free smoke test started");

    while !REMOTE_SLAB_PUBLISHED.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }

    let ptr = REMOTE_SLAB_PTR.load(Ordering::Acquire) as *mut u8;
    let owner = REMOTE_SLAB_OWNER.load(Ordering::Acquire);
    assert!(!ptr.is_null(), "remote slab smoke pointer must be non-null");
    assert_ne!(
        owner,
        crate::mp::current_cpu_id(),
        "remote slab smoke object must be AP-owned"
    );

    unsafe {
        dealloc(ptr, remote_slab_layout());
    }
    info!("BSP queued remote free for CPU {}", owner);
    REMOTE_SLAB_FREED.store(true, Ordering::Release);

    while !REMOTE_SLAB_DONE.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }

    info!("Remote slab free smoke test completed");
}

pub fn remote_slab_free_ap() {
    let cpu_id = crate::mp::current_cpu_id();
    if cpu_id != REMOTE_SLAB_SMOKE_CPU {
        return;
    }

    let layout = remote_slab_layout();
    let ptr = unsafe { alloc(layout) };
    assert!(!ptr.is_null(), "remote slab smoke allocation failed");

    REMOTE_SLAB_PTR.store(ptr as usize, Ordering::Release);
    REMOTE_SLAB_OWNER.store(cpu_id, Ordering::Release);
    REMOTE_SLAB_PUBLISHED.store(true, Ordering::Release);
    info!("AP {} published slab object {:#x}", cpu_id, ptr as usize);

    while !REMOTE_SLAB_FREED.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }

    let reclaimed = unsafe { alloc(layout) };
    assert!(
        !reclaimed.is_null(),
        "remote slab smoke reclaim allocation failed"
    );

    if reclaimed as usize == ptr as usize {
        info!("AP {} reclaimed remote freed object", cpu_id);
    } else {
        warn!(
            "AP {} drained remote frees but received different object {:#x} != {:#x}",
            cpu_id, reclaimed as usize, ptr as usize
        );
    }

    unsafe {
        dealloc(reclaimed, layout);
    }
    REMOTE_SLAB_DONE.store(true, Ordering::Release);
}
