//! Concurrent access tests for physical memory ownership metadata.

use std::{sync::mpsc, thread, time::Duration};

use super::{
    DynamicMemoryRegions, EarlyMemoryRegions, MemoryRegions, RegionTable, read_region_table,
    write_region_table,
};

/// Allows a second reader to finish while the first reader still holds the table.
#[test]
fn region_table_allows_concurrent_readers() {
    let table = RegionTable::new(MemoryRegions::Early(EarlyMemoryRegions::new()));
    let concurrent = thread::scope(|scope| {
        let (held, holding) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let table_ref = &table;
        scope.spawn(move || {
            read_region_table(table_ref, |_| {
                held.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(5)).unwrap();
            });
        });
        holding.recv_timeout(Duration::from_secs(5)).unwrap();

        let (observed, observation) = mpsc::channel();
        scope.spawn(move || {
            read_region_table(table_ref, |regions| {
                // A timed-out receiver may already have been dropped on the failure path.
                let _ = observed.send(regions.as_slice().len());
            });
        });
        let result = observation.recv_timeout(Duration::from_secs(2));
        // Release the first reader even when the second is blocked so the test can finish.
        release.send(()).unwrap();
        result
    });
    assert_eq!(concurrent, Ok(0), "readers must not exclude each other");
}

/// Keeps readers and other writers out during an update, then exposes the committed storage.
#[test]
fn region_table_updates_are_exclusive() {
    let table = RegionTable::new(MemoryRegions::Early(EarlyMemoryRegions::new()));
    write_region_table(&table, |regions| {
        assert!(table.try_read().is_none(), "a writer must exclude readers");
        assert!(table.try_write().is_none(), "a writer must exclude writers");
        *regions = MemoryRegions::Dynamic(DynamicMemoryRegions::new());
    });
    read_region_table(&table, |regions| {
        assert!(matches!(regions, MemoryRegions::Dynamic(_)));
    });
}
