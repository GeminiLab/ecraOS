//! GICv3 affinity, frame selection and software interrupt encoding.
//!
//! These pure mechanisms do not own a logical CPU inventory or a reason mailbox.

use crate::{ipi::IpiError, power::PhysicalCpuId};

/// The SGI reserved for the common kernel software notification.
///
/// Its INTID never enters the global SPI registry.
pub(crate) const IPI_SGI: u8 = 1;

/// A malformed or unmatched redistributor region.
///
/// Selection rejects ambiguous topology before any local interrupt source is enabled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TopologyError {
    /// An out-of-bounds or incomplete redistributor frame.
    ///
    /// Every frame includes all pages implied by its VLPIS bit.
    InvalidRegion,
    /// No frame with the requested physical affinity.
    ///
    /// Selection reached Last without finding the current PE.
    MissingAffinity,
    /// More than one frame with the requested affinity.
    ///
    /// The controller region does not provide an unambiguous physical target.
    DuplicateAffinity,
    /// A frame chain without a Last terminator.
    ///
    /// The region limit is reached without a valid end marker.
    MissingLast,
}

/// Returns the compact affinity value represented by GICR_TYPER.
///
/// MPIDR control bits are excluded while Aff3 is packed above Aff2, Aff1 and Aff0.
pub(crate) const fn mpidr_affinity(mpidr: u64) -> u32 {
    (((mpidr >> 32) & 0xff) << 24 | (mpidr & 0x00ff_ffff)) as u32
}

/// Returns the complete frame stride implied by GICR_TYPER.VLPIS.
///
/// Virtual LPI frames add two 64-KiB pages to the base redistributor frame pair.
pub(crate) const fn frame_stride(typer: u64) -> usize {
    if typer & 2 != 0 { 0x40000 } else { 0x20000 }
}

/// Selects one redistributor frame by physical MPIDR affinity within the firmware region.
///
/// The scan honors Last, validates full strides and rejects duplicate matching frames.
pub(crate) fn select_redistributor(
    region_size: usize,
    mpidr: u64,
    mut read_typer: impl FnMut(usize) -> Option<u64>,
) -> Result<usize, TopologyError> {
    let affinity = mpidr_affinity(mpidr);
    let mut offset = 0usize;
    let mut found = None;
    loop {
        if offset
            .checked_add(0x20000)
            .is_none_or(|end| end > region_size)
        {
            return Err(if offset == region_size {
                TopologyError::MissingLast
            } else {
                TopologyError::InvalidRegion
            });
        }
        let typer = read_typer(offset).ok_or(TopologyError::InvalidRegion)?;
        let end = offset
            .checked_add(frame_stride(typer))
            .ok_or(TopologyError::InvalidRegion)?;
        if end > region_size {
            return Err(TopologyError::InvalidRegion);
        }
        if (typer >> 32) as u32 == affinity {
            if found.is_some() {
                return Err(TopologyError::DuplicateAffinity);
            }
            found = Some(offset);
        }
        if typer & (1 << 4) != 0 {
            return found.ok_or(TopologyError::MissingAffinity);
        }
        offset = end;
    }
}

/// Encodes one Group-one SGI for a physical MPIDR affinity.
///
/// Aff0 values above fifteen require Range Selector support and are rejected in this backend.
pub(crate) fn sgi1r(target: PhysicalCpuId, intid: u8) -> Result<u64, IpiError> {
    let target = target as u64;
    let aff0 = target & 0xff;
    if aff0 >= 16 || intid >= 16 {
        return Err(IpiError::InvalidTarget);
    }
    Ok(((target >> 32) & 0xff) << 48
        | ((target >> 16) & 0xff) << 32
        | (intid as u64) << 24
        | ((target >> 8) & 0xff) << 16
        | (1 << aff0))
}

/// A decoded GIC interrupt namespace.
///
/// Private sources never enter the kernel's global-source registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GicInterrupt {
    /// The reserved software SGI.
    ///
    /// The common mailbox identifies the semantic reason.
    Software,
    /// The virtual timer PPI.
    ///
    /// The common timer handler rearms the local source.
    Timer,
    /// A shared peripheral source.
    ///
    /// The contained INTID is a controller source rather than an exception cause.
    Spi(u32),
    /// A special non-delivery INTID.
    ///
    /// These values require no end-of-interrupt write.
    Special,
    /// An otherwise unsupported private source.
    ///
    /// Architecture code completes the acknowledged delivery normally.
    Unknown,
}

/// Separates private sources, global sources and special non-delivery values.
///
/// The reserved SGI and PPI 27 have distinct semantic local-interrupt consumers.
pub(crate) const fn decode_intid(intid: u32) -> GicInterrupt {
    match intid {
        value if value == IPI_SGI as u32 => GicInterrupt::Software,
        27 => GicInterrupt::Timer,
        32..=1019 => GicInterrupt::Spi(intid),
        1020..=1023 => GicInterrupt::Special,
        _ => GicInterrupt::Unknown,
    }
}

/// Redistributor and SGI contract tests.
///
/// Synthetic register values exercise frame boundaries and physical affinity selection.
#[cfg(test)]
mod tests {
    use crate::ipi::IpiError;

    use super::{
        GicInterrupt, TopologyError, decode_intid, frame_stride, mpidr_affinity,
        select_redistributor, sgi1r,
    };

    /// Selects four distinct QEMU-style redistributor frames by affinity.
    ///
    /// The boot CPU frame cannot be reused for every AP.
    #[test]
    fn selects_four_cpu_frames() {
        let typers = [0, 1u64 << 32, 2u64 << 32, (3u64 << 32) | 16];
        for cpu in 0..4 {
            assert_eq!(
                select_redistributor(0x80000, cpu, |offset| typers.get(offset / 0x20000).copied()),
                Ok(cpu as usize * 0x20000)
            );
        }
        assert_eq!(mpidr_affinity(0x12_8034_5607), 0x1234_5607);
    }

    /// Honors VLPIS frame stride and stops at the Last redistributor.
    ///
    /// Reads must remain inside the complete firmware region and must not pass Last.
    #[test]
    fn honors_vlpis_stride_and_last() {
        assert_eq!(frame_stride(0), 0x20000);
        assert_eq!(frame_stride(2), 0x40000);
        let mut reads = 0;
        assert_eq!(
            select_redistributor(0x80000, 1, |offset| {
                reads += 1;
                match offset {
                    0 => Some(2),
                    0x40000 => Some((1u64 << 32) | 16),
                    _ => panic!("read beyond Last"),
                }
            }),
            Ok(0x40000)
        );
        assert_eq!(reads, 2);
        assert_eq!(
            select_redistributor(0x20000, 1, |_| Some(16)),
            Err(TopologyError::MissingAffinity)
        );
    }

    /// Rejects duplicate affinities, truncated frames and unterminated frame chains.
    ///
    /// Ambiguous or out-of-bounds firmware topology must fail before enabling SGIs.
    #[test]
    fn rejects_ambiguous_and_truncated_topology() {
        assert_eq!(
            select_redistributor(0x40000, 0, |offset| Some(if offset == 0 { 0 } else { 16 })),
            Err(TopologyError::DuplicateAffinity)
        );
        assert_eq!(
            select_redistributor(0x10000, 0, |_| Some(16)),
            Err(TopologyError::InvalidRegion)
        );
        assert_eq!(
            select_redistributor(0x20000, 0, |_| Some(0)),
            Err(TopologyError::MissingLast)
        );
        assert_eq!(
            select_redistributor(0x20000, 0, |_| Some(2 | 16)),
            Err(TopologyError::InvalidRegion)
        );
    }

    /// Encodes all four affinity fields and the one physical Aff0 target bit.
    ///
    /// Unsupported Aff0 values cannot turn into Range Selector or broadcast delivery.
    #[test]
    fn encodes_physical_sgi_destination() {
        assert_eq!(sgi1r(0x12_0034_560f, 1), Ok(0x0012_0034_0156_8000));
        assert_eq!(sgi1r(3, 1), Ok(0x0100_0008));
        assert_eq!(sgi1r(16, 1), Err(IpiError::InvalidTarget));
        assert_eq!(sgi1r(0, 16), Err(IpiError::InvalidTarget));
    }

    /// Separates the reserved SGI, virtual timer PPI, SPI and special INTID classes.
    ///
    /// Software delivery never enters the external-source registry.
    #[test]
    fn separates_private_and_global_sources() {
        assert_eq!(decode_intid(1), GicInterrupt::Software);
        assert_eq!(decode_intid(27), GicInterrupt::Timer);
        assert_eq!(decode_intid(32), GicInterrupt::Spi(32));
        assert_eq!(decode_intid(1019), GicInterrupt::Spi(1019));
        assert_eq!(decode_intid(1023), GicInterrupt::Special);
        assert_eq!(decode_intid(0), GicInterrupt::Unknown);
    }
}
