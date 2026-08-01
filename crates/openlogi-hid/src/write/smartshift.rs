use std::num::NonZeroU8;
use std::sync::Arc;
use std::time::Duration;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature,
        smartshift::{SmartShiftFeature, WheelMode},
        smartshift_enhanced::{SmartShiftEnhancedFeature, SmartShiftEnhancedStatusChange},
    },
};
use tracing::debug;

use crate::route::DeviceRoute;
use crate::smartshift::{SmartShiftMode, SmartShiftStatus};

use super::{
    HidppFeatureErrorKind, HidppOperation, WriteError, classify_hidpp_error, open_feature,
    with_route,
};

/// Brief pause before re-trying a SmartShift transaction that lost a race with
/// concurrent HID++ traffic on another open of the same node (#485).
const TRANSIENT_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Whether a failure to open the `0x2111` Enhanced SmartShift feature should
/// trigger the `0x2110` legacy fallback. Only a missing-`0x2111` feature
/// qualifies; transport and protocol errors propagate unchanged so a real
/// failure is never masked by a second open attempt.
pub(super) fn is_missing_enhanced(err: &WriteError) -> bool {
    matches!(
        err,
        WriteError::FeatureUnsupported { feature_hex } if *feature_hex == 0x2111
    )
}

/// Errors that, on SmartShift, have been observed to clear on a second attempt
/// with byte-identical parameters after concurrent multi-open traffic settles
/// (#485). Permanent failures (unsupported feature, bad permanent payload) are
/// not included.
pub(super) fn is_transient_smartshift_error(err: &WriteError) -> bool {
    matches!(
        err,
        WriteError::HidppFeature {
            kind: HidppFeatureErrorKind::InvalidArgument
                | HidppFeatureErrorKind::Busy
                | HidppFeatureErrorKind::HwError,
            ..
        } | WriteError::UnsupportedResponse { .. }
    )
}

/// Whether `current` already satisfies a desired SmartShift write. A zero
/// `auto_disengage` / `tunable_torque` is the firmware "do not change" sentinel
/// on the write path, so those fields are only compared when non-zero.
pub(super) fn status_matches_desired(current: SmartShiftStatus, desired: SmartShiftStatus) -> bool {
    current.mode == desired.mode
        && (desired.auto_disengage == 0 || current.auto_disengage == desired.auto_disengage)
        && (desired.tunable_torque == 0 || current.tunable_torque == desired.tunable_torque)
}

/// Map the fork's `0x2110` [`WheelMode`] onto OpenLogi's [`SmartShiftMode`].
/// A future `#[non_exhaustive]` variant maps to [`SmartShiftMode::Ratchet`],
/// the "safe" clicky default OpenLogi uses elsewhere. (Reserved wire bytes
/// never reach here — the fork's `get_ratchet_control_mode` rejects them.)
pub(super) fn wheel_mode_to_smartshift(wheel: WheelMode) -> SmartShiftMode {
    if matches!(wheel, WheelMode::Freespin) {
        SmartShiftMode::Free
    } else {
        SmartShiftMode::Ratchet
    }
}

/// Map OpenLogi's [`SmartShiftMode`] onto the fork's `0x2110` [`WheelMode`] —
/// the inverse of [`wheel_mode_to_smartshift`], used when writing the legacy
/// ratchet-control mode.
pub(super) fn smartshift_to_wheel(mode: SmartShiftMode) -> WheelMode {
    match mode {
        SmartShiftMode::Free => WheelMode::Freespin,
        SmartShiftMode::Ratchet => WheelMode::Ratchet,
    }
}

/// Whichever SmartShift feature a device exposes, normalised onto
/// [`SmartShiftMode`]. Devices ship one or the other: MX Master 3 / 3S use the
/// `0x2111` Enhanced variant, the MX Master 2S uses the original `0x2110`.
enum SmartShift {
    /// `0x2111 SmartShiftWheelEnhanced`.
    Enhanced(Arc<SmartShiftEnhancedFeature>),
    /// `0x2110 SmartShiftWheel`.
    Legacy(Arc<SmartShiftFeature>),
}

impl SmartShift {
    /// Open whichever SmartShift feature the device exposes. Tries `0x2111`
    /// first; on a missing-`0x2111` error (and only that), re-checks once before
    /// falling back to `0x2110`. A concurrent multi-open of the same HID node
    /// can mis-deliver a `root.get_feature` response and make a present `0x2111`
    /// look absent (#485) — the second probe catches that before we write the
    /// wrong feature. Any other error from either attempt propagates unchanged.
    async fn open(device: &mut Device) -> Result<Self, WriteError> {
        match open_feature::<SmartShiftEnhancedFeature>(device).await {
            Ok(feature) => Ok(Self::Enhanced(feature)),
            Err(err) if is_missing_enhanced(&err) => {
                match open_feature::<SmartShiftEnhancedFeature>(device).await {
                    Ok(feature) => Ok(Self::Enhanced(feature)),
                    Err(err) if is_missing_enhanced(&err) => {
                        let feature = open_feature::<SmartShiftFeature>(device).await?;
                        Ok(Self::Legacy(feature))
                    }
                    Err(err) => Err(err),
                }
            }
            Err(err) => Err(err),
        }
    }

    /// Read the current mode + auto-disengage threshold. Enhanced (`0x2111`)
    /// also reports tunable torque; Legacy (`0x2110`) has no such concept, so
    /// `tunable_torque` is reported as `0` per [`SmartShiftStatus`]'s contract.
    async fn status(&self) -> Result<SmartShiftStatus, WriteError> {
        match self {
            Self::Enhanced(feature) => {
                let status = feature.get_ratchet_control_mode().await.map_err(|e| {
                    classify_hidpp_error(
                        e,
                        HidppOperation::ReadSmartShift,
                        SmartShiftEnhancedFeature::ID,
                    )
                })?;
                Ok(SmartShiftStatus {
                    mode: wheel_mode_to_smartshift(status.wheel_mode),
                    auto_disengage: status.auto_disengage,
                    tunable_torque: status.current_tunable_torque,
                })
            }
            Self::Legacy(feature) => {
                let rcm = feature.get_ratchet_control_mode().await.map_err(|e| {
                    classify_hidpp_error(e, HidppOperation::ReadSmartShift, SmartShiftFeature::ID)
                })?;
                Ok(SmartShiftStatus {
                    mode: wheel_mode_to_smartshift(rcm.wheel_mode),
                    auto_disengage: rcm.auto_disengage,
                    // 0x2110 has no tunable-torque function; report 0 like
                    // `SmartShiftStatus::tunable_torque` documents for devices
                    // that don't support it.
                    tunable_torque: 0,
                })
            }
        }
    }

    /// Write a full desired status — wheel mode plus the auto-disengage
    /// threshold and (Enhanced only) tunable torque.
    ///
    /// Per the `0x2110` / `0x2111` `setRatchetControlMode` spec, `0` is the
    /// firmware's "do not change" sentinel for `autoDisengage` and
    /// `currentTunableTorque` (real values are `0x01..=0xFF`). So a zero field
    /// is sent as "preserve" rather than rejected — this is the only way to
    /// write a mode change on a device that reports `tunable_torque == 0`
    /// (e.g. one without tunable-torque hardware), which otherwise silently
    /// failed the whole write.
    async fn set_status(&self, status: SmartShiftStatus) -> Result<(), WriteError> {
        let SmartShiftStatus {
            mode,
            auto_disengage,
            tunable_torque,
        } = status;
        match self {
            Self::Enhanced(feature) => feature
                .set_ratchet_control_mode(SmartShiftEnhancedStatusChange {
                    wheel_mode: Some(smartshift_to_wheel(mode)),
                    auto_disengage: NonZeroU8::new(auto_disengage),
                    tunable_torque: NonZeroU8::new(tunable_torque),
                })
                .await
                .map(|_| ())
                .map_err(|e| {
                    classify_hidpp_error(
                        e,
                        HidppOperation::WriteSmartShift,
                        SmartShiftEnhancedFeature::ID,
                    )
                }),
            // `Some(0)` encodes as `0x00` = "do not change" per the x2110 spec
            // and `SmartShiftFeature::set_ratchet_control_mode`, so this matches
            // the Enhanced branch's `NonZeroU8::new` preserve-on-zero semantics.
            Self::Legacy(feature) => feature
                .set_ratchet_control_mode(
                    Some(smartshift_to_wheel(mode)),
                    Some(auto_disengage),
                    None,
                )
                .await
                .map_err(|e| {
                    classify_hidpp_error(e, HidppOperation::WriteSmartShift, SmartShiftFeature::ID)
                }),
        }
    }

    /// Write a new auto-disengage `sensitivity`, preserving the current mode
    /// (and, on Enhanced, the tunable torque). Reads the current status first
    /// so every preserved field is written back explicitly. The [`NonZeroU8`]
    /// rules out `0`, which the device would treat as "no change" — a silent
    /// non-write rather than a real sensitivity update.
    async fn set_sensitivity(&self, value: NonZeroU8) -> Result<(), WriteError> {
        let current = self.status().await?;
        match self {
            Self::Enhanced(feature) => feature
                .set_ratchet_control_mode(SmartShiftEnhancedStatusChange {
                    wheel_mode: Some(smartshift_to_wheel(current.mode)),
                    auto_disengage: Some(value),
                    // Preserve a reported zero as “do not change”; HID++ uses
                    // zero as the sentinel and cannot write it as a target value.
                    tunable_torque: NonZeroU8::new(current.tunable_torque),
                })
                .await
                .map(|_| ())
                .map_err(|e| {
                    classify_hidpp_error(
                        e,
                        HidppOperation::WriteSmartShift,
                        SmartShiftEnhancedFeature::ID,
                    )
                }),
            Self::Legacy(_) => {
                self.set_status(SmartShiftStatus {
                    auto_disengage: value.get(),
                    ..current
                })
                .await
            }
        }
    }
}

/// Read the device's current SmartShift mode + sensitivity — companion to
/// [`toggle_smartshift`].
pub async fn get_smartshift_status(route: &DeviceRoute) -> Result<SmartShiftStatus, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let smartshift = SmartShift::open(&mut device).await?;
        smartshift.status().await
    })
    .await
}

/// Set the SmartShift auto-disengage sensitivity on `route`, preserving the
/// current mode. Returns the read-back status after the write so the caller can
/// display and verify it.
///
/// `value` is written verbatim: `0x01..=0xfe` is the auto-disengage threshold
/// (smaller = releases sooner / more sensitive) and `0xff` is permanent ratchet.
/// The [`NonZeroU8`] parameter rules out `0` at the type level — the device
/// treats a `0` threshold as "no change", so it could never be a real write.
///
/// `FeatureUnsupported` when the device exposes neither HID++ `0x2111`
/// (MX Master 3 / 3S) nor the older `0x2110` (MX Master 2S).
pub async fn set_smartshift_sensitivity(
    route: &DeviceRoute,
    value: NonZeroU8,
) -> Result<SmartShiftStatus, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let smartshift = SmartShift::open(&mut device).await?;
        smartshift.set_sensitivity(value).await?;
        smartshift.status().await
    })
    .await
}

/// Toggle SmartShift mode (free ↔ ratchet) on `route`. Reads the current
/// mode first, then writes the opposite — keeps current sensitivity.
/// Returns the new mode written.
///
/// `FeatureUnsupported` when the device exposes neither HID++ `0x2111`
/// (MX Master 3 / 3S) nor the older `0x2110` (MX Master 2S) — i.e. it has no
/// SmartShift wheel.
pub async fn toggle_smartshift(route: &DeviceRoute) -> Result<SmartShiftMode, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        toggle_smartshift_on_channel(&channel, index).await
    })
    .await
}

/// The SmartShift toggle itself, on an already-open channel at HID++ `index`.
/// Shared by [`toggle_smartshift`] and [`toggle_smartshift_on`](super::toggle_smartshift_on).
///
/// Retries once on the same transient errors as [`set_smartshift_on_channel`] —
/// a ModeShift binding can race concurrent HID++ traffic the same way (#485).
pub(super) async fn toggle_smartshift_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<SmartShiftMode, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    match toggle_once(&mut device, index).await {
        Ok(mode) => Ok(mode),
        Err(err) if is_transient_smartshift_error(&err) => {
            debug!(
                index,
                error = ?err,
                "SmartShift toggle hit a transient error; retrying once"
            );
            tokio::time::sleep(TRANSIENT_RETRY_DELAY).await;
            toggle_once(&mut device, index).await
        }
        Err(err) => Err(err),
    }
}

async fn toggle_once(device: &mut Device, index: u8) -> Result<SmartShiftMode, WriteError> {
    let smartshift = SmartShift::open(device).await?;
    let status = smartshift.status().await?;
    let next = status.mode.flipped();
    smartshift
        .set_status(SmartShiftStatus {
            mode: next,
            ..status
        })
        .await?;
    debug!(index, ?next, "wrote SmartShift mode");
    Ok(next)
}

/// Write a full SmartShift configuration — wheel mode, auto-disengage
/// threshold, and tunable torque — to `route`. These values are volatile device
/// state and should be re-applied after reconnect. Callers that mean to change
/// only one field should read the rest via [`get_smartshift_status`] first and
/// pass them back unchanged.
/// A `0` auto-disengage threshold or tunable torque is the firmware's
/// "do not change" sentinel, not a real value to apply. On a Legacy (`0x2110`)
/// device the `tunable_torque` field is ignored.
///
/// `FeatureUnsupported` when the device exposes neither HID++ `0x2111`
/// (MX Master 3 / 3S) nor the older `0x2110` (MX Master 2S).
pub async fn set_smartshift(
    route: &DeviceRoute,
    mode: SmartShiftMode,
    auto_disengage: u8,
    tunable_torque: u8,
) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        set_smartshift_on_channel(&channel, index, mode, auto_disengage, tunable_torque).await
    })
    .await
}

/// The SmartShift write itself, on an already-open channel at HID++ `index`.
/// Shared by [`set_smartshift`] and [`set_smartshift_on`](super::set_smartshift_on).
///
/// Skips the HID++ write when the device already holds the desired config, and
/// retries once after a short delay on transient device errors — the first
/// post-start reapply races concurrent opens of the same Bolt/Unifying node and
/// can return `InvalidArgument` for byte-identical parameters (#485).
pub(super) async fn set_smartshift_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    mode: SmartShiftMode,
    auto_disengage: u8,
    tunable_torque: u8,
) -> Result<(), WriteError> {
    let desired = SmartShiftStatus {
        mode,
        auto_disengage,
        tunable_torque,
    };
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let smartshift = SmartShift::open(&mut device).await?;
    if let Ok(current) = smartshift.status().await
        && status_matches_desired(current, desired)
    {
        debug!(
            index,
            ?mode,
            auto_disengage,
            tunable_torque,
            "SmartShift already matches config; skipping write"
        );
        return Ok(());
    }
    match smartshift.set_status(desired).await {
        Ok(()) => {
            debug!(
                index,
                ?mode,
                auto_disengage,
                tunable_torque,
                "wrote SmartShift config"
            );
            Ok(())
        }
        Err(err) if is_transient_smartshift_error(&err) => {
            debug!(
                index,
                error = ?err,
                "SmartShift write hit a transient error; retrying once"
            );
            tokio::time::sleep(TRANSIENT_RETRY_DELAY).await;
            // Re-open: the first attempt may have bound the wrong feature index
            // after a mis-delivered root.get_feature response.
            let smartshift = SmartShift::open(&mut device).await?;
            smartshift.set_status(desired).await?;
            debug!(
                index,
                ?mode,
                auto_disengage,
                tunable_torque,
                "wrote SmartShift config"
            );
            Ok(())
        }
        Err(err) => Err(err),
    }
}
