use std::num::NonZeroU8;
use std::sync::Arc;
use std::time::Duration;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature,
        smartshift::{SmartShiftFeature, WheelMode},
        smartshift_enhanced::{
            SmartShiftEnhancedCapabilities, SmartShiftEnhancedFeature,
            SmartShiftEnhancedStatusChange,
        },
    },
};
use tracing::debug;

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;
use openlogi_core::hid::smartshift::{
    SmartShiftAutoDisengage, SmartShiftMode, SmartShiftStatus, TunableTorque,
};

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

/// Whether `current` already satisfies a desired SmartShift write. An absent
/// tunable-torque level means the device does not support it, so that field is
/// preserved rather than compared or written.
pub(super) fn status_matches_desired(current: SmartShiftStatus, desired: SmartShiftStatus) -> bool {
    current.mode == desired.mode
        && current.auto_disengage == desired.auto_disengage
        && desired
            .tunable_torque
            .is_none_or(|torque| current.tunable_torque == Some(torque))
}

fn decode_auto_disengage(
    value: u8,
    feature_hex: u16,
) -> Result<SmartShiftAutoDisengage, WriteError> {
    SmartShiftAutoDisengage::try_from(value).map_err(|_| WriteError::UnsupportedResponse {
        operation: HidppOperation::ReadSmartShift,
        feature_hex,
    })
}

/// A SmartShift write whose reply did not carry the fields it wrote is
/// reported as an unsupported response: the device answered, but not with
/// the state it was asked for.
fn confirm_write(confirmed: bool, feature_hex: u16) -> Result<(), WriteError> {
    if confirmed {
        Ok(())
    } else {
        Err(WriteError::UnsupportedResponse {
            operation: HidppOperation::WriteSmartShift,
            feature_hex,
        })
    }
}

/// Decode the `0x2111` status reply's torque byte: `0` carries no torque
/// level, `1..=100` is a percentage of the maximum force, and anything above
/// is not a level the firmware defines.
fn decode_tunable_torque(value: u8) -> Result<Option<TunableTorque>, WriteError> {
    if value == 0 {
        return Ok(None);
    }
    TunableTorque::try_from(value)
        .map(Some)
        .map_err(|_| WriteError::UnsupportedResponse {
            operation: HidppOperation::ReadSmartShift,
            feature_hex: SmartShiftEnhancedFeature::ID,
        })
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
    /// also reports tunable torque; Legacy (`0x2110`) has no such concept.
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
                    auto_disengage: decode_auto_disengage(
                        status.auto_disengage,
                        SmartShiftEnhancedFeature::ID,
                    )?,
                    tunable_torque: decode_tunable_torque(status.current_tunable_torque)?,
                })
            }
            Self::Legacy(feature) => {
                let rcm = feature.get_ratchet_control_mode().await.map_err(|e| {
                    classify_hidpp_error(e, HidppOperation::ReadSmartShift, SmartShiftFeature::ID)
                })?;
                Ok(SmartShiftStatus {
                    mode: wheel_mode_to_smartshift(rcm.wheel_mode),
                    auto_disengage: decode_auto_disengage(
                        rcm.auto_disengage,
                        SmartShiftFeature::ID,
                    )?,
                    tunable_torque: None,
                })
            }
        }
    }

    /// Write a full desired status — wheel mode plus the auto-disengage
    /// threshold and (Enhanced only) tunable torque.
    ///
    /// A `None` torque is sent as HID++'s zero "do not change" sentinel. A
    /// requested torque is written only when the device advertises
    /// [`SmartShiftEnhancedCapabilities::TUNABLE_TORQUE`]; callers that do not
    /// mean to change the torque pass `None` rather than echoing the value
    /// they read back.
    ///
    /// Both features answer with the status after the write, and the write
    /// counts only when that status carries every field it wrote: a device
    /// that kept another mode or threshold, or ignored the torque, is a
    /// failed write rather than a confirmed one. Fields left unwritten are
    /// not compared.
    async fn set_status(&self, status: SmartShiftStatus) -> Result<(), WriteError> {
        let SmartShiftStatus {
            mode,
            auto_disengage,
            tunable_torque,
        } = status;
        let wheel_mode = smartshift_to_wheel(mode);
        let auto_disengage = NonZeroU8::from(auto_disengage);
        match self {
            Self::Enhanced(feature) => {
                let tunable_torque = writable_torque(feature, tunable_torque).await?;
                let applied = feature
                    .set_ratchet_control_mode(SmartShiftEnhancedStatusChange {
                        wheel_mode: Some(wheel_mode),
                        auto_disengage: Some(auto_disengage),
                        tunable_torque,
                    })
                    .await
                    .map_err(|e| {
                        classify_hidpp_error(
                            e,
                            HidppOperation::WriteSmartShift,
                            SmartShiftEnhancedFeature::ID,
                        )
                    })?;
                confirm_write(
                    applied.wheel_mode == wheel_mode
                        && applied.auto_disengage == auto_disengage.get()
                        && tunable_torque
                            .is_none_or(|torque| applied.current_tunable_torque == torque.get()),
                    SmartShiftEnhancedFeature::ID,
                )
            }
            Self::Legacy(feature) => {
                let applied = feature
                    .set_ratchet_control_mode(Some(wheel_mode), Some(auto_disengage), None)
                    .await
                    .map_err(|e| {
                        classify_hidpp_error(
                            e,
                            HidppOperation::WriteSmartShift,
                            SmartShiftFeature::ID,
                        )
                    })?;
                confirm_write(
                    applied.wheel_mode == wheel_mode
                        && applied.auto_disengage == auto_disengage.get(),
                    SmartShiftFeature::ID,
                )
            }
        }
    }

    /// Write a new auto-disengage `sensitivity`, preserving the current mode
    /// and leaving the tunable torque untouched.
    async fn set_sensitivity(&self, value: SmartShiftAutoDisengage) -> Result<(), WriteError> {
        let current = self.status().await?;
        self.set_status(SmartShiftStatus {
            auto_disengage: value,
            tunable_torque: None,
            ..current
        })
        .await
    }

    /// Write a new tunable `torque` alone: the wheel mode and auto-disengage
    /// threshold go out as HID++'s zero "do not change" sentinel.
    ///
    /// The capability gate is [`writable_torque`], as for [`Self::set_status`].
    /// A full status write still applies its mode and threshold when the
    /// device keeps its own torque, but this write has nothing else to apply,
    /// so a device without
    /// [`SmartShiftEnhancedCapabilities::TUNABLE_TORQUE`] — and every
    /// `0x2110` wheel — refuses it with [`WriteError::FeatureUnsupported`].
    /// The write counts only when the reply carries the torque it wrote.
    async fn set_torque(&self, value: TunableTorque) -> Result<(), WriteError> {
        let unsupported = WriteError::FeatureUnsupported {
            feature_hex: SmartShiftEnhancedFeature::ID,
        };
        let Self::Enhanced(feature) = self else {
            return Err(unsupported);
        };
        let Some(torque) = writable_torque(feature, Some(value)).await? else {
            return Err(unsupported);
        };
        let applied = feature
            .set_ratchet_control_mode(SmartShiftEnhancedStatusChange {
                wheel_mode: None,
                auto_disengage: None,
                tunable_torque: Some(torque),
            })
            .await
            .map_err(|e| {
                classify_hidpp_error(
                    e,
                    HidppOperation::WriteSmartShift,
                    SmartShiftEnhancedFeature::ID,
                )
            })?;
        confirm_write(
            applied.current_tunable_torque == torque.get(),
            SmartShiftEnhancedFeature::ID,
        )
    }
}

/// The tunable torque to put on the wire for a requested `torque`.
///
/// The status reply's torque byte is not a capability claim — only the
/// `0x2111` capabilities bit is, and both Options+ and Solaar gate their
/// torque control on it. Without the bit the torque is sent as the zero
/// "do not change" sentinel so the mode and threshold still apply.
async fn writable_torque(
    feature: &SmartShiftEnhancedFeature,
    torque: Option<TunableTorque>,
) -> Result<Option<NonZeroU8>, WriteError> {
    let Some(torque) = torque else {
        return Ok(None);
    };
    let info = feature.get_capabilities().await.map_err(|e| {
        classify_hidpp_error(
            e,
            HidppOperation::ReadSmartShift,
            SmartShiftEnhancedFeature::ID,
        )
    })?;
    if info
        .capabilities
        .contains(SmartShiftEnhancedCapabilities::TUNABLE_TORQUE)
    {
        Ok(Some(NonZeroU8::from(torque)))
    } else {
        debug!(
            %torque,
            "device does not advertise tunable torque; leaving it unchanged"
        );
        Ok(None)
    }
}

/// Read the device's current SmartShift mode + sensitivity — companion to
/// [`toggle_smartshift`].
pub async fn get_smartshift_status(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<SmartShiftStatus, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        get_smartshift_status_on_channel(&channel, index).await
    })
    .await
}

pub(super) async fn get_smartshift_status_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<SmartShiftStatus, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let smartshift = SmartShift::open(&mut device).await?;
    smartshift.status().await
}

/// Set the SmartShift auto-disengage sensitivity on `route`, preserving the
/// current mode. Returns the read-back status after the write so the caller can
/// display and verify it.
///
/// `FeatureUnsupported` when the device exposes neither HID++ `0x2111`
/// (MX Master 3 / 3S) nor the older `0x2110` (MX Master 2S).
pub async fn set_smartshift_sensitivity(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    value: SmartShiftAutoDisengage,
) -> Result<SmartShiftStatus, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let smartshift = SmartShift::open(&mut device).await?;
        smartshift.set_sensitivity(value).await?;
        smartshift.status().await
    })
    .await
}

/// Set the SmartShift tunable torque (scrolling force) on `route`, leaving the
/// mode and auto-disengage sensitivity unchanged. Returns the read-back status
/// after the write so the caller can display and verify it.
///
/// `FeatureUnsupported` when the device has no `0x2111` SmartShift wheel, or
/// has one that does not advertise tunable torque.
pub async fn set_smartshift_torque(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    value: TunableTorque,
) -> Result<SmartShiftStatus, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let smartshift = SmartShift::open(&mut device).await?;
        smartshift.set_torque(value).await?;
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
pub async fn toggle_smartshift(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<SmartShiftMode, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        toggle_smartshift_on_channel(&channel, index).await
    })
    .await
}

/// The SmartShift toggle itself, on an already-open channel at HID++ `index`.
/// Shared by [`toggle_smartshift`] and [`toggle_smartshift_on`].
///
/// Retries once on the same transient errors as [`set_smartshift_on_channel`] —
/// a ModeShift binding can race concurrent HID++ traffic the same way (#485).
/// The target is decided once, from the first read. A retried write re-sends
/// it rather than toggling again: the first write may have landed even when
/// its reply did not confirm it, and a second read-and-flip would turn the
/// mode back.
pub(super) async fn toggle_smartshift_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<SmartShiftMode, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let (smartshift, target) = retry_transient_toggle(index, async || {
        let smartshift = SmartShift::open(&mut device).await?;
        let status = smartshift.status().await?;
        let target = SmartShiftStatus {
            mode: status.mode.flipped(),
            tunable_torque: None,
            ..status
        };
        Ok((smartshift, target))
    })
    .await?;
    retry_transient_toggle(index, async || smartshift.set_status(target).await).await?;
    debug!(index, next = ?target.mode, "wrote SmartShift mode");
    Ok(target.mode)
}

/// Run one step of the toggle, and once more after a transient error.
async fn retry_transient_toggle<T>(
    index: u8,
    mut step: impl AsyncFnMut() -> Result<T, WriteError>,
) -> Result<T, WriteError> {
    match step().await {
        Err(err) if is_transient_smartshift_error(&err) => {
            debug!(
                index,
                error = ?err,
                "SmartShift toggle hit a transient error; retrying once"
            );
            tokio::time::sleep(TRANSIENT_RETRY_DELAY).await;
            step().await
        }
        result => result,
    }
}

/// Write a full SmartShift configuration to `route`. The values are volatile
/// device state and should be re-applied after reconnect. Callers that mean to
/// change one field should read the current [`SmartShiftStatus`] and update it.
///
/// The tunable torque is written only when it differs from the device's
/// current level and the device advertises tunable torque; otherwise the
/// device keeps its own value.
///
/// `FeatureUnsupported` when the device exposes neither HID++ `0x2111`
/// (MX Master 3 / 3S) nor the older `0x2110` (MX Master 2S).
pub async fn set_smartshift(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    status: SmartShiftStatus,
) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        set_smartshift_on_channel(&channel, index, status).await
    })
    .await
}

/// The SmartShift write itself, on an already-open channel at HID++ `index`.
/// Shared by [`set_smartshift`] and [`set_smartshift_on`].
///
/// Skips the HID++ write when the device already holds the desired config, and
/// retries once after a short delay on transient device errors — the first
/// post-start reapply races concurrent opens of the same Bolt/Unifying node and
/// can return `InvalidArgument` for byte-identical parameters (#485).
pub(super) async fn set_smartshift_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    desired: SmartShiftStatus,
) -> Result<(), WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let smartshift = SmartShift::open(&mut device).await?;
    let current = smartshift.status().await.ok();
    if current.is_some_and(|current| status_matches_desired(current, desired)) {
        debug!(
            index,
            status = ?desired,
            "SmartShift already matches config; skipping write"
        );
        return Ok(());
    }
    let write = SmartShiftStatus {
        tunable_torque: desired
            .tunable_torque
            .filter(|torque| current.is_none_or(|current| current.tunable_torque != Some(*torque))),
        ..desired
    };
    match smartshift.set_status(write).await {
        Ok(()) => {
            debug!(
                index,
                status = ?desired,
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
            smartshift.set_status(write).await?;
            debug!(
                index,
                status = ?desired,
                "wrote SmartShift config"
            );
            Ok(())
        }
        Err(err) => Err(err),
    }
}

/// Toggle SmartShift on an already-open [`SharedChannel`].
pub async fn toggle_smartshift_on(shared: &SharedChannel) -> Result<SmartShiftMode, WriteError> {
    toggle_smartshift_on_channel(shared.channel(), shared.device_index()).await
}

/// Read SmartShift mode and sensitivity on an already-open [`SharedChannel`].
pub async fn get_smartshift_status_on(
    shared: &SharedChannel,
) -> Result<SmartShiftStatus, WriteError> {
    get_smartshift_status_on_channel(shared.channel(), shared.device_index()).await
}

/// Write a full SmartShift configuration on an already-open [`SharedChannel`]
/// — the fast path that skips enumeration and channel setup.
pub async fn set_smartshift_on(
    shared: &SharedChannel,
    status: SmartShiftStatus,
) -> Result<(), WriteError> {
    set_smartshift_on_channel(shared.channel(), shared.device_index(), status).await
}

#[cfg(test)]
mod tests;
