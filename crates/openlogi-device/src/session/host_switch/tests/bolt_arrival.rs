use std::time::Duration;

use hidpp::protocol::v20::ErrorType;

use super::{
    ChannelRegistry, DeviceRoute, NodeId, ReplayChannelHandle, ScriptedRawHidChannel, UNIT,
    device_io_channel, feature_error, keyboard_with_the_expected_unit, mpsc, raw_scripted_channel,
};

const LINK_UP: [u8; 7] = [0x10, 1, 0x41, 0, 2, 0x34, 0x12];

/// A receiver that can immediately confirm the announced product and unit on
/// slot one, even when the peripheral's own radio does not answer.
async fn arrival_watch(
    node: &str,
    radio: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static,
) -> (
    std::pin::Pin<Box<dyn Future<Output = bool> + Send>>,
    mpsc::UnboundedSender<Vec<u8>>,
    ReplayChannelHandle,
) {
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        if !matches!(request, [_, 0xff, 0x83, 0xb5, 0x51, ..]) {
            return radio(request);
        }
        let mut response = vec![0x11, 0xff, 0x83, 0xb5, 0x51, 2, 0x34, 0x12];
        response.extend_from_slice(&UNIT);
        response.resize(20, 0);
        Some(response)
    });
    let incoming = raw.incoming_sender();
    let channel = raw_scripted_channel(raw.presenting_as(0xc548)).await;
    let route = DeviceRoute::Bolt {
        receiver_uid: "test".into(),
        slot: 1,
    };
    let registry = ChannelRegistry::default();
    registry.publish(
        NodeId::from(node.to_owned()),
        vec![],
        vec![route.clone()],
        channel,
    );
    let (_signal, gate) = device_io_channel();
    let watch = registry.watch_bolt_link(&route, UNIT, true, gate).unwrap();
    (Box::pin(watch), incoming, handle)
}

#[tokio::test]
async fn a_bolt_link_and_fresh_pairing_record_can_confirm_before_the_radio_answers() {
    let (watch, incoming, handle) = arrival_watch("flow-fast-pairing-record", |_| None).await;
    incoming.send(LINK_UP.to_vec()).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), watch)
            .await
            .expect("pairing record confirms arrival without waking the device")
    );
    assert!(
        handle
            .written_reports()
            .iter()
            .all(|report| report[1] == 0xff),
        "the sleeping device was never queried"
    );
}

#[tokio::test]
async fn late_bolt_link_is_seen_during_the_third_unanswered_radio_probe() {
    let (watch, incoming, handle) = arrival_watch("flow-late-link", |_| None).await;
    tokio::time::pause();
    let announce = async {
        // Let the watch run its speculative probe and retry schedule. The third
        // read has started, with only 400 ms left in the transfer window.
        tokio::time::sleep(Duration::from_millis(2600)).await;
        tokio::time::resume();
        assert!(handle.written_reports().iter().any(|report| report[1] == 1));
        incoming.send(LINK_UP.to_vec()).unwrap();
    };
    let (result, ()) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(3), watch),
        announce
    );
    assert!(result.expect("receiver link must win before the handoff expires"));
}

#[tokio::test]
async fn unsupported_bolt_unit_reads_stop_but_receiver_events_still_confirm() {
    // Index zero means feature absent. Other devices return a definitive
    // feature error when asked about DeviceInformation.
    for error in [
        None,
        Some(ErrorType::InvalidArgument),
        Some(ErrorType::LogitechInternal),
    ] {
        let (reads, mut queries) = mpsc::unbounded_channel();
        let (watch, incoming, _) = arrival_watch("flow-unsupported-unit", move |request| {
            if request[2] == 0 && request[3] >> 4 == 0 && request[4..6] == [0, 3] {
                reads.send(()).unwrap();
                return Some(error.map_or_else(
                    || vec![0x10, request[1], 0, request[3], 0, 0, 0],
                    |code| feature_error(request, code.into()),
                ));
            }
            keyboard_with_the_expected_unit(request)
        })
        .await;
        let announce = async {
            queries.recv().await.expect("unit query channel closed");
            assert!(
                tokio::time::timeout(Duration::from_millis(450), queries.recv())
                    .await
                    .is_err(),
                "retried a definitive DeviceInformation response: {error:?}"
            );
            incoming.send(LINK_UP.to_vec()).unwrap();
        };
        tokio::time::timeout(Duration::from_secs(3), async {
            let (confirmed, ()) = tokio::join!(watch, announce);
            assert!(confirmed, "receiver proof still confirms the physical unit");
        })
        .await
        .expect("unit query and subsequent link proof should complete");
    }
}
