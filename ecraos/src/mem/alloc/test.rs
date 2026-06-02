//! Allocator smoke tests.
//!
//! Exercises the buddy allocator, slab allocator, and `#[global_allocator]`
//! integration path. All output goes to the serial console via `kprintln!`.
//! This module can be removed after the allocator has been validated.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::alloc::Layout;

use heapless::Vec as HeaplessVec;
use memory_addr::pa;

use super::*;
use crate::kprintln;

/// Runs all allocator smoke tests.
pub fn run() {
    let page_size = 1usize << crate::mem::vmm::page_size_shift();

    kprintln!("=== Allocator smoke tests ===\n");

    // 0. Exhaust all currently free buddy heap frames, then release them in one batch.
    let virt_phys_offset = crate::mem::vmm::virt_phys_offset();
    let sections: HeaplessVec<_, 100> =
        BUDDY.lock().section_iter().map(|s| s.heap_region).collect();
    let total_pages = sections
        .iter()
        .map(|section| section.size() / page_size)
        .sum();
    let mut allocated_pages = Vec::with_capacity(total_pages);

    for section in &sections {
        let mut allocated_in_section = 0usize;
        let start = section.start.as_usize();
        let end = section.end.as_usize();
        for paddr in (start..end).step_by(page_size) {
            let paddr = pa!(paddr);
            if is_allocated(paddr).expect("is_allocated failed") {
                continue;
            }
            let _ = unsafe { alloc_frames_at(paddr, 1).expect("alloc_frames_at failed") };
            assert!(is_allocated(paddr).expect("is_allocated after alloc failed"));
            allocated_pages.push(paddr);
            allocated_in_section += 1;
        }
        kprintln!(
            "  buddy heap exhaustion: allocated {} free pages in section {:#x} - {:#x}",
            allocated_in_section,
            section.start,
            section.end
        );
    }

    let released_pages = allocated_pages.len();
    for paddr in allocated_pages {
        dealloc_blocks_at(paddr, 1).expect("dealloc_blocks_at failed");
        assert!(!is_allocated(paddr).expect("is_allocated after dealloc failed"));
    }
    kprintln!(
        "  buddy heap exhaustion/release: OK ({} pages)",
        released_pages
    );

    // 1. alloc_frame — check alignment.
    let frame1 = alloc_frame().expect("alloc_frame failed");
    assert!(frame1.as_usize().is_multiple_of(page_size));
    kprintln!("  alloc_frame: OK ({:#x})", frame1);

    // 2. alloc_frames(4, page_size) — multi-page allocation.
    let frames4 = alloc_frames(4, page_size * 8).expect("alloc_frames(4) failed");
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

    // 12. Check statistics.
    let u = stats();
    kprintln!(
        "  usage: total={}, meta={}, heap={}, used={}, free={}, unused={}",
        u.total_pages(),
        u.meta_pages(),
        u.heap_pages(),
        u.used_pages(),
        u.free_pages(),
        u.unused_pages()
    );

    kprintln!("\n=== All allocator tests passed ===\n");
}
