//! A thumb-only change keeps the running listener and the unrelated gesture
//! button, including while firmware has accepted but not acknowledged a write.

use std::sync::RwLock;
use std::time::Duration;

use openlogi_fixture::RequestMatch;
use tokio::sync::watch;

use super::*;
use crate::channel::scripted::feature_error;
use crate::device_io_channel;
use crate::replay::{ReplayChannelHandle, ReplayResponseBarrier};

#[derive(Clone, Copy, Debug)]
enum Wheel {
    Ordinary,
    Gesture46,
}

impl Wheel {
    fn write(self, captured: bool, direction: u8) -> Vec<u8> {
        match self {
            Self::Ordinary => report(3, 0x20, &[u8::from(captured), direction]),
            Self::Gesture46 => report(3, 0x40, &[0, 1, 1, u8::from(captured)]),
        }
    }

    fn is_write(self, request: &[u8]) -> bool {
        request[2] == 3
            && request[3] >> 4
                == match self {
                    Self::Ordinary => 2,
                    Self::Gesture46 => 4,
                }
    }
}

#[derive(Default)]
struct Firmware {
    thumb: [u8; 2],
    gesture_flags: u8,
    failed_thumb_acks: usize,
}

fn report(feature: u8, function: u8, payload: &[u8]) -> Vec<u8> {
    let mut report = vec![0; 20];
    report[..4].copy_from_slice(&[0x11, 0xff, feature, function]);
    report[4..4 + payload.len()].copy_from_slice(payload);
    report
}

fn mouse(
    wheel: Wheel,
    state: Arc<Mutex<Firmware>>,
) -> (ScriptedRawHidChannel, ReplayChannelHandle) {
    ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        let mut response = report(request[2], request[3], &[]);
        let mut state = state.lock().unwrap();
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            (0, 0) => {
                let id = u16::from_be_bytes([request[4], request[5]]);
                response[4] = match (id, wheel) {
                    (0x1b04, _) => 2,
                    (0x2150, Wheel::Ordinary) | (0x6501, Wheel::Gesture46) => 3,
                    _ => 0,
                };
            }
            (2, 0) => response[4] = 1,
            (2, 1) => {
                response[4..6].copy_from_slice(&[0, 0xc3]);
                response[8] = 0x20;
                response[12] = 1;
            }
            (2, 2) => response[4..7].copy_from_slice(&[0, 0xc3, state.gesture_flags]),
            (2, 3) => {
                state.gesture_flags = request[6] & 0x11;
                return Some(request.to_vec());
            }
            (3, _) if wheel.is_write(request) => {
                state.thumb = match wheel {
                    Wheel::Ordinary => [request[4], request[5]],
                    Wheel::Gesture46 => [request[7], 0],
                };
                if state.failed_thumb_acks > 0 {
                    state.failed_thumb_acks -= 1;
                    // A failed acknowledgement cannot prove firmware did not
                    // apply the write. Model exactly that uncertainty.
                    return Some(feature_error(request, 2));
                }
                return Some(request.to_vec());
            }
            (3, 0) => match wheel {
                Wheel::Ordinary => response[4..9].copy_from_slice(&[0, 20, 0, 120, 1]),
                Wheel::Gesture46 => response[4..6].copy_from_slice(&[0x83, 46]),
            },
            (3, 1) => response[4..6].copy_from_slice(&state.thumb),
            (3, 3) => response[4] = state.thumb[0],
            _ => panic!("unexpected capture request: {request:02x?}"),
        }
        Some(response)
    })
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(2), future)
        .await
        .expect("scripted transition completes")
}

struct Session {
    wheel: Wheel,
    state: Arc<Mutex<Firmware>>,
    handle: ReplayChannelHandle,
    registry: ChannelRegistry,
    incoming: mpsc::UnboundedSender<Vec<u8>>,
    wanted: watch::Sender<bool>,
    inputs: mpsc::UnboundedReceiver<CapturedInput>,
    accum: Arc<Mutex<CaptureAccum>>,
    decoder: Arc<Mutex<arm::ThumbReports>>,
    shutdown: oneshot::Sender<CaptureSessionStop>,
    task: tokio::task::JoinHandle<CaptureSessionOutcome>,
}

impl Session {
    async fn start(wheel: Wheel, captured: bool, original: [u8; 2]) -> Self {
        let state = Arc::new(Mutex::new(Firmware {
            thumb: original,
            ..Firmware::default()
        }));
        let (raw, handle) = mouse(wheel, state.clone());
        let incoming = raw.incoming_sender();
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        };
        let registry = ChannelRegistry::default();
        registry.replace_node(
            NodeId::from("live-thumb-fixture".to_owned()),
            [route.clone()],
            scripted_channel(raw).await,
        );
        let shared = registry.lookup(&route).unwrap();
        let spec = CaptureSpec {
            capture_thumbwheel: captured,
            divert_gesture_sources: vec![0xc3],
            ..CaptureSpec::default()
        };
        let armed = arm_controls(&shared, &spec, &registry).await.unwrap();
        let decoder = armed.thumb_reports.clone();
        let mut capture = GestureCapture::new(armed);
        let accum = capture.accum.clone();
        let (wanted, wanted_rx) = watch::channel(captured);
        capture.thumb_target = Some((shared.clone(), wanted_rx));
        let ready =
            handle.hold_next_response(RequestMatch::Hidpp20, &[0x10, 0xff, 0, 0, 0x1d, 0x4b, 0]);
        let (shutdown, shutdown_rx) = oneshot::channel();
        let (sink, inputs) = mpsc::unbounded_channel();
        let session_registry = registry.clone();
        let task = tokio::spawn(async move {
            let (_signal, device_io) = device_io_channel();
            let host = CaptureHost {
                sink: Arc::new(sink) as Arc<dyn CaptureInputSink>,
                shutdown: shutdown_rx,
                channel_slot: Arc::new(RwLock::new(None)),
                registry: &registry,
                device_io,
            };
            run_capture(shared, capture, host).await
        });
        bounded(ready.request_written()).await;
        ready.release();
        Self {
            wheel,
            state,
            handle,
            registry: session_registry,
            incoming,
            wanted,
            inputs,
            accum,
            decoder,
            shutdown,
            task,
        }
    }

    fn hold(&self, captured: bool) -> ReplayResponseBarrier {
        self.handle
            .hold_next_response(RequestMatch::Hidpp20, &self.wheel.write(captured, 0))
    }

    fn reprog_writes(&self) -> Vec<Vec<u8>> {
        self.handle
            .written_reports()
            .into_iter()
            .filter(|report| report[2] == 2 && report[3] >> 4 == 3)
            .collect()
    }

    async fn input(&mut self) -> CapturedInput {
        bounded(self.inputs.recv())
            .await
            .expect("listener remains alive")
    }

    async fn press(&mut self) {
        assert_eq!(
            self.state.lock().unwrap().gesture_flags,
            0x11,
            "firmware still diverts gesture down/raw XY/up"
        );
        self.incoming.send(report(2, 0, &[0, 0xc3])).unwrap();
        assert_eq!(
            self.input().await,
            CapturedInput::ButtonDown(ButtonId::GestureButton)
        );
        self.accum.lock().unwrap().backdate_hold_for_test();
    }

    async fn swipe_and_release(&mut self) {
        // Each hold's first raw report is stale; the next must still reach
        // the same accumulator while the thumb acknowledgement is blocked.
        self.incoming.send(report(2, 0x10, &[0, 0, 0, 0])).unwrap();
        self.incoming
            .send(report(2, 0x10, &[0, 120, 0, 0]))
            .unwrap();
        assert_eq!(
            self.input().await,
            CapturedInput::Gesture(ButtonId::GestureButton, GestureDirection::Right)
        );
        self.incoming.send(report(2, 0, &[])).unwrap();
        assert_eq!(
            self.input().await,
            CapturedInput::ButtonUp(ButtonId::GestureButton)
        );
    }

    async fn native_decoder(&self) {
        bounded(async {
            while !matches!(*self.decoder.lock().unwrap(), arm::ThumbReports::Native) {
                tokio::task::yield_now().await;
            }
        })
        .await;
    }

    async fn stop(self) {
        self.shutdown.send(CaptureSessionStop::Shutdown).unwrap();
        assert!(matches!(
            bounded(self.task).await.unwrap(),
            CaptureSessionOutcome::Restored
        ));
    }
}

#[tokio::test]
async fn live_thumb_changes_preserve_new_and_held_gestures_during_both_writes() {
    for wheel in [Wheel::Ordinary, Wheel::Gesture46] {
        for captured in [false, true] {
            for already_held in [false, true] {
                let mut session = Session::start(wheel, captured, [0, 0]).await;
                if already_held {
                    session.press().await;
                }
                let transition = session.hold(!captured);
                session.wanted.send(!captured).unwrap();
                bounded(transition.request_written()).await;
                if !already_held {
                    session.press().await;
                }
                session.swipe_and_release().await;
                // The decoder is already present before an enable ack and
                // remains present until a disable ack, for both protocols.
                let (payload, resolution) = match wheel {
                    Wheel::Ordinary => (
                        vec![0, 6, 0, 0, 2, 2],
                        WheelResolution {
                            native_res: 20,
                            diverted_res: 120,
                        },
                    ),
                    Wheel::Gesture46 => (vec![0x23, 0, 3, 6], WheelResolution::UNKNOWN),
                };
                session.incoming.send(report(3, 0, &payload)).unwrap();
                assert_eq!(
                    session.input().await,
                    CapturedInput::Scroll {
                        increments: 6,
                        resolution
                    }
                );
                assert_eq!(
                    session.reprog_writes().len(),
                    1,
                    "thumb update must not restore or rearm the gesture button"
                );
                assert!(!session.task.is_finished(), "same capture session survives");
                transition.release();
                session.stop().await;
            }
        }
    }
}

#[tokio::test]
async fn live_thumb_rapid_requests_apply_the_latest_target_without_restarting() {
    for wheel in [Wheel::Ordinary, Wheel::Gesture46] {
        let mut session = Session::start(wheel, false, [0, 0]).await;
        let enable = session.hold(true);
        session.wanted.send(true).unwrap();
        bounded(enable.request_written()).await;
        for wanted in [false, true, false] {
            session.wanted.send(wanted).unwrap();
        }
        let disable = session.hold(false);
        enable.release();
        bounded(disable.request_written()).await;
        session.press().await;
        session.swipe_and_release().await;
        disable.release();
        session.native_decoder().await;
        assert_eq!(session.state.lock().unwrap().thumb, [0, 0]);
        assert_eq!(session.reprog_writes().len(), 1);
        let writes = session
            .handle
            .written_reports()
            .into_iter()
            .filter(|report| wheel.is_write(report))
            .count();
        assert_eq!(
            writes, 2,
            "intermediate watch values coalesce to the latest request"
        );
        session.stop().await;
    }
}

#[tokio::test]
async fn live_thumb_failed_acknowledgement_keeps_restoration_debt_in_both_directions() {
    for wheel in [Wheel::Ordinary, Wheel::Gesture46] {
        for captured in [false, true] {
            let session = Session::start(wheel, captured, [0, 0]).await;
            session.state.lock().unwrap().failed_thumb_acks = 1;
            session.wanted.send(!captured).unwrap();
            let outcome = bounded(session.task).await.unwrap();
            assert!(matches!(outcome, CaptureSessionOutcome::Restored));
            assert_eq!(
                session.state.lock().unwrap().thumb,
                [0, 0],
                "uncertain write is restored before the session ends"
            );
            let writes: Vec<_> = session
                .handle
                .written_reports()
                .into_iter()
                .filter(|report| wheel.is_write(report))
                .collect();
            assert_eq!(
                writes.len(),
                usize::from(captured) + 2,
                "failed transition and teardown both write the thumb wheel"
            );
            assert_eq!(
                session.state.lock().unwrap().gesture_flags,
                0,
                "a true hardware failure still uses normal session teardown"
            );
        }
    }
}

#[tokio::test]
async fn live_thumb_disable_preserves_original_reporting_and_direction() {
    for original in [[0, 1], [1, 0], [1, 1]] {
        let session = Session::start(Wheel::Ordinary, true, original).await;
        session.wanted.send(false).unwrap();
        session.native_decoder().await;
        assert_eq!(session.state.lock().unwrap().thumb, original);
        assert_eq!(session.reprog_writes().len(), 1);
        session.stop().await;
    }
}

#[tokio::test]
async fn live_thumb_reenable_publishes_resolution_and_resets_the_roll_state() {
    let mut session = Session::start(Wheel::Ordinary, true, [0, 0]).await;
    session
        .incoming
        .send(report(3, 0, &[0, 6, 0, 0, 2, 2]))
        .unwrap();
    assert_eq!(
        session.input().await,
        CapturedInput::Scroll {
            increments: 6,
            resolution: WheelResolution {
                native_res: 20,
                diverted_res: 120
            }
        }
    );
    session.wanted.send(false).unwrap();
    session.native_decoder().await;
    // A stale diverted report cannot escape after capture was handed back.
    // The next gesture down is a FIFO sentinel for processing that report.
    session
        .incoming
        .send(report(3, 0, &[0, 6, 0, 0, 2, 2]))
        .unwrap();
    session.press().await;
    session.swipe_and_release().await;
    let enable = session.hold(true);
    session.wanted.send(true).unwrap();
    bounded(enable.request_written()).await;
    // The new decoder is installed before the write is acknowledged, and
    // the previous capture's unfinished roll cannot suppress this tap.
    session
        .incoming
        .send(report(3, 0, &[0, 0, 0, 0, 0, 0x0a]))
        .unwrap();
    assert_eq!(
        session.input().await,
        CapturedInput::ButtonPulse(ButtonId::Thumbwheel)
    );
    session
        .incoming
        .send(report(3, 0, &[0, 6, 0, 0, 2, 2]))
        .unwrap();
    assert_eq!(
        session.input().await,
        CapturedInput::Scroll {
            increments: 6,
            resolution: WheelResolution {
                native_res: 20,
                diverted_res: 120
            }
        }
    );
    enable.release();
    session.stop().await;
}

#[tokio::test]
async fn live_thumb_public_entry_consumes_initial_and_during_arm_watch_changes() {
    for wheel in [Wheel::Ordinary, Wheel::Gesture46] {
        let state = Arc::new(Mutex::new(Firmware::default()));
        let (raw, handle) = mouse(wheel, state.clone());
        let incoming = raw.incoming_sender();
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        };
        let registry = ChannelRegistry::default();
        registry.replace_node(
            NodeId::from("live-thumb-entry".to_owned()),
            [route.clone()],
            scripted_channel(raw).await,
        );
        let enable = handle.hold_next_response(RequestMatch::Hidpp20, &wheel.write(true, 0));
        let disable = handle.hold_next_response(RequestMatch::Hidpp20, &wheel.write(false, 0));
        let (wanted, wanted_rx) = watch::channel(true);
        let (shutdown, shutdown_rx) = oneshot::channel();
        let (sink, mut inputs) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let (_signal, device_io) = device_io_channel();
            let host = CaptureHost {
                sink,
                shutdown: shutdown_rx,
                channel_slot: Arc::new(RwLock::new(None)),
                registry: &registry,
                device_io,
            };
            // The watch is authoritative even when the plan snapshot was older.
            run_capture_session_with_thumbwheel(
                route,
                CaptureSpec {
                    capture_thumbwheel: false,
                    divert_gesture_sources: vec![0xc3],
                    ..CaptureSpec::default()
                },
                wanted_rx,
                host,
            )
            .await
        });
        bounded(enable.request_written()).await;
        wanted.send(false).unwrap();
        enable.release();
        bounded(disable.request_written()).await;
        assert_eq!(state.lock().unwrap().gesture_flags, 0x11);
        // The API installs the normal listener and keeps it through both writes.
        incoming.send(report(2, 0, &[0, 0xc3])).unwrap();
        assert_eq!(
            bounded(inputs.recv()).await,
            Some(CapturedInput::ButtonDown(ButtonId::GestureButton))
        );
        incoming.send(report(2, 0, &[])).unwrap();
        assert_eq!(
            bounded(inputs.recv()).await,
            Some(CapturedInput::Gesture(
                ButtonId::GestureButton,
                GestureDirection::Click
            ))
        );
        assert_eq!(
            bounded(inputs.recv()).await,
            Some(CapturedInput::ButtonUp(ButtonId::GestureButton))
        );
        drop(wanted);
        // Closing the watch leaves shutdown authoritative. A shutdown received
        // during the outstanding write still waits for its acknowledged restore.
        shutdown.send(CaptureSessionStop::Shutdown).unwrap();
        disable.release();
        assert!(matches!(
            bounded(task).await.unwrap().unwrap(),
            CaptureSessionOutcome::Restored
        ));
        assert_eq!(state.lock().unwrap().thumb, [0, 0]);
        let writes: Vec<_> = handle
            .written_reports()
            .into_iter()
            .filter(|request| wheel.is_write(request))
            .collect();
        assert_eq!(
            writes.len(),
            2,
            "shutdown does not repeat an acknowledged thumb restore"
        );
    }
}

#[tokio::test]
async fn live_thumb_failed_transition_and_cleanup_retain_a_retryable_restore() {
    for wheel in [Wheel::Ordinary, Wheel::Gesture46] {
        for captured in [false, true] {
            let session = Session::start(wheel, captured, [0, 0]).await;
            session.state.lock().unwrap().failed_thumb_acks = 2;
            session.wanted.send(!captured).unwrap();
            let outcome = bounded(session.task).await.unwrap();
            let CaptureSessionOutcome::RestorePending(pending) = outcome else {
                panic!("unacknowledged restoration must remain retryable");
            };
            let restored = pending.retry(&session.registry).await;
            assert!(matches!(restored, CaptureSessionOutcome::Restored));
            let writes = session
                .handle
                .written_reports()
                .into_iter()
                .filter(|request| wheel.is_write(request))
                .count();
            assert_eq!(writes, usize::from(captured) + 3);
            assert_eq!(session.state.lock().unwrap().thumb, [0, 0]);
        }
    }
}
