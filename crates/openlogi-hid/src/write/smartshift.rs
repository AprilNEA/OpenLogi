use std::num::NonZeroU8;
use std::sync::Arc;

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

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

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
    /// first; on a missing-`0x2111` error (and only that), retries with
    /// `0x2110`. Any other error from the first attempt propagates unchanged.
    async fn open(device: &mut Device) -> Result<Self, WriteError> {
        match open_feature::<SmartShiftEnhancedFeature>(device).await {
            Ok(feature) => Ok(Self::Enhanced(feature)),
            Err(err) if is_missing_enhanced(&err) => {
                let feature = open_feature::<SmartShiftFeature>(device).await?;
                Ok(Self::Legacy(feature))
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

    /// Write a full desired status. Enhanced (`0x2111`) takes mode +
    /// auto-disengage + tunable torque directly. Legacy (`0x2110`) has no
    /// tunable-torque function, so that field is ignored; the wheel mode and
    /// auto-disengage threshold are written explicitly — never relying on the
    /// device treating a `None`/`0` field as "keep current".
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
                    auto_disengage: Some(auto_disengage),
                    tunable_torque: Some(tunable_torque),
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
        self.set_status(SmartShiftStatus {
            auto_disengage: value.get(),
            ..current
        })
        .await
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
pub(super) async fn toggle_smartshift_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<SmartShiftMode, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let smartshift = SmartShift::open(&mut device).await?;
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
/// threshold, and tunable torque — to `route`. The firmware persists all three
/// to the device's NVM. Callers that mean to change only one field should read
/// the rest via [`get_smartshift_status`] first and pass them back unchanged.
/// On a Legacy (`0x2110`) device the `tunable_torque` field is ignored.
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
pub(super) async fn set_smartshift_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    mode: SmartShiftMode,
    auto_disengage: u8,
    tunable_torque: u8,
) -> Result<(), WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let smartshift = SmartShift::open(&mut device).await?;
    smartshift
        .set_status(SmartShiftStatus {
            mode,
            auto_disengage,
            tunable_torque,
        })
        .await?;
    debug!(
        index,
        ?mode,
        auto_disengage,
        tunable_torque,
        "wrote SmartShift config"
    );
    Ok(())
}
