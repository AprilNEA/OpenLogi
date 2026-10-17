use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature,
        haptic_feedback::{
            HapticConfigFlags, HapticFeedbackFeature, HapticStatus, HapticWaveform,
            SupportedWaveforms,
        },
    },
};
use tracing::debug;

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

use super::{HidppOperation, WriteError, classify_hidpp_error, with_route};

/// How long the advisory haptic-status read may take.
///
/// It follows a capabilities read the device has just answered, so a device
/// that serves function 3 answers well inside this. Without its own deadline,
/// a device that never answers would hold the open for the channel's 5 s
/// response timeout. That is past the agent's 1 s arming and 2 s play
/// budgets, so the play would be cancelled before falling back, and no
/// location would ever be cached.
const STATUS_READ_TIMEOUT: Duration = Duration::from_millis(250);

/// The haptic feature on one device, the waveforms it advertises, and
/// whether it reports that it can play.
struct Haptics {
    feature: Arc<HapticFeedbackFeature>,
    /// Where the feature was found — the part worth caching, see
    /// [`FeatureLocation`].
    feature_index: u8,
    /// The function-0 capability mask.
    waveforms: SupportedWaveforms,
    /// The function-3 haptic status, or `None` when the device did not
    /// answer it within [`STATUS_READ_TIMEOUT`].
    status: Option<HapticStatus>,
}

impl Haptics {
    /// Refuse, before anything is sent, a play this device would not serve:
    /// a waveform it does not advertise, or any play while it reports a
    /// non-zero haptic status.
    fn check(&self, waveform: HapticWaveform) -> Result<(), WriteError> {
        ensure_advertised(self.waveforms, waveform)?;
        ensure_ready(self.status)
    }
}

/// Resolve the haptic feature on `channel`, read which waveforms it
/// advertises and its haptic status. Logi Options+ reads the same mask when
/// it starts the feature.
async fn open_haptics(
    channel: &Arc<HidppChannel>,
    device_index: u8,
) -> Result<Haptics, WriteError> {
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
    let feature = device.add_feature::<HapticFeedbackFeature>(info.index);
    let capabilities = feature.get_capabilities().await.map_err(|error| {
        classify_hidpp_error(error, HidppOperation::PlayHaptic, HapticFeedbackFeature::ID)
    })?;
    // Advisory: a device that does not answer function 3 plays as before.
    // Logi Options+ starts from a zero status and refuses only after the
    // device reports a non-zero one.
    let status = match tokio::time::timeout(STATUS_READ_TIMEOUT, feature.get_haptic_status()).await
    {
        Ok(Ok(status)) => Some(status),
        Ok(Err(error)) => {
            debug!(%error, "haptic status unreadable; playing without it");
            None
        }
        Err(_) => {
            debug!(timeout = ?STATUS_READ_TIMEOUT, "haptic status read timed out; playing without it");
            None
        }
    };
    Ok(Haptics {
        feature,
        feature_index: info.index,
        waveforms: capabilities.waveforms,
        status,
    })
}

/// Refuse, before anything is sent, a waveform the device does not
/// advertise. Logi Options+ refuses it the same way ("Waveform is not
/// supported."); sending it anyway would spend a round-trip, and the play's
/// retries, on a request the firmware never offered to serve.
fn ensure_advertised(
    waveforms: SupportedWaveforms,
    waveform: HapticWaveform,
) -> Result<(), WriteError> {
    if waveforms.supports(waveform) {
        Ok(())
    } else {
        Err(WriteError::HapticWaveformUnsupported {
            waveform: waveform.into(),
        })
    }
}

/// Refuse, before anything is sent, a play while the device reports a
/// non-zero haptic status. Logi Options+ refuses every play while the status
/// it last read is non-zero, without naming any value: the status is passed
/// on raw. An unread status refuses nothing, as Options+ starts from zero.
fn ensure_ready(status: Option<HapticStatus>) -> Result<(), WriteError> {
    match status {
        Some(status) if !status.is_ready() => Err(WriteError::HapticsUnavailable {
            status: status.raw(),
        }),
        _ => Ok(()),
    }
}

/// Where the haptic feature lives, what it advertises, and the status it
/// reported: enough to rebuild the accessor and check a play without I/O. Haptic plays are fired per ring hover, and resolving the feature
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
    waveforms: SupportedWaveforms,
    status: Option<HapticStatus>,
}

static CACHED_LOCATION: Mutex<Option<FeatureLocation>> = Mutex::new(None);

/// Rebuild the cached accessor for exactly `channel`, without I/O, with the
/// waveforms and status the open read. `None` when nothing is cached, the
/// entry belongs to another (or a dead) channel, or the device index differs.
fn cached_feature(channel: &Arc<HidppChannel>, index: u8) -> Option<Haptics> {
    let guard = CACHED_LOCATION.lock().ok()?;
    let location = guard.as_ref()?;
    (location.device_index == index
        && location
            .channel
            .upgrade()
            .is_some_and(|live| Arc::ptr_eq(&live, channel)))
    .then(|| Haptics {
        feature: Arc::new(<HapticFeedbackFeature as CreatableFeature>::new(
            Arc::clone(channel),
            index,
            location.feature_index,
        )),
        feature_index: location.feature_index,
        waveforms: location.waveforms,
        status: location.status,
    })
}

/// Remember where a successful open found the feature. Unconditional: an
/// entry for a channel the enumerator retires later holds nothing alive and
/// can never match another channel, so no store/retire ordering can produce
/// wreckage.
fn store_cached_location(channel: &Arc<HidppChannel>, device_index: u8, haptics: &Haptics) {
    if let Ok(mut guard) = CACHED_LOCATION.lock() {
        *guard = Some(FeatureLocation {
            channel: Arc::downgrade(channel),
            device_index,
            feature_index: haptics.feature_index,
            waveforms: haptics.waveforms,
            status: haptics.status,
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
/// per-hover plays then reuse it for the session's lifetime. The fresh open
/// also re-reads the haptic status, so each session plays, or refuses to,
/// by what the device reports when it starts.
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
    let haptics = open_haptics(channel, index).await?;
    store_cached_location(channel, index, &haptics);
    let feature = haptics.feature;
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
/// round-trip); any I/O error invalidates the cache and the play is retried
/// once through a fresh open, so a rebuilt channel or stale index self-heals.
///
/// A waveform the device does not advertise fails with
/// [`WriteError::HapticWaveformUnsupported`], and a play while the device
/// reports a non-zero haptic status with [`WriteError::HapticsUnavailable`],
/// before anything is sent. The cache is kept: the mask and status, read
/// once per open, are facts about the device, not signs the location went
/// stale.
pub async fn play_haptic_on(
    shared: &SharedChannel,
    waveform: HapticWaveform,
) -> Result<(), WriteError> {
    let channel = shared.channel();
    let index = shared.device_index();
    if let Some(haptics) = cached_feature(channel, index) {
        haptics.check(waveform)?;
        if haptics.feature.play(waveform).await.is_ok() {
            return Ok(());
        }
        clear_cached_location();
    }
    let haptics = open_haptics(channel, index).await?;
    store_cached_location(channel, index, &haptics);
    haptics.check(waveform)?;
    haptics.feature.play(waveform).await.map_err(|error| {
        clear_cached_location();
        classify_hidpp_error(error, HidppOperation::PlayHaptic, HapticFeedbackFeature::ID)
    })
}

/// Play a waveform immediately by route. A waveform the device does not
/// advertise fails with [`WriteError::HapticWaveformUnsupported`], and a play
/// while the device reports a non-zero haptic status with
/// [`WriteError::HapticsUnavailable`], before anything is sent.
pub async fn play_haptic(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    waveform: HapticWaveform,
) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let haptics = open_haptics(&channel, index).await?;
        haptics.check(waveform)?;
        haptics.feature.play(waveform).await.map_err(|error| {
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
    /// What an MX Master 4 needs for the Actions Ring: both ring waveforms.
    const RING_WAVEFORMS: u32 = (1 << 1) | (1 << 4);

    async fn silent_channel() -> Arc<HidppChannel> {
        let (raw, _reports) = ScriptedRawHidChannel::with_responder(|_| None);
        Arc::new(
            HidppChannel::from_raw_channel(raw)
                .await
                .expect("the scripted channel must support HID++"),
        )
    }

    /// A haptic device that advertises `waveforms`, stores what
    /// `setConfiguration` sends, lets `firmware` adjust it, answers with the
    /// result, reports a zero haptic status, and accepts every play.
    fn stateful_haptics(
        initial: [u8; 3],
        firmware: fn(&mut [u8; 3]),
        waveforms: u32,
    ) -> (ScriptedRawHidChannel, crate::replay::ReplayChannelHandle) {
        haptics_reporting(initial, firmware, waveforms, StatusReply::Status(0))
    }

    /// How a scripted device answers `getHapticInfo`.
    #[derive(Clone, Copy)]
    enum StatusReply {
        /// Reports this status byte.
        Status(u8),
        /// Answers with an invalid-function error.
        Error,
        /// Never answers.
        Silent,
    }

    /// [`stateful_haptics`] whose `getHapticInfo` answers as `status` says.
    fn haptics_reporting(
        initial: [u8; 3],
        firmware: fn(&mut [u8; 3]),
        waveforms: u32,
        status: StatusReply,
    ) -> (ScriptedRawHidChannel, crate::replay::ReplayChannelHandle) {
        let state = StdMutex::new(initial);
        ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            if request.len() < 7 || request[0] != 0x10 {
                return None;
            }
            if (request[2], request[3] >> 4) == (HAPTIC_INDEX, 0x03) {
                let raw = match status {
                    StatusReply::Status(raw) => raw,
                    // A HID++ 2.0 error: InvalidFunctionId (7).
                    StatusReply::Error => {
                        return Some(vec![0x10, request[1], 0xff, request[2], request[3], 7, 0]);
                    }
                    StatusReply::Silent => return None,
                };
                let mut response = request[..4].to_vec();
                response.extend_from_slice(&[raw, 0, 0]);
                return Some(response);
            }
            if (request[2], request[3] >> 4) == (HAPTIC_INDEX, 0x00) {
                // getCapabilities: a long reply, the mask in bytes 4..8.
                let mut response = vec![0x11, request[1], request[2], request[3]];
                response.extend_from_slice(&[0; 4]);
                response.extend_from_slice(&waveforms.to_be_bytes());
                response.resize(20, 0);
                return Some(response);
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
                (HAPTIC_INDEX, 0x04) => {}
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
        let (raw, handle) = stateful_haptics(initial, firmware, RING_WAVEFORMS);
        let shared = shared_channel(raw).await;
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

    async fn shared_channel(raw: ScriptedRawHidChannel) -> SharedChannel {
        SharedChannel::new(
            scripted_channel(raw).await,
            DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb042,
            },
        )
    }

    /// How many `getHapticInfo` requests `handle` saw.
    fn status_reads(handle: &crate::replay::ReplayChannelHandle) -> usize {
        handle
            .written_reports()
            .iter()
            .filter(|report| report[2] == HAPTIC_INDEX && report[3] >> 4 == 0x03)
            .count()
    }

    /// The waveform IDs `handle` saw played, in order.
    fn plays(handle: &crate::replay::ReplayChannelHandle) -> Vec<u8> {
        handle
            .written_reports()
            .into_iter()
            .filter(|report| report[2] == HAPTIC_INDEX && report[3] >> 4 == 0x04)
            .map(|report| report[4])
            .collect()
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
        let haptics = Haptics {
            feature: Arc::new(HapticFeedbackFeature::new(Arc::clone(&channel), 2, 7)),
            feature_index: 7,
            waveforms: SupportedWaveforms::empty(),
            status: None,
        };
        store_cached_location(&channel, 2, &haptics);
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

    #[tokio::test]
    async fn a_waveform_the_device_does_not_advertise_is_never_sent() {
        let _slot = CACHE_SLOT.lock().await;
        clear_cached_location();
        // Only the subtle collision is advertised.
        let (raw, handle) = stateful_haptics([0x03, 40, 0x34], |_| {}, 1 << 4);
        let shared = shared_channel(raw).await;

        // A fresh open reads the mask and refuses without a play request.
        assert_matches!(
            play_haptic_on(&shared, HapticWaveform::DampStateChange).await,
            Err(WriteError::HapticWaveformUnsupported { waveform: 1 })
        );
        assert_eq!(plays(&handle), Vec::<u8>::new());
        assert!(
            cached_feature(shared.channel(), shared.device_index()).is_some(),
            "a refusal is a fact about the device; the location stays cached"
        );

        // The cached mask refuses the same waveform without any I/O at all.
        let sent = handle.written_reports().len();
        assert_matches!(
            play_haptic_on(&shared, HapticWaveform::DampStateChange).await,
            Err(WriteError::HapticWaveformUnsupported { waveform: 1 })
        );
        assert_eq!(handle.written_reports().len(), sent);

        // The advertised waveform still plays through the cached location.
        assert_matches!(
            play_haptic_on(&shared, HapticWaveform::SubtleCollision).await,
            Ok(())
        );
        assert_eq!(plays(&handle), vec![4]);
        clear_cached_location();
    }

    #[tokio::test]
    async fn unnamed_capability_bits_advertise_no_ring_waveform() {
        let _slot = CACHE_SLOT.lock().await;
        clear_cached_location();
        let (raw, handle) = stateful_haptics([0x03, 40, 0x34], |_| {}, !RING_WAVEFORMS);
        let shared = shared_channel(raw).await;
        for waveform in [
            HapticWaveform::DampStateChange,
            HapticWaveform::SubtleCollision,
        ] {
            assert_matches!(
                play_haptic_on(&shared, waveform).await,
                Err(WriteError::HapticWaveformUnsupported { .. })
            );
        }
        assert_eq!(plays(&handle), Vec::<u8>::new());
        clear_cached_location();
    }

    #[tokio::test]
    async fn an_armed_session_plays_both_ring_waveforms_from_the_cached_mask() {
        let _slot = CACHE_SLOT.lock().await;
        let (raw, handle) = stateful_haptics([0x03, 40, 0x34], |_| {}, RING_WAVEFORMS);
        let shared = shared_channel(raw).await;
        assert_matches!(ensure_haptics_armed_on(&shared).await, Ok(false));
        let capability_reads = |handle: &crate::replay::ReplayChannelHandle| {
            handle
                .written_reports()
                .iter()
                .filter(|report| report[2] == HAPTIC_INDEX && report[3] >> 4 == 0x00)
                .count()
        };
        assert_eq!(capability_reads(&handle), 1, "arming reads the mask once");
        for waveform in [
            HapticWaveform::SubtleCollision,
            HapticWaveform::DampStateChange,
        ] {
            assert_matches!(play_haptic_on(&shared, waveform).await, Ok(()));
        }
        assert_eq!(plays(&handle), vec![4, 1]);
        assert_eq!(
            capability_reads(&handle),
            1,
            "the plays reuse the mask cached with the location"
        );
        clear_cached_location();
    }

    #[tokio::test]
    async fn a_non_zero_status_refuses_every_play_before_it_is_sent() {
        let _slot = CACHE_SLOT.lock().await;
        clear_cached_location();
        let (raw, handle) = haptics_reporting(
            [0x03, 40, 0x34],
            |_| {},
            RING_WAVEFORMS,
            StatusReply::Status(2),
        );
        let shared = shared_channel(raw).await;

        // A fresh open reads the status and refuses without a play request.
        assert_matches!(
            play_haptic_on(&shared, HapticWaveform::SubtleCollision).await,
            Err(WriteError::HapticsUnavailable { status: 2 })
        );
        assert_eq!(plays(&handle), Vec::<u8>::new());
        assert!(
            cached_feature(shared.channel(), shared.device_index()).is_some(),
            "a refusal is a fact about the device; the location stays cached"
        );

        // The cached status refuses again without any I/O at all.
        let sent = handle.written_reports().len();
        assert_matches!(
            play_haptic_on(&shared, HapticWaveform::DampStateChange).await,
            Err(WriteError::HapticsUnavailable { status: 2 })
        );
        assert_eq!(handle.written_reports().len(), sent);
        clear_cached_location();
    }

    #[tokio::test]
    async fn an_armed_session_plays_by_the_status_read_when_it_starts() {
        let _slot = CACHE_SLOT.lock().await;
        let (raw, handle) = stateful_haptics([0x03, 40, 0x34], |_| {}, RING_WAVEFORMS);
        let shared = shared_channel(raw).await;
        assert_matches!(ensure_haptics_armed_on(&shared).await, Ok(false));
        assert_eq!(status_reads(&handle), 1, "arming reads the status once");
        for waveform in [
            HapticWaveform::SubtleCollision,
            HapticWaveform::DampStateChange,
        ] {
            assert_matches!(play_haptic_on(&shared, waveform).await, Ok(()));
        }
        assert_eq!(plays(&handle), vec![4, 1]);
        assert_eq!(
            status_reads(&handle),
            1,
            "the plays reuse the status cached with the location"
        );

        // A session that starts while the device reports it cannot play
        // refuses its plays.
        let (raw, handle) = haptics_reporting(
            [0x03, 40, 0x34],
            |_| {},
            RING_WAVEFORMS,
            StatusReply::Status(1),
        );
        let shared = shared_channel(raw).await;
        assert_matches!(ensure_haptics_armed_on(&shared).await, Ok(false));
        assert_matches!(
            play_haptic_on(&shared, HapticWaveform::SubtleCollision).await,
            Err(WriteError::HapticsUnavailable { status: 1 })
        );
        assert_eq!(plays(&handle), Vec::<u8>::new());
        clear_cached_location();
    }

    #[tokio::test]
    async fn a_device_that_does_not_answer_the_status_still_plays() {
        let _slot = CACHE_SLOT.lock().await;
        clear_cached_location();
        let (raw, handle) =
            haptics_reporting([0x03, 40, 0x34], |_| {}, RING_WAVEFORMS, StatusReply::Error);
        let shared = shared_channel(raw).await;
        assert_matches!(
            play_haptic_on(&shared, HapticWaveform::SubtleCollision).await,
            Ok(())
        );
        assert_eq!(status_reads(&handle), 1, "the status was asked for");
        assert_eq!(plays(&handle), vec![4]);
        clear_cached_location();
    }

    /// The HID++ response timeout runs on the wall clock, so with tokio's
    /// clock paused only [`STATUS_READ_TIMEOUT`] can end the silent read
    /// quickly: without it the open would wait the channel's full 5 s. The
    /// paused clock still advances by the deadline itself, which bounds it.
    #[tokio::test(start_paused = true)]
    async fn a_status_that_never_arrives_stops_neither_arming_nor_play() {
        let _slot = CACHE_SLOT.lock().await;
        clear_cached_location();
        let (raw, handle) = haptics_reporting(
            [0x03, 40, 0x34],
            |_| {},
            RING_WAVEFORMS,
            StatusReply::Silent,
        );
        let shared = shared_channel(raw).await;
        let started = std::time::Instant::now();
        let started_paused = tokio::time::Instant::now();
        assert_matches!(ensure_haptics_armed_on(&shared).await, Ok(false));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "arming does not wait for the channel's response timeout, took {:?}",
            started.elapsed()
        );
        assert!(
            started_paused.elapsed() < Duration::from_millis(500),
            "the status deadline leaves most of the agent's 1 s arming budget, took {:?}",
            started_paused.elapsed()
        );
        assert_eq!(status_reads(&handle), 1, "arming asked for the status");
        assert_matches!(
            play_haptic_on(&shared, HapticWaveform::SubtleCollision).await,
            Ok(())
        );
        assert_matches!(
            play_haptic_on(&shared, HapticWaveform::SubtleCollision).await,
            Ok(())
        );
        assert_eq!(
            status_reads(&handle),
            1,
            "the plays reuse the location cached despite the missing status"
        );
        assert_eq!(plays(&handle), vec![4, 4]);
        clear_cached_location();
    }
}
