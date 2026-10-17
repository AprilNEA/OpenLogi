//! The read loop's pacing of a transport whose reads keep failing.

use super::*;

#[test]
fn read_retry_delay_doubles_from_the_minimum_up_to_the_cap() {
    assert_eq!(read_retry_delay(1), READ_RETRY_DELAY_MIN);
    assert_eq!(read_retry_delay(2), READ_RETRY_DELAY_MIN * 2);
    assert_eq!(read_retry_delay(3), READ_RETRY_DELAY_MIN * 4);
    for failures in 1..64 {
        assert!(read_retry_delay(failures) <= read_retry_delay(failures + 1));
    }
    assert_eq!(read_retry_delay(64), READ_RETRY_DELAY_MAX);
    assert_eq!(read_retry_delay(u32::MAX), READ_RETRY_DELAY_MAX);
}

#[test]
fn failing_reads_are_paced_and_the_next_report_still_arrives() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        handle.fail_reads();
        let channel = channel_with_reader(raw).await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_for_listener = Arc::clone(&events);
        channel.add_msg_listener(move |msg, matched| {
            events_for_listener.lock().unwrap().push((msg, matched));
        });

        futures_timer::Delay::new(Duration::from_millis(200)).await;
        // Pauses of 1, 2, 4 … 64 ms and then 100 ms leave room for about
        // nine reads in 200 ms; an unpaced loop makes millions.
        let attempts = handle.read_attempts();
        assert!(attempts >= 2, "the loop stopped retrying: {attempts}");
        assert!(attempts <= 20, "failed reads were not paced: {attempts}");

        handle.recover_reads();
        handle.send_incoming(short_msg(0x30)).await;
        wait_for_event_count(&events, 1).await;
        assert_eq!(events.lock().unwrap()[0], (short_msg(0x30), false));
    });
}
