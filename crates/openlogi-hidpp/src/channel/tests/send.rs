//! send: widening, responses, timeouts, cancellation and the pending table.

use super::*;

#[test]
fn short_payload_widens_preserving_header_and_padding() {
    // [device, feature, function|sw, p0, p1, p2]
    let short = [0xff, 0x05, 0x1e, 0xaa, 0xbb, 0xcc];
    let HidppMessage::Long(long) = HidppMessage::Short(short).widened() else {
        panic!("widening a short message must produce a long one");
    };
    assert_eq!(&long[..short.len()], &short[..]); // header + payload copied verbatim
    assert!(long[short.len()..].iter().all(|&b| b == 0)); // remainder zero-padded
    assert_eq!(long.len(), LONG_REPORT_LENGTH - 1);
}

#[test]
fn widening_an_already_long_message_is_a_no_op() {
    let long = HidppMessage::Long([0x5a; LONG_REPORT_LENGTH - 1]);

    assert_eq!(long.widened(), long);
}

#[test]
fn send_returns_response_before_timeout() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        let request = short_msg(0x10);
        let response = short_msg(0x20);
        handle.queue_response(response);

        let actual = channel
            .send_with_timeout(
                request,
                move |candidate| *candidate == response,
                Duration::from_secs(1),
            )
            .await
            .unwrap();

        assert_eq!(actual, response);
        assert_eq!(handle.written_reports().len(), 1);
        assert_pending_empty(&channel);
    });
}

#[test]
fn send_times_out_and_removes_pending_message() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;
        let request = short_msg(0x10);
        let response = short_msg(0x20);

        let started = Instant::now();
        let err = channel
            .send_with_timeout(
                request,
                move |candidate| *candidate == response,
                Duration::from_millis(25),
            )
            .await
            .unwrap_err();

        assert!(matches!(err, ChannelError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(handle.written_reports().len(), 1);
        assert_pending_empty(&channel);
    });
}

#[test]
fn send_write_through_waits_for_same_header_then_finishes_native_write() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let listener_events = Arc::clone(&events);
        channel.add_msg_listener(move |msg, matched| {
            listener_events.lock().unwrap().push((msg, matched));
        });
        let request = short_msg(0x10);
        let matches_header = move |candidate: &HidppMessage| candidate.header() == request.header();
        let mut first = Box::pin(channel.send(request, matches_header));
        assert!(futures::poll!(first.as_mut()).is_pending());

        handle.park_writes();
        let mut send = Box::pin(channel.send_write_through(
            request,
            matches_header,
            Duration::from_millis(25),
        ));
        assert!(futures::poll!(send.as_mut()).is_pending());
        assert_eq!(
            handle.written_reports().len(),
            1,
            "same header is in flight"
        );

        drop(first);
        assert!(futures::poll!(send.as_mut()).is_pending());
        assert_eq!(
            handle.written_reports().len(),
            1,
            "late reply is still owed"
        );

        // Even a byte-identical write must discard the abandoned reply.
        let late_response = same_header_msg(0x10, 0x21);
        handle.send_incoming(late_response).await;
        wait_for_event_count(&events, 1).await;
        assert_eq!(events.lock().unwrap()[0], (late_response, false));
        assert!(futures::poll!(send.as_mut()).is_pending());
        assert_eq!(handle.written_reports().len(), 2);
        assert_eq!(stale_len(&channel), 0);

        // The native write is now in progress, with no response timer yet.
        futures_timer::Delay::new(Duration::from_millis(50)).await;
        assert!(futures::poll!(send.as_mut()).is_pending());
        assert_eq!(pending_len(&channel), 1);

        let response = same_header_msg(0x10, 0x32);
        handle.queue_response(response);
        handle.release_writes();
        assert_eq!(send.await.unwrap(), response);
        assert_pending_empty(&channel);
    });
}

#[test]
fn send_write_through_times_out_and_cleans_up_after_write_completes() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        handle.park_writes();
        let observations = Arc::new(Mutex::new(Vec::new()));
        let observer_observations = Arc::clone(&observations);
        let observer: Arc<dyn ChannelObserver> = Arc::new(move |observation| {
            observer_observations.lock().unwrap().push(observation);
        });
        let channel = HidppChannel::from_raw_channel_with_observer(raw, observer)
            .await
            .expect("the mock transport speaks HID++");
        let mut send = Box::pin(channel.send_write_through(
            short_msg(0x10),
            |_| false,
            Duration::from_millis(25),
        ));

        assert!(futures::poll!(send.as_mut()).is_pending());
        futures_timer::Delay::new(Duration::from_millis(50)).await;
        assert!(futures::poll!(send.as_mut()).is_pending());

        handle.release_writes();
        let error = send.await.unwrap_err();

        assert!(matches!(error, ChannelError::Timeout));
        assert_pending_empty(&channel);
        assert!(observations.lock().unwrap().iter().any(|observation| {
            matches!(
                observation,
                ChannelObservation::RequestOutcome {
                    request_id: 1,
                    outcome: RequestOutcome::TimedOut,
                }
            )
        }));
    });
}

#[test]
fn cancelled_send_removes_pending_before_a_late_response() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        handle.park_writes();
        let channel = channel_with_reader(raw).await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let listener_events = Arc::clone(&events);
        channel.add_msg_listener(move |msg, matched| {
            listener_events.lock().unwrap().push((msg, matched));
        });

        let late_response = short_msg(0x20);
        let mut send = Box::pin(channel.send_with_timeout(
            short_msg(0x10),
            move |candidate| *candidate == late_response,
            Duration::from_secs(1),
        ));

        assert!(futures::poll!(send.as_mut()).is_pending());
        assert_eq!(channel.pending_messages.lock().unwrap().messages.len(), 1);

        drop(send);
        assert_pending_empty(&channel);

        handle.send_incoming(late_response).await;
        wait_for_event_count(&events, 1).await;
        assert_eq!(events.lock().unwrap()[0], (late_response, false));
    });
}

#[test]
fn timeout_removes_only_its_own_pending_message() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        let never_answered = short_msg(0x20);
        let slow_response = short_msg(0x21);

        let timed_out = channel.send_with_timeout(
            short_msg(0x10),
            move |candidate| *candidate == never_answered,
            Duration::from_millis(25),
        );
        let answered = channel.send_with_timeout(
            short_msg(0x11),
            move |candidate| *candidate == slow_response,
            Duration::from_secs(1),
        );
        // Answer the second request only after the first has timed out, so
        // a removal that took the wrong entry would fail this test.
        let respond_late = async {
            futures_timer::Delay::new(Duration::from_millis(100)).await;
            handle.send_incoming(slow_response).await;
        };

        let (timed_out, answered, ()) = futures::join!(timed_out, answered, respond_late);

        assert!(matches!(timed_out.unwrap_err(), ChannelError::Timeout));
        assert_eq!(answered.unwrap(), slow_response);
        assert_pending_empty(&channel);
    });
}

#[test]
fn late_response_after_timeout_is_ignored() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let listener_events = Arc::clone(&events);
        channel.add_msg_listener(move |msg, matched| {
            listener_events.lock().unwrap().push((msg, matched));
        });

        let request = short_msg(0x10);
        let late_response = short_msg(0x20);
        let err = channel
            .send_with_timeout(
                request,
                move |candidate| *candidate == late_response,
                Duration::from_millis(25),
            )
            .await
            .unwrap_err();

        assert!(matches!(err, ChannelError::Timeout));
        assert_pending_empty(&channel);

        handle.send_incoming(late_response).await;
        wait_for_event_count(&events, 1).await;
        assert_eq!(events.lock().unwrap()[0], (late_response, false));
        assert_pending_empty(&channel);

        let followup_request = short_msg(0x30);
        let followup_response = short_msg(0x40);
        handle.queue_response(followup_response);
        let actual = channel
            .send_with_timeout(
                followup_request,
                move |candidate| *candidate == followup_response,
                Duration::from_secs(1),
            )
            .await
            .unwrap();

        assert_eq!(actual, followup_response);
        wait_for_event_count(&events, 2).await;
        assert_eq!(events.lock().unwrap()[1], (followup_response, true));
        assert_pending_empty(&channel);
    });
}

#[test]
fn send_and_forget_writes_without_pending_message() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        channel.send_and_forget(short_msg(0x10)).await.unwrap();

        assert_eq!(handle.written_reports().len(), 1);
        assert_pending_empty(&channel);
    });
}
