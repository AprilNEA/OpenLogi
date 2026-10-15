//! Raw report writes and listener dispatch.

use super::*;

#[test]
fn raw_report_write_forwards_exact_bytes_and_length() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;
        let report = [0x12; MAX_RAW_REPORT_LENGTH];

        let written = channel.write_raw_report(&report).await.unwrap();

        assert_eq!(written, report.len());
        assert_eq!(handle.written_reports(), [report.to_vec()]);
    });
}

#[test]
fn raw_report_write_rejects_empty_and_oversized_inputs_without_io() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        let empty = channel.write_raw_report(&[]).await.unwrap_err();
        let oversized = channel
            .write_raw_report(&[0; MAX_RAW_REPORT_LENGTH + 1])
            .await
            .unwrap_err();

        assert!(matches!(empty, ChannelError::InvalidRawReportLength(0)));
        assert!(matches!(
            oversized,
            ChannelError::InvalidRawReportLength(65)
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
