use std::sync::{Arc, Mutex, Weak};

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature,
        haptic_feedback::{HapticConfigFlags, HapticFeedbackFeature, HapticWaveform},
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

/// Ensure the firmware haptic engine is armed: the enable flag set. Returns
/// `true` when a repair write was needed.
///
/// Nothing else in the stack ever asserts this state — devices historically
/// inherited it from Logi Options+, and some power transitions clear the
/// enable flag, after which `play` calls are accepted but produce no physical
/// feedback. The repair sets that flag only: intensity, power save, and
/// unnamed bits are the user's (Options+ writes them) and are written back as
/// read. A zero intensity is the user's "off" and is left alone. Callers
/// arm once per Actions Ring session, before the first hover — which is why
/// this deliberately bypasses the cached location and re-resolves the
/// feature: the cache key (channel identity + device index) cannot see a
/// *different device* re-paired into the same receiver slot, so each session
/// starts by overwriting the entry with a freshly-proven location, and the
/// per-hover plays then reuse it for the session's lifetime.
///
/// The repair counts only once the device's reply to the write reports the
/// enable flag. The reply is the device's state after the write, so a
/// device that kept the engine off answers without it, and reporting that
/// as a repair would leave the session playing into a disabled engine. The
/// rest of the reply is the firmware's to settle: a normalized intensity or
/// a changed power-save bit is accepted, as Logi Options+ adopts the reply.
pub async fn ensure_haptics_armed_on(shared: &SharedChannel) -> Result<bool, WriteError> {
    let channel = shared.channel();
    let index = shared.device_index();
    let (feature, feature_index) = feature_on_channel(channel, index).await?;
    store_cached_location(channel, index, feature_index);
    let config = feature.get_configuration().await.map_err(|error| {
        clear_cached_location();
        classify_hidpp_error(error, HidppOperation::PlayHaptic, HapticFeedbackFeature::ID)
    })?;
    if config.flags.contains(HapticConfigFlags::ENABLED) || config.intensity.get() == 0 {
        return Ok(false);
    }
    let applied = feature
        .set_configuration(config.flags | HapticConfigFlags::ENABLED, config.intensity)
        .await
        .map_err(|error| {
            clear_cached_location();
            classify_hidpp_error(error, HidppOperation::PlayHaptic, HapticFeedbackFeature::ID)
        })?;
    if !applied.flags.contains(HapticConfigFlags::ENABLED) {
        clear_cached_location();
        return Err(WriteError::UnsupportedResponse {
            operation: HidppOperation::PlayHaptic,
            feature_hex: HapticFeedbackFeature::ID,
        });
    }
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
    use std::assert_matches;
    use std::sync::Mutex as StdMutex;

    use super::*;
    use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};

    /// Serializes the tests that touch the one process-wide cache slot.
    static CACHE_SLOT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    const HAPTIC_INDEX: u8 = 0x0b;

    async fn silent_channel() -> Arc<HidppChannel> {
        let (raw, _reports) = ScriptedRawHidChannel::with_responder(|_| None);
        Arc::new(
            HidppChannel::from_raw_channel(raw)
                .await
                .expect("the scripted channel must support HID++"),
        )
    }

    /// A haptic device that stores what `setConfiguration` sends, lets
    /// `firmware` adjust it, and answers with the result.
    fn stateful_haptics(
        initial: [u8; 3],
        firmware: fn(&mut [u8; 3]),
    ) -> (ScriptedRawHidChannel, crate::replay::ReplayChannelHandle) {
        let state = StdMutex::new(initial);
        ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            if request.len() < 7 || request[0] != 0x10 {
                return None;
            }
            let mut payload = [0u8; 3];
            match (request[2], request[3] >> 4) {
                (0x00, 0x01) => payload[0] = 4,
                (0x00, 0x00) => {
                    if u16::from_be_bytes([request[4], request[5]]) == HapticFeedbackFeature::ID {
                        payload[0] = HAPTIC_INDEX;
                    }
                }
                (HAPTIC_INDEX, 0x01) => payload = *state.lock().expect("haptic state"),
                (HAPTIC_INDEX, 0x02) => {
                    let mut current = state.lock().expect("haptic state");
                    current[..2].copy_from_slice(&request[4..6]);
                    firmware(&mut current);
                    payload = *current;
                }
                _ => return None,
            }
            let mut response = request[..4].to_vec();
            response.extend_from_slice(&payload);
            Some(response)
        })
    }

    /// Arm a device holding `initial` whose firmware applies `firmware` to
    /// each write. Returns the result, the `setConfiguration` payloads, and
    /// whether the feature location is still cached afterwards.
    async fn arm_with(
        initial: [u8; 3],
        firmware: fn(&mut [u8; 3]),
    ) -> (Result<bool, WriteError>, Vec<[u8; 3]>, bool) {
        let (raw, handle) = stateful_haptics(initial, firmware);
        let shared = SharedChannel::new(
            scripted_channel(raw).await,
            DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb042,
            },
        );
        let result = ensure_haptics_armed_on(&shared).await;
        let writes = handle
            .written_reports()
            .into_iter()
            .filter(|report| report[2] == HAPTIC_INDEX && report[3] >> 4 == 0x02)
            .map(|report| [report[4], report[5], report[6]])
            .collect();
        let cached = cached_feature(shared.channel(), shared.device_index()).is_some();
        (result, writes, cached)
    }

    async fn arm(initial: [u8; 3]) -> (bool, Vec<[u8; 3]>) {
        let (result, writes, _) = arm_with(initial, |_| {}).await;
        let repaired = result.expect("the scripted device answers every haptic call");
        (repaired, writes)
    }

    #[tokio::test]
    async fn arming_sets_only_the_enable_bit() {
        let _slot = CACHE_SLOT.lock().await;
        // Disabled with power save on (Options+ sets byte 0 bit 1) and an
        // unnamed bit: arming keeps both and the user's intensity.
        assert_eq!(arm([0x82, 60, 0x34]).await, (true, vec![[0x83, 60, 0]]));
    }

    #[tokio::test]
    async fn arming_leaves_a_zero_intensity_and_an_armed_engine_alone() {
        let _slot = CACHE_SLOT.lock().await;
        // Zero is the user's "off"; it is never replaced by a guessed level.
        assert_eq!(arm([0x00, 0, 0x34]).await, (false, vec![]));
        assert_eq!(arm([0x03, 40, 0x34]).await, (false, vec![]));
    }

    #[tokio::test]
    async fn arming_fails_when_the_reply_shows_the_engine_still_off() {
        let _slot = CACHE_SLOT.lock().await;
        // The firmware keeps the engine disabled whatever is written.
        let (result, writes, cached) = arm_with([0x82, 60, 0x34], |state| state[0] &= !0x01).await;
        assert_matches!(
            result,
            Err(WriteError::UnsupportedResponse {
                operation: HidppOperation::PlayHaptic,
                feature_hex: HapticFeedbackFeature::ID,
            })
        );
        assert_eq!(writes, vec![[0x83, 60, 0]], "the repair was attempted once");
        assert!(!cached, "a refused repair leaves no cached location behind");
    }

    #[tokio::test]
    async fn arming_accepts_a_firmware_normalized_intensity() {
        let _slot = CACHE_SLOT.lock().await;
        // The firmware rounds the intensity to its level step and clears
        // power save; the engine is on, so the repair holds.
        let (result, writes, cached) = arm_with([0x82, 63, 0x34], |state| {
            state[1] = 60;
            state[0] &= !0x02;
        })
        .await;
        assert_matches!(result, Ok(true));
        assert_eq!(writes, vec![[0x83, 63, 0]]);
        assert!(
            cached,
            "a confirmed repair keeps the location for the plays"
        );
    }

    /// Holds [`CACHE_SLOT`]: every check below reads the shared cache slot.
    #[tokio::test]
    async fn the_cache_matches_exactly_the_channel_it_was_stored_for() {
        let _slot = CACHE_SLOT.lock().await;
        let channel = silent_channel().await;
        store_cached_location(&channel, 2, 7);
        assert!(
            cached_feature(&channel, 2).is_some(),
            "the stored channel at the stored index is a hit"
        );
        assert!(
            cached_feature(&channel, 3).is_none(),
            "another device index is a miss"
        );

        let other = silent_channel().await;
        assert!(
            cached_feature(&other, 2).is_none(),
            "another channel is a miss — identity, not address, decides"
        );

        // The enumerator retires the stored channel: nothing pins it (the
        // whole point of the Weak), and the entry can never match again —
        // including a fresh channel that might reuse the allocation, which
        // the surviving Weak makes impossible.
        drop(channel);
        let replacement = silent_channel().await;
        assert!(
            cached_feature(&replacement, 2).is_none(),
            "a dead entry must not match any later channel"
        );

        clear_cached_location();
        assert!(cached_feature(&replacement, 2).is_none());
    }
}
