//! send_v20: matching by header, report widths, and error frames.

use super::*;

// --- HID++2.0 (v20) send/matcher characterization tests -----------------
//
// `HidppChannel::send`/`send_with_timeout` above are protocol-agnostic:
// they match on an arbitrary predicate over raw `HidppMessage`s. The
// v20-specific correlation logic (matching by header, splitting out error
// frames) lives in `protocol::v20::HidppChannel::send_v20`, which is built
// directly on top of `send`. These tests pin that logic's current
// behaviour using the same mock transport as the tests above.

#[test]
fn send_v20_matches_response_by_header_ignoring_unrelated_messages() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        let header = v20::MessageHeader {
            device_index: 0x01,
            feature_index: 0x05,
            function_id: U4::from_lo(0x2),
            software_id: U4::from_lo(0x3),
        };
        let request = v20::Message::Short(header, [0x00, 0x00, 0x00]);
        let response = v20::Message::Short(header, [0xaa, 0xbb, 0xcc]);

        // Each decoy differs from the request in exactly one header field, so
        // none of them may be mistaken for its response.
        let wrong_device = v20::Message::Short(
            v20::MessageHeader {
                device_index: 0x02,
                ..header
            },
            [0, 0, 0],
        );
        let wrong_feature = v20::Message::Short(
            v20::MessageHeader {
                feature_index: 0x06,
                ..header
            },
            [0, 0, 0],
        );
        let wrong_sw_id = v20::Message::Short(
            v20::MessageHeader {
                software_id: U4::from_lo(0x4),
                ..header
            },
            [0, 0, 0],
        );

        let send_fut = channel.send_v20(request);
        let feed_fut = async {
            handle.send_incoming(wrong_device.into()).await;
            handle.send_incoming(wrong_feature.into()).await;
            handle.send_incoming(wrong_sw_id.into()).await;
            handle.send_incoming(response.into()).await;
        };

        let (result, ()) = futures::join!(send_fut, feed_fut);

        assert_eq!(result.unwrap(), response);
        assert_pending_empty(&channel);
    });
}

#[test]
fn send_v20_broadcast_event_does_not_resolve_pending_request() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let listener_events = Arc::clone(&events);
        channel.add_msg_listener(move |msg, matched| {
            listener_events.lock().unwrap().push((msg, matched));
        });

        let header = v20::MessageHeader {
            device_index: 0x01,
            feature_index: 0x05,
            function_id: U4::from_lo(0x2),
            software_id: U4::from_lo(0x3),
        };
        let request = v20::Message::Short(header, [0, 0, 0]);
        let response = v20::Message::Short(header, [0xaa, 0xbb, 0xcc]);

        // Software ID 0 is reserved for unsolicited device notifications
        // (see `feature::event_payload`). The request above uses a non-zero
        // ID, so an incoming broadcast sharing device/feature but using ID 0
        // must be routed to listeners, not consumed as this request's
        // response.
        let event = v20::Message::Short(
            v20::MessageHeader {
                software_id: U4::from_lo(0x0),
                ..header
            },
            [0x01, 0x02, 0x03],
        );

        let send_fut = channel.send_v20(request);
        let feed_fut = async {
            handle.send_incoming(event.into()).await;
            wait_for_event_count(&events, 1).await;
            handle.send_incoming(response.into()).await;
        };

        let (result, ()) = futures::join!(send_fut, feed_fut);

        assert_eq!(result.unwrap(), response);
        // The oneshot resolves before the listener loop runs on the read
        // thread; wait for both deliveries before asserting on them.
        wait_for_event_count(&events, 2).await;
        let recorded = events.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec![
                (HidppMessage::from(event), false),
                (HidppMessage::from(response), true),
            ]
        );
        assert_pending_empty(&channel);
    });
}

#[test]
fn send_v20_response_may_arrive_as_a_different_report_width() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        let header = v20::MessageHeader {
            device_index: 0x01,
            feature_index: 0x05,
            function_id: U4::from_lo(0x2),
            software_id: U4::from_lo(0x3),
        };
        let request = v20::Message::Short(header, [0, 0, 0]);
        // Quirk: `send_v20`'s response predicate compares only the parsed
        // v20 header, not the underlying report width. A device replying
        // with a long report to a short request — same header, wider
        // payload — is still accepted as the response.
        let response = v20::Message::Long(header, [0xaa; 16]);
        handle.queue_response(response.into());

        let result = channel.send_v20(request).await.unwrap();

        assert_eq!(result, response);
        assert_pending_empty(&channel);
    });
}

#[test]
fn send_v20_error_frame_resolves_to_feature_error() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        let header = v20::MessageHeader {
            device_index: 0x01,
            feature_index: 0x05,
            function_id: U4::from_lo(0x2),
            software_id: U4::from_lo(0x3),
        };
        let request = v20::Message::Short(header, [0, 0, 0]);
        let error_response = v20_error_frame(header, ErrorType::InvalidArgument.into());
        handle.queue_response(error_response.into());

        let err = channel.send_v20(request).await.unwrap_err();

        assert!(matches!(
            err,
            Hidpp20Error::Feature(ErrorType::InvalidArgument)
        ));
        assert_pending_empty(&channel);
    });
}

#[test]
fn send_v20_error_frame_with_unmapped_code_is_unsupported_response() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        let header = v20::MessageHeader {
            device_index: 0x01,
            feature_index: 0x05,
            function_id: U4::from_lo(0x2),
            software_id: U4::from_lo(0x3),
        };
        let request = v20::Message::Short(header, [0, 0, 0]);
        // 0xfe is not a defined `ErrorType` variant.
        let error_response = v20_error_frame(header, 0xfe);
        handle.queue_response(error_response.into());

        let err = channel.send_v20(request).await.unwrap_err();

        assert!(matches!(err, Hidpp20Error::UnsupportedResponse));
        assert_pending_empty(&channel);
    });
}

#[test]
fn send_v20_write_through_preserves_typed_feature_errors() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        handle.park_writes();
        let channel = channel_with_reader(raw).await;

        let header = v20::MessageHeader {
            device_index: 0x01,
            feature_index: 0x05,
            function_id: U4::from_lo(0x2),
            software_id: U4::from_lo(0x3),
        };
        let request = v20::Message::Short(header, [0, 0, 0]);
        let error_response = v20_error_frame(header, ErrorType::Busy.into());
        handle.queue_response(error_response.into());
        let mut send = Box::pin(channel.send_v20_write_through(request, |_| false));

        assert!(futures::poll!(send.as_mut()).is_pending());
        handle.release_writes();
        let error = send.await.unwrap_err();

        assert!(matches!(error, Hidpp20Error::Feature(ErrorType::Busy)));
        assert_pending_empty(&channel);
    });
}

/// Builds the HID++2.0 error-frame encoding for `request_header`: feature
/// index 0xFF, with the original feature index and function|software byte
/// shifted one byte to the right (see `v20::HidppChannel::send_v20`'s
/// `is_error` predicate for the reverse mapping).
fn v20_error_frame(request_header: v20::MessageHeader, error_code: u8) -> v20::Message {
    let error_header = v20::MessageHeader {
        device_index: request_header.device_index,
        feature_index: 0xff,
        function_id: U4::from_hi(request_header.feature_index),
        software_id: U4::from_lo(request_header.feature_index),
    };
    let mut payload = [0u8; 3];
    payload[0] = nibble::combine(request_header.function_id, request_header.software_id);
    payload[1] = error_code;
    v20::Message::Short(error_header, payload)
}
