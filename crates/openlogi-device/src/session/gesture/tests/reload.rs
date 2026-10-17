//! Live reconfiguration: a new spec is applied on the session's open channel,
//! writing only the controls whose diversion changes.

use std::time::Duration;

use hidpp::nibble::U4;
use tokio::sync::watch;

use super::*;

const CID: u16 = reprog_controls::GESTURE_BUTTON_CID;
const REPROG: u8 = 2;

/// A mouse whose only `0x1b04` control is the gesture button, divertable with
/// raw XY. `setCidReporting` writes are echoed.
fn gesture_button_device() -> (ScriptedRawHidChannel, crate::replay::ReplayChannelHandle) {
    ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        let mut response = vec![0; 20];
        response[..4].copy_from_slice(&request[..4]);
        response[0] = 0x11;
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            (0, 0) => {
                if u16::from_be_bytes([request[4], request[5]]) == 0x1b04 {
                    response[4] = REPROG;
                }
            }
            (REPROG, 0) => response[4] = 1,
            (REPROG, 1) => {
                response[4..6].copy_from_slice(&CID.to_be_bytes());
                response[8] = 0x20;
                response[12] = 0x01;
            }
            (REPROG, 2) => response[4..6].copy_from_slice(&CID.to_be_bytes()),
            (REPROG, 3) => return Some(request.to_vec()),
            _ => panic!("unexpected capture request: {request:02x?}"),
        }
        Some(response)
    })
}

fn plain() -> CaptureSpec {
    CaptureSpec {
        divert_buttons: vec![(CID, ButtonId::GestureButton)],
        ..CaptureSpec::default()
    }
}

fn gesture() -> CaptureSpec {
    CaptureSpec {
        divert_gesture_sources: vec![CID],
        ..CaptureSpec::default()
    }
}

async fn armed_gesture_button(
    spec: &CaptureSpec,
) -> (
    ArmedControls,
    SharedChannel,
    crate::replay::ReplayChannelHandle,
) {
    let (raw, handle) = gesture_button_device();
    let channel = scripted_channel(raw).await;
    let device = Device::new(channel.clone(), 0xff).await.unwrap();
    let mut armed = ArmedControls::default();
    arm_controls_into(&device, &channel, 0xff, spec, &mut armed)
        .await
        .unwrap();
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb034,
    };
    (armed, SharedChannel::new(channel, route), handle)
}

/// Every `getCidReporting` (function 2) and `setCidReporting` (function 3)
/// written after the first `from` reports, as `(function, flags)`.
fn reporting_traffic(handle: &crate::replay::ReplayChannelHandle, from: usize) -> Vec<(u8, u8)> {
    handle.written_reports()[from..]
        .iter()
        .filter(|report| report[2] == REPROG && matches!(report[3] >> 4, 2 | 3))
        .map(|report| (report[3] >> 4, report[6]))
        .collect()
}

fn diverted_buttons(cids: &[u16]) -> v20::Message {
    let mut payload = [0; 16];
    for (slot, cid) in payload.as_chunks_mut::<2>().0.iter_mut().zip(cids) {
        slot.copy_from_slice(&cid.to_be_bytes());
    }
    v20::Message::Long(
        v20::MessageHeader {
            device_index: 0xff,
            feature_index: REPROG,
            function_id: U4::from_lo(0),
            software_id: U4::from_lo(0),
        },
        payload,
    )
}

#[tokio::test]
async fn reload_writes_only_controls_whose_diversion_changes() {
    let (mut armed, shared, handle) = armed_gesture_button(&plain()).await;

    let mark = handle.written_reports().len();
    assert!(armed.reconfigure(&shared, &plain()).await);
    assert_eq!(
        reporting_traffic(&handle, mark),
        [],
        "a control both specs divert the same way is neither re-read nor rewritten"
    );
    assert_eq!(armed.button_cids, [(CID, ButtonId::GestureButton)]);

    let mark = handle.written_reports().len();
    assert!(armed.reconfigure(&shared, &gesture()).await);
    assert_eq!(
        reporting_traffic(&handle, mark),
        [(3, 0x33)],
        "a mode change switches the live diversion without re-reading it"
    );
    assert_eq!(armed.gesture_cids, [CID]);
    assert!(armed.button_cids.is_empty());

    let mark = handle.written_reports().len();
    assert!(armed.reconfigure(&shared, &CaptureSpec::default()).await);
    assert_eq!(
        reporting_traffic(&handle, mark),
        [(3, 0x22)],
        "a control the new spec drops is handed back"
    );
    assert!(armed.gesture_cids.is_empty());
    assert!(
        armed.into_pending(&shared).is_none(),
        "nothing stays owed once every dropped control is restored"
    );
}

#[tokio::test]
async fn a_new_spec_reroutes_the_live_report_handler() {
    let (armed, shared, _handle) = armed_gesture_button(&plain()).await;
    let (spec_tx, spec_rx) = watch::channel(plain());
    let mut capture = GestureCapture::new(armed, plain(), spec_rx, mpsc::unbounded_channel().0);
    let (sink, mut inputs) = mpsc::unbounded_channel();
    let handler = capture.report_handler(0xff, sink);

    handler(&diverted_buttons(&[CID]));
    assert_eq!(
        inputs.try_recv().unwrap(),
        CapturedInput::ButtonDown(ButtonId::GestureButton)
    );

    spec_tx.send(gesture()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), capture.reconfigure_requested())
        .await
        .expect("a changed spec must wake the live session");
    assert!(capture.reconfigure(&shared).await);

    // The press began under the old routing; the reload forgets it rather
    // than reporting a release for a control that is now a gesture source.
    handler(&diverted_buttons(&[]));
    inputs.try_recv().unwrap_err();
    handler(&diverted_buttons(&[CID]));
    handler(&diverted_buttons(&[]));
    let emitted: Vec<_> = std::iter::from_fn(|| inputs.try_recv().ok()).collect();
    assert!(
        emitted.contains(&CapturedInput::Gesture(
            ButtonId::GestureButton,
            GestureDirection::Click
        )),
        "the handler registered before the reload must decode the new gesture source: {emitted:?}"
    );

    assert!(
        tokio::time::timeout(Duration::from_millis(50), capture.reconfigure_requested())
            .await
            .is_err(),
        "an applied spec must not wake the session again"
    );
}
