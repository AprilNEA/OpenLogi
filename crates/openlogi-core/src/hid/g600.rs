//! Logitech G600 onboard DPI — pure data, no I/O.
//!
//! The device registry owns the validated G600 types; they are re-exported
//! here so IPC clients name them beside the rest of the wire vocabulary.
//! [`G600Failure`] is what a G600 read or write can fail with beyond a
//! transport error. The feature-report reads and writes live in
//! `openlogi_device::write::g600`.

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use openlogi_device_registry::g600::{
    ActiveSelection, DPI_STAGE_COUNT, DpiSettings, DpiSettingsError, DpiStage, DpiState, DpiValue,
    G600_DRIVER_ID, G600_NAME, G600_PRODUCT_ID, G600_USAGE_ID, G600_USAGE_PAGE, InvalidValue,
    OnboardProfile, PROFILE_COUNT, PROFILE_WRITE_DEADLINE, ProfileDpi, StageAfterStore,
    StageAfterStoreError,
};

/// A stage that cannot follow a profile write, refused before anything was
/// written.
impl From<StageAfterStoreError> for G600Failure {
    fn from(error: StageAfterStoreError) -> Self {
        match error {
            StageAfterStoreError::NotInUse {
                profile, active, ..
            } => Self::ProfileNotInUse { profile, active },
            StageAfterStoreError::StageDisabled { profile, stage } => {
                Self::StageDisabled { profile, stage }
            }
        }
    }
}

/// Why a G600 onboard-DPI read or write failed, beyond a transport error or
/// a timeout.
///
/// [`Self::NotG600Route`], [`Self::Malformed`], [`Self::StageDisabled`] and
/// [`Self::ProfileNotInUse`] mean nothing was written: a read-back that fails after a write is
/// [`Self::ProfileUnverified`] or [`Self::StageUnverified`] instead. Every
/// other variant comes after a report reached the mouse.
///
/// Variant order is wire format because this travels inside
/// [`super::WriteError`].
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq, Serialize, Deserialize)]
pub enum G600Failure {
    /// The route does not address a G600's vendor feature-report collection.
    #[error("the route does not address a G600")]
    NotG600Route,
    /// A report came back with a layout the decoder does not accept.
    #[error("G600 report {report_id:#04x} came back malformed ({len} bytes)")]
    Malformed {
        /// The report that was read.
        report_id: u8,
        /// How many bytes came back.
        len: usize,
    },
    /// The stage to select is disabled in the active profile, or in the
    /// settings about to be stored in it, so nothing was written.
    #[error("DPI stage {} is disabled in the active profile {}", stage.number(), profile.number())]
    StageDisabled {
        /// The active profile.
        profile: OnboardProfile,
        /// The stage that was asked for.
        stage: DpiStage,
    },
    /// The profile was written, but reading it back failed.
    #[error(
        "G600 profile {} (report {:#04x}) was written but could not be verified",
        profile.number(),
        profile.report_id()
    )]
    ProfileUnverified {
        /// The profile that was written.
        profile: OnboardProfile,
    },
    /// The profile read back after the write differs from what was written.
    #[error(
        "G600 profile {} (report {:#04x}) was written but read back differently",
        profile.number(),
        profile.report_id()
    )]
    ProfileMismatch {
        /// The profile that was written.
        profile: OnboardProfile,
    },
    /// The active profile was written and verified, but selecting a DPI
    /// stage afterwards, so the mouse applies the new values, failed.
    #[error(
        "G600 profile {} (report {:#04x}) is stored, but selecting DPI stage {} failed",
        profile.number(),
        profile.report_id(),
        stage.number()
    )]
    StageNotReselected {
        /// The profile that was written.
        profile: OnboardProfile,
        /// The stage that was being selected.
        stage: DpiStage,
    },
    /// The stage was selected, but reading the active stage back failed.
    #[error("selected DPI stage {} but could not read the active stage back", stage.number())]
    StageUnverified {
        /// The stage that was selected.
        stage: DpiStage,
    },
    /// The mouse reports another DPI stage than the one just selected.
    #[error(
        "the G600 is on DPI stage {} after selecting stage {}",
        actual.number(),
        expected.number()
    )]
    StageMismatch {
        /// The stage that was selected.
        expected: DpiStage,
        /// The stage the mouse reported afterwards.
        actual: DpiStage,
    },
    /// A profile write named the stage to use afterwards, but the mouse is
    /// using another profile, so nothing was written.
    #[error(
        "G600 profile {} is not in use (the mouse is on profile {}), so no DPI stage can follow its write",
        profile.number(),
        active.number()
    )]
    ProfileNotInUse {
        /// The profile that was to be written.
        profile: OnboardProfile,
        /// The profile the mouse is using.
        active: OnboardProfile,
    },
    /// The mouse moved to another profile, by its own profile button, while
    /// a DPI stage was being selected in `expected`. A selection already
    /// sent landed on the profile now in use; one not yet sent was not sent
    /// at all.
    #[error(
        "the G600 switched from profile {} to profile {} while a DPI stage was being selected",
        expected.number(),
        active.number()
    )]
    ProfileSwitched {
        /// The profile the stage was meant for.
        expected: OnboardProfile,
        /// The profile the mouse is using now.
        active: OnboardProfile,
    },
}
