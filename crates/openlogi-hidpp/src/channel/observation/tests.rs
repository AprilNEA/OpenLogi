use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use super::{ChannelObservation, ChannelObserver, RequestOutcome};
use crate::channel::{
    ChannelError, HidppChannel, HidppMessage, LONG_REPORT_LENGTH,
    tests::{MockRawHidChannel, MockRawHidHandle},
};

#[test]
fn observes_post_normalization_request_bytes() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::long_only();
        let (observer, observations) = collecting_observer();
        let channel = observed_channel(raw, observer).await;
        let request = short_msg(0x10);
        let response = short_msg(0x20).widened();
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
        wait_for_observation_count(&observations, 3).await;
        let expected = raw_report_for_message(request.widened());
        assert!(observations.lock().unwrap().iter().any(|observation| {
            matches!(
                observation,
                ChannelObservation::OutgoingReport {
                    request_id: Some(1),
                    report,
                } if report.as_bytes() == expected.as_slice()
            )
        }));
        assert_eq!(handle.written_reports(), [expected]);
    });
}

#[test]
fn associates_out_of_order_responses_with_concurrent_request_ids() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let (observer, observations) = collecting_observer();
        let channel = observed_channel(raw, observer).await;
        let first_response = short_msg(0x30);
        let second_response = short_msg(0x40);

        let first = channel.send_with_timeout(
            short_msg(0x10),
            move |candidate| *candidate == first_response,
            Duration::from_secs(1),
        );
        let second = channel.send_with_timeout(
            short_msg(0x20),
            move |candidate| *candidate == second_response,
            Duration::from_secs(1),
        );
        let respond_out_of_order = async {
            wait_for_write_count(&handle, 2).await;
            handle.send_incoming(second_response).await;
            handle.send_incoming(first_response).await;
        };

        let (first, second, ()) = futures::join!(first, second, respond_out_of_order);

        assert_eq!(first.unwrap(), first_response);
        assert_eq!(second.unwrap(), second_response);
        wait_for_observation_count(&observations, 6).await;
        let observations = observations.lock().unwrap();
        assert_eq!(matching_request_id(&observations, first_response), Some(1));
        assert_eq!(matching_request_id(&observations, second_response), Some(2));
        assert!(has_outcome(&observations, 1, RequestOutcome::Succeeded));
        assert!(has_outcome(&observations, 2, RequestOutcome::Succeeded));
    });
}

#[test]
fn observes_unmatched_and_malformed_incoming_reports() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let (observer, observations) = collecting_observer();
        let _channel = observed_channel(raw, observer).await;
        let notification = short_msg(0x20);
        let malformed = vec![0x10, 0xff, 0x01];

        handle.send_incoming(notification).await;
        handle.send_incoming_raw(malformed.clone()).await;
        wait_for_observation_count(&observations, 2).await;

        let notification = raw_report_for_message(notification);
        let observations = observations.lock().unwrap();
        assert!(observations.iter().any(|observation| {
            matches!(
                observation,
                ChannelObservation::IncomingReport {
                    request_id: None,
                    report,
                } if report.as_bytes() == notification.as_slice()
            )
        }));
        assert!(observations.iter().any(|observation| {
            matches!(
                observation,
                ChannelObservation::MalformedIncomingReport { report }
                    if report.as_bytes() == malformed
            )
        }));
    });
}

#[test]
fn observes_timeout_write_failure_and_cancellation_outcomes() {
    futures::executor::block_on(async {
        let (raw, _handle) = MockRawHidChannel::new();
        let (observer, timeout_observations) = collecting_observer();
        let channel = observed_channel(raw, observer).await;
        let timeout = channel
            .send_with_timeout(short_msg(0x10), |_| false, Duration::from_millis(25))
            .await
            .unwrap_err();
        assert!(matches!(timeout, ChannelError::Timeout));
        assert!(has_outcome(
            &timeout_observations.lock().unwrap(),
            1,
            RequestOutcome::TimedOut
        ));

        let (raw, handle) = MockRawHidChannel::new();
        handle.fail_writes();
        let (observer, failure_observations) = collecting_observer();
        let channel = observed_channel(raw, observer).await;
        let failure = channel
            .send_with_timeout(short_msg(0x10), |_| false, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(matches!(failure, ChannelError::Implementation(_)));
        assert!(has_outcome(
            &failure_observations.lock().unwrap(),
            1,
            RequestOutcome::WriteFailed
        ));

        let (raw, handle) = MockRawHidChannel::new();
        handle.park_writes();
        let (observer, cancellation_observations) = collecting_observer();
        let channel = observed_channel(raw, observer).await;
        let mut request =
            Box::pin(channel.send_with_timeout(short_msg(0x10), |_| false, Duration::from_secs(1)));
        assert!(futures::poll!(request.as_mut()).is_pending());

        drop(request);

        assert!(has_outcome(
            &cancellation_observations.lock().unwrap(),
            1,
            RequestOutcome::Cancelled
        ));
    });
}

#[test]
fn observes_response_free_writes_without_request_ids() {
    futures::executor::block_on(async {
        let (raw, _handle) = MockRawHidChannel::new();
        let (observer, observations) = collecting_observer();
        let channel = observed_channel(raw, observer).await;
        let fire_and_forget = short_msg(0x10);
        let raw_report = [0x12, 0x01, 0x02, 0x03];

        channel.send_and_forget(fire_and_forget).await.unwrap();
        channel.write_raw_report(&raw_report).await.unwrap();

        let fire_and_forget_report = raw_report_for_message(fire_and_forget);
        let observations = observations.lock().unwrap();
        let outgoing: Vec<_> = observations
            .iter()
            .filter_map(|observation| match observation {
                ChannelObservation::OutgoingReport { request_id, report } => {
                    Some((*request_id, report.as_bytes()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            outgoing,
            [
                (None, fire_and_forget_report.as_slice()),
                (None, raw_report.as_slice()),
            ]
        );
    });
}

#[test]
fn observer_panic_does_not_change_request_or_shutdown() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let observer: Arc<dyn ChannelObserver> = Arc::new(|_| panic!("observer failure"));
        let channel = observed_channel(raw, observer).await;
        let response = short_msg(0x20);
        handle.queue_response(response);

        let actual = channel
            .send_with_timeout(
                short_msg(0x10),
                move |candidate| *candidate == response,
                Duration::from_secs(1),
            )
            .await
            .unwrap();

        assert_eq!(actual, response);
        drop(channel);
    });
}

async fn observed_channel(
    raw: MockRawHidChannel,
    observer: Arc<dyn ChannelObserver>,
) -> HidppChannel {
    HidppChannel::from_raw_channel_with_observer(raw, observer)
        .await
        .expect("the mock transport speaks HID++")
}

fn collecting_observer() -> (
    Arc<dyn ChannelObserver>,
    Arc<Mutex<Vec<ChannelObservation>>>,
) {
    let observations = Arc::new(Mutex::new(Vec::new()));
    let observer_observations = Arc::clone(&observations);
    let observer: Arc<dyn ChannelObserver> = Arc::new(move |observation| {
        observer_observations.lock().unwrap().push(observation);
    });
    (observer, observations)
}

fn short_msg(marker: u8) -> HidppMessage {
    HidppMessage::Short([0xff, marker, 0x10, marker, marker, marker])
}

fn raw_report_for_message(msg: HidppMessage) -> Vec<u8> {
    let mut buf = [0; LONG_REPORT_LENGTH];
    let len = msg.write_raw(&mut buf);
    buf[..len].to_vec()
}

fn matching_request_id(observations: &[ChannelObservation], response: HidppMessage) -> Option<u64> {
    let response = raw_report_for_message(response);
    observations
        .iter()
        .find_map(|observation| match observation {
            ChannelObservation::IncomingReport { request_id, report }
                if report.as_bytes() == response =>
            {
                *request_id
            }
            _ => None,
        })
}

fn has_outcome(
    observations: &[ChannelObservation],
    expected_id: u64,
    expected_outcome: RequestOutcome,
) -> bool {
    observations.iter().any(|observation| {
        matches!(
            observation,
            ChannelObservation::RequestOutcome {
                request_id,
                outcome,
            } if *request_id == expected_id && *outcome == expected_outcome
        )
    })
}

async fn wait_for_write_count(handle: &MockRawHidHandle, expected: usize) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(1) {
        if handle.written_reports().len() >= expected {
            return;
        }
        futures_timer::Delay::new(Duration::from_millis(10)).await;
    }

    panic!("timed out waiting for {expected} writes");
}

async fn wait_for_observation_count(
    observations: &Mutex<Vec<ChannelObservation>>,
    expected: usize,
) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(1) {
        if observations.lock().unwrap().len() >= expected {
            return;
        }
        futures_timer::Delay::new(Duration::from_millis(10)).await;
    }

    panic!("timed out waiting for {expected} observations");
}
