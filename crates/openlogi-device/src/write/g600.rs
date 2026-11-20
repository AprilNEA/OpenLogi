//! Driver for the Logitech G600's onboard DPI settings.
//!
//! The G600 answers no HID++, so its settings are vendor feature reports on
//! its `0xff80/0x0080` collection. The device registry owns the report layout,
//! decodes it, and encodes the writes; this driver finds the node a route
//! addresses, moves the reports, and checks each write by reading it back.

use openlogi_device_registry::g600::{
    ACTIVE_REPORT_ID, ACTIVE_REPORT_LEN, PROFILE_COMMIT_TIME, PROFILE_REPORT_LEN, REPORT_TIMEOUT,
    decode_active, decode_profile_dpi, encode_set_active_stage, matches_g600, patch_profile_dpi,
};

use crate::backend::{BackendError, HidBackend, NodeInfo};
use crate::channel::route::{DeviceRoute, find_raw_route_node};

use super::{HidppOperation, WriteError};
use openlogi_core::hid::g600::G600Failure;

pub use openlogi_device_registry::g600::{
    ActiveSelection, DPI_STAGE_COUNT, DpiSettings, DpiSettingsError, DpiStage, DpiState, DpiValue,
    G600_DRIVER_ID, OnboardProfile, PROFILE_COUNT, ProfileDpi, StageAfterStore,
    StageAfterStoreError,
};

/// Why the G600's DPI settings could not be read.
#[derive(Debug, thiserror::Error)]
pub enum G600ReadError {
    /// The route does not address a G600's vendor feature-report collection.
    #[error("the route does not address a G600")]
    NotG600Route,
    /// The route resolved to no node, or to several.
    #[error(transparent)]
    Route(#[from] WriteError),
    /// The backend failed to read a report.
    #[error(transparent)]
    Backend(#[from] BackendError),
    /// A report read did not complete in time.
    #[error("reading G600 report {report_id:#04x} timed out")]
    TimedOut {
        /// The report that was being read.
        report_id: u8,
    },
    /// A report came back with a layout the decoder does not accept.
    #[error("G600 report {report_id:#04x} came back malformed ({len} bytes)")]
    Malformed {
        /// The report that was read.
        report_id: u8,
        /// How many bytes the backend returned.
        len: usize,
    },
}

/// Why a G600 DPI write failed or could not be confirmed.
///
/// [`Self::Read`], [`Self::StageDisabled`] and [`Self::ProfileNotInUse`]
/// mean nothing was written.
/// [`Self::Backend`] and [`Self::TimedOut`] name the report whose write
/// failed, and that report may still have reached the mouse. Every other
/// variant comes after a write was sent.
#[derive(Debug, thiserror::Error)]
pub enum G600WriteError {
    /// Finding the device or a read failed before any report was written.
    #[error(transparent)]
    Read(#[from] G600ReadError),
    /// The backend failed to write a report. The report may still have
    /// reached the mouse.
    #[error("writing G600 report {report_id:#04x} failed; the mouse may still have applied it")]
    Backend {
        /// The report that was being written.
        report_id: u8,
        /// The backend's failure.
        #[source]
        source: BackendError,
    },
    /// A report write did not complete in time. The device may still apply
    /// it.
    #[error("writing G600 report {report_id:#04x} timed out; the mouse may still apply it")]
    TimedOut {
        /// The report that was being written.
        report_id: u8,
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
    /// The profile was written, but reading it back failed.
    #[error(
        "G600 profile {} (report {:#04x}) was written but could not be verified",
        profile.number(),
        profile.report_id()
    )]
    Unverified {
        /// The profile that was written.
        profile: OnboardProfile,
        /// Why the read-back failed.
        #[source]
        source: G600ReadError,
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
        /// Why selecting it failed.
        #[source]
        source: Box<G600WriteError>,
    },
    /// The stage was selected, but reading the active stage back failed.
    #[error("selected DPI stage {} but could not read the active stage back", stage.number())]
    StageUnverified {
        /// The stage that was selected.
        stage: DpiStage,
        /// Why the read-back failed.
        #[source]
        source: G600ReadError,
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
    /// The stage selection was sent, but the mouse reports another profile
    /// than `expected`, the one it was chosen for: its own profile button
    /// moved it, so the selection landed on whichever profile was in use.
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
    /// The mouse left `profile` for `active`, by its own profile button,
    /// while it stored `profile`, so no stage selection was sent: one
    /// carries no profile and would have landed on `active`. Only the cause
    /// of [`Self::StageNotReselected`].
    #[error(
        "the G600 left profile {} for profile {} while storing it, so no DPI stage was selected",
        profile.number(),
        active.number()
    )]
    ProfileLeft {
        /// The profile that was stored.
        profile: OnboardProfile,
        /// The profile the mouse is using now.
        active: OnboardProfile,
    },
}

impl From<StageAfterStoreError> for G600WriteError {
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

/// Carries a read failure across the IPC boundary: a transport failure as
/// text, a timeout as [`WriteError::RequestTimedOut`], and the G600's own
/// failures as [`WriteError::G600`].
impl From<G600ReadError> for WriteError {
    fn from(error: G600ReadError) -> Self {
        match error {
            G600ReadError::NotG600Route => G600Failure::NotG600Route.into(),
            G600ReadError::Route(error) => error,
            G600ReadError::Backend(error) => error.into(),
            G600ReadError::TimedOut { .. } => Self::RequestTimedOut {
                operation: HidppOperation::ReadG600Dpi,
            },
            G600ReadError::Malformed { report_id, len } => {
                G600Failure::Malformed { report_id, len }.into()
            }
        }
    }
}

/// Carries a write failure across the IPC boundary. A failed report write
/// keeps its warning that the report may still have reached the mouse in
/// its text; a timed-out one names the report it was writing.
impl From<G600WriteError> for WriteError {
    fn from(error: G600WriteError) -> Self {
        match error {
            G600WriteError::Read(error) => error.into(),
            G600WriteError::Backend { ref source, .. } => Self::Hid(format!("{error}: {source}")),
            G600WriteError::TimedOut { report_id } => Self::RequestTimedOut {
                operation: if report_id == ACTIVE_REPORT_ID {
                    HidppOperation::WriteG600Stage
                } else {
                    HidppOperation::WriteG600Profile
                },
            },
            G600WriteError::StageDisabled { profile, stage } => {
                G600Failure::StageDisabled { profile, stage }.into()
            }
            G600WriteError::ProfileNotInUse { profile, active } => {
                G600Failure::ProfileNotInUse { profile, active }.into()
            }
            G600WriteError::Unverified { profile, .. } => {
                G600Failure::ProfileUnverified { profile }.into()
            }
            G600WriteError::ProfileMismatch { profile } => {
                G600Failure::ProfileMismatch { profile }.into()
            }
            G600WriteError::StageNotReselected { profile, stage, .. } => {
                G600Failure::StageNotReselected { profile, stage }.into()
            }
            G600WriteError::StageUnverified { stage, .. } => {
                G600Failure::StageUnverified { stage }.into()
            }
            G600WriteError::StageMismatch { expected, actual } => {
                G600Failure::StageMismatch { expected, actual }.into()
            }
            G600WriteError::ProfileSwitched { expected, active }
            | G600WriteError::ProfileLeft {
                profile: expected,
                active,
            } => G600Failure::ProfileSwitched { expected, active }.into(),
        }
    }
}

/// Read the active selection and the onboard DPI settings of the G600 on
/// `route`.
pub async fn read_dpi(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<DpiState, G600ReadError> {
    let node = g600_node(backend, route).await?;
    let active = read_active(backend, &node).await?;
    // Destructuring ties the reads below to `PROFILE_COUNT` at compile time.
    let [first, second, third] = OnboardProfile::ALL;
    let profiles = [
        read_profile(backend, &node, first).await?,
        read_profile(backend, &node, second).await?,
        read_profile(backend, &node, third).await?,
    ];
    Ok(DpiState { active, profiles })
}

/// Switch the G600 on `route` to DPI `stage` of its active profile, and
/// confirm the switch by reading the active selection back.
///
/// A stage the active profile has disabled is refused before anything is
/// written.
pub async fn set_active_stage(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    stage: DpiStage,
) -> Result<(), G600WriteError> {
    let node = g600_node(backend, route).await?;
    let profile = read_active(backend, &node).await?.profile;
    if !read_profile(backend, &node, profile)
        .await?
        .is_enabled(stage)
    {
        return Err(G600WriteError::StageDisabled { profile, stage });
    }
    apply_active_stage(backend, &node, profile, stage).await
}

/// Store `settings` in onboard `profile` of the G600 on `route`, then put
/// the mouse on the stage `after` asks for.
///
/// The profile report is read, patched in its DPI-stage and default-stage
/// bytes only, written whole, and, once the mouse has had time to store it,
/// read back; anything but a byte-for-byte match is an error. A profile that
/// already holds `settings` is not written. When `profile` is the active one,
/// the stage [`ActiveSelection::stage_after_store`] names for `after` is
/// selected so the mouse applies the new values; over an unwritten profile,
/// only when the mouse is on another stage.
///
/// A stage `after` names that the mouse cannot use once `settings` are
/// stored, or one named for a profile not in use, is refused before anything
/// is written.
///
/// The profile button works while the mouse stores a profile, so after a
/// write the stage is decided again from the selection the mouse reports
/// then. A mouse that has left `profile` by then gets no stage selection:
/// under [`StageAfterStore::Keep`] the call succeeds, since the mouse loads
/// the stored default stage when it returns to `profile`; a named stage is
/// [`G600WriteError::StageNotReselected`] caused by
/// [`G600WriteError::ProfileLeft`]. A mouse that leaves `profile` after that
/// check, before the stage selection lands, takes the selection on its new
/// profile: [`G600WriteError::StageNotReselected`] caused by
/// [`G600WriteError::ProfileSwitched`].
///
/// Nothing reaches the mouse until [`PROFILE_COMMIT_TIME`] after the profile
/// report went out, even when its write failed or timed out: the report may
/// still have landed, and the call returns only once the mouse has stored it.
pub async fn set_profile_dpi(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    profile: OnboardProfile,
    settings: DpiSettings,
    after: StageAfterStore,
) -> Result<(), G600WriteError> {
    let node = g600_node(backend, route).await?;
    let active = read_active(backend, &node).await?;
    let stage = active.stage_after_store(profile, &settings, after)?;
    let report_id = profile.report_id();
    let current = read_report::<PROFILE_REPORT_LEN>(backend, &node, report_id).await?;
    let patched = patch_profile_dpi(&current, &settings);
    let rewrite = patched != current;
    let stage = if rewrite {
        let written = write_report(backend, &node, &patched).await;
        tokio::time::sleep(PROFILE_COMMIT_TIME).await;
        written?;
        let stored = read_report::<PROFILE_REPORT_LEN>(backend, &node, report_id)
            .await
            .map_err(|source| G600WriteError::Unverified { profile, source })?;
        if stored != patched {
            return Err(G600WriteError::ProfileMismatch { profile });
        }
        match stage {
            Some(stage) => stage_after_commit(backend, &node, profile, &settings, after)
                .await
                .map_err(|source| G600WriteError::StageNotReselected {
                    profile,
                    stage,
                    source: Box::new(source),
                })?,
            None => None,
        }
    } else {
        stage.filter(|stage| *stage != active.dpi_stage)
    };
    if let Some(stage) = stage {
        apply_active_stage(backend, &node, profile, stage)
            .await
            .map_err(|source| G600WriteError::StageNotReselected {
                profile,
                stage,
                source: Box::new(source),
            })?;
    }
    Ok(())
}

/// The stage to select once `settings` are stored in `profile`, decided
/// again from the selection the mouse reports after the commit wait: the
/// profile button may have moved it to another profile meanwhile, and a
/// stage selection carries no profile.
async fn stage_after_commit(
    backend: &dyn HidBackend,
    node: &NodeInfo,
    profile: OnboardProfile,
    settings: &DpiSettings,
    after: StageAfterStore,
) -> Result<Option<DpiStage>, G600WriteError> {
    let now = read_active(backend, node).await?;
    now.stage_after_store(profile, settings, after)
        .map_err(|error| match error {
            StageAfterStoreError::NotInUse { active, .. } => {
                G600WriteError::ProfileLeft { profile, active }
            }
            error @ StageAfterStoreError::StageDisabled { .. } => error.into(),
        })
}

/// The node of the G600 vendor collection `route` addresses.
async fn g600_node(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<NodeInfo, G600ReadError> {
    let DeviceRoute::RawHid {
        vendor_id,
        product_id,
        usage_page,
        usage_id,
        ..
    } = route
    else {
        return Err(G600ReadError::NotG600Route);
    };
    if !matches_g600(*vendor_id, *product_id, *usage_page, *usage_id) {
        return Err(G600ReadError::NotG600Route);
    }
    Ok(find_raw_route_node(backend, route)
        .await?
        .ok_or(WriteError::DeviceNotFound)?)
}

/// Select `stage` and confirm by reading the selection back that the mouse
/// is on it in `profile`, the profile it was chosen for.
async fn apply_active_stage(
    backend: &dyn HidBackend,
    node: &NodeInfo,
    profile: OnboardProfile,
    stage: DpiStage,
) -> Result<(), G600WriteError> {
    write_report(backend, node, &encode_set_active_stage(stage)).await?;
    let now = read_active(backend, node)
        .await
        .map_err(|source| G600WriteError::StageUnverified { stage, source })?;
    if now.profile != profile {
        return Err(G600WriteError::ProfileSwitched {
            expected: profile,
            active: now.profile,
        });
    }
    let actual = now.dpi_stage;
    if actual != stage {
        return Err(G600WriteError::StageMismatch {
            expected: stage,
            actual,
        });
    }
    Ok(())
}

async fn read_active(
    backend: &dyn HidBackend,
    node: &NodeInfo,
) -> Result<ActiveSelection, G600ReadError> {
    read_decoded::<ACTIVE_REPORT_LEN, _>(backend, node, ACTIVE_REPORT_ID, decode_active).await
}

async fn read_profile(
    backend: &dyn HidBackend,
    node: &NodeInfo,
    profile: OnboardProfile,
) -> Result<ProfileDpi, G600ReadError> {
    read_decoded::<PROFILE_REPORT_LEN, _>(backend, node, profile.report_id(), decode_profile_dpi)
        .await
}

/// Read feature report `report_id`, `N` bytes long with its ID, and decode
/// it.
async fn read_decoded<const N: usize, T>(
    backend: &dyn HidBackend,
    node: &NodeInfo,
    report_id: u8,
    decode: fn(&[u8]) -> Option<T>,
) -> Result<T, G600ReadError> {
    let report = read_report::<N>(backend, node, report_id).await?;
    decode(&report).ok_or(G600ReadError::Malformed { report_id, len: N })
}

/// Read feature report `report_id`, which must come back `N` bytes long with
/// its ID.
async fn read_report<const N: usize>(
    backend: &dyn HidBackend,
    node: &NodeInfo,
    report_id: u8,
) -> Result<[u8; N], G600ReadError> {
    let mut report = [0; N];
    report[0] = report_id;
    let len = tokio::time::timeout(
        REPORT_TIMEOUT,
        backend.read_feature_report(node, &mut report),
    )
    .await
    .map_err(|_| G600ReadError::TimedOut { report_id })??;
    // A profile decoder accepts any of the three profile IDs, so a reply
    // carrying another report's ID would land in the wrong profile.
    if len != N || report[0] != report_id {
        return Err(G600ReadError::Malformed { report_id, len });
    }
    Ok(report)
}

/// Write `report`, ID byte first.
async fn write_report(
    backend: &dyn HidBackend,
    node: &NodeInfo,
    report: &[u8],
) -> Result<(), G600WriteError> {
    let report_id = report[0];
    tokio::time::timeout(REPORT_TIMEOUT, backend.write_feature_report(node, report))
        .await
        .map_err(|_| G600WriteError::TimedOut { report_id })?
        .map_err(|source| G600WriteError::Backend { report_id, source })
}

#[cfg(test)]
mod tests;
