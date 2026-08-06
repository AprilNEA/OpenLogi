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
/// HID++ `RgbEffects` (`0x8071`) — the per-cluster effect engine that succeeds
/// `0x8070`. G-series *mice* expose this one instead: a G903 LIGHTSPEED reports
/// `0x8071` and neither `0x8070` nor `0x8080`, so without this path its LEDs are
/// unreachable and it earns no Lighting tab at all.
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
// The old raw `0x8070` path intentionally wrote only zones 0..4: enough for the
// keyboards this path targets and bounded by a small, predictable delay budget.
// Keep that cap even though the typed wrapper can query the reported zone count;
// a malformed or unexpectedly large count should not stall a color apply.
const MAX_COLOR_LED_EFFECT_ZONES: u8 = 4;
// Zones are paced apart because the controller can drop closely-spaced reports.
const FRAME_GAP: Duration = Duration::from_millis(8);

// 0x8071 `RgbEffects`: `effectID` 0x0001 is the fixed/solid-colour effect. Only
// the *id* is stable — the effect *index* passed to `setRgbClusterEffect` is
// per-cluster and firmware-ordered, so it's located by id at runtime rather than
// assumed (a device's clusters need not agree on ordering).
const RGB_EFFECT_ID_FIXED: u16 = 0x0001;
// Same reasoning as `MAX_COLOR_LED_EFFECT_ZONES`: bound the per-apply work so a
// malformed cluster count can't stall a colour pick. A G903 reports 2 clusters
// (logo and DPI indicator); 8 leaves room for denser devices.
const MAX_RGB_CLUSTERS: u8 = 8;

/// Which HID++ lighting path drives a solid keyboard colour. [`Auto`] is what
/// the GUI/agent use; the explicit variants exist for the `diag` A/B test.
///
/// [`Auto`]: LightingMethod::Auto
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightingMethod {
    /// Walk `RgbEffects` (`0x8071`) → `ColorLedEffects` (`0x8070`) →
    /// `PerKeyLighting` (`0x8080`), taking the first the device exposes.
    Auto,
    /// Force `RgbEffects` (`0x8071`) — the per-cluster fixed-effect override.
    RgbEffects,
    /// Force `ColorLedEffects` (`0x8070`) — the per-zone fixed-effect override.
    Effects,
    /// Force `PerKeyLighting` (`0x8080`) — the per-key stream.
    PerKey,
}

/// Set a device to a solid `(r, g, b)` colour, choosing the HID++ path
/// automatically: the `0x8071` cluster engine (G-series mice), else the `0x8070`
/// zone engine, else the `0x8080` per-key stream. Both effect engines override a
/// running onboard profile. `FeatureUnsupported` when the device exposes none.
///
/// Named for keyboards because they were the only lit devices when it landed; it
/// drives any device with one of those three features.
pub async fn set_keyboard_color(
    route: &DeviceRoute,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    set_keyboard_color_with(route, LightingMethod::Auto, r, g, b).await
}

/// [`set_keyboard_color`] with an explicit [`LightingMethod`]. `Auto` steps down
/// the ladder only on a `FeatureUnsupported` naming the rung's own feature — any
/// other error propagates, so a present-but-failing engine surfaces as a failure
/// instead of silently repainting the device key-by-key.
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
        LightingMethod::RgbEffects => set_color_rgb_effects(route, r, g, b).await,
        LightingMethod::Auto => match set_color_rgb_effects(route, r, g, b).await {
            Err(WriteError::FeatureUnsupported { feature_hex })
                if feature_hex == RGB_EFFECTS_FEATURE =>
            {
                debug!("no 0x8071 cluster engine — trying the 0x8070 zone engine");
                set_color_effects_or_per_key(route, r, g, b).await
            }
            other => other,
        },
    }
}

/// The `Auto` ladder below `0x8071`: the `0x8070` zone engine, falling back to
/// the `0x8080` per-key stream when the device exposes no effect engine.
async fn set_color_effects_or_per_key(
    route: &DeviceRoute,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    match set_color_effects(route, r, g, b).await {
        Err(WriteError::FeatureUnsupported { feature_hex })
            if feature_hex == COLOR_LED_EFFECTS_FEATURE =>
        {
            debug!("no 0x8070 effect engine — falling back to 0x8080 per-key");
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

/// Classify a HID++ error from the `RgbEffects` functions.
fn classify_rgb_error(error: hidpp::protocol::v20::Hidpp20Error) -> WriteError {
    classify_hidpp_error(error, HidppOperation::Lighting, RgbEffectsFeature::ID)
}

/// Set a solid colour via `RgbEffects` (`0x8071`): the fixed effect on every
/// cluster, in RAM only. `FeatureUnsupported` when the device exposes no
/// `0x8071`.
///
/// Every cluster is driven, not just the first: a G903 splits its LEDs into a
/// logo cluster and a DPI-indicator cluster, and lighting only one leaves the
/// device visibly half-painted.
///
/// Volatile like the `0x8070` path — the effect shows live and overrides the
/// running onboard effect without touching EEPROM, and the agent re-applies the
/// saved colour on device arrival rather than spending flash cycles per pick.
async fn set_color_rgb_effects(route: &DeviceRoute, r: u8, g: u8, b: u8) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let mut device = Device::new(std::sync::Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let feature = open_feature::<RgbEffectsFeature>(&mut device).await?;

        // `setRgbClusterEffect` is refused until software takes the clusters off
        // the device's own effect engine, so claim control before writing. Not
        // requesting POWER_MODES: this path never drives power modes, and asking
        // for control we don't use would keep the firmware from managing its own
        // RGB power saving.
        feature
            .set_sw_control(
                SwControlFlags::ALL_CLUSTERS,
                EventsNotificationFlags::empty(),
            )
            .await
            .map_err(classify_rgb_error)?;

        let cluster_count = feature
            .get_device_info()
            .await
            .map_err(classify_rgb_error)?
            .cluster_count;
        if cluster_count == 0 {
            // Unlike 0x8070 there's no legacy zone count worth guessing here, and
            // a blind write would target a cluster the device never claimed.
            debug!(index, "0x8071 reported zero clusters — nothing to drive");
            return Err(WriteError::UnsupportedResponse {
                operation: HidppOperation::Lighting,
                feature_hex: RGB_EFFECTS_FEATURE,
            });
        }
        let clusters_to_write = cluster_count.min(MAX_RGB_CLUSTERS);
        if cluster_count > MAX_RGB_CLUSTERS {
            debug!(
                index,
                cluster_count,
                capped_cluster_count = MAX_RGB_CLUSTERS,
                "0x8071 cluster count capped to the per-apply write limit"
            );
        }

        let mut params = [0u8; CLUSTER_EFFECT_PARAM_COUNT];
        params[0] = r;
        params[1] = g;
        params[2] = b;
        for cluster in 0..clusters_to_write {
            let effect = fixed_effect_index(&feature, cluster).await?;
            feature
                .set_rgb_cluster_effect(
                    cluster,
                    effect,
                    params,
                    RgbPersistence::VOLATILE,
                    PowerModeTarget::FullPower,
                )
                .await
                .map_err(classify_rgb_error)?;
            tokio::time::sleep(FRAME_GAP).await;
        }
        debug!(
            index,
            cluster_count, clusters_to_write, r, g, b, "set device colour via 0x8071"
        );
        Ok(())
    })
    .await
}

/// Locate `cluster`'s fixed (solid-colour) effect index by scanning its effects
/// for [`RGB_EFFECT_ID_FIXED`].
///
/// `UnsupportedResponse` when the cluster offers no fixed effect — it answered,
/// but with nothing this path can drive (an animation-only cluster).
async fn fixed_effect_index(feature: &RgbEffectsFeature, cluster: u8) -> Result<u8, WriteError> {
    let effect_count = feature
        .get_cluster_info(cluster)
        .await
        .map_err(classify_rgb_error)?
        .effects_number;
    for effect in 0..effect_count {
        let info = feature
            .get_effect_info(cluster, effect)
            .await
            .map_err(classify_rgb_error)?;
        if info.effect_id == RGB_EFFECT_ID_FIXED {
            return Ok(effect);
        }
    }
    debug!(
        cluster,
        effect_count, "0x8071 cluster exposes no fixed effect"
    );
    Err(WriteError::UnsupportedResponse {
        operation: HidppOperation::Lighting,
        feature_hex: RGB_EFFECTS_FEATURE,
    })
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
