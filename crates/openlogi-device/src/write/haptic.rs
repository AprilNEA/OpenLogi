use std::sync::{Arc, Mutex, Weak};

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature,
        haptic_feedback::{HapticFeedbackFeature, HapticIntensity, HapticWaveform},
    },
};

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

use super::{HidppOperation, WriteError, classify_hidpp_error, with_route};

/// Resolve the haptic feature on `channel`, returning the accessor and the
/// feature index it was found at (the part worth caching — see
/// [`FeatureLocation`]).
async fn feature_on_channel(
    channel: &Arc<HidppChannel>,
    device_index: u8,
) -> Result<(Arc<HapticFeedbackFeature>, u8), WriteError> {
    let mut device = Device::new(Arc::clone(channel), device_index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable {
            index: device_index,
        })?;
    let info = device
        .root()
        .get_feature(HapticFeedbackFeature::ID)
        .await
        .map_err(|e| {
            classify_hidpp_error(e, HidppOperation::ResolveFeature, HapticFeedbackFeature::ID)
        })?
        .ok_or(WriteError::FeatureUnsupported {
            feature_hex: HapticFeedbackFeature::ID,
        })?;
    Ok((
        device.add_feature::<HapticFeedbackFeature>(info.index),
        info.index,
    ))
}

/// Where the haptic feature lives: enough to rebuild the accessor without
/// I/O. Haptic plays are fired per ring hover, and resolving the feature
/// (device ping + root lookup) costs two extra HID++ round-trips per play —
/// on a busy receiver each round-trip is a fresh chance to lose the reply
/// under concurrent pointer traffic. One entry suffices: haptics come from
/// one pointing device at a time.
///
/// The channel is held **weakly** on purpose. A strong `Arc` here once pinned
/// a retired channel past the enumerator's reopen gate
/// (`Arc::strong_count == 1`), wedging the node forever — the Actions Ring
/// trigger died with capture, so no later haptic call arrived to invalidate
/// the entry, and every retire path had to remember a manual cache clear. A
/// `Weak` cannot pin, so that deadlock class is unrepresentable rather than
/// re-checked: a stale entry simply fails its identity check on the next play
/// (the `Weak` keeps the allocation alive, so the address cannot be recycled
/// into a false match) and the feature is re-resolved.
struct FeatureLocation {
    channel: Weak<HidppChannel>,
    device_index: u8,
    feature_index: u8,
}

static CACHED_LOCATION: Mutex<Option<FeatureLocation>> = Mutex::new(None);

/// Rebuild the cached accessor for exactly `channel`, without I/O. `None`
/// when nothing is cached, the entry belongs to another (or a dead) channel,
/// or the device index differs.
fn cached_feature(channel: &Arc<HidppChannel>, index: u8) -> Option<Arc<HapticFeedbackFeature>> {
    let guard = CACHED_LOCATION.lock().ok()?;
    let location = guard.as_ref()?;
    (location.device_index == index
        && location
            .channel
            .upgrade()
            .is_some_and(|live| Arc::ptr_eq(&live, channel)))
    .then(|| {
        Arc::new(<HapticFeedbackFeature as CreatableFeature>::new(
            Arc::clone(channel),
            index,
            location.feature_index,
        ))
    })
}

/// Remember where a successful open found the feature. Unconditional: an
/// entry for a channel the enumerator retires later holds nothing alive and
/// can never match another channel, so no store/retire ordering can produce
/// wreckage.
fn store_cached_location(channel: &Arc<HidppChannel>, device_index: u8, feature_index: u8) {
    if let Ok(mut guard) = CACHED_LOCATION.lock() {
        *guard = Some(FeatureLocation {
            channel: Arc::downgrade(channel),
            device_index,
            feature_index,
        });
    }
}

/// Forget the cached location — called when I/O through it fails, so the next
/// play re-resolves instead of replaying a location the device disowned.
fn clear_cached_location() {
    if let Ok(mut guard) = CACHED_LOCATION.lock() {
        *guard = None;
    }
}

/// Ensure the firmware haptic engine is armed: enabled, with a non-zero
/// intensity. Returns `true` when a repair write was needed.
///
/// Nothing else in the stack ever asserts this state — devices historically
/// inherited it from Logi Options+, and some power transitions clear it, after
/// which `play` calls are accepted but produce no physical feedback. Callers
/// arm once per Actions Ring session, before the first hover — which is why
/// this deliberately bypasses the cached location and re-resolves the
/// feature: the cache key (channel identity + device index) cannot see a
/// *different device* re-paired into the same receiver slot, so each session
/// starts by overwriting the entry with a freshly-proven location, and the
/// per-hover plays then reuse it for the session's lifetime.
pub async fn ensure_haptics_armed_on(shared: &SharedChannel) -> Result<bool, WriteError> {
    let channel = shared.channel();
    let index = shared.device_index();
    let (feature, feature_index) = feature_on_channel(channel, index).await?;
    store_cached_location(channel, index, feature_index);
    let config = feature.get_configuration().await.map_err(|error| {
        clear_cached_location();
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
            clear_cached_location();
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
        clear_cached_location();
    }
    let (feature, feature_index) = feature_on_channel(channel, index).await?;
    let result = feature.play(waveform).await.map_err(|error| {
        classify_hidpp_error(error, HidppOperation::PlayHaptic, HapticFeedbackFeature::ID)
    });
    if result.is_ok() {
        store_cached_location(channel, index, feature_index);
    }
    result
}

/// Play a waveform immediately by route.
pub async fn play_haptic(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    waveform: HapticWaveform,
) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let (feature, _) = feature_on_channel(&channel, index).await?;
        feature.play(waveform).await.map_err(|error| {
            classify_hidpp_error(error, HidppOperation::PlayHaptic, HapticFeedbackFeature::ID)
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::scripted::ScriptedRawHidChannel;

    async fn scripted_channel() -> Arc<HidppChannel> {
        let (raw, _reports) = ScriptedRawHidChannel::with_responder(|_| None);
        Arc::new(
            HidppChannel::from_raw_channel(raw)
                .await
                .expect("the scripted channel must support HID++"),
        )
    }

    /// These tests share the one process-wide cache slot, so they must not run
    /// concurrently with each other; a single test keeps them serialized.
    #[tokio::test]
    async fn the_cache_matches_exactly_the_channel_it_was_stored_for() {
        let channel = scripted_channel().await;
        store_cached_location(&channel, 2, 7);
        assert!(
            cached_feature(&channel, 2).is_some(),
            "the stored channel at the stored index is a hit"
        );
        assert!(
            cached_feature(&channel, 3).is_none(),
            "another device index is a miss"
        );

        let other = scripted_channel().await;
        assert!(
            cached_feature(&other, 2).is_none(),
            "another channel is a miss — identity, not address, decides"
        );

        // The enumerator retires the stored channel: nothing pins it (the
        // whole point of the Weak), and the entry can never match again —
        // including a fresh channel that might reuse the allocation, which
        // the surviving Weak makes impossible.
        drop(channel);
        let replacement = scripted_channel().await;
        assert!(
            cached_feature(&replacement, 2).is_none(),
            "a dead entry must not match any later channel"
        );

        clear_cached_location();
        assert!(cached_feature(&replacement, 2).is_none());
    }
}
