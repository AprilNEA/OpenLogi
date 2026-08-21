use std::sync::Arc;
use std::time::Duration;

use hidpp::{
    channel::{ChannelError, HidppChannel},
    device::Device,
    feature::{
        CreatableFeature,
        color_led_effects::{ColorLedEffectsFeature, Persistence, ZONE_EFFECT_PARAM_COUNT},
        per_key_lighting::{
            FramePersistence, MAX_SINGLE_VALUE_ZONES, PerKeyLightingFeature, Rgb,
            ZONE_PRESENCE_PAGE_LEN, ZonePresencePage,
        },
    },
};
use tracing::debug;

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

/// HID++ `PerKeyLighting` (`0x8080`) — streams each key's colour individually.
/// Its feature *index* varies per device, so it's resolved at runtime.
const PER_KEY_LIGHTING_FEATURE: u16 = 0x8080;
/// HID++ `ColorLedEffects` (`0x8070`) — the keyboard's effect engine. Writing a
/// *fixed* effect here replaces a running onboard profile, which a per-key
/// (`0x8080`) write can't override on G-series keyboards (the firmware keeps
/// replaying its stored effect). Preferred for a solid colour for that reason.
const COLOR_LED_EFFECTS_FEATURE: u16 = 0x8070;

// HID++ 2.0 report ids: 0x12 is the 64-byte "very long" report that streams a
// batch of (keyID, R, G, B) entries; 0x11 is the 20-byte "long" report used both
// to commit a per-key frame and to carry a single `ColorLedEffects` request.
const REPORT_SET_KEYS: u8 = 0x12;
const REPORT_LONG: u8 = 0x11;
// Function byte = `function_id << 4 | software_id`. Software id 0xa just tags our
// requests; for 0x8080: function 0x3 streams a key range, 0x5 commits the frame.
const SW_ID: u8 = 0x0a;
const FN_SET_KEY_RANGE: u8 = 0x3;
const FN_FRAME_END: u8 = 0x5;
// Fixed bytes of the "set key range" payload: a mode flag (byte 5) and the
// per-frame entry count (byte 7), which is also the chunk size below.
const SET_RANGE_MODE: u8 = 0x01;
const KEYS_PER_FRAME: u8 = 0x0e;

// 0x8070 `ColorLedEffects`: zone-effect index 0x01 is the fixed/static single
// colour, applied volatilely (RAM only) so it shows live and overrides the
// running onboard profile without touching flash. Reboot survival comes from the
// agent re-applying the saved colour on device arrival (orchestrator reapply),
// avoiding flash wear on every colour pick.
const EFFECT_FIXED: u8 = 0x01;
// The old raw `0x8070` path intentionally wrote only zones 0..4: enough for the
// keyboards this path targets and bounded by a small, predictable delay budget.
// Keep that cap even though the typed wrapper can query the reported zone count;
// a malformed or unexpectedly large count should not stall a color apply.
const MAX_COLOR_LED_EFFECT_ZONES: u8 = 4;
// Zones are paced apart because the controller can drop closely-spaced reports.
const FRAME_GAP: Duration = Duration::from_millis(8);

/// Which HID++ lighting path drives a solid keyboard colour. [`Auto`] is what
/// the GUI/agent use; the explicit variants exist for the `diag` A/B test.
///
/// [`Auto`]: LightingMethod::Auto
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightingMethod {
    /// Prefer `ColorLedEffects` (`0x8070`), falling back to `PerKeyLighting2`
    /// (`0x8081`) and then `PerKeyLighting` (`0x8080`) when the device exposes
    /// no effect engine.
    Auto,
    /// Force `ColorLedEffects` (`0x8070`) — the fixed-effect override.
    Effects,
    /// Force `PerKeyLighting` (`0x8080`) — the raw per-key stream.
    PerKey,
    /// Force `PerKeyLighting2` (`0x8081`) — the zone-addressed successor to
    /// `0x8080`.
    PerKeyV2,
}

/// Set a keyboard to a solid `(r, g, b)` colour, choosing the HID++ path
/// automatically: the `0x8070` effect engine (which overrides the onboard
/// profile) when present, else the `0x8080` per-key stream. `FeatureUnsupported`
/// when the device exposes neither.
pub async fn set_keyboard_color(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    set_keyboard_color_with(backend, route, LightingMethod::Auto, r, g, b).await
}

/// [`set_keyboard_color`] with an explicit [`LightingMethod`]. `Auto` tries
/// `0x8070` first and falls back to `0x8080` only when the effect engine is
/// absent (a missing-`0x8070` `FeatureUnsupported`); any other error propagates.
pub async fn set_keyboard_color_with(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    method: LightingMethod,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    let device_index = route.device_index();
    with_route(backend, route, move |channel| async move {
        set_keyboard_color_with_on_channel(&channel, device_index, method, r, g, b).await
    })
    .await
}

pub(super) async fn set_keyboard_color_with_on_channel(
    channel: &Arc<HidppChannel>,
    device_index: u8,
    method: LightingMethod,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    match method {
        LightingMethod::PerKey => set_color_per_key(channel, device_index, r, g, b).await,
        LightingMethod::PerKeyV2 => set_color_per_key_v2(channel, device_index, r, g, b).await,
        LightingMethod::Effects => set_color_effects(channel, device_index, r, g, b).await,
        LightingMethod::Auto => match set_color_effects(channel, device_index, r, g, b).await {
            Err(WriteError::FeatureUnsupported { feature_hex })
                if feature_hex == COLOR_LED_EFFECTS_FEATURE =>
            {
                debug!("no 0x8070 effect engine — trying the per-key paths");
                // 0x8081 supersedes 0x8080 and is the one newer keyboards ship,
                // so it is tried first; a device with neither reports the
                // original 0x8080 as missing, which is the error this fallback
                // chain has always ended with.
                match set_color_per_key_v2(channel, device_index, r, g, b).await {
                    Err(WriteError::FeatureUnsupported { feature_hex })
                        if feature_hex == PerKeyLightingFeature::ID =>
                    {
                        debug!("no 0x8081 per-key zones — falling back to 0x8080 per-key");
                        set_color_per_key(channel, device_index, r, g, b).await
                    }
                    other => other,
                }
            }
            other => other,
        },
    }
}

/// Resolve `route`'s runtime feature *index* for HID++ `feature_id`. `Ok(None)`
/// when the device doesn't expose it; the index differs per device, so callers
/// can't hard-code it.
async fn resolve_feature_index(
    channel: &Arc<HidppChannel>,
    device_index: u8,
    feature_id: u16,
) -> Result<Option<u8>, WriteError> {
    let device = Device::new(Arc::clone(channel), device_index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable {
            index: device_index,
        })?;
    let info = device
        .root()
        .get_feature(feature_id)
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::ResolveFeature, feature_id))?;
    Ok(info.map(|i| i.index))
}

/// Set a solid colour via `ColorLedEffects` (`0x8070`): a fixed effect per zone,
/// stored in RAM only (overrides the running onboard profile without touching
/// flash). `FeatureUnsupported` when the device exposes no `0x8070`.
///
/// Uses the typed [`ColorLedEffectsFeature`] wrapper: the real zone count is read
/// first so only existing zones are driven (a typed `set_zone_effect` awaits the
/// device's reply, so unlike the former raw fire-and-forget path a write to a
/// non-existent zone would surface as an error rather than a silent no-op).
async fn set_color_effects(
    channel: &Arc<HidppChannel>,
    index: u8,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<ColorLedEffectsFeature>(&mut device).await?;
    let zone_count = feature
        .get_info()
        .await
        .map_err(classify_lighting_error)?
        .zone_count;

    let mut params = [0u8; ZONE_EFFECT_PARAM_COUNT];
    params[0] = r;
    params[1] = g;
    params[2] = b;
    let zones_to_write = if zone_count == 0 {
        debug!(
            index,
            "0x8070 reported zero zones; applying legacy 4-zone fallback"
        );
        MAX_COLOR_LED_EFFECT_ZONES
    } else {
        zone_count.min(MAX_COLOR_LED_EFFECT_ZONES)
    };
    if zone_count > MAX_COLOR_LED_EFFECT_ZONES {
        debug!(
            index,
            zone_count,
            capped_zone_count = MAX_COLOR_LED_EFFECT_ZONES,
            "0x8070 zone count capped to legacy write limit"
        );
    }
    for zone in 0..zones_to_write {
        feature
            .set_zone_effect(zone, EFFECT_FIXED, params, Persistence::Volatile)
            .await
            .map_err(classify_lighting_error)?;
        tokio::time::sleep(FRAME_GAP).await;
    }
    debug!(
        index,
        zone_count, zones_to_write, r, g, b, "set keyboard colour via typed 0x8070"
    );
    Ok(())
}

/// Classify a HID++ error from the `ColorLedEffects` functions.
fn classify_lighting_error(error: hidpp::protocol::v20::Hidpp20Error) -> WriteError {
    classify_hidpp_error(error, HidppOperation::Lighting, ColorLedEffectsFeature::ID)
}

/// Set a solid colour via `PerKeyLighting2` (`0x8081`): paint every zone the
/// device reports as present, then commit the frame. `FeatureUnsupported` when
/// the device exposes no `0x8081` or reports no zones.
///
/// `0x8081` supersedes `0x8080`. It addresses *zones* rather than HID key
/// usages and answers each request, so unlike the raw `0x8080` stream a write
/// to a zone the device does not have surfaces as an error instead of being
/// swallowed. Nothing had ever driven it, which left a keyboard exposing only
/// `0x8081` with no way to set its colour at all.
///
/// Committed volatilely for the same reason as the `0x8070` path: the colour
/// shows live without a flash write on every colour pick, and the agent
/// re-applies the saved colour on device arrival.
async fn set_color_per_key_v2(
    channel: &Arc<HidppChannel>,
    index: u8,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<PerKeyLightingFeature>(&mut device).await?;

    let zones = present_zones(&feature).await?;
    if zones.is_empty() {
        // The device announces 0x8081 but claims no zones, so there is nothing
        // to paint — and that won't change on retry. Reported as unsupported so
        // `Auto` falls through to the 0x8080 stream.
        debug!(index, "0x8081 reported no present zones");
        return Err(WriteError::FeatureUnsupported {
            feature_hex: PerKeyLightingFeature::ID,
        });
    }

    let color = Rgb {
        red: r,
        green: g,
        blue: b,
    };
    // One request carries at most MAX_SINGLE_VALUE_ZONES ids and silently
    // ignores the rest, so the chunking is the caller's job.
    for chunk in zones.chunks(MAX_SINGLE_VALUE_ZONES) {
        feature
            .set_rgb_zones_single_value(color, chunk)
            .await
            .map_err(classify_per_key_v2_error)?;
    }
    feature
        .frame_end(FramePersistence::Volatile, 0, 0)
        .await
        .map_err(classify_per_key_v2_error)?;

    debug!(
        index,
        zone_count = zones.len(),
        r,
        g,
        b,
        "set keyboard colour via typed 0x8081"
    );
    Ok(())
}

/// Every zone id `0x8081` reports as present, read across all three presence
/// pages.
///
/// Ids `0` and `0xff` are end-of-list sentinels the feature rejects, so they
/// are skipped even if a device sets their bits.
async fn present_zones(feature: &PerKeyLightingFeature) -> Result<Vec<u8>, WriteError> {
    let mut zones = Vec::new();
    for (page, base) in [
        (ZonePresencePage::Zones0To111, 0u16),
        (ZonePresencePage::Zones112To223, 112),
        (ZonePresencePage::Zones224To255, 224),
    ] {
        let bitfield = feature
            .get_rgb_zone_presence(page)
            .await
            .map_err(classify_per_key_v2_error)?;
        collect_present_zones(base, &bitfield, &mut zones);
    }
    Ok(zones)
}

/// Appends the zone ids whose presence bit is set in `bitfield`, a 112-bit
/// field covering ids `base..base + 112` (bit `i` LSB-first within each byte).
///
/// The last page covers only 224..=255, so its high bits are padding; ids past
/// 255 are skipped rather than wrapped. Ids `0` and `0xff` are the feature's
/// end-of-list sentinels and are skipped even if a device sets their bits.
pub(super) fn collect_present_zones(
    base: u16,
    bitfield: &[u8; ZONE_PRESENCE_PAGE_LEN],
    zones: &mut Vec<u8>,
) {
    for (byte_index, byte) in bitfield.iter().enumerate() {
        for bit in 0..8u16 {
            if byte & (1 << bit) == 0 {
                continue;
            }
            let Ok(offset) = u16::try_from(byte_index * 8) else {
                continue;
            };
            let Ok(zone_id) = u8::try_from(base + offset + bit) else {
                continue;
            };
            if !matches!(zone_id, 0 | 0xff) {
                zones.push(zone_id);
            }
        }
    }
}

/// Classify a HID++ error from the `PerKeyLighting2` functions.
fn classify_per_key_v2_error(error: hidpp::protocol::v20::Hidpp20Error) -> WriteError {
    classify_hidpp_error(error, HidppOperation::Lighting, PerKeyLightingFeature::ID)
}

/// Set a solid colour via `PerKeyLighting` (`0x8080`): stream every key's colour
/// in 64-byte `0x12` frames, then commit. `FeatureUnsupported` when the device
/// exposes no `0x8080`.
async fn set_color_per_key(
    channel: &Arc<HidppChannel>,
    device_index: u8,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    let feature_index = resolve_feature_index(channel, device_index, PER_KEY_LIGHTING_FEATURE)
        .await?
        .ok_or(WriteError::FeatureUnsupported {
            feature_hex: PER_KEY_LIGHTING_FEATURE,
        })?;

    for report in per_key_reports(device_index, feature_index, r, g, b) {
        let written = channel
            .write_raw_report(&report)
            .await
            .map_err(classify_raw_lighting_error)?;
        if written != report.len() {
            return Err(WriteError::Hidpp(format!(
                "raw lighting report wrote {written} of {} bytes",
                report.len()
            )));
        }
    }
    debug!(
        device_index,
        feature_index, r, g, b, "set keyboard colour via 0x8080"
    );
    Ok(())
}

pub(super) fn per_key_reports(
    device_index: u8,
    feature_index: u8,
    r: u8,
    g: u8,
    b: u8,
) -> Vec<Vec<u8>> {
    let mut reports = Vec::new();
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
        reports.push(rep);
    }
    let mut commit = vec![0u8; 20];
    commit[0] = REPORT_LONG;
    commit[1] = device_index;
    commit[2] = feature_index;
    commit[3] = (FN_FRAME_END << 4) | SW_ID;
    reports.push(commit);
    reports
}

fn classify_raw_lighting_error(error: ChannelError) -> WriteError {
    match error {
        ChannelError::Timeout => WriteError::RequestTimedOut {
            operation: HidppOperation::Lighting,
        },
        other => WriteError::Hidpp(format!("{other:?}")),
    }
}

/// Set a solid keyboard colour on an already-open [`SharedChannel`], using
/// [`LightingMethod::Auto`].
pub async fn set_keyboard_color_on(
    shared: &SharedChannel,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    set_keyboard_color_with_on(shared, LightingMethod::Auto, r, g, b).await
}

/// Set a solid keyboard colour on an already-open [`SharedChannel`] with an
/// explicit lighting method.
pub async fn set_keyboard_color_with_on(
    shared: &SharedChannel,
    method: LightingMethod,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    set_keyboard_color_with_on_channel(shared.channel(), shared.device_index(), method, r, g, b)
        .await
}
