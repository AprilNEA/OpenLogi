//! What capture owes the firmware and when it stops: the restore plan, the
//! stop reasons and the reporting writes shared by mouse and keyboard capture
//! without coupling their manager loops or input semantics. The token that
//! carries an unfinished restore is `session::restore`'s.

use std::fmt;
use std::future::Future;
use std::sync::{Arc, RwLock};

use hidpp::protocol::v20::Hidpp20Error;
use thiserror::Error;

use super::restore::{PendingRestore, RestoreOutcome, RestorePlan, SessionFailure};
use crate::backend::BackendError;
use crate::reprog_controls::{self, ReprogControlsV4};
use crate::thumbwheel::Thumbwheel;
use crate::{ChannelRegistry, IoSuspended, SharedChannel};

/// Shared slot holding the active capture session's open channel, so bounded
/// hardware writes can reuse it instead of opening a second connection.
pub type CaptureChannelSlot = Arc<RwLock<Option<SharedChannel>>>;

/// Why a capture session could not start (or had to stop).
#[derive(Debug, Error)]
pub enum CaptureError {
    /// HID transport-level failure while enumerating or opening the device.
    #[error("HID transport error")]
    Hid(#[from] BackendError),
    /// No connected device matched the capture route.
    #[error("no connected device matched the capture route")]
    DeviceNotFound,
    /// The device at the target index did not answer HID++.
    #[error("device at index {0:#04x} did not respond to HID++")]
    DeviceUnreachable(u8),
    /// A HID++ feature call returned an error; inner string carries context.
    #[error("HID++ protocol error: {0}")]
    Hidpp(String),
}

impl From<Hidpp20Error> for CaptureError {
    fn from(error: Hidpp20Error) -> Self {
        Self::Hidpp(format!("{error:?}"))
    }
}

impl From<IoSuspended> for CaptureError {
    fn from(error: IoSuspended) -> Self {
        Self::Hid(error.into())
    }
}

/// One `0x1b04` control whose original reporting state can restore a failed
/// or completed capture transaction.
#[derive(Clone, Copy)]
pub(crate) struct ArmedReporting {
    pub(crate) cid: u16,
    pub(crate) original: reprog_controls::CidReporting,
}

/// A non-empty set of `0x1b04` controls owned through one feature index.
pub(crate) struct ReprogRestore {
    feature_index: u8,
    controls: Vec<ArmedReporting>,
}

impl ReprogRestore {
    pub(crate) fn new(feature_index: u8, controls: Vec<ArmedReporting>) -> Option<Self> {
        (!controls.is_empty()).then_some(Self {
            feature_index,
            controls,
        })
    }
}

#[derive(Clone, Copy)]
pub(crate) enum CaptureStop {
    /// The owner deliberately requested teardown.
    Shutdown,
    /// Inventory removed or replaced the channel that armed capture.
    ChannelChanged,
}

/// What a capture session writes to hand its controls back: every diverted
/// `0x1b04` control's reporting, and the thumb wheel's.
///
/// One untimed attempt per control; a failed one leaves the whole plan owed.
pub struct CaptureRestorePlan {
    reprog: Option<ReprogRestore>,
    thumb_index: Option<u8>,
}

impl fmt::Debug for CaptureRestorePlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CaptureRestorePlan")
            .field(
                "reporting_count",
                &self
                    .reprog
                    .as_ref()
                    .map_or(0, |reprog| reprog.controls.len()),
            )
            .field("has_thumbwheel", &self.thumb_index.is_some())
            .finish()
    }
}

impl RestorePlan for CaptureRestorePlan {
    async fn restore_on(&self, current: &SharedChannel) -> bool {
        let channel = Arc::clone(current.channel());
        let device_index = current.device_index();
        let mut restored = true;
        if let Some(reprog) = &self.reprog {
            let controls =
                ReprogControlsV4::new(channel.clone(), device_index, reprog.feature_index);
            for &reporting in &reprog.controls {
                restored &= restore_reporting(&controls, reporting, "captured control").await;
            }
        }
        if let Some(feature_index) = self.thumb_index {
            let thumbwheel = Thumbwheel::new(channel, device_index, feature_index);
            restored &= restore_result(thumbwheel.undivert().await, "thumb wheel");
        }
        restored
    }
}

/// Firmware ownership a capture session could not release, to retry on the
/// channel inventory publishes next.
pub type PendingCaptureRestore = PendingRestore<CaptureRestorePlan>;

/// How a capture session completed its firmware teardown.
pub type CaptureSessionOutcome = RestoreOutcome<CaptureRestorePlan>;

/// A capture setup failure plus any firmware ownership its rollback could not
/// release.
pub type CaptureSessionFailure = SessionFailure<CaptureError, CaptureRestorePlan>;

impl PendingCaptureRestore {
    /// The restore owed for `reprog` and the thumb wheel, or `None` when the
    /// session diverted nothing.
    pub(crate) fn new(
        retired: &SharedChannel,
        reprog: Option<ReprogRestore>,
        thumb_index: Option<u8>,
    ) -> Option<Self> {
        if reprog.is_none() && thumb_index.is_none() {
            return None;
        }
        Some(Self::owing(
            retired,
            CaptureRestorePlan {
                reprog,
                thumb_index,
            },
        ))
    }
}

/// Release firmware ownership after an active session stops.
pub(crate) async fn restore_after_stop(
    stop: CaptureStop,
    pending: Option<PendingCaptureRestore>,
    registry: &ChannelRegistry,
) -> CaptureSessionOutcome {
    let Some(pending) = pending else {
        return CaptureSessionOutcome::Restored;
    };
    match stop {
        CaptureStop::Shutdown => pending.allow_current_channel().retry(registry).await,
        CaptureStop::ChannelChanged => pending.retry(registry).await,
    }
}

/// Re-check inventory at shutdown so a simultaneously ready stop request does
/// not win over publication replacement and write through a retired channel.
pub(crate) fn stop_for_current_publication(
    registry: &ChannelRegistry,
    retired: &SharedChannel,
) -> CaptureStop {
    if registry.is_current(retired) {
        CaptureStop::Shutdown
    } else {
        CaptureStop::ChannelChanged
    }
}

/// Wait until inventory removes or replaces the channel on which capture was
/// armed. The returned reason never carries a cached replacement across an
/// await; restoration performs a fresh registry lookup instead.
pub(crate) async fn wait_for_channel_change(
    registry: &ChannelRegistry,
    retired: &SharedChannel,
) -> CaptureStop {
    let mut changes = registry.subscribe();
    loop {
        if !registry.is_current(retired) {
            return CaptureStop::ChannelChanged;
        }
        if changes.changed().await.is_err() {
            // The borrowed registry owns the sender, so this is unreachable;
            // stay pending if that invariant ever changes rather than
            // inventing a channel transition.
            return std::future::pending().await;
        }
    }
}

/// Keep accepting diverted reports until firmware teardown has completed.
pub(crate) async fn drop_listener_after<T, R>(listener: T, teardown: impl Future<Output = R>) -> R {
    let result = teardown.await;
    drop(listener);
    result
}

/// Divert a control in the requested mode while preserving its remap target.
pub(crate) fn divert_change(
    reporting: reprog_controls::CidReporting,
    raw_xy: bool,
) -> reprog_controls::CidReportingChange {
    reprog_controls::CidReportingChange {
        diverted: Some(true),
        raw_xy: Some(raw_xy),
        remap: reporting.remap,
        ..Default::default()
    }
}

/// Restore one captured reporting record, preserving its remap target and
/// touching only the diversion bits capture owns.
pub(crate) async fn restore_reporting(
    controls: &ReprogControlsV4,
    reporting: ArmedReporting,
    what: &str,
) -> bool {
    let result = controls
        .set_cid_reporting_full(reporting.cid, undivert_change(reporting.original))
        .await
        .map(|_| ());
    restore_result(result, what)
}

pub(crate) fn restore_result<E: fmt::Display>(result: Result<(), E>, what: &str) -> bool {
    if let Err(error) = result {
        tracing::warn!(%error, control = what, "failed to restore control mapping");
        false
    } else {
        true
    }
}

/// Clear diversion while preserving the control's original remap target.
pub(crate) fn undivert_change(
    reporting: reprog_controls::CidReporting,
) -> reprog_controls::CidReportingChange {
    reprog_controls::CidReportingChange {
        diverted: Some(false),
        raw_xy: Some(false),
        remap: reporting.remap,
        ..Default::default()
    }
}
