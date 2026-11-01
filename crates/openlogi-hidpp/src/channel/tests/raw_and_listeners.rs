//! Raw report writes and listener dispatch.

use super::super::mock::raw_report as raw_report_bytes;
use super::*;

#[test]
fn raw_report_write_forwards_exact_bytes_and_length() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;
        let report = [0x12; MAX_INPUT_REPORT_LENGTH];

        let written = channel.write_raw_report(&report).await.unwrap();

        assert_eq!(written, report.len());
        assert_eq!(handle.written_reports(), [report.to_vec()]);
    });
}

/// The keypad's image write is the widest output any node takes: a full
/// 4095-byte report reaches the transport intact, and an observer sees all of
/// it rather than a prefix bounded by the input buffer.
#[test]
fn raw_report_write_carries_and_observes_the_widest_output_report() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let observations = Arc::new(Mutex::new(Vec::new()));
        let observer_observations = Arc::clone(&observations);
        let observer: Arc<dyn ChannelObserver> = Arc::new(move |observation| {
            observer_observations.lock().unwrap().push(observation);
        });
        let channel = HidppChannel::from_raw_channel_with_observer(raw, observer)
            .await
            .expect("the mock transport speaks HID++");
        let mut report = vec![0xa5; MAX_RAW_WRITE_LENGTH];
        report[0] = 0x14;

        let written = channel.write_raw_report(&report).await.unwrap();

        assert_eq!(written, MAX_RAW_WRITE_LENGTH);
        assert_eq!(handle.written_reports(), [report.clone()]);
        assert!(observations.lock().unwrap().iter().any(|observation| {
            matches!(
                observation,
                ChannelObservation::OutgoingReport { report: outgoing, .. }
                    if outgoing.as_bytes() == report.as_slice()
            )
        }));
    });
}

#[test]
fn raw_report_write_rejects_empty_and_oversized_inputs_without_io() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        let empty = channel.write_raw_report(&[]).await.unwrap_err();
        let oversized = channel
            .write_raw_report(&[0; MAX_RAW_WRITE_LENGTH + 1])
            .await
            .unwrap_err();

        assert!(matches!(empty, ChannelError::InvalidRawReportLength(0)));
        assert!(matches!(
            oversized,
            ChannelError::InvalidRawReportLength(4096)
        ));
        assert!(handle.written_reports().is_empty());
    });
}

#[test]
fn raw_report_write_times_out_when_the_transport_parks() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        handle.park_writes();
        let channel = channel_with_reader(raw).await;
        let started = Instant::now();

        let error = channel
            .write_raw_report_with_timeout(&[LONG_REPORT_ID], Duration::from_millis(25))
            .await
            .unwrap_err();

        assert!(matches!(error, ChannelError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(1));
    });
}

#[test]
fn listener_can_remove_another_listener_during_dispatch() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        let removed_listener_calls = Arc::new(AtomicUsize::new(0));
        let removing_listener_calls = Arc::new(AtomicUsize::new(0));

        let removed_listener_calls_for_listener = Arc::clone(&removed_listener_calls);
        let removed_hdl = channel.add_msg_listener(move |_, _| {
            removed_listener_calls_for_listener.fetch_add(1, Ordering::SeqCst);
        });

        let channel_for_listener = Arc::clone(&channel);
        let removing_listener_calls_for_listener = Arc::clone(&removing_listener_calls);
        channel.add_msg_listener(move |_, _| {
            removing_listener_calls_for_listener.fetch_add(1, Ordering::SeqCst);
            channel_for_listener.remove_msg_listener(removed_hdl);
        });

        handle.send_incoming(short_msg(0x20)).await;
        wait_for_atomic_count(&removing_listener_calls, 1).await;
        wait_for_atomic_count(&removed_listener_calls, 1).await;

        handle.send_incoming(short_msg(0x21)).await;
        wait_for_atomic_count(&removing_listener_calls, 2).await;

        assert_eq!(removed_listener_calls.load(Ordering::SeqCst), 1);
    });
}

async fn wait_for_atomic_count(count: &AtomicUsize, expected: usize) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(1) {
        if count.load(Ordering::SeqCst) >= expected {
            return;
        }
        futures_timer::Delay::new(Duration::from_millis(10)).await;
    }

    panic!("timed out waiting for atomic count {expected}");
}

/// A grid-key vendor PDU of the MX Creative Keypad: report ID `0x13`, the
/// `0xff` broadcast address, report type `0x02`, then `00 [seq] 01 [key]`
/// padded to the 32 bytes the node actually carries.
pub(crate) fn grid_key_pdu(seq: u8, key: u8) -> Vec<u8> {
    let mut pdu = vec![0x13, 0xff, 0x02, 0x00, seq, 0x01, key];
    pdu.resize(32, 0);
    pdu
}

/// Raw listeners exist precisely for the reports the message path filters away
/// — the keypad's `0x13` PDUs share the node with HID++ and never parse as
/// messages. They must arrive whole (the read buffer must not truncate them),
/// alongside reports that also reach the message listener.
#[test]
fn raw_listener_gets_every_report_intact_including_foreign_vendor_pdus() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        let short = short_msg(0x21);

        let seen = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        channel.add_raw_report_listener({
            let seen = Arc::clone(&seen);
            let calls = Arc::clone(&calls);
            move |report| {
                seen.lock().unwrap().push(report.to_vec());
                calls.fetch_add(1, Ordering::SeqCst);
            }
        });
        let msg_calls = Arc::new(AtomicUsize::new(0));
        channel.add_msg_listener({
            let msg_calls = Arc::clone(&msg_calls);
            move |_, _| {
                msg_calls.fetch_add(1, Ordering::SeqCst);
            }
        });

        handle.send_incoming_raw(grid_key_pdu(0x09, 0x02)).await;
        wait_for_atomic_count(&calls, 1).await;
        handle.send_incoming(short).await;
        wait_for_atomic_count(&calls, 2).await;

        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[grid_key_pdu(0x09, 0x02), raw_report_bytes(short)]
        );
        // Only the message parses as HID++: a vendor PDU is never delivered
        // to the message path.
        assert_eq!(msg_calls.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn raw_listener_guard_drop_stops_delivery() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        let calls = Arc::new(AtomicUsize::new(0));
        let liveness = Arc::new(AtomicUsize::new(0));
        channel.add_msg_listener({
            let liveness = Arc::clone(&liveness);
            move |_, _| {
                liveness.fetch_add(1, Ordering::SeqCst);
            }
        });

        let guard = {
            let calls = Arc::clone(&calls);
            channel.add_raw_report_listener_guarded(move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
            })
        };

        handle.send_incoming_raw(grid_key_pdu(0x01, 0x01)).await;
        wait_for_atomic_count(&calls, 1).await;

        drop(guard);

        // FIFO on the one read thread: once the message after this PDU is
        // delivered, the PDU cannot still be in flight, so `calls` has had its
        // chance to reach 2.
        handle.send_incoming_raw(grid_key_pdu(0x02, 0x01)).await;
        handle.send_incoming(short_msg(0x23)).await;
        wait_for_atomic_count(&liveness, 1).await;

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    });
}
