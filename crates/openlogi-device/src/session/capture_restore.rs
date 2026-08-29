//! Firmware ownership and channel-lifecycle primitives shared by mouse and
//! keyboard capture without coupling their manager loops or input semantics.

use std::fmt;
use std::future::Future;
use std::sync::{Arc, RwLock, Weak};

use hidpp::channel::HidppChannel;
use thiserror::Error;

use crate::backend::BackendError;
use crate::reprog_controls::{self, ReprogControlsV4};
use crate::thumbwheel::Thumbwheel;
use crate::{ChannelRegistry, DeviceRoute, SharedChannel};

/// Shared slot holding the active capture session's open channel, so bounded
/// hardware writes can reuse it instead of opening a second connection.
pub type CaptureChannel = Arc<RwLock<Option<SharedChannel>>>;

/// Why a capture session could not start (or had to stop).
#[derive(Debug, Error)]
pub enum GestureError {
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

/// Whether a retry may use the transport on which capture originally ran.
#[derive(Clone, Copy)]
enum RetiredChannelPolicy {
    /// Inventory declared the transport obsolete; wait for another current
    /// publication instead of writing underneath its replacement.
    ReplacementOnly,
    /// Capture stopped normally but a restore write failed, so retrying the
    /// still-current original transport is safe.
    CurrentAllowed,
}

/// How a capture session completed its firmware teardown.
#[must_use = "a pending restore must be retained until firmware ownership is released"]
pub enum CaptureSessionOutcome {
    /// Every diverted control was restored before the session returned.
    Restored,
    /// Firmware restoration is incomplete. The caller must retain this token
    /// and retry it before arming a successor for the same physical device.
    RestorePending(PendingCaptureRestore),
}

/// A capture setup failure plus any firmware ownership its rollback could not
/// release.
#[derive(Debug, Error)]
#[error("{error}")]
pub struct CaptureSessionFailure {
    #[source]
    error: GestureError,
    pending_restore: Option<PendingCaptureRestore>,
}

impl CaptureSessionFailure {
    pub(crate) fn clean(error: GestureError) -> Self {
        Self {
            error,
            pending_restore: None,
        }
    }

    pub(crate) fn with_pending(error: GestureError, pending: PendingCaptureRestore) -> Self {
        Self {
            error,
            pending_restore: Some(pending),
        }
    }

    /// Split the setup error from firmware ownership the caller must retain.
    #[must_use]
    pub fn into_parts(self) -> (GestureError, Option<PendingCaptureRestore>) {
        (self.error, self.pending_restore)
    }
}

impl From<GestureError> for CaptureSessionFailure {
    fn from(error: GestureError) -> Self {
        Self::clean(error)
    }
}

/// Opaque firmware ownership that survives the transport which armed it.
///
/// The token owns its original route and is consumed by every retry. A failed
/// retry returns the token through [`CaptureSessionOutcome::RestorePending`],
/// so callers cannot accidentally treat a borrowed `false` as completion.
pub struct PendingCaptureRestore {
    route: DeviceRoute,
    retired_channel: Weak<HidppChannel>,
    retired_policy: RetiredChannelPolicy,
    reprog: Option<ReprogRestore>,
    thumb_index: Option<u8>,
}

impl fmt::Debug for PendingCaptureRestore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingCaptureRestore")
            .field("route", &self.route)
            .field(
                "reporting_count",
                &self
                    .reprog
                    .as_ref()
                    .map_or(0, |reprog| reprog.controls.len()),
            )
            .field("has_thumbwheel", &self.thumb_index.is_some())
            .finish_non_exhaustive()
    }
}

impl PendingCaptureRestore {
    pub(crate) fn new(
        retired: &SharedChannel,
        reprog: Option<ReprogRestore>,
        thumb_index: Option<u8>,
    ) -> Option<Self> {
        if reprog.is_none() && thumb_index.is_none() {
            return None;
        }
        Some(Self {
            route: retired.route().clone(),
            retired_channel: Arc::downgrade(retired.channel()),
            retired_policy: RetiredChannelPolicy::ReplacementOnly,
            reprog,
            thumb_index,
        })
    }

    /// Retry through the exact-route channel currently published by inventory.
    ///
    /// Success is accepted only if the same publication remains current after
    /// every awaited restore write. A concurrent replacement returns this
    /// token as pending so the new winner is restored on the next attempt.
    pub async fn retry(self, registry: &ChannelRegistry) -> CaptureSessionOutcome {
        let Some(current) = registry.lookup(&self.route) else {
            return CaptureSessionOutcome::RestorePending(self);
        };
        if matches!(self.retired_policy, RetiredChannelPolicy::ReplacementOnly)
            && self
                .retired_channel
                .upgrade()
                .is_some_and(|retired| Arc::ptr_eq(current.channel(), &retired))
        {
            return CaptureSessionOutcome::RestorePending(self);
        }
        let restored = self.restore_on(&current).await;
        if restored && registry.is_current(&current) {
            CaptureSessionOutcome::Restored
        } else {
            CaptureSessionOutcome::RestorePending(self)
        }
    }

    /// Restore on the original standalone channel, where no registry
    /// publication can supersede the session.
    pub(crate) async fn restore_standalone(self, retired: &SharedChannel) -> CaptureSessionOutcome {
        if self.restore_on(retired).await {
            CaptureSessionOutcome::Restored
        } else {
            CaptureSessionOutcome::RestorePending(self)
        }
    }

    /// Permit a retry on the original channel after a normal teardown write
    /// failed while that publication was still current.
    pub(crate) fn allow_current_channel(mut self) -> Self {
        self.retired_policy = RetiredChannelPolicy::CurrentAllowed;
        self
    }

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

/// Roll back a partially armed session without losing firmware ownership when
/// any compensating write fails.
pub(crate) async fn rollback_capture_start(
    error: GestureError,
    pending: Option<PendingCaptureRestore>,
    retired: &SharedChannel,
    registry: Option<&ChannelRegistry>,
) -> CaptureSessionFailure {
    let Some(pending) = pending else {
        return CaptureSessionFailure::clean(error);
    };
    let pending = pending.allow_current_channel();
    let outcome = match registry {
        Some(registry) => pending.retry(registry).await,
        None => pending.restore_standalone(retired).await,
    };
    match outcome {
        CaptureSessionOutcome::Restored => CaptureSessionFailure::clean(error),
        CaptureSessionOutcome::RestorePending(pending) => {
            CaptureSessionFailure::with_pending(error, pending)
        }
    }
}

/// Release firmware ownership after an active session stops.
pub(crate) async fn restore_after_stop(
    stop: CaptureStop,
    pending: Option<PendingCaptureRestore>,
    retired: &SharedChannel,
    registry: Option<&ChannelRegistry>,
) -> CaptureSessionOutcome {
    let Some(pending) = pending else {
        return CaptureSessionOutcome::Restored;
    };
    match stop {
        CaptureStop::Shutdown => {
            let pending = pending.allow_current_channel();
            match registry {
                Some(registry) => pending.retry(registry).await,
                None => pending.restore_standalone(retired).await,
            }
        }
        CaptureStop::ChannelChanged => match registry {
            Some(registry) => pending.retry(registry).await,
            None => CaptureSessionOutcome::RestorePending(pending),
        },
    }
}

/// Re-check inventory at shutdown so a simultaneously ready stop request does
/// not win over publication replacement and write through a retired channel.
pub(crate) fn stop_for_current_publication(
    registry: Option<&ChannelRegistry>,
    retired: &SharedChannel,
) -> CaptureStop {
    if registry.is_some_and(|registry| !registry.is_current(retired)) {
        CaptureStop::ChannelChanged
    } else {
        // A standalone session has no publication that can supersede it.
        CaptureStop::Shutdown
    }
}

/// Wait until inventory removes or replaces the channel on which capture was
/// armed. The returned reason never carries a cached replacement across an
/// await; restoration performs a fresh registry lookup instead.
pub(crate) async fn wait_for_channel_change(
    registry: Option<&ChannelRegistry>,
    retired: &SharedChannel,
) -> CaptureStop {
    let Some(registry) = registry else {
        return std::future::pending().await;
    };
    let mut changes = registry.subscribe();
    loop {
        if !registry.is_current(retired) {
            return CaptureStop::ChannelChanged;
        }
        if changes.changed().await.is_err() {
            // The borrowed registry owns the sender, so this is unreachable;
            // preserve standalone-like pending semantics if that invariant is
            // ever changed rather than inventing a channel transition.
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
