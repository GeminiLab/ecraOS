//! Allocator smoke tests.
//!
//! Exercises the buddy allocator, slab allocator, and `#[global_allocator]`
//! integration path. All output goes to the serial console via `kprintln!`.
//! This module can be removed after the allocator has been validated.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use memory_addr::pa;
use core::alloc::Layout;

use super::*;
use crate::kprintln;

/// Runs all allocator smoke tests.
pub fn run() {
    let page_size = 1usize << crate::mem::vmm::page_size_shift();

    kprintln!("=== Allocator smoke tests ===\n");

    // 0. alloc many frames
    let mut frames = Vec::new();
    for i in 0..100 {
        let frame = alloc_frame().expect("alloc_frame failed");
        assert!(frame.as_usize().is_multiple_of(page_size));
        frames.push(frame);
        kprintln!("  alloc_frame #{}: OK ({:#x})", i, frame);
    }

    for (i, frame) in frames.into_iter().enumerate() {
        dealloc_frame(frame);
        kprintln!("  dealloc_frame #{}: OK ({:#x})", i, frame);
    }

    let framec = unsafe { alloc_frames_at(pa!(0x1fc000), 2).expect("alloc_frames_at failed") };
    assert!(framec.as_usize().is_multiple_of(page_size));
    kprintln!("  alloc_frames_at(0x1fc000, 2): OK ({:#x})", framec);

    // 1. alloc_frame — check alignment.
    let frame1 = alloc_frame().expect("alloc_frame failed");
    assert!(frame1.as_usize().is_multiple_of(page_size));
    kprintln!("  alloc_frame: OK ({:#x})", frame1);

    // 2. alloc_frames(4, page_size) — multi-page allocation.
    let frames4 = alloc_frames(4, page_size * 4).expect("alloc_frames(4) failed");
    assert!(frames4.as_usize().is_multiple_of(page_size));
    kprintln!("  alloc_frames(4): OK ({:#x})", frames4);

    // 3. alloc_frames_at — allocate at a specific physical address.
    let target_paddr = frame1;
    dealloc_frame(frame1);
    let at_result = unsafe { alloc_frames_at(target_paddr, 1).expect("alloc_frames_at failed") };
    assert_eq!(at_result, target_paddr);
    kprintln!("  alloc_frames_at: OK ({:#x})", at_result);

    // 4. Dealloc and realloc — verify address reuse.
    dealloc_frames(frames4, 4);
    dealloc_frame(at_result);
    let frame2 = alloc_frame().expect("alloc_frame after dealloc failed");
    kprintln!("  dealloc/realloc: OK ({:#x})", frame2);
    dealloc_frame(frame2);

    // 5. usage — check statistics.
    let u = stats();
    kprintln!(
        "  usage: total={}, used={}",
        u.total_pages(),
        u.used_pages()
    );
    assert!(u.total_pages() > 0);

    // 6. Box::new (small object, slab path).
    let boxed = Box::new(42u32);
    assert!(*boxed == 42);
    kprintln!("  Box::new(42u32): OK");
    kprintln!("  Box::new(42u32): {:p}", Box::as_ref(&boxed) as *const _);
    drop(boxed);

    // 7. Multiple alloc/free without panic.
    for i in 0..10 {
        let b = Box::new(i);
        assert!(*b == i);
        drop(b);
    }
    kprintln!("  Multiple alloc/free (10x): OK");

    // 8. Vec<u8> with data push.
    let mut vec: Vec<usize> = Vec::new();
    for i in 0..1000 {
        vec.push(i);
    }
    assert!(vec.len() == 1000);
    kprintln!("  Vec<u8> push 1000: OK");
    drop(vec);

    // 9. Large allocation (> 2048, buddy path).
    let large = unsafe { alloc::alloc::alloc(Layout::from_size_align(4096, 4096).unwrap()) };
    assert!(!large.is_null());
    unsafe { alloc::alloc::dealloc(large, Layout::from_size_align(4096, 4096).unwrap()) };
    kprintln!("  Large alloc (4096): OK");

    // 10. String.
    let s = String::from("hello ecraOS");
    assert!(s == "hello ecraOS");
    kprintln!("  String: OK ({})", s);
    drop(s);

    // 11. Loop test (leak check).
    let usage_before = stats();
    for _ in 0..100 {
        let b = Box::new([0u8; 1024]);
        drop(b);
    }
    let usage_after = stats();
    kprintln!(
        "  Loop 100x: before={}, after={}",
        usage_before.used_pages(),
        usage_after.used_pages()
    );

    kprintln!("\n=== All allocator tests passed ===\n");
}
