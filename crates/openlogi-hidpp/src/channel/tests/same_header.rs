//! Same-header requests: waiting, quarantine after a timeout, adoption, and ordering.

use super::*;

/// A request with the same header as one still in flight waits for it. Two
/// such requests get replies nothing on the wire tells apart, and a wireless
/// receiver can answer them out of order: on a Bolt-connected MX Master 4,
/// three startup sessions resolving features through root `getFeature` at once
/// had the thumbwheel's index handed to the wheel session and vice versa,
/// which pinned the wrong features for the rest of the session.
#[test]
fn a_request_waits_while_the_same_header_is_in_flight() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        // Writes park, so the first request stays in flight for as long as the
        // test wants.
        handle.park_writes();
        let channel = channel_with_reader(raw).await;

        let first_reply = short_msg(0x11);
        let mut first =
            Box::pin(channel.send(short_msg(0x10), move |candidate| *candidate == first_reply));
        assert!(futures::poll!(first.as_mut()).is_pending());
        assert_eq!(handle.written_reports().len(), 1);
        assert_eq!(pending_len(&channel), 1);

        // Same device/feature/function bytes as `first`, different payload:
        // must not reach the wire, and must not be registered, while `first`
        // is pending.
        let mut same_header = Box::pin(channel.send(same_header_msg(0x10, 0xa2), |_| true));
        for _ in 0..5 {
            assert!(futures::poll!(same_header.as_mut()).is_pending());
            futures_timer::Delay::new(Duration::from_millis(5)).await;
        }
        assert_eq!(
            handle.written_reports().len(),
            1,
            "the second request went out early"
        );
        assert_eq!(pending_len(&channel), 1);

        // A different header is unaffected.
        let other_reply = short_msg(0x21);
        let mut other =
            Box::pin(channel.send(short_msg(0x20), move |candidate| *candidate == other_reply));
        assert!(futures::poll!(other.as_mut()).is_pending());
        assert_eq!(handle.written_reports().len(), 2);
        assert_eq!(pending_len(&channel), 2);

        // Cancelling `first` unanswered does not free its header yet: its
        // reply is still expected, and would answer the parked request.
        drop(first);
        assert_eq!(pending_len(&channel), 1);
        assert_eq!(stale_len(&channel), 1);
        assert!(futures::poll!(same_header.as_mut()).is_pending());
        assert_eq!(
            handle.written_reports().len(),
            2,
            "the parked request went out while a reply with its header was outstanding"
        );

        // The late reply is discarded, and only then does the parked request
        // register and write.
        handle.send_incoming(first_reply).await;
        for _ in 0..20 {
            if handle.written_reports().len() == 3 {
                break;
            }
            assert!(futures::poll!(same_header.as_mut()).is_pending());
            futures_timer::Delay::new(Duration::from_millis(5)).await;
        }
        assert_eq!(handle.written_reports().len(), 3);
        assert_eq!(stale_len(&channel), 0);
        assert_eq!(pending_len(&channel), 2);
    });
}

/// A request that timed out unanswered keeps its header reserved until its
/// reply lands: the reply is discarded, and the next request with that header
/// — which nothing on the wire could tell it from — gets its own.
#[test]
fn a_reply_landing_after_a_timeout_cannot_answer_the_next_same_header_request() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let listener_events = Arc::clone(&events);
        channel.add_msg_listener(move |msg, matched| {
            listener_events.lock().unwrap().push((msg, matched));
        });

        // Both requests share a header and accept any reply, as two root
        // `getFeature` calls for different features do.
        let err = channel
            .send_with_timeout(short_msg(0x10), |_| true, Duration::from_millis(25))
            .await
            .unwrap_err();
        assert!(matches!(err, ChannelError::Timeout));
        assert_pending_empty(&channel);
        assert_eq!(stale_len(&channel), 1);

        let mut second = Box::pin(channel.send(same_header_msg(0x10, 0xa2), |_| true));
        for _ in 0..5 {
            assert!(futures::poll!(second.as_mut()).is_pending());
            futures_timer::Delay::new(Duration::from_millis(5)).await;
        }
        assert_eq!(
            handle.written_reports().len(),
            1,
            "the second request went out with a reply to the first still expected"
        );

        // The first request's reply arrives late: discarded, not matched.
        let late_reply = short_msg(0x11);
        handle.send_incoming(late_reply).await;
        wait_for_event_count(&events, 1).await;
        assert_eq!(events.lock().unwrap()[0], (late_reply, false));

        // Now the second request goes out and is answered by its own reply.
        let second_reply = short_msg(0x12);
        handle.queue_response(second_reply);
        assert_eq!(second.await.unwrap(), second_reply);
        assert_eq!(handle.written_reports().len(), 2);
        assert_pending_empty(&channel);
        assert_eq!(stale_len(&channel), 0);
    });
}

/// A reply that never comes must not block its header for good: the
/// reservation lapses after [`STALE_REPLY_GRACE`].
#[test]
fn an_unanswered_header_frees_after_the_grace() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        let abandoned = Instant::now();
        let err = channel
            .send_with_timeout(short_msg(0x10), |_| true, Duration::from_millis(25))
            .await
            .unwrap_err();
        assert!(matches!(err, ChannelError::Timeout));
        assert_eq!(stale_len(&channel), 1);

        let second_reply = short_msg(0x12);
        handle.queue_response(second_reply);
        let actual = channel
            .send_with_timeout(
                same_header_msg(0x10, 0xa2),
                |_| true,
                STALE_REPLY_GRACE + Duration::from_secs(2),
            )
            .await
            .unwrap();

        assert_eq!(actual, second_reply);
        let waited = abandoned.elapsed();
        assert!(
            waited >= STALE_REPLY_GRACE,
            "the second request went out {waited:?} after the first was abandoned"
        );
        assert_eq!(handle.written_reports().len(), 2);
        assert_pending_empty(&channel);
        assert_eq!(stale_len(&channel), 0);
    });
}

/// Re-asking an abandoned request byte for byte gets no shortcut: the reply
/// still owed to the first ask is discarded when it lands, and only then does
/// the re-ask go out and get its own. The bytes say what was asked, not what
/// the answer is — see
/// [`a_re_asked_read_cannot_adopt_a_reply_from_before_an_intervening_write`].
#[test]
fn an_identical_re_ask_waits_for_the_quarantined_reply() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let listener_events = Arc::clone(&events);
        channel.add_msg_listener(move |msg, matched| {
            listener_events.lock().unwrap().push((msg, matched));
        });

        let err = channel
            .send_with_timeout(short_msg(0x10), |_| true, Duration::from_millis(25))
            .await
            .unwrap_err();
        assert!(matches!(err, ChannelError::Timeout));
        assert_eq!(stale_len(&channel), 1);

        // The identical re-ask is parked like any other same-header request.
        let mut retry = Box::pin(channel.send(short_msg(0x10), |_| true));
        for _ in 0..5 {
            assert!(futures::poll!(retry.as_mut()).is_pending());
            futures_timer::Delay::new(Duration::from_millis(5)).await;
        }
        assert_eq!(
            handle.written_reports().len(),
            1,
            "the re-ask went out with the first ask's reply still owed"
        );
        assert_eq!(pending_len(&channel), 0);

        // The first ask's reply lands late: discarded, not handed to the
        // re-ask.
        let first_reply = short_msg(0x11);
        handle.send_incoming(first_reply).await;
        wait_for_event_count(&events, 1).await;
        assert_eq!(events.lock().unwrap()[0], (first_reply, false));

        // Only now does the re-ask go out, answered by its own reply.
        let retry_reply = short_msg(0x12);
        handle.queue_response(retry_reply);
        assert_eq!(retry.await.unwrap(), retry_reply);
        assert_eq!(handle.written_reports().len(), 2);
        assert_pending_empty(&channel);
        assert_eq!(stale_len(&channel), 0);
    });
}

/// A query of immutable state may say so and skip the wait: re-asking an
/// abandoned request byte for byte under [`AbandonedReply::AdoptIdentical`]
/// — what a feature-table read does when the link drops a report — goes out
/// at once and is answered by whichever reply comes first. The other is then
/// discarded rather than handed to a later, different request with the same
/// header.
#[test]
fn an_identical_re_ask_may_adopt_the_outstanding_reply_when_it_says_so() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let listener_events = Arc::clone(&events);
        channel.add_msg_listener(move |msg, matched| {
            listener_events.lock().unwrap().push((msg, matched));
        });

        let err = channel
            .send_with_timeout(short_msg(0x10), |_| true, Duration::from_millis(25))
            .await
            .unwrap_err();
        assert!(matches!(err, ChannelError::Timeout));
        assert_eq!(stale_len(&channel), 1);

        // The identical re-ask goes straight out, adopting the owed reply.
        let mut retry = Box::pin(channel.send_with(
            short_msg(0x10),
            |_| true,
            SEND_RESPONSE_TIMEOUT,
            AbandonedReply::AdoptIdentical,
        ));
        assert!(futures::poll!(retry.as_mut()).is_pending());
        assert_eq!(handle.written_reports().len(), 2, "the re-ask waited");
        assert_eq!(pending_len(&channel), 1);
        assert_eq!(stale_len(&channel), 0);

        // The first send's reply lands late and answers the re-ask; the
        // re-ask's own reply is now the one owed.
        let first_reply = short_msg(0x11);
        handle.send_incoming(first_reply).await;
        assert_eq!(retry.await.unwrap(), first_reply);
        assert_pending_empty(&channel);
        assert_eq!(stale_len(&channel), 1);

        // A different question under the same header waits for it...
        let mut other = Box::pin(channel.send(same_header_msg(0x10, 0xa2), |_| true));
        for _ in 0..5 {
            assert!(futures::poll!(other.as_mut()).is_pending());
            futures_timer::Delay::new(Duration::from_millis(5)).await;
        }
        assert_eq!(
            handle.written_reports().len(),
            2,
            "the other request went out early"
        );

        // ...and goes out once it has been discarded.
        let retry_reply = short_msg(0x12);
        handle.send_incoming(retry_reply).await;
        wait_for_event_count(&events, 2).await;
        assert_eq!(events.lock().unwrap()[1], (retry_reply, false));
        let other_reply = short_msg(0x13);
        handle.queue_response(other_reply);
        for _ in 0..20 {
            if handle.written_reports().len() == 3 {
                break;
            }
            assert!(futures::poll!(other.as_mut()).is_pending());
            futures_timer::Delay::new(Duration::from_millis(5)).await;
        }
        assert_eq!(other.await.unwrap(), other_reply);
        assert_eq!(handle.written_reports().len(), 3);
        assert_pending_empty(&channel);
        assert_eq!(stale_len(&channel), 0);
    });
}

/// Adoption is the re-ask's choice, not the abandoned request's: an
/// identical re-ask that does not opt in is quarantined like any other.
/// (The byte-identical case with the default policy is
/// [`an_identical_re_ask_waits_for_the_quarantined_reply`]; this pins that
/// the abandoned request's own policy plays no part.)
#[test]
fn adoption_is_decided_by_the_re_ask_not_the_abandoned_request() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        let err = channel
            .send_with(
                short_msg(0x10),
                |_| true,
                Duration::from_millis(25),
                AbandonedReply::AdoptIdentical,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ChannelError::Timeout));
        assert_eq!(stale_len(&channel), 1);

        let mut retry = Box::pin(channel.send(short_msg(0x10), |_| true));
        for _ in 0..5 {
            assert!(futures::poll!(retry.as_mut()).is_pending());
            futures_timer::Delay::new(Duration::from_millis(5)).await;
        }
        assert_eq!(
            handle.written_reports().len(),
            1,
            "a quarantining re-ask went out on the strength of the abandoned request's policy"
        );
    });
}

/// `AdjustableDpi` functions and payloads for
/// [`a_re_asked_read_cannot_adopt_a_reply_from_before_an_intervening_write`].
const GET_SENSOR_DPI: u8 = 2;

const SET_SENSOR_DPI: u8 = 3;

/// 800 dpi.
const BEFORE_WRITE: [u8; 3] = [0x00, 0x03, 0x20];

/// 1600 dpi.
const AFTER_WRITE: [u8; 3] = [0x00, 0x06, 0x40];

/// Why byte equality is not reply equivalence: a DPI read abandoned before
/// its reply, a DPI write acknowledged, then the read re-asked — all inside
/// one grace window. The re-ask must read the written value, not take the
/// first read's late reply carrying the value from before the write.
#[test]
fn a_re_asked_read_cannot_adopt_a_reply_from_before_an_intervening_write() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let listener_events = Arc::clone(&events);
        channel.add_msg_listener(move |msg, matched| {
            listener_events.lock().unwrap().push((msg, matched));
        });

        let sw_id = channel.get_sw_id();
        let dpi = move |function: u8, payload: [u8; 3]| {
            v20::Message::Short(
                v20::MessageHeader {
                    device_index: 0x01,
                    feature_index: 0x0a,
                    function_id: nibble::U4::from_lo(function),
                    software_id: sw_id,
                },
                payload,
            )
        };

        // The read goes out, and its caller gives up before the reply lands.
        let mut read = Box::pin(channel.send_v20(dpi(GET_SENSOR_DPI, [0; 3])));
        assert!(futures::poll!(read.as_mut()).is_pending());
        assert_eq!(handle.written_reports().len(), 1);
        drop(read);
        assert_eq!(stale_len(&channel), 1);

        // A write under its own header is unaffected, and acknowledged.
        handle.queue_response(dpi(SET_SENSOR_DPI, AFTER_WRITE).into());
        channel
            .send_v20(dpi(SET_SENSOR_DPI, AFTER_WRITE))
            .await
            .unwrap();
        assert_eq!(handle.written_reports().len(), 2);
        wait_for_event_count(&events, 1).await;

        // The read re-asked byte for byte waits: the first read's reply is
        // still owed, and it is not this read's answer.
        let mut reread = Box::pin(channel.send_v20(dpi(GET_SENSOR_DPI, [0; 3])));
        for _ in 0..5 {
            assert!(futures::poll!(reread.as_mut()).is_pending());
            futures_timer::Delay::new(Duration::from_millis(5)).await;
        }
        assert_eq!(
            handle.written_reports().len(),
            2,
            "the re-asked read went out with the first read's reply still owed"
        );

        // The first read's reply — the value from before the write — lands:
        // discarded.
        let stale_reply: HidppMessage = dpi(GET_SENSOR_DPI, BEFORE_WRITE).into();
        handle.send_incoming(stale_reply).await;
        wait_for_event_count(&events, 2).await;
        assert_eq!(events.lock().unwrap()[1], (stale_reply, false));

        // The re-ask goes out and reads what was written.
        handle.queue_response(dpi(GET_SENSOR_DPI, AFTER_WRITE).into());
        let answer = reread.await.unwrap();
        assert_eq!(answer.extend_payload()[..3], AFTER_WRITE);
        assert_eq!(handle.written_reports().len(), 3);
        assert_pending_empty(&channel);
        assert_eq!(stale_len(&channel), 0);
    });
}

/// Two same-header requests issued together are answered in order, each by
/// its own reply — the serialisation costs nothing but the wait.
#[test]
fn same_header_requests_are_answered_in_order() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        let first_reply = short_msg(0x31);
        let second_reply = short_msg(0x32);
        handle.queue_response(first_reply);
        handle.queue_response(second_reply);

        let (first, second) = futures::join!(
            channel.send(short_msg(0x30), |_| true),
            channel.send(short_msg(0x30), |_| true),
        );

        assert_eq!(first.unwrap(), first_reply);
        assert_eq!(second.unwrap(), second_reply);
        assert_eq!(handle.written_reports().len(), 2);
        assert_pending_empty(&channel);
    });
}
