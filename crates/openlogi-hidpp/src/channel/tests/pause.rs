//! Request pauses: a raw transaction that keeps HID++ requests off the wire.
//!
//! The MX Creative Keypad drops a root `getFeature` that is in flight when a
//! key image arrives, and a few seconds later it can restart (#2281). The key
//! image write takes a pause so that the two never overlap.

use super::*;

/// How long a test waits for a pause it expects to be taken before it fails,
/// instead of hanging the run. A request is bounded by its own response
/// timeout; taking a pause has none.
const DEADLINE: Duration = Duration::from_secs(5);

/// `future`'s output, or a failure naming `what` once [`DEADLINE`] passes.
/// Runtime-agnostic, like these tests' executor.
async fn within<F: std::future::Future>(future: F, what: &str) -> F::Output {
    let future = std::pin::pin!(future);
    let deadline = futures_timer::Delay::new(DEADLINE);
    match futures::future::select(future, deadline).await {
        futures::future::Either::Left((output, _)) => output,
        futures::future::Either::Right(((), _)) => {
            panic!("expected {what} within {DEADLINE:?}")
        }
    }
}

/// Poll `future` a few times with short sleeps between, and require it to
/// stay pending.
async fn assert_stays_pending<F: std::future::Future + Unpin>(future: &mut F) {
    for _ in 0..5 {
        assert!(futures::poll!(&mut *future).is_pending());
        futures_timer::Delay::new(Duration::from_millis(5)).await;
    }
}

/// Poll `future` until `handle` has seen `count` written reports.
async fn wait_for_writes<F: std::future::Future + Unpin>(
    handle: &super::super::mock::MockRawHidHandle,
    future: &mut F,
    count: usize,
) {
    for _ in 0..20 {
        if handle.written_reports().len() >= count {
            return;
        }
        let _ = futures::poll!(&mut *future);
        futures_timer::Delay::new(Duration::from_millis(5)).await;
    }
    panic!("expected {count} written reports");
}

#[test]
fn a_request_waits_while_a_pause_holds_the_channel() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;
        // Nothing is in flight, so the pause is taken at once.
        let pause = within(channel.pause_requests(), "the free channel's pause").await;

        let reply = short_msg(0x11);
        let mut request =
            Box::pin(channel.send(short_msg(0x10), move |candidate| *candidate == reply));
        assert_stays_pending(&mut request).await;
        assert!(
            handle.written_reports().is_empty(),
            "the request went out during the pause"
        );
        assert_pending_empty(&channel);

        drop(pause);
        wait_for_writes(&handle, &mut request, 1).await;
        handle.send_incoming(reply).await;
        assert_eq!(request.await.unwrap(), reply);
    });
}

#[test]
fn a_pause_waits_for_the_requests_in_flight() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;
        let reply = short_msg(0x11);
        let mut request =
            Box::pin(channel.send(short_msg(0x10), move |candidate| *candidate == reply));
        assert!(futures::poll!(request.as_mut()).is_pending());
        assert_eq!(handle.written_reports().len(), 1);

        let mut pause = Box::pin(channel.pause_requests());
        assert_stays_pending(&mut pause).await;

        handle.send_incoming(reply).await;
        assert_eq!(request.await.unwrap(), reply);
        let _pause = within(pause, "the pause once the request was answered").await;
        assert_pending_empty(&channel);
    });
}

/// A request that comes after a pause started waiting parks behind it, so a
/// steady stream of requests cannot keep the pause from ever being taken.
#[test]
fn a_waiting_pause_holds_back_new_requests() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;
        let first_reply = short_msg(0x11);
        let mut first =
            Box::pin(channel.send(short_msg(0x10), move |candidate| *candidate == first_reply));
        assert!(futures::poll!(first.as_mut()).is_pending());

        let mut pause = Box::pin(channel.pause_requests());
        assert!(futures::poll!(pause.as_mut()).is_pending());

        // A different header, which no in-flight request blocks.
        let later_reply = short_msg(0x21);
        let mut later =
            Box::pin(channel.send(short_msg(0x20), move |candidate| *candidate == later_reply));
        assert_stays_pending(&mut later).await;
        assert_eq!(handle.written_reports().len(), 1);

        handle.send_incoming(first_reply).await;
        assert_eq!(first.await.unwrap(), first_reply);
        let pause = within(pause, "the pause once the first request was answered").await;
        assert_stays_pending(&mut later).await;
        assert_eq!(
            handle.written_reports().len(),
            1,
            "the later request went out during the pause"
        );

        drop(pause);
        wait_for_writes(&handle, &mut later, 2).await;
        handle.send_incoming(later_reply).await;
        assert_eq!(later.await.unwrap(), later_reply);
    });
}

/// Giving up on a pause before it is taken releases the requests parked
/// behind it.
#[test]
fn dropping_a_waiting_pause_lets_requests_go() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;
        let first_reply = short_msg(0x11);
        let mut first =
            Box::pin(channel.send(short_msg(0x10), move |candidate| *candidate == first_reply));
        assert!(futures::poll!(first.as_mut()).is_pending());

        let mut pause = Box::pin(channel.pause_requests());
        assert!(futures::poll!(pause.as_mut()).is_pending());
        let later_reply = short_msg(0x21);
        let mut later =
            Box::pin(channel.send(short_msg(0x20), move |candidate| *candidate == later_reply));
        assert_stays_pending(&mut later).await;

        drop(pause);
        wait_for_writes(&handle, &mut later, 2).await;
        assert_eq!(pending_len(&channel), 2);
        handle.send_incoming(later_reply).await;
        assert_eq!(later.await.unwrap(), later_reply);
        handle.send_incoming(first_reply).await;
        assert_eq!(first.await.unwrap(), first_reply);
    });
}
