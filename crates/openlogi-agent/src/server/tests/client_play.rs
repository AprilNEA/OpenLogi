//! Haptics a client plays through [`RingHapticPlayer::play_external`]: the
//! burst arms a disarmed engine once, plays, and hands the engine back once
//! the plays stop.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use openlogi_core::device::{
    Capabilities, DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo,
};
use openlogi_device::test_support::{ScriptedRawHidHandle, publish_scripted_channel};
use openlogi_fixture::RequestMatch;
use openlogi_hid::HapticWaveform;

use super::super::{ARM_BUDGET, BURST_IDLE, RingHapticPlayer, check_haptic_target};
use super::{DeviceRoute, ObservableState, Orchestrator, WriteError};

/// The device layer caches the haptic feature location process-wide.
static CACHE_SLOT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// How long one scenario may take once it holds [`CACHE_SLOT`]: every wait
/// in it is a few seconds of real time at most.
const SCENARIO_DEADLINE: Duration = Duration::from_secs(30);

/// How long a scenario may wait for the others to let go of [`CACHE_SLOT`].
const SLOT_WAIT: Duration = Duration::from_secs(180);

/// Run `scenario` alone against the feature cache, and fail it rather than
/// hang the test job when a regression strands one of its waits.
async fn bounded<T>(scenario: impl Future<Output = T>) -> T {
    let _slot = tokio::time::timeout(SLOT_WAIT, CACHE_SLOT.lock())
        .await
        .expect("no other scenario holds the haptic feature cache this long");
    tokio::time::timeout(SCENARIO_DEADLINE, scenario)
        .await
        .expect("the scenario finishes in time instead of stranding a wait")
}

const HAPTIC_INDEX: u8 = 0x0b;
const SUBTLE_COLLISION: u32 = 1 << 4;
const BOTH_WAVEFORMS: u32 = (1 << 1) | SUBTLE_COLLISION;
/// Disabled, with power save and the user's 60 % intensity; the third byte
/// is the level count and step the firmware reports.
const DISARMED: [u8; 3] = [0x82, 60, 0x34];

fn mouse() -> DeviceRoute {
    DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb042,
    }
}

fn other_mouse() -> DeviceRoute {
    DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb034,
    }
}

fn third_mouse() -> DeviceRoute {
    DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb035,
    }
}

const ARMED: [u8; 3] = [0x83, 60, 0x34];

/// A `0x19b0` device advertising `waveforms`, keeping the configuration that
/// `setConfiguration` writes, reporting a zero haptic status, and accepting
/// every play. It has no `0x0003`, so arming records no unit ID.
fn haptic_device(
    config: Arc<StdMutex<[u8; 3]>>,
    waveforms: u32,
) -> impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static {
    move |request| {
        if request.len() < 7 || request[0] != 0x10 {
            return None;
        }
        let function = request[3] >> 4;
        if (request[2], function) == (HAPTIC_INDEX, 0x00) {
            // getCapabilities: a long reply, the waveform mask in bytes 8..12.
            let mut response = vec![0x11, request[1], request[2], request[3], 0, 0, 0, 0];
            response.extend_from_slice(&waveforms.to_be_bytes());
            response.resize(20, 0);
            return Some(response);
        }
        let mut payload = [0u8; 3];
        match (request[2], function) {
            (0x00, 0x01) => payload[0] = 4,
            (0x00, 0x00) => {
                if u16::from_be_bytes([request[4], request[5]]) == 0x19b0 {
                    payload[0] = HAPTIC_INDEX;
                }
            }
            (HAPTIC_INDEX, 0x01) => payload = *config.lock().expect("config"),
            (HAPTIC_INDEX, 0x02) => {
                let mut current = config.lock().expect("config");
                current[..2].copy_from_slice(&request[4..6]);
                payload = *current;
            }
            // getHapticInfo (status 0) and playWaveform.
            (HAPTIC_INDEX, 0x03 | 0x04) => {}
            _ => return None,
        }
        let mut response = request[..4].to_vec();
        response.extend_from_slice(&payload);
        Some(response)
    }
}

fn reports(handle: &ScriptedRawHidHandle, function: u8) -> Vec<Vec<u8>> {
    handle
        .written_reports()
        .into_iter()
        .filter(|report| report[2] == HAPTIC_INDEX && report[3] >> 4 == function)
        .collect()
}

/// The `setConfiguration` flag and intensity bytes, in order.
fn config_writes(handle: &ScriptedRawHidHandle) -> Vec<[u8; 2]> {
    reports(handle, 0x02)
        .into_iter()
        .map(|report| [report[4], report[5]])
        .collect()
}

/// The waveform IDs played, in order.
fn plays(handle: &ScriptedRawHidHandle) -> Vec<u8> {
    reports(handle, 0x04)
        .into_iter()
        .map(|report| report[4])
        .collect()
}

/// A worker over one scripted haptic mouse holding `config`.
async fn player_for(
    config: Arc<StdMutex<[u8; 3]>>,
    waveforms: u32,
) -> (RingHapticPlayer, ScriptedRawHidHandle) {
    let (player, mut handles) =
        player_over(vec![(mouse(), Box::new(haptic_device(config, waveforms)))]).await;
    (player, handles.remove(0))
}

/// How a scripted device answers a request.
type Responder = Box<dyn Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync>;

/// [`haptic_device`] holding `config` with both waveforms, which stops
/// answering plays and configuration writes once `lossy` is set.
fn lossy_haptic_device(config: Arc<StdMutex<[u8; 3]>>, lossy: Arc<AtomicBool>) -> Responder {
    let device = haptic_device(config, BOTH_WAVEFORMS);
    Box::new(move |request| {
        let lost = lossy.load(Ordering::SeqCst)
            && request.len() >= 4
            && request[2] == HAPTIC_INDEX
            && matches!(request[3] >> 4, 0x02 | 0x04);
        if lost { None } else { device(request) }
    })
}

/// [`haptic_device`] holding `config` with both waveforms, whose arming read
/// is never answered: every arming on it takes its whole budget.
fn slow_arming_haptic_device(config: Arc<StdMutex<[u8; 3]>>) -> Responder {
    let device = haptic_device(config, BOTH_WAVEFORMS);
    Box::new(move |request| {
        let arming_read =
            request.len() >= 4 && request[2] == HAPTIC_INDEX && request[3] >> 4 == 0x01;
        if arming_read { None } else { device(request) }
    })
}

/// A worker over one scripted device per entry, each answering on its
/// route.
async fn player_over(
    devices: Vec<(DeviceRoute, Responder)>,
) -> (RingHapticPlayer, Vec<ScriptedRawHidHandle>) {
    let observable = Arc::new(ObservableState::new("test".to_owned()));
    let shared = Orchestrator::new(openlogi_core::config::Config::default(), observable).shared();
    let mut handles = Vec::new();
    for (index, (route, responder)) in devices.into_iter().enumerate() {
        let (_, handle) = publish_scripted_channel(
            &shared.channel_registry,
            &format!("mouse-{index}"),
            route,
            responder,
        )
        .await;
        handles.push(handle);
    }
    (RingHapticPlayer::spawn(shared), handles)
}

/// Wait until `done`, or a few seconds have passed.
async fn until(mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !done() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wait until the engine reads `expected`, or a few seconds have passed.
async fn until_engine_reads(config: &StdMutex<[u8; 3]>, expected: [u8; 3]) {
    until(|| *config.lock().expect("config") == expected).await;
}

/// The `setConfiguration` (2) and `playWaveform` (4) functions written, in
/// order.
fn engine_writes(handle: &ScriptedRawHidHandle) -> Vec<u8> {
    handle
        .written_reports()
        .into_iter()
        .filter(|report| report[2] == HAPTIC_INDEX && matches!(report[3] >> 4, 0x02 | 0x04))
        .map(|report| report[3] >> 4)
        .collect()
}

/// Let the burst go idle, then wait until the engine reads `expected`.
///
/// Real time, not a paused clock: the scripted device answers on a thread,
/// so a paused tokio clock jumps past the worker's play and restore budgets
/// while it waits for those answers, and the tests flake under load.
async fn after_the_burst(config: &StdMutex<[u8; 3]>, expected: [u8; 3]) {
    tokio::time::sleep(BURST_IDLE + Duration::from_millis(300)).await;
    until_engine_reads(config, expected).await;
}

#[tokio::test]
async fn a_burst_on_a_disarmed_engine_arms_once_plays_and_hands_it_back() {
    bounded(async {
        let config = Arc::new(StdMutex::new(DISARMED));
        let (player, handle) = player_for(Arc::clone(&config), BOTH_WAVEFORMS).await;
        let mut sessions = 10..;

        for waveform in [
            HapticWaveform::SubtleCollision,
            HapticWaveform::SubtleCollision,
            HapticWaveform::DampStateChange,
        ] {
            player
                .play_external(mouse(), waveform, || sessions.next().expect("an ID"))
                .await
                .expect("the scripted mouse plays");
        }
        assert_eq!(plays(&handle), vec![4, 4, 1]);
        assert_eq!(
            config_writes(&handle),
            vec![[0x83, 60]],
            "the burst's first play turned the engine on, and only it"
        );
        assert_eq!(
            reports(&handle, 0x01).len(),
            1,
            "one arming read for the whole burst; later plays take one round-trip"
        );
        assert_eq!(sessions.next(), Some(11), "the burst opened one session");

        after_the_burst(&config, DISARMED).await;
        assert_eq!(
            config_writes(&handle),
            vec![[0x83, 60], [0x82, 60]],
            "the enable the burst made was handed back once it went idle"
        );
        assert_eq!(*config.lock().expect("config"), DISARMED);

        // The next play is a new burst: it arms again rather than playing into
        // the engine it just handed back.
        player
            .play_external(mouse(), HapticWaveform::SubtleCollision, || 20)
            .await
            .expect("the scripted mouse plays");
        assert_eq!(config_writes(&handle).len(), 3);
        after_the_burst(&config, DISARMED).await;
        assert_eq!(*config.lock().expect("config"), DISARMED);
    })
    .await;
}

#[tokio::test]
async fn an_armed_engine_is_played_on_and_left_alone() {
    bounded(async {
        let armed = [0x83, 60, 0x34];
        let config = Arc::new(StdMutex::new(armed));
        let (player, handle) = player_for(Arc::clone(&config), BOTH_WAVEFORMS).await;
        player
            .play_external(mouse(), HapticWaveform::SubtleCollision, || 1)
            .await
            .expect("the scripted mouse plays");
        after_the_burst(&config, armed).await;
        assert_eq!(plays(&handle), vec![4]);
        assert!(
            config_writes(&handle).is_empty(),
            "nothing to arm or hand back"
        );
    })
    .await;
}

#[tokio::test]
async fn an_unadvertised_waveform_is_refused_and_leaves_no_engine_on() {
    bounded(async {
        let config = Arc::new(StdMutex::new(DISARMED));
        let (player, handle) = player_for(Arc::clone(&config), SUBTLE_COLLISION).await;
        assert!(matches!(
            player
                .play_external(mouse(), HapticWaveform::DampStateChange, || 1)
                .await,
            Err(WriteError::HapticWaveformUnsupported { waveform: 1 })
        ));
        assert!(plays(&handle).is_empty(), "the refusal sent no play");
        after_the_burst(&config, DISARMED).await;
        assert_eq!(
            *config.lock().expect("config"),
            DISARMED,
            "the arming the refused play opened with was handed back too"
        );
    })
    .await;
}

/// A ring opened on the mouse a client burst plays on shares its engine: the
/// burst going idle leaves the engine on for the ring, and the ring's close
/// hands it back.
#[tokio::test]
async fn a_burst_going_idle_leaves_the_engine_on_for_an_open_ring() {
    bounded(async {
        let config = Arc::new(StdMutex::new(DISARMED));
        let (player, handle) = player_for(Arc::clone(&config), BOTH_WAVEFORMS).await;
        player.begin_session(5, Some(mouse()));
        until_engine_reads(&config, ARMED).await;
        player
            .play_external(mouse(), HapticWaveform::SubtleCollision, || 6)
            .await
            .expect("the scripted mouse plays");

        tokio::time::sleep(BURST_IDLE + Duration::from_millis(500)).await;
        assert_eq!(
            *config.lock().expect("config"),
            ARMED,
            "the burst is over, but the ring still plays on the engine"
        );
        player.end_session(5);
        until_engine_reads(&config, DISARMED).await;
        assert_eq!(
            config_writes(&handle),
            vec![[0x83, 60], [0x82, 60]],
            "armed once, handed back once both sessions closed"
        );
    })
    .await;
}

/// The other order: a ring armed and closed inside a burst leaves the engine
/// on for the burst, which hands it back once it goes idle.
#[tokio::test]
async fn a_ring_closing_inside_a_burst_leaves_the_engine_on_for_it() {
    bounded(async {
        let config = Arc::new(StdMutex::new(DISARMED));
        let (player, handle) = player_for(Arc::clone(&config), BOTH_WAVEFORMS).await;
        player
            .play_external(mouse(), HapticWaveform::SubtleCollision, || 5)
            .await
            .expect("the scripted mouse plays");
        let reads = reports(&handle, 0x01).len();
        player.begin_session(6, Some(mouse()));
        // The ring's arming reads the engine, finds it on, and owes nothing
        // of its own; only then does the ring close.
        until(|| reports(&handle, 0x01).len() > reads).await;
        assert_eq!(reports(&handle, 0x01).len(), reads + 1, "the ring armed");
        player.end_session(6);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            *config.lock().expect("config"),
            ARMED,
            "the ring is over, but the burst still plays on the engine"
        );
        player
            .play_external(mouse(), HapticWaveform::SubtleCollision, || 7)
            .await
            .expect("the scripted mouse plays");
        assert_eq!(
            config_writes(&handle),
            vec![[0x83, 60]],
            "the burst's later play found its engine still on"
        );
        after_the_burst(&config, DISARMED).await;
        assert_eq!(config_writes(&handle), vec![[0x83, 60], [0x82, 60]]);
    })
    .await;
}

/// A play for a device whose session opened while the worker was arming
/// another device's waits for its own arming, rather than playing into an
/// engine that is still off.
#[tokio::test]
async fn a_play_queued_behind_another_devices_arming_is_armed_first() {
    bounded(async {
        let stalled = Arc::new(StdMutex::new(DISARMED));
        let config = Arc::new(StdMutex::new(DISARMED));
        let (player, handles) = player_over(vec![
            (
                other_mouse(),
                Box::new(haptic_device(Arc::clone(&stalled), BOTH_WAVEFORMS)),
            ),
            (
                mouse(),
                Box::new(haptic_device(Arc::clone(&config), BOTH_WAVEFORMS)),
            ),
        ])
        .await;
        // The other mouse's ring arms first; its configuration read stalls.
        let read = handles[0].hold_next_response(
            RequestMatch::Hidpp20,
            &[0x10, 0xff, HAPTIC_INDEX, 0x10, 0, 0, 0],
        );
        player.begin_session(1, Some(other_mouse()));
        read.request_written().await;

        let (published, opened) = tokio::sync::oneshot::channel();
        let client = {
            let player = player.clone();
            tokio::spawn(async move {
                player
                    .play_external(mouse(), HapticWaveform::SubtleCollision, move || {
                        let _ = published.send(());
                        2
                    })
                    .await
            })
        };
        opened.await.expect("the client play opened its session");
        read.release();

        client
            .await
            .expect("the client task")
            .expect("the scripted mouse plays");
        let written = engine_writes(&handles[1]);
        assert_eq!(
            written.first(),
            Some(&0x02),
            "the mouse's engine was turned on before its play: {written:?}"
        );
        player.end_session(1);
        after_the_burst(&config, DISARMED).await;
    })
    .await;
}

/// A client play stuck behind a worker held up by a lost ring buzz and an
/// unreachable restore keeps its burst open until it has played, so the
/// burst cannot close before its arming and play into an engine still off.
#[tokio::test]
async fn a_burst_stays_open_while_its_play_waits_behind_a_stalled_worker() {
    bounded(async {
        let lossy = Arc::new(AtomicBool::new(false));
        let stalled = Arc::new(StdMutex::new(DISARMED));
        let config = Arc::new(StdMutex::new(DISARMED));
        let (player, handles) = player_over(vec![
            (
                other_mouse(),
                lossy_haptic_device(Arc::clone(&stalled), Arc::clone(&lossy)),
            ),
            (
                mouse(),
                Box::new(haptic_device(Arc::clone(&config), BOTH_WAVEFORMS)),
            ),
        ])
        .await;
        player.begin_session(1, Some(other_mouse()));
        until_engine_reads(&stalled, ARMED).await;
        // The ring's last buzz is lost, holding the worker for its whole
        // play budget, and so is the restore its close owes after it.
        lossy.store(true, Ordering::SeqCst);
        player.play(
            1,
            Some(other_mouse()),
            HapticWaveform::SubtleCollision,
            "hover",
        );
        player.end_session(1);
        until(|| !plays(&handles[0]).is_empty()).await;

        player
            .play_external(mouse(), HapticWaveform::SubtleCollision, || 2)
            .await
            .expect("the scripted mouse plays");
        let written = engine_writes(&handles[1]);
        assert_eq!(
            written.first(),
            Some(&0x02),
            "the mouse's engine was turned on before its play: {written:?}"
        );
        after_the_burst(&config, DISARMED).await;
    })
    .await;
}

/// Two clients alternating between two mice faster than either can be
/// armed open a new burst with every play. Each worker pass still plays a
/// pulse and reaches the restores, rather than arming the newest burst over
/// and over while nothing plays and nothing is handed back.
#[tokio::test]
async fn alternating_plays_during_slow_arming_still_play_and_restore() {
    bounded(async {
        let ringed = Arc::new(StdMutex::new(DISARMED));
        let (player, handles) = player_over(vec![
            (
                third_mouse(),
                Box::new(haptic_device(Arc::clone(&ringed), BOTH_WAVEFORMS)),
            ),
            (
                mouse(),
                slow_arming_haptic_device(Arc::new(StdMutex::new(DISARMED))),
            ),
            (
                other_mouse(),
                slow_arming_haptic_device(Arc::new(StdMutex::new(DISARMED))),
            ),
        ])
        .await;
        // A ring on a third mouse owes its engine back once it closes.
        player.begin_session(1, Some(third_mouse()));
        until_engine_reads(&ringed, ARMED).await;

        let stop = Arc::new(AtomicBool::new(false));
        let clients = {
            let player = player.clone();
            let stop = Arc::clone(&stop);
            tokio::spawn(async move {
                let ids = Arc::new(AtomicU64::new(10));
                for route in [mouse(), other_mouse()].into_iter().cycle() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let player = player.clone();
                    let ids = Arc::clone(&ids);
                    tokio::spawn(async move {
                        let _ = player
                            .play_external(route, HapticWaveform::SubtleCollision, || {
                                ids.fetch_add(1, Ordering::SeqCst)
                            })
                            .await;
                    });
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
            })
        };
        tokio::time::sleep(Duration::from_millis(500)).await;
        player.end_session(1);
        tokio::time::sleep(ARM_BUDGET * 4).await;

        // Read while the plays still keep coming.
        let pulses = plays(&handles[1]).len() + plays(&handles[2]).len();
        let ring_engine = *ringed.lock().expect("config");
        stop.store(true, Ordering::SeqCst);
        clients.await.expect("the clients");
        assert!(
            pulses >= 2,
            "pulses kept playing between armings, but {pulses} played"
        );
        assert_eq!(
            ring_engine, DISARMED,
            "the closed ring's engine was handed back meanwhile"
        );
    })
    .await;
}

/// A hand-back ends the burst with every other session, so the next play
/// after the agent stays opens a new burst that arms again rather than
/// playing into the engine just handed back.
#[tokio::test]
async fn a_play_after_a_hand_back_arms_the_engine_again() {
    bounded(async {
        let config = Arc::new(StdMutex::new(DISARMED));
        let (player, handle) = player_for(Arc::clone(&config), BOTH_WAVEFORMS).await;
        player
            .play_external(mouse(), HapticWaveform::SubtleCollision, || 1)
            .await
            .expect("the scripted mouse plays");
        player.finish().await;
        assert_eq!(*config.lock().expect("config"), DISARMED, "handed back");

        player.resume();
        player
            .play_external(mouse(), HapticWaveform::SubtleCollision, || 2)
            .await
            .expect("the scripted mouse plays");
        assert_eq!(
            config_writes(&handle),
            vec![[0x83, 60], [0x82, 60], [0x83, 60]],
            "the play after the agent stayed armed the engine again"
        );
        after_the_burst(&config, DISARMED).await;
        assert_eq!(config_writes(&handle).len(), 4);
    })
    .await;
}

/// While the agent is leaving or handing over, nothing arms the engine, so
/// a client play is refused instead of reporting a buzz that may not have
/// been felt.
#[tokio::test]
async fn a_client_play_while_the_agent_is_leaving_is_refused() {
    bounded(async {
        let config = Arc::new(StdMutex::new(DISARMED));
        let (player, handle) = player_for(Arc::clone(&config), BOTH_WAVEFORMS).await;
        player.finish().await;
        assert!(matches!(
            player
                .play_external(mouse(), HapticWaveform::SubtleCollision, || 1)
                .await,
            Err(WriteError::AgentUnavailable)
        ));
        assert!(plays(&handle).is_empty(), "the refused play sent nothing");
        assert!(config_writes(&handle).is_empty(), "nor armed anything");
    })
    .await;
}

#[test]
fn only_an_online_device_with_measured_haptics_is_played_on() {
    let device = |online, capabilities: Option<Capabilities>| DeviceInventory {
        receiver: ReceiverInfo {
            name: "MX Master 4".into(),
            vendor_id: 0x046d,
            product_id: 0xb042,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: 0xff,
            codename: Some("MX Master 4".into()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online,
            battery: None,
            model_info: None,
            capabilities,
            settings_revision: 0,
            hidpp10: false,
        }],
    };
    let haptics = Some(Capabilities::from_feature_ids(&[0x19b0]));
    assert_eq!(
        DeviceRoute::for_slot(&device(true, None), 0xff),
        Some(mouse()),
        "the fixture addresses the route under test"
    );
    assert!(matches!(
        check_haptic_target(&[device(true, haptics)], &mouse()),
        Ok(())
    ));
    assert!(matches!(
        check_haptic_target(&[], &mouse()),
        Err(WriteError::DeviceNotFound)
    ));
    assert!(matches!(
        check_haptic_target(&[device(false, haptics)], &mouse()),
        Err(WriteError::DeviceUnreachable { .. })
    ));
    for capabilities in [None, Some(Capabilities::default())] {
        assert!(matches!(
            check_haptic_target(&[device(true, capabilities)], &mouse()),
            Err(WriteError::FeatureUnsupported {
                feature_hex: 0x19b0
            })
        ));
    }
}
