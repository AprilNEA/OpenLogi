//! Haptics a client plays through [`RingHapticPlayer::play_external`]: the
//! burst arms a disarmed engine once, plays, and hands the engine back once
//! the plays stop.

use std::future::Future;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use openlogi_core::device::{
    Capabilities, DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo,
};
use openlogi_device::test_support::{ScriptedRawHidHandle, publish_scripted_channel};
use openlogi_hid::HapticWaveform;

use super::super::{BURST_IDLE, RingHapticPlayer, check_haptic_target};
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
    let observable = Arc::new(ObservableState::new("test".to_owned()));
    let shared = Orchestrator::new(openlogi_core::config::Config::default(), observable).shared();
    let (_, handle) = publish_scripted_channel(
        &shared.channel_registry,
        "mouse",
        mouse(),
        haptic_device(config, waveforms),
    )
    .await;
    (RingHapticPlayer::spawn(shared), handle)
}

/// Let the burst go idle, then wait until the engine reads `expected`.
///
/// Real time, not a paused clock: the scripted device answers on a thread,
/// so a paused tokio clock jumps past the worker's play and restore budgets
/// while it waits for those answers, and the tests flake under load.
async fn after_the_burst(config: &StdMutex<[u8; 3]>, expected: [u8; 3]) {
    tokio::time::sleep(BURST_IDLE + Duration::from_millis(300)).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while *config.lock().expect("config") != expected && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
