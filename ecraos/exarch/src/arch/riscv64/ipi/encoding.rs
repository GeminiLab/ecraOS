//! SBI IPI physical target and error encoding.
//!
//! Pure helpers validate the firmware boundary without executing an SBI call.

use crate::{ipi::IpiError, power::PhysicalCpuId};

/// Encodes one physical hart using a one-bit mask and that hart's base.
///
/// The all-ones base is SBI's broadcast sentinel and cannot identify a single hart.
pub(super) fn hart_mask_parts(target: PhysicalCpuId) -> Result<(usize, usize), IpiError> {
    if target == usize::MAX {
        return Err(IpiError::InvalidTarget);
    }
    Ok((1, target))
}

/// Maps a firmware return code to the pre-submission mechanism contract.
///
/// A successful SBI call delegates completion checking to the common exact-generation waiter.
pub(super) fn map_sbi_error(error: usize) -> Result<(), IpiError> {
    match error as isize {
        0 => Ok(()),
        -2 => Err(IpiError::Unsupported),
        -3 => Err(IpiError::InvalidTarget),
        _ => Err(IpiError::Transport),
    }
}

/// SBI physical-delivery contract tests.
///
/// Sparse hart IDs remain physical destinations rather than logical mask indices.
#[cfg(test)]
mod tests {
    use crate::ipi::IpiError;

    use super::{hart_mask_parts, map_sbi_error};

    /// Preserves sparse physical hart IDs and rejects the SBI all-harts sentinel.
    ///
    /// A single-target request must never turn into an implicit broadcast.
    #[test]
    fn encodes_one_hart_at_physical_base() {
        assert_eq!(hart_mask_parts(0), Ok((1, 0)));
        assert_eq!(hart_mask_parts(65), Ok((1, 65)));
        assert_eq!(hart_mask_parts(4096), Ok((1, 4096)));
        assert_eq!(hart_mask_parts(usize::MAX), Err(IpiError::InvalidTarget));
    }

    /// Distinguishes firmware success, unsupported delivery and rejected physical targets.
    ///
    /// Any other firmware rejection remains a transport failure rather than an acknowledgement.
    #[test]
    fn preserves_firmware_rejection() {
        assert_eq!(map_sbi_error(0), Ok(()));
        assert_eq!(
            map_sbi_error((-2isize) as usize),
            Err(IpiError::Unsupported)
        );
        assert_eq!(
            map_sbi_error((-3isize) as usize),
            Err(IpiError::InvalidTarget)
        );
        assert_eq!(map_sbi_error((-1isize) as usize), Err(IpiError::Transport));
        assert_eq!(map_sbi_error((-4isize) as usize), Err(IpiError::Transport));
    }
}
