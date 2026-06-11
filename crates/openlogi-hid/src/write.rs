//! HID++ writes back to the device — DPI and SmartShift.
//!
//! Each entry point takes a [`DeviceRoute`] and resolves it to an open channel
//! through [`open_route_channel`], so the same call works whether the device is
//! behind a Bolt receiver or attached directly (USB cable / Bluetooth). Each
//! call re-enumerates and re-opens — fine at the frequency this is invoked
//! (once per slider release) — unless a [`SharedChannel`] from the capture
//! session is reused.

use std::num::NonZeroU8;
use std::sync::Arc;

use async_hid::AsyncHidWrite;
use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::CreatableFeature,
    feature::adjustable_dpi::AdjustableDpiFeature,
    feature::smartshift::{SmartShiftFeature, WheelMode},
    protocol::v20::{ErrorType, Hidpp20Error},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;

use crate::route::{DeviceRoute, open_route_channel};
use crate::smartshift::{SmartShiftFeatureV0, SmartShiftMode, SmartShiftStatus};

// Serializable + Clone so it can cross the agent↔GUI IPC unchanged: the GUI
// classifies a device read/write error as permanent (FeatureUnsupported /
// EmptyDpiList) vs transient, so the discriminating variant must survive the
// wire — stringifying it would collapse every case to "transient" and a device
// that genuinely lacks a feature would be re-probed forever. Variant order is
// therefore wire format: changes require a `PROTOCOL_VERSION` bump (guarded
// by `openlogi-agent-core/tests/wire_format.rs`).
#[derive(Debug, Clone, Error, Serialize, Deserialize)]
pub enum WriteError {
    // `async_hid::HidError` isn't `Serialize`, so carry its message as text; the
    // typed error is never matched on (only constructed + displayed).
    #[error("HID transport error: {0}")]
    Hid(String),
    #[error("no connected device matched the route")]
    DeviceNotFound,
    #[error("device at index {index:#04x} did not respond to HID++")]
    DeviceUnreachable { index: u8 },
    #[error("device does not expose HID++ feature {feature_hex:#06x}")]
    FeatureUnsupported { feature_hex: u16 },
    #[error("device returned no supported DPI values")]
    EmptyDpiList,
    #[error("HID++ protocol error: {0}")]
    Hidpp(String),
}

impl From<async_hid::HidError> for WriteError {
    fn from(e: async_hid::HidError) -> Self {
        Self::Hid(e.to_string())
    }
}

/// Supported DPI values reported by a device's HID++ AdjustableDpi feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpiCapabilities {
    values: Vec<u16>,
}

impl DpiCapabilities {
    /// Build capabilities from a device-reported DPI list. Values are sorted
    /// and deduplicated so callers can rely on stable ordering.
    pub fn new(mut values: Vec<u16>) -> Result<Self, WriteError> {
        values.sort_unstable();
        values.dedup();
        if values.is_empty() {
            return Err(WriteError::EmptyDpiList);
        }
        Ok(Self { values })
    }

    /// All supported DPI values, sorted ascending.
    #[must_use]
    pub fn values(&self) -> &[u16] {
        &self.values
    }

    /// Minimum supported DPI.
    #[must_use]
    pub fn min(&self) -> u16 {
        self.values[0]
    }

    /// Maximum supported DPI.
    #[must_use]
    pub fn max(&self) -> u16 {
        self.values[self.values.len() - 1]
    }

    /// Whether `dpi` is exactly supported by the device.
    #[must_use]
    pub fn contains(&self, dpi: u16) -> bool {
        self.values.binary_search(&dpi).is_ok()
    }

    /// The supported DPI nearest to `dpi`.
    #[must_use]
    pub fn nearest(&self, dpi: u32) -> u16 {
        let mut nearest = self.values[0];
        let mut best_delta = u32::from(nearest).abs_diff(dpi);
        for &candidate in &self.values[1..] {
            let delta = u32::from(candidate).abs_diff(dpi);
            if delta < best_delta {
                nearest = candidate;
                best_delta = delta;
            }
        }
        nearest
    }

    /// Snap `dpi` to the nearest supported value, widened to `u32` for UI math.
    /// The single home for "round a DPI onto this device's grid" — callers that
    /// hold an `Option<DpiCapabilities>` should `map_or(dpi, |c| c.snap(dpi))`.
    #[must_use]
    pub fn snap(&self, dpi: u32) -> u32 {
        u32::from(self.nearest(dpi))
    }

    /// Best-effort step size for UI widgets that need a single increment.
    /// Returns the smallest positive gap between adjacent reported values.
    #[must_use]
    pub fn step_hint(&self) -> u16 {
        self.values
            .windows(2)
            .filter_map(|pair| pair[1].checked_sub(pair[0]))
            .filter(|step| *step > 0)
            .min()
            .unwrap_or(1)
    }

    /// A supported value different from `current`, for diagnostic write tests.
    #[must_use]
    pub fn adjacent_test_target(&self, current: u16) -> Option<u16> {
        if self.values.len() < 2 {
            return None;
        }
        match self.values.binary_search(&current) {
            Ok(index) if index + 1 < self.values.len() => Some(self.values[index + 1]),
            Ok(index) if index > 0 => Some(self.values[index - 1]),
            Ok(_) => None,
            Err(index) if index < self.values.len() => Some(self.values[index]),
            Err(_) => self.values.last().copied(),
        }
        .filter(|target| *target != current)
    }
}

/// Current DPI plus the supported values reported by the device.
///
/// Crosses the agent↔GUI IPC (`read_dpi`, [`DpiCapabilities`] included), so
/// field order is wire format — changes require a `PROTOCOL_VERSION` bump
/// (guarded by `openlogi-agent-core/tests/wire_format.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpiInfo {
    /// DPI currently configured on sensor 0.
    pub current: u16,
    /// Supported values reported by the device for sensor 0.
    pub capabilities: DpiCapabilities,
}

/// Snapshot of one HID++ feature exposed by a device: protocol ID +
/// version. Returned by [`dump_features`] for diagnostics.
#[derive(Debug, Clone, Copy)]
pub struct FeatureEntry {
    pub id: u16,
    pub version: u8,
}

/// Enumerate every HID++ feature the device on `route` reports — used by
/// `openlogi diag features` to confirm which DPI / SmartShift / etc.
/// feature IDs a given peripheral actually exposes (e.g. some mice use
/// `0x2202 ExtendedAdjustableDpi` instead of `0x2201 AdjustableDpi`).
pub async fn dump_features(route: &DeviceRoute) -> Result<Vec<FeatureEntry>, WriteError> {
    use hidpp::feature::feature_set::FeatureSetFeature;
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        // The root feature exposes the FeatureSet (0x0001) at a fixed
        // address; we look it up directly rather than going through
        // `enumerate_features` so the iteration is observable.
        let feature_set_info = device
            .root()
            .get_feature(FeatureSetFeature::ID)
            .await
            .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?
            .ok_or(WriteError::FeatureUnsupported {
                feature_hex: FeatureSetFeature::ID,
            })?;
        let feature_set = device.add_feature::<FeatureSetFeature>(feature_set_info.index);
        let count = feature_set
            .count()
            .await
            .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
        let mut entries = Vec::with_capacity(usize::from(count));
        for i in 0..=count {
            let info = feature_set
                .get_feature(i)
                .await
                .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
            entries.push(FeatureEntry {
                id: info.id,
                version: info.version,
            });
        }
        Ok(entries)
    })
    .await
}

/// Look up `F` on a device by HID++ feature ID, register it with
/// [`Device::add_feature`], and return the typed wrapper.
///
/// The direct lookup via `root().get_feature(id)` returns the assigned index
/// unconditionally; `add_feature` then attaches our wrapper to that index. This
/// keeps route-based write/read paths independent from full feature-table
/// enumeration and also works for feature wrappers that are not in the central
/// registry yet.
async fn open_feature<F: CreatableFeature + 'static>(
    device: &mut Device,
) -> Result<Arc<F>, WriteError> {
    let info = device
        .root()
        .get_feature(F::ID)
        .await
        .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?
        .ok_or(WriteError::FeatureUnsupported { feature_hex: F::ID })?;
    Ok(device.add_feature::<F>(info.index))
}

/// Whether a failure to open the `0x2111` Enhanced SmartShift feature should
/// trigger the `0x2110` legacy fallback. Only a missing-`0x2111` feature
/// qualifies; transport and protocol errors propagate unchanged so a real
/// failure is never masked by a second open attempt.
fn is_missing_enhanced(err: &WriteError) -> bool {
    matches!(
        err,
        WriteError::FeatureUnsupported { feature_hex } if *feature_hex == 0x2111
    )
}

/// Map the fork's `0x2110` [`WheelMode`] onto OpenLogi's [`SmartShiftMode`].
/// A future `#[non_exhaustive]` variant maps to [`SmartShiftMode::Ratchet`],
/// the "safe" clicky default OpenLogi uses elsewhere. (Reserved wire bytes
/// never reach here — the fork's `get_ratchet_control_mode` rejects them.)
fn wheel_mode_to_smartshift(wheel: WheelMode) -> SmartShiftMode {
    if matches!(wheel, WheelMode::Freespin) {
        SmartShiftMode::Free
    } else {
        SmartShiftMode::Ratchet
    }
}

/// Map OpenLogi's [`SmartShiftMode`] onto the fork's `0x2110` [`WheelMode`] —
/// the inverse of [`wheel_mode_to_smartshift`], used when writing the legacy
/// ratchet-control mode.
fn smartshift_to_wheel(mode: SmartShiftMode) -> WheelMode {
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
    Enhanced(Arc<SmartShiftFeatureV0>),
    /// `0x2110 SmartShiftWheel`.
    Legacy(Arc<SmartShiftFeature>),
}

impl SmartShift {
    /// Open whichever SmartShift feature the device exposes. Tries `0x2111`
    /// first; on a missing-`0x2111` error (and only that), retries with
    /// `0x2110`. Any other error from the first attempt propagates unchanged.
    async fn open(device: &mut Device) -> Result<Self, WriteError> {
        match open_feature::<SmartShiftFeatureV0>(device).await {
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
            Self::Enhanced(feature) => feature
                .get_status()
                .await
                .map_err(|e| WriteError::Hidpp(format!("{e:?}"))),
            Self::Legacy(feature) => {
                let rcm = feature
                    .get_ratchet_control_mode()
                    .await
                    .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
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
                .set_status(mode, auto_disengage, tunable_torque)
                .await
                .map_err(|e| WriteError::Hidpp(format!("{e:?}"))),
            Self::Legacy(feature) => feature
                .set_ratchet_control_mode(
                    Some(smartshift_to_wheel(mode)),
                    Some(auto_disengage),
                    None,
                )
                .await
                .map_err(|e| WriteError::Hidpp(format!("{e:?}"))),
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

/// Read the device's current DPI on sensor 0 — companion to [`set_dpi`].
/// Used by `openlogi diag dpi` and any future Settings → Diagnostics
/// surface that wants to display the current value without writing.
pub async fn get_dpi(route: &DeviceRoute) -> Result<u16, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let feature = open_feature::<AdjustableDpiFeature>(&mut device).await?;
        feature
            .get_sensor_dpi(0)
            .await
            .map_err(|e| WriteError::Hidpp(format!("{e:?}")))
    })
    .await
}

/// Classify a HID++ error from the AdjustableDpi functions. A device that
/// announces `0x2201` but rejects a function (`Unsupported` /
/// `InvalidFunctionId`) or returns a structurally invalid DPI list
/// (`UnsupportedResponse`) will keep doing so, so these map to the permanent
/// [`WriteError::FeatureUnsupported`]; channel/timeout errors stay transient
/// [`WriteError::Hidpp`] so callers may retry.
fn classify_dpi_error(error: Hidpp20Error) -> WriteError {
    match error {
        Hidpp20Error::Feature(ErrorType::Unsupported | ErrorType::InvalidFunctionId)
        | Hidpp20Error::UnsupportedResponse => WriteError::FeatureUnsupported {
            feature_hex: AdjustableDpiFeature::ID,
        },
        other => WriteError::Hidpp(format!("{other:?}")),
    }
}

/// Read the current DPI and the supported DPI values for sensor 0 in one
/// route/channel session.
pub async fn get_dpi_info(route: &DeviceRoute) -> Result<DpiInfo, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let feature = open_feature::<AdjustableDpiFeature>(&mut device).await?;
        let sensor_count = feature
            .get_sensor_count()
            .await
            .map_err(classify_dpi_error)?;
        if sensor_count == 0 {
            // The device claims AdjustableDpi but exposes no sensor — it cannot
            // report DPI, and that won't change on retry.
            return Err(WriteError::FeatureUnsupported {
                feature_hex: AdjustableDpiFeature::ID,
            });
        }
        let current = feature
            .get_sensor_dpi(0)
            .await
            .map_err(classify_dpi_error)?;
        let values = feature
            .get_sensor_dpi_list(0)
            .await
            .map_err(classify_dpi_error)?;
        Ok(DpiInfo {
            current,
            capabilities: DpiCapabilities::new(values)?,
        })
    })
    .await
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

pub async fn set_dpi(route: &DeviceRoute, dpi: u16) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        set_dpi_on_channel(&channel, index, dpi).await
    })
    .await
}

/// HID++ `PerKeyLighting` feature — drives the per-key RGB on wired G-series
/// keyboards. Its feature *index* varies per device, so it's resolved at runtime.
const PER_KEY_LIGHTING_FEATURE: u16 = 0x8080;

// HID++ 2.0 report ids for the 0x8080 per-key frames: 0x12 is the 64-byte "very
// long" report that streams a batch of (keyID, R, G, B) entries; 0x11 is the
// 20-byte "long" report that commits the streamed frame.
const REPORT_SET_KEYS: u8 = 0x12;
const REPORT_FRAME_END: u8 = 0x11;
// Function byte = `function_id << 4 | software_id`. Software id 0xa just tags our
// requests; function 0x3 streams a key range, 0x5 ends/commits the frame.
const SW_ID: u8 = 0x0a;
const FN_SET_KEY_RANGE: u8 = 0x3;
const FN_FRAME_END: u8 = 0x5;
// Fixed bytes of the "set key range" payload: a mode flag (byte 5) and the
// per-frame entry count (byte 7), which is also the chunk size below.
const SET_RANGE_MODE: u8 = 0x01;
const KEYS_PER_FRAME: u8 = 0x0e;

/// Set a keyboard's per-key RGB to a solid `(r, g, b)` colour via HID++
/// `PerKeyLighting` (`0x8080`): stream every key's colour in 64-byte `0x12`
/// "set group keys" frames, then commit the frame.
///
/// The `0x8080` feature index is resolved per device (it differs across the
/// G-series), so this isn't tied to the G513. `FeatureUnsupported` when the
/// device exposes no `0x8080` (e.g. a non-RGB peripheral).
pub async fn set_keyboard_color(
    route: &DeviceRoute,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    // Resolve this device's runtime feature index for 0x8080 over the HID++
    // channel; the per-key frames are then sent as raw 0x12 reports (which the
    // typed channel can't model) on a freshly opened writer.
    let device_index = route.device_index();
    let feature_index = with_route(route, move |channel| async move {
        let device = Device::new(Arc::clone(&channel), device_index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable {
                index: device_index,
            })?;
        let info = device
            .root()
            .get_feature(PER_KEY_LIGHTING_FEATURE)
            .await
            .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?
            .ok_or(WriteError::FeatureUnsupported {
                feature_hex: PER_KEY_LIGHTING_FEATURE,
            })?;
        Ok(info.index)
    })
    .await?;

    let Some(mut writer) = crate::transport::open_route_writer(route).await? else {
        return Err(WriteError::DeviceNotFound);
    };
    // Each 64-byte `0x12` "set group keys" packet carries up to 14
    // `(keyID, R, G, B)` entries; keyIDs are HID usage codes. Cover the whole
    // keyboard usage range (incl. modifiers at `0xe0..`) so every key lights,
    // then commit the frame.
    let key_ids: Vec<u8> = (0x00u8..=0xe8).collect();
    for chunk in key_ids.chunks(KEYS_PER_FRAME as usize) {
        let mut rep = vec![0u8; 64];
        rep[0] = REPORT_SET_KEYS;
        rep[1] = device_index;
        rep[2] = feature_index;
        rep[3] = (FN_SET_KEY_RANGE << 4) | SW_ID;
        rep[5] = SET_RANGE_MODE;
        rep[7] = KEYS_PER_FRAME;
        for (i, &key) in chunk.iter().enumerate() {
            let off = 8 + i * 4;
            rep[off] = key;
            rep[off + 1] = r;
            rep[off + 2] = g;
            rep[off + 3] = b;
        }
        writer
            .write_output_report(&rep)
            .await
            .map_err(WriteError::from)?;
    }
    let mut commit = vec![0u8; 20];
    commit[0] = REPORT_FRAME_END;
    commit[1] = device_index;
    commit[2] = feature_index;
    commit[3] = (FN_FRAME_END << 4) | SW_ID;
    writer
        .write_output_report(&commit)
        .await
        .map_err(WriteError::from)?;
    debug!(
        device_index,
        feature_index, r, g, b, "wrote keyboard colour"
    );
    Ok(())
}

/// The DPI write itself, on an already-open channel at HID++ `index`. Shared by
/// [`set_dpi`] (which opens a fresh channel) and [`set_dpi_on`] (which reuses
/// one).
async fn set_dpi_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    dpi: u16,
) -> Result<(), WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<AdjustableDpiFeature>(&mut device).await?;
    feature
        .set_sensor_dpi(0, dpi)
        .await
        .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
    // Read back to confirm the firmware accepted the value. A mismatch is a
    // silent failure mode that's otherwise invisible — devices in low-power
    // states or with unsupported DPI ranges can ACK the write yet keep the old
    // value. We log a warning but still return Ok because the request reached
    // the device.
    if let Ok(actual) = feature.get_sensor_dpi(0).await {
        if actual == dpi {
            debug!(index, dpi, "wrote DPI (verified)");
        } else {
            tracing::warn!(
                index,
                requested = dpi,
                actual,
                "DPI write accepted but device reports a different value — \
                 likely out of the device's supported range"
            );
        }
    } else {
        debug!(index, dpi, "wrote DPI (read-back skipped)");
    }
    Ok(())
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
/// Shared by [`toggle_smartshift`] and [`toggle_smartshift_on`].
async fn toggle_smartshift_on_channel(
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
/// Shared by [`set_smartshift`] and [`set_smartshift_on`].
async fn set_smartshift_on_channel(
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

/// An open HID++ channel to a device, shared so DPI / SmartShift writes can
/// reuse the capture session's connection instead of re-enumerating and
/// opening a fresh channel each time (which costs ~100ms+).
///
/// Cheap to clone (an `Arc` plus the [`DeviceRoute`] it points at). Built by
/// the capture session via [`SharedChannel::new`] and stashed in a slot the
/// GUI's write path consults.
#[derive(Clone)]
pub struct SharedChannel {
    channel: Arc<HidppChannel>,
    route: DeviceRoute,
}

impl SharedChannel {
    /// Wrap an open channel that reaches `route`.
    #[must_use]
    pub(crate) fn new(channel: Arc<HidppChannel>, route: DeviceRoute) -> Self {
        Self { channel, route }
    }

    /// Whether this channel reaches `route` — so the write path only reuses it
    /// for the device it actually points at.
    #[must_use]
    pub fn matches(&self, route: &DeviceRoute) -> bool {
        self.route == *route
    }
}

/// Write DPI on an already-open [`SharedChannel`] — the fast path that skips
/// enumeration and channel setup.
pub async fn set_dpi_on(shared: &SharedChannel, dpi: u16) -> Result<(), WriteError> {
    set_dpi_on_channel(&shared.channel, shared.route.device_index(), dpi).await
}

/// Toggle SmartShift on an already-open [`SharedChannel`].
pub async fn toggle_smartshift_on(shared: &SharedChannel) -> Result<SmartShiftMode, WriteError> {
    toggle_smartshift_on_channel(&shared.channel, shared.route.device_index()).await
}

/// Write a full SmartShift configuration on an already-open [`SharedChannel`]
/// — the fast path that skips enumeration and channel setup.
pub async fn set_smartshift_on(
    shared: &SharedChannel,
    mode: SmartShiftMode,
    auto_disengage: u8,
    tunable_torque: u8,
) -> Result<(), WriteError> {
    set_smartshift_on_channel(
        &shared.channel,
        shared.route.device_index(),
        mode,
        auto_disengage,
        tunable_torque,
    )
    .await
}

/// Boilerplate-eater: open the channel that reaches `route`, then run `f` once
/// with it. The caller addresses features at [`DeviceRoute::device_index`].
async fn with_route<F, Fut, T>(route: &DeviceRoute, f: F) -> Result<T, WriteError>
where
    F: FnOnce(Arc<HidppChannel>) -> Fut,
    Fut: std::future::Future<Output = Result<T, WriteError>>,
{
    match open_route_channel(route).await? {
        Some(channel) => f(channel).await,
        None => Err(WriteError::DeviceNotFound),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_sort_and_deduplicate_values() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![1600, 400, 800, 800])?;

        assert_eq!(caps.values(), [400, 800, 1600]);
        assert_eq!(caps.min(), 400);
        assert_eq!(caps.max(), 1600);
        Ok(())
    }

    #[test]
    fn capabilities_reject_empty_list() {
        assert!(matches!(
            DpiCapabilities::new(Vec::new()),
            Err(WriteError::EmptyDpiList)
        ));
    }

    #[test]
    fn nearest_returns_closest_supported_value() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![400, 800, 1600])?;

        assert_eq!(caps.nearest(390), 400);
        assert_eq!(caps.nearest(1000), 800);
        assert_eq!(caps.nearest(2000), 1600);
        Ok(())
    }

    #[test]
    fn step_hint_returns_smallest_positive_gap() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![400, 800, 1200, 2000])?;

        assert_eq!(caps.step_hint(), 400);
        Ok(())
    }

    #[test]
    fn adjacent_test_target_prefers_next_then_previous_value() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![400, 800, 1600])?;

        assert_eq!(caps.adjacent_test_target(400), Some(800));
        assert_eq!(caps.adjacent_test_target(800), Some(1600));
        assert_eq!(caps.adjacent_test_target(1600), Some(800));
        Ok(())
    }

    #[test]
    fn adjacent_test_target_handles_current_outside_list() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![400, 800, 1600])?;

        assert_eq!(caps.adjacent_test_target(1000), Some(1600));
        assert_eq!(caps.adjacent_test_target(2000), Some(1600));
        Ok(())
    }

    #[test]
    fn smartshift_and_wheel_mode_byte_encodings_match() {
        // The whole design relies on 0x2110 WheelMode and 0x2111
        // SmartShiftMode sharing one wire encoding (Free/Freespin = 1,
        // Ratchet = 2). If the fork ever renumbers WheelMode this fails loudly.
        assert_eq!(
            u8::from(SmartShiftMode::Free),
            u8::from(WheelMode::Freespin)
        );
        assert_eq!(
            u8::from(SmartShiftMode::Ratchet),
            u8::from(WheelMode::Ratchet)
        );
    }

    #[test]
    fn wheel_mode_maps_to_smartshift_mode() {
        assert_eq!(
            wheel_mode_to_smartshift(WheelMode::Freespin),
            SmartShiftMode::Free
        );
        assert_eq!(
            wheel_mode_to_smartshift(WheelMode::Ratchet),
            SmartShiftMode::Ratchet
        );
    }

    #[test]
    fn smartshift_to_wheel_round_trips() {
        // smartshift_to_wheel is the inverse of wheel_mode_to_smartshift.
        for mode in [SmartShiftMode::Free, SmartShiftMode::Ratchet] {
            assert_eq!(wheel_mode_to_smartshift(smartshift_to_wheel(mode)), mode);
        }
    }

    #[test]
    fn missing_enhanced_triggers_fallback() {
        assert!(is_missing_enhanced(&WriteError::FeatureUnsupported {
            feature_hex: 0x2111,
        }));
    }

    #[test]
    fn missing_legacy_does_not_trigger_fallback() {
        // A device missing 0x2110 must NOT loop back — it genuinely has no
        // SmartShift.
        assert!(!is_missing_enhanced(&WriteError::FeatureUnsupported {
            feature_hex: 0x2110,
        }));
    }

    #[test]
    fn transport_errors_do_not_trigger_fallback() {
        // Real failures must propagate, not be masked by a fallback attempt.
        assert!(!is_missing_enhanced(&WriteError::DeviceUnreachable {
            index: 0xff,
        }));
        assert!(!is_missing_enhanced(&WriteError::Hidpp("boom".into())));
    }
}
