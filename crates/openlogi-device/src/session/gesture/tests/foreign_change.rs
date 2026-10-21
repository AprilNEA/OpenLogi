//! A capture session that another client changes underneath: the `0x1b04`
//! reporting changes it answers, and the ones it must not.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use super::*;
use crate::device_io_channel;
use crate::replay::ReplayChannelHandle;

const REPROG_INDEX: u8 = 2;
const GESTURE_CID: u16 = reprog_controls::GESTURE_BUTTON_CID;
const MIDDLE_CID: u16 = 0x0052;
/// The software id of every request on a scripted channel.
const OWN_SW_ID: u8 = 0x01;
/// A software id that no request on a scripted channel carries.
const FOREIGN_SW_ID: u8 = 0x0a;
/// The `getCidReporting` flags of a control diverted with raw XY.
const DIVERTED_RAW_XY: u8 = 0x11;

/// Each control's `getCidReporting` flags byte and remap.
type ReportingState = Arc<Mutex<BTreeMap<u16, (u8, [u8; 2])>>>;

/// Where another client's response is injected; `None` once used.
type OtherClient = Arc<Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>>;

/// Both controls native and mapped to themselves.
fn native_controls() -> ReportingState {
    Arc::new(Mutex::new(BTreeMap::from([
        (GESTURE_CID, (0x00, [0, 0])),
        (MIDDLE_CID, (0x00, [0, 0])),
    ])))
}

/// A mouse with the gesture button and the middle button on `0x1b04` at
/// [`REPROG_INDEX`], and no other feature. `setCidReporting` writes update
/// `state` and are echoed.
///
/// When `other_client` holds a sender, the first write to the middle button
/// also plays another client: it hands the gesture button back, and that
/// client's response goes out through the sender.
fn mouse(
    state: ReportingState,
    other_client: OtherClient,
) -> (ScriptedRawHidChannel, ReplayChannelHandle) {
    ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        let mut response = vec![0; 20];
        response[..4].copy_from_slice(&request[..4]);
        response[0] = 0x11;
        // A feature id for the root lookup, a control id for reporting.
        let id = u16::from_be_bytes([request[4], request[5]]);
        let mut state = state.lock().unwrap();
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            (0, 0) if id == reprog_controls::FEATURE_ID => response[4] = REPROG_INDEX,
            (0, 0) => {}
            (REPROG_INDEX, 0) => response[4] = 2,
            (REPROG_INDEX, 1) => {
                // Both controls are divertable; only the gesture button
                // reports raw XY.
                let (cid, raw_xy) = if request[4] == 0 {
                    (GESTURE_CID, 0x01)
                } else {
                    (MIDDLE_CID, 0x00)
                };
                response[4..6].copy_from_slice(&cid.to_be_bytes());
                response[8] = 0x20;
                response[12] = raw_xy;
            }
            (REPROG_INDEX, 2) => {
                let (flags, remap) = state[&id];
                response[4..6].copy_from_slice(&id.to_be_bytes());
                response[6] = flags;
                response[7..9].copy_from_slice(&remap);
            }
            (REPROG_INDEX, 3) => {
                let (flags, remap) = state.get_mut(&id).unwrap();
                let written = request[6];
                // Each valid bit sits one above the setting it guards.
                for setting in [0x01, 0x10] {
                    if written & (setting << 1) != 0 {
                        *flags = (*flags & !setting) | (written & setting);
                    }
                }
                // Remap `0` keeps the current remap.
                if request[7..9] != [0, 0] {
                    remap.copy_from_slice(&request[7..9]);
                }
                if id == MIDDLE_CID
                    && let Some(sender) = other_client.lock().unwrap().take()
                {
                    state.insert(GESTURE_CID, (0x00, [0, 0]));
                    sender
                        .send(undivert_response(FOREIGN_SW_ID, GESTURE_CID, [0, 0]))
                        .unwrap();
                }
                return Some(request.to_vec());
            }
            _ => panic!("unexpected capture request: {request:02x?}"),
        }
        Some(response)
    })
}

/// A `setCidReporting` response for `cid` with software id `sw_id`, as the
/// device sends it to every open handle: diversion and raw XY cleared, and
/// the control remapped to `remap`.
fn undivert_response(sw_id: u8, cid: u16, remap: [u8; 2]) -> Vec<u8> {
    let mut report = vec![0; 20];
    report[..4].copy_from_slice(&[0x11, 0xff, REPROG_INDEX, 0x30 | sw_id]);
    report[4..6].copy_from_slice(&cid.to_be_bytes());
    report[6] = 0x22;
    report[7..9].copy_from_slice(&remap);
    report
}

/// The `function` requests to `0x1b04` for `cid` in `reports`.
fn requests(reports: &[Vec<u8>], function: u8, cid: u16) -> Vec<Vec<u8>> {
    reports
        .iter()
        .filter(|report| {
            report[2] == REPROG_INDEX
                && report[3] >> 4 == function
                && u16::from_be_bytes([report[4], report[5]]) == cid
        })
        .cloned()
        .collect()
}

/// Wait until `done` holds for the reports written so far.
async fn written_until(
    handle: &ReplayChannelHandle,
    what: &str,
    done: impl Fn(&[Vec<u8>]) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !done(&handle.written_reports()) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the session never wrote {what}"));
}

/// A running gesture capture session over `raw`: the gesture button diverted
/// with raw XY, then the middle button diverted plainly.
struct Session {
    shutdown: oneshot::Sender<CaptureSessionStop>,
    task: tokio::task::JoinHandle<Result<CaptureSessionOutcome, CaptureSessionFailure>>,
}

impl Session {
    async fn start(raw: ScriptedRawHidChannel) -> Self {
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        };
        let registry = ChannelRegistry::default();
        registry.replace_node(
            NodeId::from("mouse-node".to_owned()),
            [route.clone()],
            scripted_channel(raw).await,
        );
        let (shutdown, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (sink, _inputs) = mpsc::unbounded_channel();
            let (_signal, device_io) = device_io_channel();
            let spec = CaptureSpec {
                divert_gesture_sources: vec![GESTURE_CID],
                divert_buttons: vec![(MIDDLE_CID, ButtonId::MiddleClick)],
                ..CaptureSpec::default()
            };
            let host = CaptureHost {
                sink,
                shutdown: shutdown_rx,
                channel_slot: Arc::new(RwLock::new(None)),
                registry: &registry,
                device_io,
            };
            run_capture_session(route, spec, host).await
        });
        Self { shutdown, task }
    }

    /// Stop the session and require that it handed every control back.
    async fn stop(self) {
        self.shutdown.send(CaptureSessionStop::Shutdown).unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(2), self.task)
            .await
            .expect("the session stops")
            .expect("the session task does not panic");
        assert!(matches!(outcome, Ok(CaptureSessionOutcome::Restored)));
    }
}

#[tokio::test]
async fn another_clients_undivert_is_answered_and_this_channels_own_is_not() {
    let state = native_controls();
    let (raw, handle) = mouse(Arc::clone(&state), OtherClient::default());
    let incoming = raw.incoming_sender();
    let session = Session::start(raw).await;

    // Arming has finished once the session looks up `0x1d4b`.
    written_until(&handle, "the wireless lookup", |reports| {
        reports
            .iter()
            .any(|report| report[2] == 0 && report[3] >> 4 == 0 && report[4..6] == [0x1d, 0x4b])
    })
    .await;
    let armed = handle.written_reports();
    assert_eq!(requests(&armed, 3, GESTURE_CID).len(), 1);
    assert_eq!(requests(&armed, 3, MIDDLE_CID).len(), 1);

    // Another client hands both controls back and remaps the gesture
    // button. The response to the middle button's write carries this
    // channel's own software id, so it may answer a request of this channel:
    // the session must leave it alone. The gesture button's carries another
    // client's id.
    let remap = [0x00, 0x56];
    for (cid, remap) in [(GESTURE_CID, remap), (MIDDLE_CID, [0, 0])] {
        state.lock().unwrap().insert(cid, (0x00, remap));
    }
    incoming
        .send(undivert_response(OWN_SW_ID, MIDDLE_CID, [0, 0]))
        .unwrap();
    incoming
        .send(undivert_response(FOREIGN_SW_ID, GESTURE_CID, remap))
        .unwrap();

    written_until(&handle, "the gesture button's re-divert", |reports| {
        requests(reports, 3, GESTURE_CID).len() == 2
    })
    .await;
    let reports = handle.written_reports();
    let reads = requests(&reports, 2, GESTURE_CID);
    assert_eq!(
        reads.len(),
        2,
        "arming and the change each read the control once"
    );
    let redivert = &requests(&reports, 3, GESTURE_CID)[1];
    let [cid_hi, cid_lo] = GESTURE_CID.to_be_bytes();
    assert_eq!(
        redivert[4..9],
        [cid_hi, cid_lo, 0x33, 0, 0],
        "the re-divert restores diversion and raw XY, and writes no remap"
    );
    assert_eq!(
        state.lock().unwrap()[&GESTURE_CID],
        (DIVERTED_RAW_XY, remap),
        "the other client's remap survives the re-divert"
    );
    // The middle button's response was handled first, so any answer to it
    // would already be written.
    assert_eq!(
        requests(&reports, 2, MIDDLE_CID).len(),
        1,
        "only arming reads the middle button"
    );
    assert_eq!(
        requests(&reports, 3, MIDDLE_CID).len(),
        1,
        "only arming writes the middle button"
    );

    session.stop().await;
}

#[tokio::test]
async fn a_change_while_arming_continues_is_answered_once_capture_runs() {
    // Another client hands the gesture button back right after the session
    // armed it, while the session still arms the middle button. Its only
    // report of the change arrives before capture runs.
    let state = native_controls();
    let other_client = OtherClient::default();
    let (raw, handle) = mouse(Arc::clone(&state), Arc::clone(&other_client));
    *other_client.lock().unwrap() = Some(raw.incoming_sender());
    let session = Session::start(raw).await;

    written_until(&handle, "the gesture button's re-divert", |reports| {
        requests(reports, 3, GESTURE_CID).len() == 2
    })
    .await;
    assert!(
        other_client.lock().unwrap().is_none(),
        "the other client's change happened while arming"
    );
    let reports = handle.written_reports();
    assert_eq!(
        requests(&reports, 2, GESTURE_CID).len(),
        2,
        "arming and the change each read the control once"
    );
    assert_eq!(state.lock().unwrap()[&GESTURE_CID].0, DIVERTED_RAW_XY);

    session.stop().await;
}
