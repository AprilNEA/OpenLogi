use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use openlogi_core::binding::{ButtonId, Rotary};
use openlogi_core::hid::PairingError;
use openlogi_fixture::{
    CassetteExchange, FIXTURE_SCHEMA_VERSION, HidCassette, ReportSupport, RequestMatch,
};
use tokio::sync::{mpsc, oneshot, watch};

use super::{
    ChannelConnection, NodePresence, OpenOutcome, RawWriterAvailability, ReplayBackend,
    ReplayChannel, ReplayNode, ReplayResponseBarrier, ReplayTopology,
};
use crate::session::gesture::CaptureSpec;
use crate::{
    CaptureChannelSlot, CaptureHost, CaptureSessionFailure, CaptureSessionOutcome,
    CaptureSessionStop, CapturedInput, ChannelRegistry, DeviceIoGate, DeviceIoSignal, DeviceRoute,
    Enumerator, GamingWanted, KeyboardWanted, NodeId, NodeInfo, PairingCommand, PairingEvent,
    ReceiverSelector, SharedChannel, device_io_channel, reprog_controls, run_capture_session,
    run_keyboard_capture_session, run_pairing,
};

const CAPTURE_CHANNEL: &str = "capture-session";
const PAIRING_CHANNEL: &str = "bolt-pairing-session";
const DIRECT_PRODUCT_ID: u16 = 0xb35b;
const BOLT_PRODUCT_ID: u16 = 0xc548;
const REPROG_FEATURE_INDEX: u8 = 0x02;
const GESTURE_CID: u16 = reprog_controls::GESTURE_BUTTON_CID;
/// The Mute key (`0x1b04` control `0x00e7`).
const KEYBOARD_CID: u16 = 0x00e7;
const ORIGINAL_REMAP_CID: u16 = 0x0053;
/// A `getCidInfo` capability pair (bytes 4 and 8): a mouse control that is
/// reprogrammable and divertable, with raw XY.
const GESTURE_CONTROL_FLAGS: (u8, u8) = (0x31, 0x01);
/// A function-row control that is reprogrammable and divertable, no raw XY.
const KEYBOARD_CONTROL_FLAGS: (u8, u8) = (0x32, 0x00);
/// `setCidReporting` flags that arm gesture capture: divert and raw XY, both
/// marked valid.
const GESTURE_ARMED_FLAGS: u8 = 0x33;
/// `setCidReporting` flags that arm keyboard capture: divert marked valid, raw
/// XY untouched since keyboard controls do not support raw XY.
const KEYBOARD_ARMED_FLAGS: u8 = 0x03;
/// `setCidReporting` flags that hand a mouse control back: divert and raw XY both
/// marked valid and off.
const GESTURE_RESTORED_FLAGS: u8 = 0x22;
/// `setCidReporting` flags that hand a keyboard control back: divert marked valid and off.
const KEYBOARD_RESTORED_FLAGS: u8 = 0x02;

#[tokio::test]
async fn gesture_capture_replay_restores_original_reporting_on_normal_shutdown() {
    let replay = ArmedReplay::enumerate(capture_cassette(
        "gesture capture normal shutdown",
        GESTURE_CID,
        GESTURE_CONTROL_FLAGS,
        GESTURE_ARMED_FLAGS,
        GESTURE_RESTORED_FLAGS,
    ))
    .await;
    let (sink, _captured) = mpsc::unbounded_channel();
    let (shutdown, host) = replay.host(sink);
    let capture = run_capture_session(
        replay.route.clone(),
        CaptureSpec {
            divert_gesture_sources: vec![GESTURE_CID],
            ..CaptureSpec::default()
        },
        host,
    );

    let (outcome, ()) = tokio::join!(capture, replay.stop_after_arm(shutdown));

    replay.assert_restored(
        outcome,
        GESTURE_CID,
        GESTURE_ARMED_FLAGS,
        GESTURE_RESTORED_FLAGS,
    );
}

/// Keyboard capture arms its own way — `0x1b04` diversion on the wanted
/// controls only, without raw XY — and then runs the skeleton the gesture
/// session runs, so it has to publish its channel before it monitors and hand
/// the control back before it completes, exactly as that session does.
#[tokio::test]
async fn keyboard_capture_replay_restores_original_reporting_on_normal_shutdown() {
    let replay = ArmedReplay::enumerate(capture_cassette(
        "keyboard capture normal shutdown",
        KEYBOARD_CID,
        KEYBOARD_CONTROL_FLAGS,
        KEYBOARD_ARMED_FLAGS,
        KEYBOARD_RESTORED_FLAGS,
    ))
    .await;
    let (sink, _captured) = mpsc::unbounded_channel();
    let (shutdown, host) = replay.host(sink);
    let capture = run_keyboard_capture_session(
        replay.route.clone(),
        watch::channel(keyboard_keys(&[KEYBOARD_CID])).1,
        host,
    );

    let (outcome, ()) = tokio::join!(capture, replay.stop_after_arm(shutdown));

    replay.assert_restored(
        outcome,
        KEYBOARD_CID,
        KEYBOARD_ARMED_FLAGS,
        KEYBOARD_RESTORED_FLAGS,
    );
}

/// The MX Creative Dialpad's product ID: a direct route with it is the
/// dialpad its registry entry describes.
const DIALPAD_PRODUCT_ID: u16 = 0xbc00;

/// The `0x4610` feature index the dialpad replay places `MultiRoller` at.
const MULTI_ROLLER_FEATURE_INDEX: u8 = 0x03;

/// The `0x4610` request or echoed reply for function `function` with
/// `payload` on the dialpad replay's roller feature.
fn roller_report(function: u8, payload: [u8; 3]) -> Vec<u8> {
    hidpp20_short(0xff, MULTI_ROLLER_FEATURE_INDEX, function, payload)
}

/// The dialpad replay's request diverting the dial (roller 1).
fn divert_dial_request() -> Vec<u8> {
    roller_report(3, [1, 1, 0])
}

/// A dialpad replay whose session diverts the dial, then hands it back: the
/// strict cassette fails on any other request, so the unbound roller is
/// resolved but never written.
async fn dialpad_dial_replay(name: &str) -> ArmedReplay {
    let echoed = |function, payload| {
        hidpp20_exchange(
            roller_report(function, payload),
            roller_report(function, payload),
        )
    };
    ArmedReplay::enumerate_product(
        keyboard_cassette(
            name,
            &[KEYBOARD_CID],
            vec![
                root_feature_lookup_exchange(0x4610, MULTI_ROLLER_FEATURE_INDEX, 0),
                // Resolution of the dial (roller 1), then the roller (0).
                hidpp20_exchange(
                    roller_report(1, [1, 0, 0]),
                    roller_report(1, [0xb4, 0, 0x1f]),
                ),
                hidpp20_exchange(
                    roller_report(1, [0, 0, 0]),
                    roller_report(1, [0x28, 0, 0x1f]),
                ),
                // Divert the dial.
                echoed(3, [1, 1, 0]),
                root_feature_lookup_exchange(0x1d4b, 0, 0),
                // Hand the dial back.
                echoed(3, [1, 0, 0]),
            ],
        ),
        DIALPAD_PRODUCT_ID,
    )
    .await
}

/// A session wanting only the dial.
fn dial_wanted() -> watch::Receiver<KeyboardWanted> {
    watch::channel(KeyboardWanted {
        rotary: BTreeSet::from([Rotary::Dial]),
        ..KeyboardWanted::default()
    })
    .1
}

/// Turn `roller` by `delta` increments on the dialpad replay's device.
fn turn_roller(replay: &ArmedReplay, roller: u8, delta: u8) {
    let mut payload = [0; 16];
    payload[0] = roller;
    payload[1] = delta;
    replay
        .backend
        .emit_channel_report(
            CAPTURE_CHANNEL,
            &hidpp20_long(0xff, MULTI_ROLLER_FEATURE_INDEX, 0, payload),
        )
        .expect("the rotation reaches the session");
}

/// How long a dialpad replay may take before it counts as hung.
const DIALPAD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// A dialpad session diverts the roller a bound direction names, reports its
/// turns as rotations of that control — including a turn the dial sends
/// while its diversion's reply is still on the way — and hands it back to
/// native reporting before it completes. The strict cassette fails on any
/// other request: the unbound small roller is resolved but never written.
#[tokio::test]
async fn keyboard_capture_replay_diverts_and_hands_back_a_dialpad_roller() {
    let replay = dialpad_dial_replay("dialpad capture diverts the dial").await;
    let divert_reply = replay
        .backend
        .hold_next_response(
            CAPTURE_CHANNEL,
            RequestMatch::Hidpp20,
            &divert_dial_request(),
        )
        .expect("the dial's diversion reply can be held");
    let (sink, mut captured) = mpsc::unbounded_channel();
    let (shutdown, host) = replay.host(sink);
    let capture = run_keyboard_capture_session(replay.route.clone(), dial_wanted(), host);
    let turn_then_stop = async {
        // The firmware diverts the dial once it has the request, and the dial
        // can turn before its reply reaches the host: two increments
        // counter-clockwise, sent while arming still waits.
        divert_reply.request_written().await;
        turn_roller(&replay, 1, 0xfe);
        divert_reply.release();
        replay.wireless_lookup.request_written().await;
        // A turn of the undiverted roller, which must not be reported.
        turn_roller(&replay, 0, 0x01);
        let rotation = tokio::time::timeout(DIALPAD_DEADLINE, captured.recv())
            .await
            .expect("the session neither reported the dial's turn nor ended");
        // `None`: the session ended — its wake lookup is still held — without
        // ever reporting the turn.
        assert_eq!(
            rotation,
            Some(CapturedInput::Rotation {
                rotary: Rotary::Dial,
                increments: -2,
                per_revolution: 180,
            }),
            "the dial's two counter-clockwise increments, turned while its \
             diversion was being armed, were never reported"
        );
        replay.stop_after_arm(shutdown).await;
        captured
    };

    let (outcome, mut captured) = tokio::time::timeout(DIALPAD_DEADLINE, async {
        tokio::join!(capture, turn_then_stop)
    })
    .await
    .expect("the dialpad session neither reported the dial nor shut down");

    assert!(matches!(
        outcome.expect("capture session shuts down cleanly"),
        CaptureSessionOutcome::Restored
    ));
    assert!(
        captured.try_recv().is_err(),
        "the undiverted roller's turn was reported"
    );
    replay
        .backend
        .require_complete()
        .expect("the dialpad cassette is strictly consumed");
}

/// A diversion whose native write stalls past the HID++ response deadline
/// must finish before anything else is sent for that roller: a hand-back
/// that overtook it would be undone by the late diversion, leaving the
/// roller captured after teardown. The strict cassette fails on a hand-back
/// written before the diversion completed.
#[tokio::test]
async fn roller_restore_waits_for_stalled_take() {
    let replay = dialpad_dial_replay("dialpad restore waits for a stalled diversion").await;
    let stalled = replay
        .backend
        .hold_next_write_completion(
            CAPTURE_CHANNEL,
            RequestMatch::Hidpp20,
            &divert_dial_request(),
        )
        .expect("the dial's diversion write can be held");
    let (sink, _captured) = mpsc::unbounded_channel();
    let (shutdown, host) = replay.host(sink);
    let mut capture = std::pin::pin!(run_keyboard_capture_session(
        replay.route.clone(),
        dial_wanted(),
        host
    ));

    let stall = async {
        stalled.write_accepted().await;
        // Past the 5-second HID++ deadline: a bounded call gives up here and
        // arming rolls back with a hand-back while the diversion is pending.
        tokio::time::sleep(
            hidpp::channel::SEND_RESPONSE_TIMEOUT + std::time::Duration::from_secs(1),
        )
        .await;
    };
    // Bounded too: a session that neither writes the diversion nor ends
    // must fail here, not hang.
    tokio::time::timeout(
        hidpp::channel::SEND_RESPONSE_TIMEOUT + DIALPAD_DEADLINE,
        async {
            tokio::select! {
                outcome = &mut capture => panic!(
                    "the session ended while its diversion was still on the wire: {:?}",
                    outcome.map(|_| ()).map_err(|_| "arming failed")
                ),
                () = stall => {}
            }
        },
    )
    .await
    .expect("the session never wrote the dial's diversion");
    stalled.release();
    let (outcome, ()) = tokio::time::timeout(DIALPAD_DEADLINE, async {
        tokio::join!(capture, replay.stop_after_arm(shutdown))
    })
    .await
    .expect("the session arms and shuts down once the diversion completes");

    assert!(matches!(
        outcome.expect("capture session shuts down cleanly"),
        CaptureSessionOutcome::Restored
    ));
    replay
        .backend
        .require_complete()
        .expect("the dialpad cassette is strictly consumed, in order");
}

/// The `0x8010` feature index the G-key replay places `GamingGKeys` at.
const G_KEYS_FEATURE_INDEX: u8 = 0x03;

/// Taking a G-series keyboard's G-key row is a whole-row write, owed back
/// on teardown like a diverted control: the session reads the row's length,
/// takes it, and hands it back before it completes. The strict cassette
/// fails on any other request — a session asked for nothing of the gaming
/// cluster never looks it up (the other keyboard replays prove that).
#[tokio::test]
async fn keyboard_capture_replay_takes_and_hands_back_the_g_key_row() {
    let replay = ArmedReplay::enumerate(keyboard_cassette(
        "keyboard capture takes the G-key row",
        &[KEYBOARD_CID],
        vec![
            root_feature_lookup_exchange(0x8010, G_KEYS_FEATURE_INDEX, 0),
            root_feature_lookup_exchange(0x8020, 0, 0),
            root_feature_lookup_exchange(0x8030, 0, 0),
            hidpp20_exchange(
                hidpp20_short(0xff, G_KEYS_FEATURE_INDEX, 0, [0, 0, 0]),
                hidpp20_short(0xff, G_KEYS_FEATURE_INDEX, 0, [5, 0, 0]),
            ),
            hidpp20_exchange(
                hidpp20_short(0xff, G_KEYS_FEATURE_INDEX, 2, [1, 0, 0]),
                hidpp20_short(0xff, G_KEYS_FEATURE_INDEX, 2, [0, 0, 0]),
            ),
            root_feature_lookup_exchange(0x1d4b, 0, 0),
            hidpp20_exchange(
                hidpp20_short(0xff, G_KEYS_FEATURE_INDEX, 2, [0, 0, 0]),
                hidpp20_short(0xff, G_KEYS_FEATURE_INDEX, 2, [0, 0, 0]),
            ),
        ],
    ))
    .await;
    let (sink, _captured) = mpsc::unbounded_channel();
    let (shutdown, host) = replay.host(sink);
    let capture = run_keyboard_capture_session(
        replay.route.clone(),
        watch::channel(KeyboardWanted {
            gaming: GamingWanted {
                take_g_row: true,
                mode_keys: std::collections::BTreeSet::new(),
            },
            ..KeyboardWanted::default()
        })
        .1,
        host,
    );

    let (outcome, ()) = tokio::join!(capture, replay.stop_after_arm(shutdown));

    assert!(matches!(
        outcome.expect("capture session shuts down cleanly"),
        CaptureSessionOutcome::Restored
    ));
    replay
        .backend
        .require_complete()
        .expect("the G-key cassette is strictly consumed");
}

/// A G-key row whose release write fails is still owed: the firmware may
/// hold the row. The failed change ends the session, and its teardown writes
/// the release again instead of forgetting the row.
#[tokio::test]
async fn keyboard_capture_replay_hands_back_a_g_key_row_whose_release_failed() {
    let release = hidpp20_short(0xff, G_KEYS_FEATURE_INDEX, 2, [0, 0, 0]);
    let replay = ArmedReplay::enumerate(keyboard_cassette(
        "keyboard capture retries a failed G-key row release",
        &[KEYBOARD_CID],
        vec![
            root_feature_lookup_exchange(0x8010, G_KEYS_FEATURE_INDEX, 0),
            root_feature_lookup_exchange(0x8020, 0, 0),
            root_feature_lookup_exchange(0x8030, 0, 0),
            hidpp20_exchange(
                hidpp20_short(0xff, G_KEYS_FEATURE_INDEX, 0, [0, 0, 0]),
                hidpp20_short(0xff, G_KEYS_FEATURE_INDEX, 0, [5, 0, 0]),
            ),
            hidpp20_exchange(
                hidpp20_short(0xff, G_KEYS_FEATURE_INDEX, 2, [1, 0, 0]),
                hidpp20_short(0xff, G_KEYS_FEATURE_INDEX, 2, [0, 0, 0]),
            ),
            root_feature_lookup_exchange(0x1d4b, 0, 0),
            hidpp20_exchange(
                release.clone(),
                crate::channel::scripted::feature_error(&release, 0x08),
            ),
            hidpp20_exchange(release.clone(), release),
        ],
    ))
    .await;
    let g_row = |take_g_row| KeyboardWanted {
        gaming: GamingWanted {
            take_g_row,
            mode_keys: std::collections::BTreeSet::new(),
        },
        ..KeyboardWanted::default()
    };
    let (wanted_tx, wanted) = watch::channel(g_row(true));
    let (sink, _captured) = mpsc::unbounded_channel();
    let (_shutdown, host) = replay.host(sink);
    let capture = run_keyboard_capture_session(replay.route.clone(), wanted, host);
    let hand_back = async {
        replay.wireless_lookup.request_written().await;
        wanted_tx.send_replace(g_row(false));
        replay.wireless_lookup.release();
    };

    // Bounds the hand-back too: a session that ends before its wake lookup
    // would otherwise leave it waiting forever.
    let (outcome, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(capture, hand_back)
    })
    .await
    .expect("a failed release ends the session without a shutdown request");

    assert!(matches!(
        outcome.expect("the session's teardown hands the row back"),
        CaptureSessionOutcome::Restored
    ));
    replay
        .backend
        .require_complete()
        .expect("the G-key cassette is strictly consumed");
}

/// A per-app binding change retargets a running keyboard session in place
/// (#2140). The session hands back only the key that left the set and
/// diverts only the key that joined it. It does not read the control table
/// again or write the key that stays, and teardown hands back exactly the
/// keys diverted at that moment. The strict cassette fails on any other
/// request.
#[tokio::test]
async fn keyboard_capture_replay_changes_the_diverted_keys_in_place() {
    const STAYS: u16 = KEYBOARD_CID;
    const LEAVES: u16 = 0x00e8;
    const JOINS: u16 = 0x00e9;
    let replay = ArmedReplay::enumerate(keyboard_cassette(
        "keyboard capture changes its keys in place",
        &[STAYS, LEAVES, JOINS],
        vec![
            reprog_reporting_state_exchange(STAYS),
            reprog_reporting_change_exchange(STAYS, KEYBOARD_ARMED_FLAGS),
            reprog_reporting_state_exchange(LEAVES),
            reprog_reporting_change_exchange(LEAVES, KEYBOARD_ARMED_FLAGS),
            root_feature_lookup_exchange(0x1d4b, 0, 0),
            reprog_reporting_change_exchange(LEAVES, KEYBOARD_RESTORED_FLAGS),
            reprog_reporting_state_exchange(JOINS),
            reprog_reporting_change_exchange(JOINS, KEYBOARD_ARMED_FLAGS),
            reprog_reporting_change_exchange(STAYS, KEYBOARD_RESTORED_FLAGS),
            reprog_reporting_change_exchange(JOINS, KEYBOARD_RESTORED_FLAGS),
        ],
    ))
    .await;
    let outcome = replay
        .retarget_keyboard_then_stop(&[STAYS, LEAVES], &[STAYS, JOINS], JOINS)
        .await;
    assert!(matches!(
        outcome.expect("keyboard capture shuts down cleanly"),
        CaptureSessionOutcome::Restored
    ));
    let completion = replay
        .backend
        .channel_completion(CAPTURE_CHANNEL)
        .expect("known capture channel");
    assert_eq!(
        reporting_writes(&completion.written_reports),
        [
            (STAYS, KEYBOARD_ARMED_FLAGS),
            (LEAVES, KEYBOARD_ARMED_FLAGS),
            (LEAVES, KEYBOARD_RESTORED_FLAGS),
            (JOINS, KEYBOARD_ARMED_FLAGS),
            (STAYS, KEYBOARD_RESTORED_FLAGS),
            (JOINS, KEYBOARD_RESTORED_FLAGS),
        ]
        .map(|(cid, flags)| reprog_reporting_change_payload(cid, flags).to_vec())
    );
    replay
        .backend
        .require_complete()
        .expect("keyboard cassette is strictly consumed");
}

/// A keyboard that binds keys only for some apps holds its session with no
/// key diverted, and diverts a key in place when the app in front binds it.
#[tokio::test]
async fn keyboard_capture_replay_diverts_a_key_into_an_empty_session() {
    let replay = ArmedReplay::enumerate(keyboard_cassette(
        "keyboard capture starts with no key diverted",
        &[KEYBOARD_CID],
        vec![
            root_feature_lookup_exchange(0x1d4b, 0, 0),
            reprog_reporting_state_exchange(KEYBOARD_CID),
            reprog_reporting_change_exchange(KEYBOARD_CID, KEYBOARD_ARMED_FLAGS),
            reprog_reporting_change_exchange(KEYBOARD_CID, KEYBOARD_RESTORED_FLAGS),
        ],
    ))
    .await;
    let outcome = replay
        .retarget_keyboard_then_stop(&[], &[KEYBOARD_CID], KEYBOARD_CID)
        .await;
    replay.assert_restored(
        outcome,
        KEYBOARD_CID,
        KEYBOARD_ARMED_FLAGS,
        KEYBOARD_RESTORED_FLAGS,
    );
}

/// A failed feature lookup is not evidence that wake reports are unsupported.
/// End the epoch and restore its controls so the supervisor can retry instead
/// of leaving a live session that cannot recover after the device's next nap.
#[tokio::test]
async fn capture_replay_failed_wake_discovery_restores_before_retry() {
    for keyboard in [false, true] {
        for timeout in [false, true] {
            let (cid, control_flags, armed_flags, restored_flags) = if keyboard {
                (
                    KEYBOARD_CID,
                    KEYBOARD_CONTROL_FLAGS,
                    KEYBOARD_ARMED_FLAGS,
                    KEYBOARD_RESTORED_FLAGS,
                )
            } else {
                (
                    GESTURE_CID,
                    GESTURE_CONTROL_FLAGS,
                    GESTURE_ARMED_FLAGS,
                    GESTURE_RESTORED_FLAGS,
                )
            };
            let mut cassette = capture_cassette(
                "failed capture wake discovery",
                cid,
                control_flags,
                armed_flags,
                restored_flags,
            );
            let lookup = cassette
                .exchanges
                .iter_mut()
                .find(|exchange| exchange.request == root_feature_lookup_request(0x1d4b))
                .expect("capture probes the wake feature");
            lookup.response = if timeout {
                None
            } else {
                Some(crate::channel::scripted::feature_error(
                    &lookup.request,
                    0x09,
                ))
            };
            let replay = ArmedReplay::enumerate(cassette).await;
            let (sink, _captured) = mpsc::unbounded_channel();
            let (_shutdown, host) = replay.host(sink);
            replay.wireless_lookup.release();
            let capture = async {
                if keyboard {
                    run_keyboard_capture_session(
                        replay.route.clone(),
                        watch::channel(keyboard_keys(&[cid])).1,
                        host,
                    )
                    .await
                } else {
                    run_capture_session(
                        replay.route.clone(),
                        CaptureSpec {
                            divert_gesture_sources: vec![cid],
                            ..CaptureSpec::default()
                        },
                        host,
                    )
                    .await
                }
            };
            let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), capture)
                .await
                .expect("failed wake discovery must end capture without a shutdown request");
            replay.assert_restored(outcome, cid, armed_flags, restored_flags);
        }
    }
}

#[tokio::test]
async fn capture_replay_without_wake_feature_stays_active_until_shutdown() {
    let replay = ArmedReplay::enumerate(capture_cassette(
        "capture without a wake feature",
        GESTURE_CID,
        GESTURE_CONTROL_FLAGS,
        GESTURE_ARMED_FLAGS,
        GESTURE_RESTORED_FLAGS,
    ))
    .await;
    let (sink, _captured) = mpsc::unbounded_channel();
    let (shutdown, host) = replay.host(sink);
    replay.wireless_lookup.release();
    let capture = run_capture_session(
        replay.route.clone(),
        CaptureSpec {
            divert_gesture_sources: vec![GESTURE_CID],
            ..CaptureSpec::default()
        },
        host,
    );
    tokio::pin!(capture);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut capture)
            .await
            .is_err(),
        "a successful unsupported-feature response must not cause a restart loop"
    );
    assert!(replay.wireless_lookup.is_request_written());
    shutdown
        .send(CaptureSessionStop::Shutdown)
        .expect("capture remains active");
    replay.assert_restored(
        capture.await,
        GESTURE_CID,
        GESTURE_ARMED_FLAGS,
        GESTURE_RESTORED_FLAGS,
    );
}

/// One replayed direct device, enumerated the production way, whose `0x1d4b`
/// lookup is held so a capture session can be observed between arming and
/// monitoring.
struct ArmedReplay {
    backend: Arc<ReplayBackend>,
    registry: ChannelRegistry,
    node_id: NodeId,
    route: DeviceRoute,
    original_publication: SharedChannel,
    channel_slot: CaptureChannelSlot,
    wireless_lookup: ReplayResponseBarrier,
    /// Held so the gate stays open for the session's lifetime.
    _io_signal: DeviceIoSignal,
    io_gate: DeviceIoGate,
}

impl ArmedReplay {
    async fn enumerate(cassette: HidCassette) -> Self {
        Self::enumerate_product(cassette, DIRECT_PRODUCT_ID).await
    }

    /// [`Self::enumerate`] for a direct device with `product_id`, so the
    /// route names a known product.
    async fn enumerate_product(cassette: HidCassette, product_id: u16) -> Self {
        let node_id = NodeId::from("capture-node".to_string());
        let route = DeviceRoute::Direct {
            vendor_id: crate::LOGITECH_VENDOR_ID,
            product_id,
        };
        let backend = Arc::new(
            ReplayBackend::new(
                replay_topology(
                    replay_node(node_id.clone(), product_id, "Capture Device"),
                    CAPTURE_CHANNEL,
                ),
                vec![cassette],
            )
            .expect("capture replay topology is valid"),
        );
        let registry = ChannelRegistry::default();
        let mut enumerator =
            Enumerator::with_backend(backend.clone()).with_registry(registry.clone());

        let inventory = enumerator
            .enumerate()
            .await
            .expect("production feature probe succeeds");
        assert_eq!(inventory.len(), 1);
        let original_publication = registry
            .lookup(&route)
            .expect("the enumerator publishes the direct route");
        assert!(registry.is_current(&original_publication));
        assert_eq!(
            backend
                .channel_lifetime_count(CAPTURE_CHANNEL)
                .expect("known capture channel"),
            1
        );

        let wireless_lookup = backend
            .hold_next_response(
                CAPTURE_CHANNEL,
                RequestMatch::Hidpp20,
                &root_feature_lookup_request(0x1d4b),
            )
            .expect("wireless feature lookup can be held");
        let (io_signal, io_gate) = device_io_channel();
        Self {
            backend,
            registry,
            node_id,
            route,
            original_publication,
            channel_slot: Arc::new(RwLock::new(None)),
            wireless_lookup,
            _io_signal: io_signal,
            io_gate,
        }
    }

    /// The host handles one session runs against, and the sender that shuts
    /// it down.
    fn host(
        &self,
        sink: mpsc::UnboundedSender<CapturedInput>,
    ) -> (oneshot::Sender<CaptureSessionStop>, CaptureHost<'_>) {
        let (shutdown, shutdown_rx) = oneshot::channel();
        let host = CaptureHost {
            sink,
            shutdown: shutdown_rx,
            channel_slot: Arc::clone(&self.channel_slot),
            registry: &self.registry,
            device_io: self.io_gate.clone(),
        };
        (shutdown, host)
    }

    /// Once the session has armed and asked for `0x1d4b`, check that it has
    /// published its channel, then shut it down and let the lookup answer.
    async fn stop_after_arm(&self, shutdown: oneshot::Sender<CaptureSessionStop>) {
        self.wireless_lookup.request_written().await;
        let published = self
            .channel_slot
            .read()
            .expect("capture channel slot is readable")
            .clone()
            .expect("capture channel is published before the wireless lookup");
        assert!(self.registry.is_current(&published));
        shutdown
            .send(CaptureSessionStop::Shutdown)
            .expect("capture session still owns its shutdown receiver");
        self.wireless_lookup.release();
    }

    /// Run a keyboard session over the keys `from`, ask for the keys `to`
    /// once it monitors, and shut it down while the change's last write —
    /// diverting `last_diverted` — waits for its response, so shutdown can
    /// only land after the change.
    async fn retarget_keyboard_then_stop(
        &self,
        from: &[u16],
        to: &[u16],
        last_diverted: u16,
    ) -> Result<CaptureSessionOutcome, CaptureSessionFailure> {
        let (wanted_tx, wanted) = watch::channel(keyboard_keys(from));
        let (sink, _captured) = mpsc::unbounded_channel();
        let (shutdown, host) = self.host(sink);
        let last_write = self
            .backend
            .hold_next_response(
                CAPTURE_CHANNEL,
                RequestMatch::Hidpp20,
                &reprog_reporting_change_request(last_diverted, KEYBOARD_ARMED_FLAGS),
            )
            .expect("the change's last write can be held");
        let capture = run_keyboard_capture_session(self.route.clone(), wanted, host);
        let change_then_stop = async {
            self.wireless_lookup.request_written().await;
            wanted_tx.send_replace(keyboard_keys(to));
            self.wireless_lookup.release();
            // A session that ends instead of changing never writes it.
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                last_write.request_written(),
            )
            .await
            .expect("the change diverts its last key");
            shutdown
                .send(CaptureSessionStop::Shutdown)
                .expect("capture session still owns its shutdown receiver");
            last_write.release();
        };
        let (outcome, ()) = tokio::join!(capture, change_then_stop);
        outcome
    }

    /// A normal shutdown restored `cid`'s reporting through the channel the
    /// enumerator owns, cleared the slot, and consumed the whole cassette.
    fn assert_restored(
        &self,
        outcome: Result<CaptureSessionOutcome, CaptureSessionFailure>,
        cid: u16,
        armed_flags: u8,
        restored_flags: u8,
    ) {
        assert!(matches!(
            outcome.expect("capture session shuts down cleanly"),
            CaptureSessionOutcome::Restored
        ));
        assert!(
            self.channel_slot
                .read()
                .expect("capture channel slot is readable")
                .is_none(),
            "normal shutdown must clear the captured channel slot"
        );
        assert!(
            self.registry.is_current(&self.original_publication),
            "normal shutdown must restore through and retain the original publication"
        );
        assert_eq!(
            self.backend
                .open_count(&self.node_id)
                .expect("known capture node"),
            1
        );
        assert_eq!(
            self.backend
                .channel_lifetime_count(CAPTURE_CHANNEL)
                .expect("known capture channel"),
            1,
            "capture must reuse the enumerator-owned channel"
        );

        let completion = self
            .backend
            .channel_completion(CAPTURE_CHANNEL)
            .expect("known capture channel");
        let reporting_writes: Vec<_> = completion
            .written_reports
            .iter()
            .filter(|report| {
                report[0] == 0x11 && report[2] == REPROG_FEATURE_INDEX && report[3] >> 4 == 3
            })
            .collect();
        assert_eq!(reporting_writes.len(), 2);
        assert_eq!(
            &reporting_writes[0][4..],
            &reprog_reporting_change_payload(cid, armed_flags),
            "arming sets only the diversion this capture needs while preserving the original remap"
        );
        assert_eq!(
            &reporting_writes[1][4..],
            &reprog_reporting_change_payload(cid, restored_flags),
            "shutdown clears only diversion while preserving the original remap"
        );
        assert_eq!(completion.channel_open_count, 1);
        self.backend
            .require_complete()
            .expect("capture cassette is strictly consumed");
    }
}

#[tokio::test]
async fn bolt_pairing_replay_cancels_discovery_and_restores_notifications() {
    let node_id = NodeId::from("bolt-pairing-node".to_string());
    let backend = ReplayBackend::new(
        replay_topology(bolt_pairing_node(node_id.clone()), PAIRING_CHANNEL),
        vec![bolt_pairing_cancel_cassette()],
    )
    .expect("pairing replay topology is valid");
    let (command_tx, commands) = mpsc::unbounded_channel();
    let (event_tx, mut events) = mpsc::unbounded_channel();

    let pairing = run_pairing(&backend, ReceiverSelector::First, commands, event_tx);
    let cancel_after_searching = async {
        let searching = events
            .recv()
            .await
            .expect("pairing emits its searching phase");
        assert!(matches!(searching, PairingEvent::Searching));
        command_tx
            .send(PairingCommand::Cancel)
            .expect("the searching session accepts cancellation");

        let terminal = events.recv().await.expect("pairing emits a terminal event");
        assert!(matches!(
            terminal,
            PairingEvent::Failed(PairingError::Cancelled)
        ));
        assert!(
            events.recv().await.is_none(),
            "searching and failed must be the complete event sequence"
        );
    };

    let (result, ()) = tokio::join!(pairing, cancel_after_searching);
    assert!(matches!(result, Err(PairingError::Cancelled)));
    assert_eq!(backend.open_count(&node_id).expect("known pairing node"), 1);

    let completion = backend
        .channel_completion(PAIRING_CHANNEL)
        .expect("known pairing channel");
    assert_eq!(
        completion.written_reports,
        vec![
            receiver_notification_flags_read(),
            receiver_notification_flags_write([0x00, 0x09, 0x00]),
            bolt_discovery_write([30, 0x01, 0x00]),
            bolt_discovery_write([30, 0x02, 0x00]),
            receiver_notification_flags_write([0x00, 0x01, 0x00]),
        ],
        "cancel while searching must stop Bolt discovery before restoring the notification \
         flags found before pairing (inventory's WIRELESS stays on)"
    );
    assert_eq!(completion.channel_open_count, 1);
    assert_eq!(
        backend
            .channel_lifetime_count(PAIRING_CHANNEL)
            .expect("known pairing channel"),
        0,
        "the pairing receiver channel closes when the session returns"
    );
    backend
        .require_complete()
        .expect("pairing cassette is strictly consumed");
}

fn bolt_pairing_node(id: NodeId) -> ReplayNode {
    replay_node(id, BOLT_PRODUCT_ID, "Logi Bolt Receiver")
}

fn replay_node(id: NodeId, product_id: u16, name: &str) -> ReplayNode {
    ReplayNode {
        info: NodeInfo {
            id,
            vendor_id: crate::LOGITECH_VENDOR_ID,
            product_id,
            usage_page: 0xff00,
            usage_id: 0x0002,
            name: name.to_string(),
            manufacturer: Some("Logitech".to_string()),
            serial_number: None,
        },
        presence: NodePresence::Present,
        open_outcome: OpenOutcome::Hidpp,
        channel: Some(if product_id == BOLT_PRODUCT_ID {
            PAIRING_CHANNEL.to_string()
        } else {
            CAPTURE_CHANNEL.to_string()
        }),
        raw_writer: RawWriterAvailability::Unavailable,
        receiver_slots: Vec::new(),
    }
}

fn replay_topology(node: ReplayNode, channel: &str) -> ReplayTopology {
    ReplayTopology {
        nodes: vec![node],
        channels: vec![ReplayChannel {
            id: channel.to_string(),
            connection: ChannelConnection::Connected,
            report_support: ReportSupport::ShortAndLong,
        }],
    }
}

/// The exchanges one capture session over `cid` costs, from the production
/// probe through arming, the held `0x1d4b` lookup, and the restore on shutdown.
fn capture_cassette(
    name: &str,
    cid: u16,
    control_flags: (u8, u8),
    armed_flags: u8,
    restored_flags: u8,
) -> HidCassette {
    let session = [
        reprog_reporting_state_exchange(cid),
        reprog_reporting_change_exchange(cid, armed_flags),
        root_feature_lookup_exchange(0x1d4b, 0, 0),
        reprog_reporting_change_exchange(cid, restored_flags),
    ];
    cassette(
        name,
        CAPTURE_CHANNEL,
        capture_prelude(&[cid], control_flags)
            .into_iter()
            .chain(session)
            .collect(),
    )
}

/// The exchanges of a keyboard session over the control table `table`: the
/// shared prelude, then `session` from the first reporting read on.
fn keyboard_cassette(name: &str, table: &[u16], session: Vec<CassetteExchange>) -> HidCassette {
    cassette(
        name,
        CAPTURE_CHANNEL,
        capture_prelude(table, KEYBOARD_CONTROL_FLAGS)
            .into_iter()
            .chain(session)
            .collect(),
    )
}

/// The exchanges before a capture session's first reporting read: the
/// production probe, then the session's own open and control-table read.
fn capture_prelude(table: &[u16], control_flags: (u8, u8)) -> Vec<CassetteExchange> {
    let count = u8::try_from(table.len()).expect("a test control table fits a count byte");
    let control_table = || {
        (0u8..)
            .zip(table)
            .map(|(index, &cid)| reprog_control_info_exchange(index, cid, control_flags))
    };
    [
        root_ping_exchange(),
        root_feature_lookup_exchange(0x0001, 0x01, 0),
        feature_set_count_exchange(2),
        feature_set_entry_exchange(1, 0x0001),
        feature_set_entry_exchange(2, reprog_controls::FEATURE_ID),
        reprog_control_count_exchange(count),
    ]
    .into_iter()
    .chain(control_table())
    .chain([
        root_ping_exchange(),
        root_feature_lookup_exchange(reprog_controls::FEATURE_ID, REPROG_FEATURE_INDEX, 4),
        reprog_control_count_exchange(count),
    ])
    .chain(control_table())
    .collect()
}

/// `0x1b04` controls that dispatch as their own control buttons.
fn keyboard_keys(cids: &[u16]) -> KeyboardWanted {
    KeyboardWanted {
        controls: cids
            .iter()
            .map(|&cid| (cid, ButtonId::control(cid)))
            .collect(),
        grid: BTreeMap::new(),
        gaming: GamingWanted::default(),
        rotary: BTreeSet::new(),
    }
}

/// The payload of every `setCidReporting` write in `reports`, in order.
fn reporting_writes(reports: &[Vec<u8>]) -> Vec<Vec<u8>> {
    reports
        .iter()
        .filter(|report| {
            report[0] == 0x11 && report[2] == REPROG_FEATURE_INDEX && report[3] >> 4 == 3
        })
        .map(|report| report[4..].to_vec())
        .collect()
}

fn bolt_pairing_cancel_cassette() -> HidCassette {
    // Inventory keeps WIRELESS on; pairing adds SOFTWARE_PRESENT only.
    let mut flags_read = receiver_notification_flags_read();
    flags_read[5] = 0x01;
    cassette(
        "Bolt discovery cancellation",
        PAIRING_CHANNEL,
        std::iter::once(CassetteExchange {
            request_match: RequestMatch::Exact,
            request: receiver_notification_flags_read(),
            response: Some(flags_read),
            required: true,
        })
        .chain(
            [
                receiver_notification_flags_write([0x00, 0x09, 0x00]),
                bolt_discovery_write([30, 0x01, 0x00]),
                bolt_discovery_write([30, 0x02, 0x00]),
                receiver_notification_flags_write([0x00, 0x01, 0x00]),
            ]
            .into_iter()
            .map(exact_echo_exchange),
        )
        .collect(),
    )
}

fn cassette(name: &str, channel: &str, exchanges: Vec<CassetteExchange>) -> HidCassette {
    HidCassette {
        schema_version: FIXTURE_SCHEMA_VERSION,
        name: name.to_string(),
        channel: channel.to_string(),
        report_support: ReportSupport::ShortAndLong,
        exchanges,
    }
}

fn root_ping_exchange() -> CassetteExchange {
    hidpp20_exchange(
        hidpp20_short(0xff, 0x00, 1, [0, 0, 0]),
        hidpp20_short(0xff, 0x00, 1, [4, 0, 0]),
    )
}

fn root_feature_lookup_request(feature_id: u16) -> Vec<u8> {
    let [high, low] = feature_id.to_be_bytes();
    hidpp20_short(0xff, 0x00, 0, [high, low, 0])
}

fn root_feature_lookup_exchange(
    feature_id: u16,
    feature_index: u8,
    version: u8,
) -> CassetteExchange {
    hidpp20_exchange(
        root_feature_lookup_request(feature_id),
        hidpp20_short(0xff, 0x00, 0, [feature_index, 0, version]),
    )
}

fn feature_set_count_exchange(count: u8) -> CassetteExchange {
    hidpp20_exchange(
        hidpp20_short(0xff, 0x01, 0, [0, 0, 0]),
        hidpp20_short(0xff, 0x01, 0, [count, 0, 0]),
    )
}

fn feature_set_entry_exchange(index: u8, feature_id: u16) -> CassetteExchange {
    let [high, low] = feature_id.to_be_bytes();
    hidpp20_exchange(
        hidpp20_short(0xff, 0x01, 1, [index, 0, 0]),
        hidpp20_short(0xff, 0x01, 1, [high, low, 0]),
    )
}

fn reprog_control_count_exchange(count: u8) -> CassetteExchange {
    hidpp20_exchange(
        hidpp20_short(0xff, REPROG_FEATURE_INDEX, 0, [0, 0, 0]),
        hidpp20_short(0xff, REPROG_FEATURE_INDEX, 0, [count, 0, 0]),
    )
}

fn reprog_control_info_exchange(
    index: u8,
    cid: u16,
    (flags, flags_high): (u8, u8),
) -> CassetteExchange {
    let mut request = [0u8; 16];
    request[0] = index;
    let mut response = [0u8; 16];
    response[0..2].copy_from_slice(&cid.to_be_bytes());
    response[2..4].copy_from_slice(&0x009cu16.to_be_bytes());
    response[4] = flags;
    response[8] = flags_high;
    hidpp20_exchange(
        hidpp20_long(0xff, REPROG_FEATURE_INDEX, 1, request),
        hidpp20_long(0xff, REPROG_FEATURE_INDEX, 1, response),
    )
}

fn reprog_reporting_state_exchange(cid: u16) -> CassetteExchange {
    let [cid_high, cid_low] = cid.to_be_bytes();
    let mut response = [0u8; 16];
    response[0..2].copy_from_slice(&cid.to_be_bytes());
    response[2] = 0x44;
    response[3..5].copy_from_slice(&ORIGINAL_REMAP_CID.to_be_bytes());
    response[5] = 0x05;
    hidpp20_exchange(
        hidpp20_short(0xff, REPROG_FEATURE_INDEX, 2, [cid_high, cid_low, 0]),
        hidpp20_long(0xff, REPROG_FEATURE_INDEX, 2, response),
    )
}

fn reprog_reporting_change_exchange(cid: u16, flags: u8) -> CassetteExchange {
    let report = reprog_reporting_change_request(cid, flags);
    hidpp20_exchange(report.clone(), report)
}

fn reprog_reporting_change_request(cid: u16, flags: u8) -> Vec<u8> {
    hidpp20_long(
        0xff,
        REPROG_FEATURE_INDEX,
        3,
        reprog_reporting_change_payload(cid, flags),
    )
}

fn reprog_reporting_change_payload(cid: u16, flags: u8) -> [u8; 16] {
    let mut payload = [0u8; 16];
    payload[0..2].copy_from_slice(&cid.to_be_bytes());
    payload[2] = flags;
    payload[3..5].copy_from_slice(&ORIGINAL_REMAP_CID.to_be_bytes());
    payload
}

fn receiver_notification_flags_write(flags: [u8; 3]) -> Vec<u8> {
    hidpp10_register_write(0x00, flags)
}

fn receiver_notification_flags_read() -> Vec<u8> {
    vec![0x10, 0xff, 0x81, 0x00, 0x00, 0x00, 0x00]
}

fn bolt_discovery_write(payload: [u8; 3]) -> Vec<u8> {
    hidpp10_register_write(0xc0, payload)
}

fn hidpp10_register_write(address: u8, payload: [u8; 3]) -> Vec<u8> {
    vec![
        0x10, 0xff, 0x80, address, payload[0], payload[1], payload[2],
    ]
}

fn exact_echo_exchange(report: Vec<u8>) -> CassetteExchange {
    CassetteExchange {
        request_match: RequestMatch::Exact,
        request: report.clone(),
        response: Some(report),
        required: true,
    }
}

fn hidpp20_exchange(request: Vec<u8>, response: Vec<u8>) -> CassetteExchange {
    CassetteExchange {
        request_match: RequestMatch::Hidpp20,
        request,
        response: Some(response),
        required: true,
    }
}

fn hidpp20_short(device_index: u8, feature_index: u8, function: u8, payload: [u8; 3]) -> Vec<u8> {
    vec![
        0x10,
        device_index,
        feature_index,
        function << 4,
        payload[0],
        payload[1],
        payload[2],
    ]
}

fn hidpp20_long(device_index: u8, feature_index: u8, function: u8, payload: [u8; 16]) -> Vec<u8> {
    let mut report = vec![0x11, device_index, feature_index, function << 4];
    report.extend_from_slice(&payload);
    report
}
