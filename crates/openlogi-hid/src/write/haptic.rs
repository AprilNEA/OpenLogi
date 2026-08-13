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
///
/// Stores are epoch-guarded: opening the feature awaits HID++ round-trips, and
/// the enumerator may retire the channel (and clear this cache) while that
/// open is in flight. An unguarded store would then re-pin the retired
/// channel's `Arc` after the retire-time clear ran — recreating the reopen
/// deadlock the clear exists to break. Every clear bumps the epoch, and a
/// store whose resolution began before the clear is discarded.
struct EpochGuarded<T> {
    epoch: u64,
    entry: Option<(usize, u8, T)>,
}

impl<T: Clone> EpochGuarded<T> {
    const fn new() -> Self {
        Self {
            epoch: 0,
            entry: None,
        }
    }

    fn get(&self, ptr: usize, index: u8) -> Option<T> {
        let (entry_ptr, entry_index, value) = self.entry.as_ref()?;
        (*entry_ptr == ptr && *entry_index == index).then(|| value.clone())
    }

    /// Store `value`, unless a clear ran since `epoch` was snapshotted.
    fn store(&mut self, epoch: u64, ptr: usize, index: u8, value: T) {
        if self.epoch == epoch {
            self.entry = Some((ptr, index, value));
        }
    }

    fn clear(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.entry = None;
    }

    /// Drop the entry if it belongs to `ptr`. Always bumps the epoch: the
    /// caller is retiring that channel, so a store racing this clear must be
    /// discarded even when nothing (or another channel's entry) is cached yet.
    fn clear_for(&mut self, ptr: usize) {
        self.epoch = self.epoch.wrapping_add(1);
        if self
            .entry
            .as_ref()
            .is_some_and(|(entry_ptr, _, _)| *entry_ptr == ptr)
        {
            self.entry = None;
        }
    }
}

static CACHED_FEATURE: std::sync::Mutex<EpochGuarded<Arc<HapticFeedbackFeature>>> =
    std::sync::Mutex::new(EpochGuarded::new());

/// Snapshot the cache epoch before starting a feature open; pass the result to
/// [`store_cached_feature`] so a clear that lands mid-open wins over the store.
fn cache_epoch() -> u64 {
    CACHED_FEATURE.lock().map_or(0, |guard| guard.epoch)
}

fn cached_feature(channel: &Arc<HidppChannel>, index: u8) -> Option<Arc<HapticFeedbackFeature>> {
    let guard = CACHED_FEATURE.lock().ok()?;
    guard.get(Arc::as_ptr(channel) as usize, index)
}

fn store_cached_feature(
    epoch: u64,
    channel: &Arc<HidppChannel>,
    index: u8,
    feature: &Arc<HapticFeedbackFeature>,
) {
    if let Ok(mut guard) = CACHED_FEATURE.lock() {
        guard.store(
            epoch,
            Arc::as_ptr(channel) as usize,
            index,
            Arc::clone(feature),
        );
    }
}

fn clear_cached_feature() {
    if let Ok(mut guard) = CACHED_FEATURE.lock() {
        guard.clear();
    }
}

/// Drop the cached haptic feature handle (and with it the `Arc<HidppChannel>`
/// it pins). MUST be called whenever route resolution fails: the inventory
/// enumerator only reopens a retired node once every clone of its channel has
/// dropped (`Arc::strong_count == 1`), and a stale cache entry otherwise
/// deadlocks recovery — the node can't reopen because the cache pins the old
/// channel, and the cache is never invalidated because route lookups fail
/// before any haptic I/O touches it.
pub fn clear_haptic_feature_cache() {
    clear_cached_feature();
}

/// Drop the cached haptic feature handle if it belongs to `channel`.
///
/// The enumerator calls this the moment it retires a channel. Clearing only on
/// route-miss (above) is not enough: a route miss requires a haptic attempt,
/// and once capture has died no haptic attempt can happen — the Actions Ring
/// trigger is itself a diverted control that died with capture. The cache
/// entry then pins the retired channel forever and the node never reopens.
pub(crate) fn clear_haptic_feature_cache_for(channel: &Arc<HidppChannel>) {
    if let Ok(mut guard) = CACHED_FEATURE.lock() {
        guard.clear_for(Arc::as_ptr(channel) as usize);
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
        let epoch = cache_epoch();
        let feature = feature_on_channel(channel, index).await?;
        store_cached_feature(epoch, channel, index, &feature);
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
    let epoch = cache_epoch();
    let feature = feature_on_channel(channel, index).await?;
    let result = feature.play(waveform).await.map_err(|error| {
        classify_hidpp_error(error, HidppOperation::PlayHaptic, HapticFeedbackFeature::ID)
    });
    if result.is_ok() {
        store_cached_feature(epoch, channel, index, &feature);
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

#[cfg(test)]
mod tests {
    use super::EpochGuarded;

    #[test]
    fn a_store_started_before_a_clear_is_discarded() {
        let mut cache = EpochGuarded::new();
        let epoch = cache.epoch;
        // The channel retires while the feature open is in flight…
        cache.clear_for(0xA);
        // …so the open's belated success must not re-pin the channel.
        cache.store(epoch, 0xA, 2, "stale");
        assert_eq!(cache.get(0xA, 2), None);
    }

    #[test]
    fn a_store_with_a_current_epoch_lands() {
        let mut cache = EpochGuarded::new();
        cache.store(cache.epoch, 0xA, 2, "fresh");
        assert_eq!(cache.get(0xA, 2), Some("fresh"));
        assert_eq!(cache.get(0xB, 2), None);
        assert_eq!(cache.get(0xA, 3), None);
    }

    #[test]
    fn retiring_one_channel_keeps_anothers_entry_but_blocks_stale_stores() {
        let mut cache = EpochGuarded::new();
        cache.store(cache.epoch, 0xA, 2, "kept");
        let epoch = cache.epoch;
        cache.clear_for(0xB);
        assert_eq!(cache.get(0xA, 2), Some("kept"));
        cache.store(epoch, 0xB, 1, "stale");
        assert_eq!(cache.get(0xB, 1), None);
    }

    #[test]
    fn a_full_clear_empties_the_entry_and_blocks_stale_stores() {
        let mut cache = EpochGuarded::new();
        let epoch = cache.epoch;
        cache.store(epoch, 0xA, 2, "cached");
        cache.clear();
        assert_eq!(cache.get(0xA, 2), None);
        cache.store(epoch, 0xA, 2, "stale");
        assert_eq!(cache.get(0xA, 2), None);
    }
}
