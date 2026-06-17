use x86::io::{inb, outb};

use crate::dbcn_println;

/// PIT frequency in Hz.
const PIT_FREQ_HZ: u64 = 1193182;

/// Calibration time in milliseconds.
const CALIBRATION_MS_LIST: [u64; 3] = [50, 30, 10];

/// Minimum number of loops we expect when calibrating TSC with PIT.
///
/// Used for sanity check. Copied from Linux kernel.
const fn calibration_min_loops(calibration_ms: u64) -> u64 {
    calibration_ms * 100
}

/// Calibration latch value.
///
/// It's the number of ticks that the PIT will count before the timer overflows.
const fn calibration_latch(calibration_ms: u64) -> u64 {
    PIT_FREQ_HZ * calibration_ms / 1000
}

/// Calibration latch value in 2 bytes, low byte first.
const fn calibration_latch_bytes(calibration_ms: u64) -> [u8; 2] {
    let latch = calibration_latch(calibration_ms);
    [(latch & 0xff) as u8, ((latch >> 8) & 0xff) as u8]
}

/// Calibrate TSC with PIT for once.
fn calibrate_tsc_with_pit_once(calibration_ms: u64) -> Result<u64, u64> {
    unsafe {
        // Set the Gate high, disable speaker
        outb(0x61, (inb(0x61) & !0x02) | 0x01);

        // Setup channel 2 to mode 0, binary count. Begin count.
        outb(0x43, 0xb0);
        let [latch_low, latch_high] = calibration_latch_bytes(calibration_ms);
        outb(0x42, latch_low);
        outb(0x42, latch_high);

        // PIT begins counting.

        let mut loop_count = 0;
        let tick_start = super::current_ticks();
        let mut t1 = tick_start;
        let mut t2 = tick_start;
        let mut interval_max = 0;
        let mut interval_min = u64::MAX;

        while inb(0x61) & 0x20 == 0 {
            t2 = super::current_ticks();

            let interval = t2.0 - t1.0;
            t1 = t2;

            interval_max = interval_max.max(interval);
            interval_min = interval_min.min(interval);

            loop_count += 1;
        }

        let delta = t2.0 - tick_start.0;
        let freq_khz = delta / calibration_ms;

        if loop_count < calibration_min_loops(calibration_ms) || interval_max > interval_min * 10 {
            return Err(freq_khz);
        }

        Ok(freq_khz)
    }
}

/// Uses PIT to calibrate TSC, try different calibration times.
///
/// Linux uses 10ms then 50ms. However, when testing ecraOS on qemu, I found that longer
/// calibration times are more accurate but more likely to fail due to long interval, so I chose
/// 50ms, 30ms, 10ms.
///
/// Also, if none of the tries succeed, but their results are close enough (<= +/-0.5% from the
/// average), return the average.
fn calibrate_tsc_with_pit_slow() -> Option<u64> {
    let mut tries_results = [0u64; CALIBRATION_MS_LIST.len()];

    for (tri, calibration_ms) in CALIBRATION_MS_LIST.iter().enumerate() {
        match calibrate_tsc_with_pit_once(*calibration_ms) {
            Ok(freq) => {
                dbcn_println!(
                    "Succeeded to calibrate TSC with PIT at try #{}({} ms)",
                    tri,
                    calibration_ms
                );
                return Some(freq);
            }
            Err(freq) => tries_results[tri] = freq,
        }
    }

    let avg_freq = tries_results.iter().sum::<u64>() / CALIBRATION_MS_LIST.len() as u64;
    if tries_results.iter().all(|&freq| {
        freq.checked_signed_diff(avg_freq)
            .is_some_and(|diff| diff.abs() <= avg_freq as i64 / 200)
    }) {
        dbcn_println!(
            "Accepted the PIT calibration result: {:?} kHz",
            tries_results
        );
        return Some(avg_freq);
    }

    None
}

/// Calibrate TSC during early boot.
///
/// This function tries to get TSC frequency from:
/// - CPUID Leaf 0x15 (TSC Info),
/// - CPUID Leaf 0x16 (Processor Frequency Info),
/// - PIT calibration.
pub fn calibrate_tsc_early() -> Option<u64> {
    let cpu_id = raw_cpuid::CpuId::new();

    // Get TSC frequency from CPUID
    if let Some(tsc_info) = cpu_id.get_tsc_info()
        && let Some(tsc_frequency) = tsc_info.tsc_frequency()
    {
        dbcn_println!("TSC frequency from CPUID: {} kHz", tsc_frequency / 1000);
        return Some(tsc_frequency / 1000);
    } else {
        dbcn_println!("Failed to get TSC frequency from CPUID");
    }

    // Get CPU frequency and use it as TSC frequency
    if let Some(freq_info) = cpu_id.get_processor_frequency_info() {
        let freq_mhz = freq_info.processor_base_frequency() as u64;
        dbcn_println!(
            "CPU frequency from CPUID: {} kHz, use it as TSC frequency",
            freq_mhz * 1000
        );
        return Some(freq_mhz * 1000);
    } else {
        dbcn_println!("Failed to get CPU frequency from CPUID");
    }

    // TODO: read it from MSR

    // TODO: Use PIT to calibrate TSC (fast path).

    // Use PIT to calibrate TSC (slow path).
    if let Some(freq) = calibrate_tsc_with_pit_slow() {
        dbcn_println!("Succeeded to calibrate TSC with PIT: {} kHz", freq);
        return Some(freq);
    }

    None
}
