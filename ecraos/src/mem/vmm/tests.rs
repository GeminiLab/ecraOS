//! VMM initialization errors and local TLB invalidation tests.

use core::cell::RefCell;

use expt::pt::cleanup::{PendingCleanup, TlbFlush, TlbFlusher, TlbFlushes};
use memory_addr::{VirtAddr, pa, va};

use crate::tlb::TlbShootdownError;

use super::{
    MappingTransaction, direct_mapping_phys_to_virt, direct_mapping_virt_to_phys,
    finish_local_cleanup, is_layout_initialized, is_page_table_active,
};

/// Verifies that an uninitialized VMM returns `false` for readiness and `None` for conversions.
#[test]
fn readiness_and_conversions_reject_uninitialized_vmm() {
    assert!(!is_layout_initialized());
    assert!(!is_page_table_active());
    assert_eq!(direct_mapping_phys_to_virt(pa!(0x1000)), None);
    assert_eq!(
        direct_mapping_virt_to_phys(va!(0xffff_8000_0000_1000)),
        None
    );
}

/// Verifies that an uninitialized VMM rejects transactions without panicking.
#[test]
fn transaction_rejects_uninitialized_vmm() {
    assert!(matches!(
        MappingTransaction::begin(),
        Err(TlbShootdownError::InvalidRequest)
    ));
    assert!(matches!(
        MappingTransaction::try_begin(),
        Err(TlbShootdownError::InvalidRequest)
    ));
}

std::thread_local! {
    /// The local invalidations completed by the calling test thread.
    ///
    /// Each test starts with an independent empty recording.
    static FLUSHES: RefCell<Vec<Option<VirtAddr>>> = const { RefCell::new(Vec::new()) };
}

/// A test flusher that records local TLB invalidations.
struct RecordingFlusher;

/// Local TLB invalidation recording for tests.
impl TlbFlusher<VirtAddr> for RecordingFlusher {
    /// Records a local page invalidation, or a full invalidation for `None`.
    fn flush(address: Option<VirtAddr>) {
        FLUSHES.with_borrow_mut(|flushes| flushes.push(address));
    }
}

/// Verifies that local page invalidations finish and their remote requests are appended.
#[test]
fn finishes_local_pages_before_remote_completion() {
    let mut cleanup = PendingCleanup::new_with::<RecordingFlusher>();
    cleanup.push_flush(TlbFlush::Page(va!(0x1000)));
    cleanup.push_flush(TlbFlush::Page(va!(0x2000)));
    let mut remote = TlbFlushes::None + TlbFlush::Page(va!(0x3000));
    finish_local_cleanup(cleanup, &mut remote);
    FLUSHES.with_borrow(|flushes| {
        assert_eq!(flushes.as_slice(), &[Some(va!(0x1000)), Some(va!(0x2000))]);
    });
    assert!(matches!(remote, TlbFlushes::Pages(pages)
        if pages.as_slice() == [va!(0x3000), va!(0x1000), va!(0x2000)]));
}

/// Verifies that a full cleanup flushes locally and replaces pending remote page requests.
#[test]
fn full_cleanup_requires_full_remote_invalidation() {
    let mut cleanup = PendingCleanup::new_with::<RecordingFlusher>();
    cleanup.push_flush(TlbFlush::Page(va!(0x1000)));
    cleanup.push_flush(TlbFlush::Full);
    let mut remote = TlbFlushes::None + TlbFlush::Page(va!(0x3000));
    finish_local_cleanup(cleanup, &mut remote);
    FLUSHES.with_borrow(|flushes| assert_eq!(flushes.as_slice(), &[None]));
    assert!(matches!(remote, TlbFlushes::Full));
}

/// Verifies that pending full remote invalidation does not suppress local page invalidation.
#[test]
fn full_remote_batch_still_completes_local_pages() {
    let mut cleanup = PendingCleanup::new_with::<RecordingFlusher>();
    cleanup.push_flush(TlbFlush::Page(va!(0x1000)));
    let mut remote = TlbFlushes::Full;
    finish_local_cleanup(cleanup, &mut remote);
    FLUSHES.with_borrow(|flushes| assert_eq!(flushes.as_slice(), &[Some(va!(0x1000))]));
    assert!(matches!(remote, TlbFlushes::Full));
}
