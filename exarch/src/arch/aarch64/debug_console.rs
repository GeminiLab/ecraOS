//! PL011 early console support discovered from the boot Device Tree.

use core::sync::atomic::{AtomicUsize, Ordering};

use ecraldr_base::PlatformBootArg;
use fdt_rs::{base::DevTree, prelude::FallibleIterator};
use memory_addr::{PhysAddr, PhysAddrRange};

use crate::{
    mem::{DEFAULT_MMIO_FLAGS, MemoryRegion},
    platform::dt::DeviceTreeNodeExt,
};

/// The PL011 data-register offset.
const UARTDR: usize = 0x00;
/// The PL011 flag-register offset.
const UARTFR: usize = 0x18;
/// Indicates that the transmit FIFO is full.
const FR_TXFF: u32 = 1 << 5;
/// Indicates that the receive FIFO is empty.
const FR_RXFE: u32 = 1 << 4;

/// The discovered physical PL011 base address, or zero before initialization.
static PL011_BASE: AtomicUsize = AtomicUsize::new(0);
/// The discovered PL011 register span.
static PL011_SIZE: AtomicUsize = AtomicUsize::new(0);

/// Discovers the enabled PL011 MMIO range without allocating memory.
///
/// This early parser supports the 64-bit address and size cells used by the AArch64 QEMU platform.
/// Invalid or absent required firmware data returns `None` so initialization can halt before any
/// access to an unverified device address.
fn probe_pl011(arg: PlatformBootArg) -> Option<(PhysAddr, usize)> {
    let PlatformBootArg::DeviceTree(dtb_addr) = arg else {
        return None;
    };
    let dtb_ptr = crate::kernel_if::phys_to_virt(dtb_addr).as_ptr();
    // SAFETY: the loader supplies a valid DTB pointer and the early identity/direct mapping keeps
    // the blob accessible until the VMM transition completes.
    let dtb = unsafe { DevTree::from_raw_pointer(dtb_ptr).ok()? };
    for node in dtb.nodes().iterator() {
        let node = node.ok()?;
        if !node.is_compatible("arm,pl011").ok()? || !node.is_enabled().ok()? {
            continue;
        }
        let base = usize::try_from(node.u64_property("reg", 0).ok()??).ok()?;
        let size = usize::try_from(node.u64_property("reg", 1).ok()??).ok()?;
        // The DTB supplies the only early device boundary. Reject an empty, null, or overflowing
        // span here so later register accesses cannot escape it. UARTFR is the highest register
        // touched by this console, so the range must cover that register in full.
        let required_size = UARTFR + core::mem::size_of::<u32>();
        if base == 0 || size < required_size || base.checked_add(size).is_none() {
            return None;
        }
        return Some((PhysAddr::from_usize(base), size));
    }
    None
}

/// Returns the discovered console register address for volatile access.
fn register(offset: usize) -> *mut u32 {
    let base = PL011_BASE.load(Ordering::Acquire);
    assert_ne!(base, 0, "AArch64 early console is not initialized");
    // The register offset is a compile-time console constant, but the firmware may describe a
    // shorter span. Check it before forming the MMIO pointer so malformed DT data cannot make a
    // console access touch a neighboring device.
    assert!(
        offset
            .checked_add(core::mem::size_of::<u32>())
            .is_some_and(|end| { end <= PL011_SIZE.load(Ordering::Relaxed) }),
        "AArch64 early console register exceeds its discovered MMIO range"
    );
    // SAFETY: the base and register offset were validated against the firmware-described span.
    crate::kernel_if::phys_to_virt(PhysAddr::from_usize(base + offset)).as_mut_ptr_of()
}

/// Writes bytes through the discovered PL011 transmit FIFO.
pub fn write_bytes(bytes: &[u8]) {
    for &byte in bytes {
        // SAFETY: register validates the firmware-described span before returning this pointer.
        while unsafe { register(UARTFR).read_volatile() } & FR_TXFF != 0 {
            core::hint::spin_loop();
        }
        // SAFETY: register validates the firmware-described span before returning this pointer.
        unsafe { register(UARTDR).write_volatile(byte as u32) };
    }
}

/// Reads bytes immediately available in the discovered PL011 receive FIFO.
pub fn read_bytes(bytes: &mut [u8]) -> usize {
    let mut count = 0;
    for byte in bytes {
        // SAFETY: register validates the firmware-described span before returning this pointer.
        if unsafe { register(UARTFR).read_volatile() } & FR_RXFE != 0 {
            break;
        }
        // SAFETY: register validates the firmware-described span before returning this pointer.
        *byte = unsafe { register(UARTDR).read_volatile() as u8 };
        count += 1;
    }
    count
}

/// Discovers and stores the firmware-described PL011 console before exceptions are installed.
///
/// The supported early environment provides the DTB identity mapping and no allocator. Returns
/// false when firmware data cannot identify a usable console.
pub fn init(arg: PlatformBootArg) -> bool {
    let Some((base, size)) = probe_pl011(arg) else {
        return false;
    };
    PL011_SIZE.store(size, Ordering::Relaxed);
    PL011_BASE.store(base.as_usize(), Ordering::Release);
    true
}

/// Returns the discovered PL011 range for early and platform memory mapping.
pub fn mmio_region() -> Option<MemoryRegion> {
    let base = PL011_BASE.load(Ordering::Acquire);
    (base != 0).then(|| MemoryRegion {
        range: PhysAddrRange::from_start_size(
            PhysAddr::from_usize(base),
            PL011_SIZE.load(Ordering::Relaxed),
        ),
        flags: DEFAULT_MMIO_FLAGS,
        desc: "PL011 early console",
    })
}
