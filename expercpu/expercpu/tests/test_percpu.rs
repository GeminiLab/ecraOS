#![cfg(target_os = "linux")]

use expercpu::*;
use memory_addr::VirtAddr;

#[def_percpu]
static BOOL: bool = true;

#[def_percpu]
static U8: u8 = 1;

#[def_percpu]
static U16: u16 = 2;

#[def_percpu]
static U32: u32 = 3;

#[def_percpu]
static U64: u64 = 4;

#[def_percpu]
static USIZE: usize = 5;

struct TestStruct {
    foo: usize,
    bar: u8,
}

#[def_percpu]
static STRUCT: TestStruct = TestStruct { foo: 6, bar: 7 };

fn alloc_area() -> (std::alloc::Layout, *mut u8) {
    let layout = expercpu::percpu_area_layout();
    let base = unsafe { std::alloc::alloc_zeroed(layout) };
    assert!(!base.is_null());
    (layout, base)
}

fn keep_active_area_for_process_lifetime(_layout: std::alloc::Layout, _base: *mut u8) {
    // The Linux test writes GS to this per-CPU area. Keep it alive for the
    // process lifetime so GS never points at freed memory after a test returns.
}

fn assert_initial_values() {
    assert!(BOOL.read_current());
    assert_eq!(U8.read_current(), 1);
    assert_eq!(U16.read_current(), 2);
    assert_eq!(U32.read_current(), 3);
    assert_eq!(U64.read_current(), 4);
    assert_eq!(USIZE.read_current(), 5);
    STRUCT.with_current(|s| {
        assert_eq!(s.foo, 6);
        assert_eq!(s.bar, 7);
    });
}

#[test]
fn init_sets_current_base_and_copies_initial_values() {
    let (layout, base) = alloc_area();
    let base_va = VirtAddr::from_usize(base as usize);

    // SAFETY: `base` was allocated with `percpu_area_layout`, is writable,
    // aligned, and distinct from the linked `.percpu` initial image. Setting
    // the current CPU's per-CPU register is valid for this Linux test thread.
    unsafe { expercpu::init(base_va) };

    assert_eq!(expercpu::read_percpu_reg(), base_va);
    // SAFETY: `init` above set this test thread's per-CPU register to the live
    // allocation, and these calls only compute raw pointers for offset checks.
    unsafe {
        assert_eq!(
            BOOL.current_ptr() as usize,
            base_va.as_usize() + BOOL.offset()
        );
        assert_eq!(U8.current_ptr() as usize, base_va.as_usize() + U8.offset());
        assert_eq!(
            U16.current_ptr() as usize,
            base_va.as_usize() + U16.offset()
        );
        assert_eq!(
            U32.current_ptr() as usize,
            base_va.as_usize() + U32.offset()
        );
        assert_eq!(
            U64.current_ptr() as usize,
            base_va.as_usize() + U64.offset()
        );
        assert_eq!(
            USIZE.current_ptr() as usize,
            base_va.as_usize() + USIZE.offset()
        );
        assert_eq!(
            STRUCT.current_ptr() as usize,
            base_va.as_usize() + STRUCT.offset()
        );
    }

    assert_initial_values();
    keep_active_area_for_process_lifetime(layout, base);
}

#[test]
fn current_accessors_read_write_and_reset_current_cpu_area() {
    let (layout, base) = alloc_area();

    // SAFETY: `base` was allocated with `percpu_area_layout`, is writable,
    // aligned, and distinct from the linked `.percpu` initial image. Setting
    // the current CPU's per-CPU register is valid for this Linux test thread.
    unsafe { expercpu::init(VirtAddr::from_usize(base as usize)) };

    BOOL.write_current(false);
    U8.write_current(123);
    U16.write_current(0xabcd);
    U32.write_current(0xdead_beef);
    U64.write_current(0xa2ce_a2ce_a2ce_a2ce);
    USIZE.write_current(0xffff_0000);
    STRUCT.with_current(|s| {
        s.foo = 0x2333;
        s.bar = 100;
    });

    assert!(!BOOL.read_current());
    assert_eq!(U8.read_current(), 123);
    assert_eq!(U16.read_current(), 0xabcd);
    assert_eq!(U32.read_current(), 0xdead_beef);
    assert_eq!(U64.read_current(), 0xa2ce_a2ce_a2ce_a2ce);
    assert_eq!(USIZE.read_current(), 0xffff_0000);
    STRUCT.with_current(|s| {
        assert_eq!(s.foo, 0x2333);
        assert_eq!(s.bar, 100);
    });

    BOOL.reset_to_init();
    U8.reset_to_init();
    U16.reset_to_init();
    U32.reset_to_init();
    U64.reset_to_init();
    USIZE.reset_to_init();
    STRUCT.reset_to_init();
    assert_initial_values();

    keep_active_area_for_process_lifetime(layout, base);
}

#[cfg(feature = "remote-access")]
mod remote_tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use crate::{BOOL, STRUCT, TestStruct, U8, U16, U32, U64, USIZE};
    use expercpu::PerCPUAreaIf;
    use memory_addr::VirtAddr;

    static CPU0_BASE: AtomicUsize = AtomicUsize::new(0);
    static CPU1_BASE: AtomicUsize = AtomicUsize::new(0);

    struct TestPerCPUArea;

    #[crate_interface::impl_interface]
    impl PerCPUAreaIf for TestPerCPUArea {
        fn percpu_area_base_for(cpu_id: usize) -> VirtAddr {
            let base = match cpu_id {
                0 => CPU0_BASE.load(Ordering::SeqCst),
                1 => CPU1_BASE.load(Ordering::SeqCst),
                _ => panic!("unexpected CPU ID {cpu_id}"),
            };
            VirtAddr::from_usize(base)
        }
    }

    #[test]
    fn remote_access_uses_interface_base() {
        let (layout0, base0) = super::alloc_area();
        let stride = expercpu::percpu_area_layout().size();
        let (layout1, base1) = loop {
            let candidate = super::alloc_area();
            if candidate.1 as usize != base0 as usize + stride {
                break candidate;
            }
            super::keep_active_area_for_process_lifetime(candidate.0, candidate.1);
        };
        assert_ne!(base1 as usize, base0 as usize + stride);

        CPU0_BASE.store(base0 as usize, Ordering::SeqCst);
        CPU1_BASE.store(base1 as usize, Ordering::SeqCst);

        // SAFETY: `base0` was allocated with `percpu_area_layout`, is writable,
        // aligned, distinct from the linked `.percpu` initial image, and this
        // Linux test may set the current thread GS base.
        unsafe { expercpu::init(VirtAddr::from_usize(base0 as usize)) };

        // SAFETY: `base0` and `base1` are distinct live allocations created
        // with `percpu_area_layout`, and `base1` is writable for the full
        // per-CPU area size copied from initialized `base0`.
        unsafe {
            core::ptr::copy_nonoverlapping(base0, base1, expercpu::percpu_area_size());
        }

        // SAFETY: `PerCPUAreaIf` maps CPU 0 to live `base0` and CPU 1 to live
        // `base1`, which contains a copied initialized per-CPU area. This test
        // has unique ownership of `base1`, so mutable remote references do not
        // alias with other accesses, and both allocations are intentionally kept
        // alive for the process lifetime below.
        unsafe {
            assert_eq!(BOOL.remote_ptr(0) as usize, base0 as usize + BOOL.offset());
            assert_eq!(BOOL.remote_ptr(1) as usize, base1 as usize + BOOL.offset());
            assert!(*BOOL.remote_ref_raw(1));
            assert_eq!(*U8.remote_ref_raw(1), 1);
            assert_eq!(*U16.remote_ref_raw(1), 2);
            assert_eq!(*U32.remote_ref_raw(1), 3);
            assert_eq!(*U64.remote_ref_raw(1), 4);
            assert_eq!(*USIZE.remote_ref_raw(1), 5);
            assert_eq!((*STRUCT.remote_ref_raw(1)).foo, 6);
            assert_eq!((*STRUCT.remote_ref_raw(1)).bar, 7);

            *BOOL.remote_ref_mut_raw(1) = false;
            *U8.remote_ref_mut_raw(1) = 222;
            *U16.remote_ref_mut_raw(1) = 0x1234;
            *U32.remote_ref_mut_raw(1) = 0xf00d_f00d;
            *U64.remote_ref_mut_raw(1) = 0xfeed_feed_feed_feed;
            *USIZE.remote_ref_mut_raw(1) = 0x0000_ffff;
            *STRUCT.remote_ref_mut_raw(1) = TestStruct {
                foo: 0x2333,
                bar: 100,
            };

            assert!(!*BOOL.remote_ref_raw(1));
            assert_eq!(*U8.remote_ref_raw(1), 222);
            assert_eq!(*U16.remote_ref_raw(1), 0x1234);
            assert_eq!(*U32.remote_ref_raw(1), 0xf00d_f00d);
            assert_eq!(*U64.remote_ref_raw(1), 0xfeed_feed_feed_feed);
            assert_eq!(*USIZE.remote_ref_raw(1), 0x0000_ffff);
            assert_eq!((*STRUCT.remote_ref_raw(1)).foo, 0x2333);
            assert_eq!((*STRUCT.remote_ref_raw(1)).bar, 100);
        }

        super::keep_active_area_for_process_lifetime(layout0, base0);
        super::keep_active_area_for_process_lifetime(layout1, base1);
    }
}
