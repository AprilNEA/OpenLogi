use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature as _,
        haptic_feedback::{HapticFeedbackFeature, HapticIntensity, HapticWaveform},
    },
};

use crate::channel_registry::ChannelRegistry;
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
/// Stores are guarded twice, because a retire can land on either side of an
/// open and both leave the same wreckage: an entry pinning a channel's `Arc`
/// after the retire-time clear ran, which recreates the exact reopen deadlock
/// that clear exists to break.
///
/// - A retire *during* the open is caught by the epoch: every clear bumps it,
///   and a store whose snapshot predates the clear is discarded.
/// - A retire *before* the open is invisible to the epoch — the snapshot is
///   already post-clear — so the store additionally asks the registry whether
///   it still publishes the channel.
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

    /// Store `value`, unless a clear ran since `epoch` was snapshotted or
    /// `still_current` reports the channel is no longer published.
    ///
    /// The epoch alone only sees a clear that lands *during* the open. A
    /// channel retired *before* it began leaves nothing to violate: the
    /// snapshot is already post-clear, so the store would land and re-pin a
    /// dead channel. `still_current` is what closes that half, and it is
    /// evaluated here — under the caller's lock — on purpose: checked earlier,
    /// it could be overtaken by a retire that both unpublishes the channel and
    /// clears this cache, leaving the entry pinning it forever.
    fn store(
        &mut self,
        epoch: u64,
        ptr: usize,
        index: u8,
        value: T,
        still_current: impl FnOnce() -> bool,
    ) {
        if self.epoch == epoch && still_current() {
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

/// Cache the freshly-opened feature, unless the enumerator retired its channel
/// while the open was under way — in either direction, see [`EpochGuarded`].
fn store_cached_feature(
    epoch: u64,
    registry: &ChannelRegistry,
    shared: &SharedChannel,
    feature: &Arc<HapticFeedbackFeature>,
) {
    if let Ok(mut guard) = CACHED_FEATURE.lock() {
        guard.store(
            epoch,
            Arc::as_ptr(shared.channel()) as usize,
            shared.device_index(),
            Arc::clone(feature),
            || registry.is_current(shared),
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
pub async fn ensure_haptics_armed_on(
    registry: &ChannelRegistry,
    shared: &SharedChannel,
) -> Result<bool, WriteError> {
    let channel = shared.channel();
    let index = shared.device_index();
    let feature = if let Some(feature) = cached_feature(channel, index) {
        feature
    } else {
        let epoch = cache_epoch();
        let feature = feature_on_channel(channel, index).await?;
        store_cached_feature(epoch, registry, shared, &feature);
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
    registry: &ChannelRegistry,
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
        store_cached_feature(epoch, registry, shared, &feature);
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

    /// The registry still publishes the channel the open resolved.
    const CURRENT: fn() -> bool = || true;
    /// The registry has dropped it — the enumerator retired the node.
    const RETIRED: fn() -> bool = || false;

    #[test]
    fn a_store_started_before_a_clear_is_discarded() {
        let mut cache = EpochGuarded::new();
        let epoch = cache.epoch;
        // The channel retires while the feature open is in flight…
        cache.clear_for(0xA);
        // …so the open's belated success must not re-pin the channel.
        cache.store(epoch, 0xA, 2, "stale", CURRENT);
        assert_eq!(cache.get(0xA, 2), None);
    }

    /// The mirror of the case above, and the one an epoch alone cannot see:
    /// the retire lands *before* the open begins rather than during it.
    ///
    /// `hardware::play_haptic` resolves the channel from the registry and then
    /// awaits its I/O lease. A retire during that wait already ran
    /// `clear_for`, so by the time `play_haptic_on` snapshots the epoch there
    /// is nothing left to violate: the play succeeds on the still-writable
    /// handle and the store would re-pin the retired channel. The enumerator
    /// reopens a node only once every clone of its channel is gone
    /// (`Arc::strong_count == 1`), so that entry wedges the node for good —
    /// the Actions Ring trigger died with capture, so no later haptic can come
    /// along to invalidate it. Only the registry can still tell.
    #[test]
    fn a_store_for_a_channel_retired_before_the_open_is_discarded() {
        let mut cache = EpochGuarded::new();
        // The enumerator retires the channel while the play waits for its lease…
        cache.clear_for(0xA);
        // …so the open that follows snapshots an epoch that is already current.
        let epoch = cache.epoch;

        cache.store(epoch, 0xA, 2, "retired", RETIRED);

        assert_eq!(
            cache.get(0xA, 2),
            None,
            "a channel the enumerator has retired must never be cached again"
        );
    }

    #[test]
    fn a_store_with_a_current_epoch_lands() {
        let mut cache = EpochGuarded::new();
        cache.store(cache.epoch, 0xA, 2, "fresh", CURRENT);
        assert_eq!(cache.get(0xA, 2), Some("fresh"));
        assert_eq!(cache.get(0xB, 2), None);
        assert_eq!(cache.get(0xA, 3), None);
    }

    #[test]
    fn retiring_one_channel_keeps_anothers_entry_but_blocks_stale_stores() {
        let mut cache = EpochGuarded::new();
        cache.store(cache.epoch, 0xA, 2, "kept", CURRENT);
        let epoch = cache.epoch;
        cache.clear_for(0xB);
        assert_eq!(cache.get(0xA, 2), Some("kept"));
        cache.store(epoch, 0xB, 1, "stale", CURRENT);
        assert_eq!(cache.get(0xB, 1), None);
    }

    #[test]
    fn a_full_clear_empties_the_entry_and_blocks_stale_stores() {
        let mut cache = EpochGuarded::new();
        let epoch = cache.epoch;
        cache.store(epoch, 0xA, 2, "cached", CURRENT);
        cache.clear();
        assert_eq!(cache.get(0xA, 2), None);
        cache.store(epoch, 0xA, 2, "stale", CURRENT);
        assert_eq!(cache.get(0xA, 2), None);
    }
}
