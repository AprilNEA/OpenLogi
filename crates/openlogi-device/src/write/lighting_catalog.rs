//! Firmware catalog apply, zone enumeration, and host-frame writers.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        color_led_effects::{
            ColorLedEffectsFeature, EffectId, Persistence, SwControl, ZONE_EFFECT_PARAM_COUNT,
        },
        per_key_lighting::{FramePersistence, PerKeyLightingFeature},
    },
};
use openlogi_core::config::Lighting;
use openlogi_core::hid::{
    LightingEffect, LightingInfo, LightingZone, LightingZoneLocation, firmware_intensity,
    speed_to_period_ms,
};
use tracing::debug;

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

use super::lighting::{
    LightingMethod, classify_lighting_error, classify_per_key_v2_error, present_zones,
    set_keyboard_color_with_on_channel,
};
use super::{WriteError, open_feature, with_route};

const COLOR_LED_EFFECTS_FEATURE: u16 = 0x8070;
const MAX_COLOR_LED_EFFECT_ZONES: u8 = 4;
const FRAME_GAP: Duration = Duration::from_millis(8);

static ZONE_EFFECT_SLOTS: LazyLock<Mutex<HashMap<String, HashMap<u8, HashMap<u16, u8>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn remember_zone_slot(route: &DeviceRoute, zone: u8, effect_id: u16, slot: u8) {
    let Ok(mut cache) = ZONE_EFFECT_SLOTS.lock() else {
        return;
    };
    cache
        .entry(route.to_string())
        .or_default()
        .entry(zone)
        .or_default()
        .insert(effect_id, slot);
}

fn cached_zone_slot(route: &DeviceRoute, zone: u8, effect_id: EffectId) -> Option<u8> {
    let cache = ZONE_EFFECT_SLOTS.lock().ok()?;
    cache
        .get(&route.to_string())?
        .get(&zone)?
        .get(&u16::from(effect_id))
        .copied()
}

/// Outcome of a firmware lighting apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightingApply {
    /// Firmware owns the LEDs (`SwControl::Firmware`).
    Firmware,
    /// The caller must run the host renderer (`SwControl::Software`).
    Host,
}

/// Probe `0x8070` zones and whether `0x8081` per-key lighting is present.
pub async fn read_lighting_info(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    mouse: bool,
    screen_sampler: bool,
    audio_visualizer: bool,
) -> Result<LightingInfo, WriteError> {
    let device_index = route.device_index();
    let fetch_route = route.clone();
    with_route(backend, route, move |channel| async move {
        read_lighting_info_on_channel(
            &channel,
            device_index,
            &fetch_route,
            mouse,
            screen_sampler,
            audio_visualizer,
        )
        .await
    })
    .await
}

/// [`read_lighting_info`] on an already-open channel.
pub async fn read_lighting_info_on(
    shared: &SharedChannel,
    route: &DeviceRoute,
    mouse: bool,
    screen_sampler: bool,
    audio_visualizer: bool,
) -> Result<LightingInfo, WriteError> {
    read_lighting_info_on_channel(
        shared.channel(),
        shared.device_index(),
        route,
        mouse,
        screen_sampler,
        audio_visualizer,
    )
    .await
}

async fn read_lighting_info_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    route: &DeviceRoute,
    mouse: bool,
    screen_sampler: bool,
    audio_visualizer: bool,
) -> Result<LightingInfo, WriteError> {
    let zones = match enumerate_color_led_zones(channel, index, route).await {
        Ok(zones) => zones,
        Err(WriteError::FeatureUnsupported { .. }) => Vec::new(),
        Err(error) => return Err(error),
    };
    let per_key = match per_key_zone_ids(channel, index).await {
        Ok(ids) => !ids.is_empty(),
        Err(WriteError::FeatureUnsupported { .. }) => false,
        Err(error) => return Err(error),
    };
    if zones.is_empty() && !per_key {
        return Err(WriteError::FeatureUnsupported {
            feature_hex: COLOR_LED_EFFECTS_FEATURE,
        });
    }
    Ok(LightingInfo {
        mouse,
        per_key,
        zones,
        screen_sampler,
        audio_visualizer,
    })
}

async fn enumerate_color_led_zones(
    channel: &Arc<HidppChannel>,
    index: u8,
    route: &DeviceRoute,
) -> Result<Vec<LightingZone>, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<ColorLedEffectsFeature>(&mut device).await?;
    let zone_count = feature
        .get_info()
        .await
        .map_err(classify_lighting_error)?
        .zone_count;
    let mut zones = Vec::new();
    for zone_index in 0..zone_count {
        let info = feature
            .get_zone_info(zone_index)
            .await
            .map_err(classify_lighting_error)?;
        let mut firmware_effects = Vec::new();
        for effect_index in 0..info.effects_number {
            let effect = feature
                .get_zone_effect_info(zone_index, effect_index)
                .await
                .map_err(classify_lighting_error)?;
            let id = u16::from(effect.effect_id);
            remember_zone_slot(route, zone_index, id, effect_index);
            if id != 0 && !firmware_effects.contains(&id) {
                firmware_effects.push(id);
            }
        }
        zones.push(LightingZone {
            index: zone_index,
            location: LightingZoneLocation::from_hidpp(u16::from(info.location)),
            firmware_effects,
        });
    }
    Ok(zones)
}

/// Apply a firmware lighting config (or solid-colour fallback). Host effects
/// return [`LightingApply::Host`] so the agent can start its renderer loop.
pub async fn apply_lighting(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    lighting: &Lighting,
) -> Result<LightingApply, WriteError> {
    let device_index = route.device_index();
    let lighting = lighting.clone();
    let fetch_route = route.clone();
    with_route(backend, route, move |channel| async move {
        apply_lighting_on_channel(&channel, device_index, &fetch_route, &lighting).await
    })
    .await
}

/// [`apply_lighting`] on an already-open channel.
pub async fn apply_lighting_on(
    shared: &SharedChannel,
    route: &DeviceRoute,
    lighting: &Lighting,
) -> Result<LightingApply, WriteError> {
    apply_lighting_on_channel(shared.channel(), shared.device_index(), route, lighting).await
}

async fn apply_lighting_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    route: &DeviceRoute,
    lighting: &Lighting,
) -> Result<LightingApply, WriteError> {
    if lighting.enabled && lighting.effect == LightingEffect::EchoPress {
        let mut firmware = lighting.clone();
        firmware.effect = LightingEffect::LightOnPress;
        match apply_firmware_effect(channel, index, route, &firmware).await {
            Ok(0) | Err(WriteError::FeatureUnsupported { .. }) => {
                return Ok(LightingApply::Host);
            }
            Ok(_) => return Ok(LightingApply::Firmware),
            Err(error) => return Err(error),
        }
    }
    if lighting.enabled && lighting.effect.is_host() {
        return Ok(LightingApply::Host);
    }
    match apply_firmware_effect(channel, index, route, lighting).await {
        Err(WriteError::FeatureUnsupported { feature_hex })
            if feature_hex == COLOR_LED_EFFECTS_FEATURE
                && (!lighting.enabled || lighting.effect == LightingEffect::Solid) =>
        {
            let (r, g, b) = scaled_solid_rgb(lighting);
            set_keyboard_color_with_on_channel(channel, index, LightingMethod::Auto, r, g, b)
                .await?;
            Ok(LightingApply::Firmware)
        }
        Ok(_) => Ok(LightingApply::Firmware),
        Err(error) => Err(error),
    }
}

async fn apply_firmware_effect(
    channel: &Arc<HidppChannel>,
    index: u8,
    route: &DeviceRoute,
    lighting: &Lighting,
) -> Result<usize, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<ColorLedEffectsFeature>(&mut device).await?;
    feature
        .set_sw_control(SwControl::Firmware, false)
        .await
        .map_err(classify_lighting_error)?;
    let zone_count = feature
        .get_info()
        .await
        .map_err(classify_lighting_error)?
        .zone_count;
    let zones_to_write = selected_zone_indexes(lighting, zone_count);
    let (effect_id, params) = firmware_effect_and_params(lighting);
    let mut written = 0usize;
    for zone in zones_to_write {
        let Some(effect_index) = lookup_zone_effect_index(&feature, route, zone, effect_id).await?
        else {
            debug!(index, zone, ?effect_id, "zone does not advertise effect");
            continue;
        };
        feature
            .set_zone_effect(zone, effect_index, params, Persistence::Volatile)
            .await
            .map_err(classify_lighting_error)?;
        written += 1;
        tokio::time::sleep(FRAME_GAP).await;
    }
    Ok(written)
}

fn selected_zone_indexes(lighting: &Lighting, zone_count: u8) -> Vec<u8> {
    if lighting.zones.is_empty() {
        return (0..zone_count.min(MAX_COLOR_LED_EFFECT_ZONES)).collect();
    }
    lighting
        .zones
        .iter()
        .copied()
        .filter(|zone| *zone < zone_count)
        .collect()
}

fn firmware_effect_and_params(lighting: &Lighting) -> (EffectId, [u8; ZONE_EFFECT_PARAM_COUNT]) {
    if !lighting.enabled {
        return (EffectId::Disabled, [0; ZONE_EFFECT_PARAM_COUNT]);
    }
    let id = lighting
        .effect
        .firmware_id()
        .and_then(|id| EffectId::try_from(id).ok())
        .unwrap_or(EffectId::FixedColor);
    let (r, g, b) = solid_rgb(lighting);
    let (r, g, b) = match id {
        EffectId::Starlight | EffectId::Ripple => dim_rgb((r, g, b), lighting.brightness),
        _ => (r, g, b),
    };
    (
        id,
        pack_effect_params(id, r, g, b, lighting.speed, lighting.brightness),
    )
}

fn solid_rgb(lighting: &Lighting) -> (u8, u8, u8) {
    if !lighting.enabled {
        return (0, 0, 0);
    }
    lighting.color.components()
}

fn scaled_solid_rgb(lighting: &Lighting) -> (u8, u8, u8) {
    dim_rgb(solid_rgb(lighting), lighting.brightness)
}

fn dim_rgb(color: (u8, u8, u8), brightness: u8) -> (u8, u8, u8) {
    let scale =
        |c: u8| u8::try_from(u16::from(c) * u16::from(brightness.min(100)) / 100).unwrap_or(c);
    (scale(color.0), scale(color.1), scale(color.2))
}

/// Pack the ten `setZoneEffect` parameter bytes for a firmware effect.
#[must_use]
pub fn pack_effect_params(
    effect: EffectId,
    r: u8,
    g: u8,
    b: u8,
    speed: u8,
    brightness: u8,
) -> [u8; ZONE_EFFECT_PARAM_COUNT] {
    let mut params = [0u8; ZONE_EFFECT_PARAM_COUNT];
    let period = speed_to_period_ms(speed).to_be_bytes();
    let intensity = firmware_intensity(brightness);
    match effect {
        EffectId::Disabled => {}
        EffectId::Cycling | EffectId::ColorWave => {
            params[4] = period[0];
            params[5] = period[1];
            params[6] = intensity;
        }
        EffectId::Starlight => {
            params[0] = r;
            params[1] = g;
            params[2] = b;
            params[3] = r;
            params[4] = g;
            params[5] = b;
        }
        EffectId::PulsingBreathingWaveform | EffectId::PulsingBreathingLegacy => {
            params[0] = r;
            params[1] = g;
            params[2] = b;
            params[3] = period[0];
            params[4] = period[1];
            params[6] = intensity;
        }
        EffectId::Ripple => {
            params[0] = r;
            params[1] = g;
            params[2] = b;
            params[4] = period[0];
            params[5] = period[1];
        }
        EffectId::FixedColor
        | EffectId::LightOnPress
        | EffectId::AudioVisualizer
        | EffectId::BootUp
        | EffectId::DemoMode
        | _ => {
            params[0] = r;
            params[1] = g;
            params[2] = b;
            params[6] = intensity;
        }
    }
    params
}

async fn lookup_zone_effect_index(
    feature: &ColorLedEffectsFeature,
    route: &DeviceRoute,
    zone: u8,
    effect_id: EffectId,
) -> Result<Option<u8>, WriteError> {
    if let Some(slot) = cached_zone_slot(route, zone, effect_id) {
        return Ok(Some(slot));
    }
    let slot = lookup_named_effect_index(feature, zone, effect_id).await?;
    if let Some(slot) = slot {
        remember_zone_slot(route, zone, u16::from(effect_id), slot);
    }
    if slot.is_none() && effect_id == EffectId::Disabled {
        return lookup_named_effect_index(feature, zone, EffectId::FixedColor).await;
    }
    Ok(slot)
}

async fn lookup_named_effect_index(
    feature: &ColorLedEffectsFeature,
    zone: u8,
    effect_id: EffectId,
) -> Result<Option<u8>, WriteError> {
    let info = feature
        .get_zone_info(zone)
        .await
        .map_err(classify_lighting_error)?;
    for effect_index in 0..info.effects_number {
        let effect = feature
            .get_zone_effect_info(zone, effect_index)
            .await
            .map_err(classify_lighting_error)?;
        if effect.effect_id == effect_id {
            return Ok(Some(effect_index));
        }
    }
    Ok(None)
}

/// Hand LED ownership to firmware or software.
pub async fn set_led_software_control_on(
    shared: &SharedChannel,
    software: bool,
) -> Result<(), WriteError> {
    let index = shared.device_index();
    let mut device = Device::new(Arc::clone(shared.channel()), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<ColorLedEffectsFeature>(&mut device).await?;
    let control = if software {
        SwControl::Software
    } else {
        SwControl::Firmware
    };
    feature
        .set_sw_control(control, false)
        .await
        .map_err(classify_lighting_error)
}

/// Paint selected `0x8070` zones a fixed colour (host renderer frames).
pub async fn set_zonal_colors_on(
    shared: &SharedChannel,
    colors: &[(u8, u8, u8, u8)],
) -> Result<(), WriteError> {
    let index = shared.device_index();
    let mut device = Device::new(Arc::clone(shared.channel()), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<ColorLedEffectsFeature>(&mut device).await?;
    let route = shared.route();
    for &(zone, r, g, b) in colors {
        let Some(effect_index) =
            lookup_zone_effect_index(&feature, route, zone, EffectId::FixedColor).await?
        else {
            continue;
        };
        let mut params = [0u8; ZONE_EFFECT_PARAM_COUNT];
        params[0] = r;
        params[1] = g;
        params[2] = b;
        feature
            .set_zone_effect(zone, effect_index, params, Persistence::Volatile)
            .await
            .map_err(classify_lighting_error)?;
        tokio::time::sleep(FRAME_GAP).await;
    }
    Ok(())
}

/// Paint `0x8081` zones individually and commit a volatile frame.
pub async fn set_per_key_colors_on(
    shared: &SharedChannel,
    colors: &[(u8, u8, u8, u8)],
) -> Result<(), WriteError> {
    let index = shared.device_index();
    let mut device = Device::new(Arc::clone(shared.channel()), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<PerKeyLightingFeature>(&mut device).await?;
    for chunk in colors.chunks(4) {
        let zones: Vec<_> = chunk
            .iter()
            .map(
                |&(zone_id, r, g, b)| hidpp::feature::per_key_lighting::RgbZone {
                    zone_id,
                    color: hidpp::feature::per_key_lighting::Rgb {
                        red: r,
                        green: g,
                        blue: b,
                    },
                },
            )
            .collect();
        feature
            .set_individual_rgb_zones(&zones)
            .await
            .map_err(classify_per_key_v2_error)?;
    }
    feature
        .frame_end(FramePersistence::Volatile, 0, 0)
        .await
        .map_err(classify_per_key_v2_error)
}

/// Present `0x8081` zone ids, when the feature exists.
pub async fn per_key_zone_ids_on(shared: &SharedChannel) -> Result<Vec<u8>, WriteError> {
    per_key_zone_ids(shared.channel(), shared.device_index()).await
}

async fn per_key_zone_ids(channel: &Arc<HidppChannel>, index: u8) -> Result<Vec<u8>, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<PerKeyLightingFeature>(&mut device).await?;
    present_zones(&feature).await
}

#[cfg(test)]
mod tests {
    use super::{EffectId, pack_effect_params};

    #[test]
    fn packs_fixed_color_rgb_and_intensity() {
        let params = pack_effect_params(EffectId::FixedColor, 0x11, 0x22, 0x33, 50, 100);
        assert_eq!(&params[0..3], &[0x11, 0x22, 0x33]);
        assert_eq!(params[6], 0);
        let dim = pack_effect_params(EffectId::FixedColor, 0xff, 0xff, 0xff, 50, 40);
        assert_eq!(dim[6], 40);
    }

    #[test]
    fn packs_cycle_period_and_intensity() {
        let params = pack_effect_params(EffectId::Cycling, 0, 0, 0, 0, 40);
        assert_eq!(&params[4..7], &[0x0b, 0xb8, 40]);
    }

    #[test]
    fn packs_breathing_color_period_and_intensity() {
        let params = pack_effect_params(EffectId::PulsingBreathingWaveform, 1, 2, 3, 100, 100);
        assert_eq!(&params[0..5], &[1, 2, 3, 0x00, 0x96]);
        assert_eq!(params[6], 0);
    }
}
