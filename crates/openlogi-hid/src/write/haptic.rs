use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature as _,
        haptic_feedback::{HapticFeedbackFeature, HapticIntensity, HapticWaveform},
    },
};

use crate::route::DeviceRoute;

use super::{
    HidppOperation, SharedChannel, WriteError, classify_hidpp_error, open_feature, with_route,
};

async fn feature_on_channel(
    channel: &Arc<HidppChannel>,
    device_index: u8,
) -> Result<Arc<HapticFeedbackFeature>, WriteError> {
    let mut device = Device::new(Arc::clone(channel), device_index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable {
            index: device_index,
        })?;
    open_feature::<HapticFeedbackFeature>(&mut device).await
}

/// Last successfully-opened haptic feature, keyed by channel identity and
/// device index. Haptic plays are fired per ring hover, and the open sequence
/// (device ping + feature lookup) costs two extra HID++ round-trips per play —
/// on a busy receiver each round-trip is a fresh chance to lose the reply
/// under concurrent pointer traffic. One entry suffices: haptics come from
/// one pointing device at a time.
static CACHED_FEATURE: std::sync::Mutex<Option<(usize, u8, Arc<HapticFeedbackFeature>)>> =
    std::sync::Mutex::new(None);

fn cached_feature(channel: &Arc<HidppChannel>, index: u8) -> Option<Arc<HapticFeedbackFeature>> {
    let guard = CACHED_FEATURE.lock().ok()?;
    let (ptr, idx, feature) = guard.as_ref()?;
    (*ptr == Arc::as_ptr(channel) as usize && *idx == index).then(|| Arc::clone(feature))
}

fn store_cached_feature(
    channel: &Arc<HidppChannel>,
    index: u8,
    feature: &Arc<HapticFeedbackFeature>,
) {
    if let Ok(mut guard) = CACHED_FEATURE.lock() {
        *guard = Some((Arc::as_ptr(channel) as usize, index, Arc::clone(feature)));
    }
}

fn clear_cached_feature() {
    if let Ok(mut guard) = CACHED_FEATURE.lock() {
        *guard = None;
    }
}

/// Ensure the firmware haptic engine is armed: enabled, with a non-zero
/// intensity. Returns `true` when a repair write was needed.
///
/// Nothing else in the stack ever asserts this state — devices historically
/// inherited it from Logi Options+, and some power transitions clear it, after
/// which `play` calls are accepted but produce no physical feedback. Callers
/// arm once per Actions Ring session, before the first hover.
pub async fn ensure_haptics_armed_on(shared: &SharedChannel) -> Result<bool, WriteError> {
    let channel = shared.channel();
    let index = shared.device_index();
    let feature = if let Some(feature) = cached_feature(channel, index) {
        feature
    } else {
        let feature = feature_on_channel(channel, index).await?;
        store_cached_feature(channel, index, &feature);
        feature
    };
    let config = feature.get_configuration().await.map_err(|error| {
        clear_cached_feature();
        classify_hidpp_error(error, HidppOperation::PlayHaptic, HapticFeedbackFeature::ID)
    })?;
    let intensity = if config.intensity.get() == 0 {
        HapticIntensity::new(25).unwrap_or(config.intensity)
    } else {
        config.intensity
    };
    if config.enabled && intensity == config.intensity {
        return Ok(false);
    }
    feature
        .set_configuration(true, intensity)
        .await
        .map_err(|error| {
            clear_cached_feature();
            classify_hidpp_error(error, HidppOperation::PlayHaptic, HapticFeedbackFeature::ID)
        })?;
    Ok(true)
}

/// Play a waveform immediately on an open capture channel.
///
/// Reuses the cached feature handle when it belongs to this channel (one
/// round-trip); any error invalidates the cache and the play is retried once
/// through a fresh open, so a rebuilt channel or stale index self-heals.
pub async fn play_haptic_on(
    shared: &SharedChannel,
    waveform: HapticWaveform,
) -> Result<(), WriteError> {
    let channel = shared.channel();
    let index = shared.device_index();
    if let Some(feature) = cached_feature(channel, index) {
        if feature.play(waveform).await.is_ok() {
            return Ok(());
        }
        clear_cached_feature();
    }
    let feature = feature_on_channel(channel, index).await?;
    let result = feature.play(waveform).await.map_err(|error| {
        classify_hidpp_error(error, HidppOperation::PlayHaptic, HapticFeedbackFeature::ID)
    });
    if result.is_ok() {
        store_cached_feature(channel, index, &feature);
    }
    result
}

/// Play a waveform immediately by route.
pub async fn play_haptic(route: &DeviceRoute, waveform: HapticWaveform) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let feature = feature_on_channel(&channel, index).await?;
        feature.play(waveform).await.map_err(|error| {
            classify_hidpp_error(error, HidppOperation::PlayHaptic, HapticFeedbackFeature::ID)
        })
    })
    .await
}
