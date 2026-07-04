use std::time::Duration;

use async_hid::AsyncHidWrite;
use hidpp::{
    device::Device,
    feature::{
        CreatableFeature,
        color_led_effects::{ColorLedEffectsFeature, Persistence, ZONE_EFFECT_PARAM_COUNT},
        rgb_effects::{
            CLUSTER_EFFECT_PARAM_COUNT, EventsNotificationFlags, PowerModeTarget,
            RgbEffectsFeature, RgbPersistence, SwControlFlags,
        },
    },
};
use tracing::debug;

use crate::route::DeviceRoute;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

/// HID++ `PerKeyLighting` (`0x8080`) — streams each key's colour individually.
/// Its feature *index* varies per device, so it's resolved at runtime.
const PER_KEY_LIGHTING_FEATURE: u16 = 0x8080;
/// HID++ `ColorLedEffects` (`0x8070`) — the keyboard's effect engine. Writing a
/// *fixed* effect here replaces a running onboard profile, which a per-key
/// (`0x8080`) write can't override on G-series keyboards (the firmware keeps
/// replaying its stored effect). Preferred for a solid colour for that reason.
const COLOR_LED_EFFECTS_FEATURE: u16 = 0x8070;
/// HID++ `RgbEffects` (`0x8071`) — modern per-cluster RGB effect engine used
/// by newer G-series devices such as the G502 X PLUS.
const RGB_EFFECTS_FEATURE: u16 = 0x8071;

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
// 0x8071's effect id for a static RGB colour. The per-cluster *index* is
// discovered at runtime because firmware can order supported effects freely.
const RGB_EFFECT_STATIC: u16 = 0x0001;
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
    /// Prefer `ColorLedEffects` (`0x8070`), falling back to `PerKeyLighting`
    /// (`0x8080`) when the device exposes no effect engine.
    Auto,
    /// Force `ColorLedEffects` (`0x8070`) — the fixed-effect override.
    Effects,
    /// Force `PerKeyLighting` (`0x8080`) — the per-key stream.
    PerKey,
}

/// Set a keyboard to a solid `(r, g, b)` colour, choosing the HID++ path
/// automatically: the `0x8070` effect engine (which overrides the onboard
/// profile) when present, else the `0x8080` per-key stream. `FeatureUnsupported`
/// when the device exposes neither.
pub async fn set_keyboard_color(
    route: &DeviceRoute,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    set_keyboard_color_with(route, LightingMethod::Auto, r, g, b).await
}

/// [`set_keyboard_color`] with an explicit [`LightingMethod`]. `Auto` tries
/// `0x8070` first and falls back to `0x8080` only when the effect engine is
/// absent (a missing-`0x8070` `FeatureUnsupported`); any other error propagates.
pub async fn set_keyboard_color_with(
    route: &DeviceRoute,
    method: LightingMethod,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    match method {
        LightingMethod::PerKey => set_color_per_key(route, r, g, b).await,
        LightingMethod::Effects => set_color_effects(route, r, g, b).await,
        LightingMethod::Auto => set_color_auto(route, r, g, b).await,
    }
}

async fn set_color_auto(route: &DeviceRoute, r: u8, g: u8, b: u8) -> Result<(), WriteError> {
    match set_color_effects(route, r, g, b).await {
        Err(WriteError::FeatureUnsupported { feature_hex })
            if feature_hex == COLOR_LED_EFFECTS_FEATURE =>
        {
            debug!("no 0x8070 effect engine — trying 0x8071 RGB effects");
        }
        other => return other,
    }

    match set_color_rgb_effects(route, r, g, b).await {
        Err(WriteError::FeatureUnsupported { feature_hex })
            if feature_hex == RGB_EFFECTS_FEATURE =>
        {
            debug!("no 0x8071 effect engine — falling back to 0x8080 per-key");
            set_color_per_key(route, r, g, b).await
        }
        other => other,
    }
}

/// Resolve `route`'s runtime feature *index* for HID++ `feature_id`. `Ok(None)`
/// when the device doesn't expose it; the index differs per device, so callers
/// can't hard-code it.
async fn resolve_feature_index(
    route: &DeviceRoute,
    feature_id: u16,
) -> Result<Option<u8>, WriteError> {
    let device_index = route.device_index();
    with_route(route, move |channel| async move {
        let device = Device::new(std::sync::Arc::clone(&channel), device_index)
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
    })
    .await
}

/// Set a solid colour via `ColorLedEffects` (`0x8070`): a fixed effect per zone,
/// stored in RAM only (overrides the running onboard profile without touching
/// flash). `FeatureUnsupported` when the device exposes no `0x8070`.
///
/// Uses the typed [`ColorLedEffectsFeature`] wrapper: the real zone count is read
/// first so only existing zones are driven (a typed `set_zone_effect` awaits the
/// device's reply, so unlike the former raw fire-and-forget path a write to a
/// non-existent zone would surface as an error rather than a silent no-op).
async fn set_color_effects(route: &DeviceRoute, r: u8, g: u8, b: u8) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let mut device = Device::new(std::sync::Arc::clone(&channel), index)
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
    })
    .await
}

/// Classify a HID++ error from the `ColorLedEffects` functions.
fn classify_lighting_error(error: hidpp::protocol::v20::Hidpp20Error) -> WriteError {
    classify_hidpp_error(error, HidppOperation::Lighting, ColorLedEffectsFeature::ID)
}

/// Set a solid colour via `RgbEffects` (`0x8071`): take software control, find
/// each cluster's advertised static-colour effect, and apply it in RAM.
async fn set_color_rgb_effects(route: &DeviceRoute, r: u8, g: u8, b: u8) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let mut device = Device::new(std::sync::Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let feature = open_feature::<RgbEffectsFeature>(&mut device).await?;
        feature
            .set_sw_control(
                SwControlFlags::ALL_CLUSTERS,
                EventsNotificationFlags::empty(),
            )
            .await
            .map_err(classify_rgb_lighting_error)?;

        let cluster_count = feature
            .get_device_info()
            .await
            .map_err(classify_rgb_lighting_error)?
            .cluster_count;
        let mut params = [0u8; CLUSTER_EFFECT_PARAM_COUNT];
        params[0] = r;
        params[1] = g;
        params[2] = b;

        let mut written = 0u8;
        for cluster in 0..cluster_count {
            let Some(effect_index) = static_rgb_effect_index(&feature, cluster).await? else {
                debug!(index, cluster, "0x8071 cluster has no static RGB effect");
                continue;
            };
            feature
                .set_rgb_cluster_effect(
                    cluster,
                    effect_index,
                    params,
                    RgbPersistence::VOLATILE,
                    PowerModeTarget::FullPower,
                )
                .await
                .map_err(classify_rgb_lighting_error)?;
            written = written.saturating_add(1);
            tokio::time::sleep(FRAME_GAP).await;
        }
        if written == 0 {
            return Err(WriteError::UnsupportedResponse {
                operation: HidppOperation::Lighting,
                feature_hex: RGB_EFFECTS_FEATURE,
            });
        }
        debug!(
            index,
            cluster_count, written, r, g, b, "set colour via 0x8071 RGB effects"
        );
        Ok(())
    })
    .await
}

async fn static_rgb_effect_index(
    feature: &RgbEffectsFeature,
    cluster: u8,
) -> Result<Option<u8>, WriteError> {
    let effects_number = feature
        .get_cluster_info(cluster)
        .await
        .map_err(classify_rgb_lighting_error)?
        .effects_number;
    for effect_index in 0..effects_number {
        let info = feature
            .get_effect_info(cluster, effect_index)
            .await
            .map_err(classify_rgb_lighting_error)?;
        if info.effect_id == RGB_EFFECT_STATIC {
            return Ok(Some(effect_index));
        }
    }
    Ok(None)
}

fn classify_rgb_lighting_error(error: hidpp::protocol::v20::Hidpp20Error) -> WriteError {
    classify_hidpp_error(error, HidppOperation::Lighting, RgbEffectsFeature::ID)
}

/// Set a solid colour via `PerKeyLighting` (`0x8080`): stream every key's colour
/// in 64-byte `0x12` frames, then commit. `FeatureUnsupported` when the device
/// exposes no `0x8080`.
async fn set_color_per_key(route: &DeviceRoute, r: u8, g: u8, b: u8) -> Result<(), WriteError> {
    let device_index = route.device_index();
    let feature_index = resolve_feature_index(route, PER_KEY_LIGHTING_FEATURE)
        .await?
        .ok_or(WriteError::FeatureUnsupported {
            feature_hex: PER_KEY_LIGHTING_FEATURE,
        })?;

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
    commit[0] = REPORT_LONG;
    commit[1] = device_index;
    commit[2] = feature_index;
    commit[3] = (FN_FRAME_END << 4) | SW_ID;
    writer
        .write_output_report(&commit)
        .await
        .map_err(WriteError::from)?;
    debug!(
        device_index,
        feature_index, r, g, b, "set keyboard colour via 0x8080"
    );
    Ok(())
}
