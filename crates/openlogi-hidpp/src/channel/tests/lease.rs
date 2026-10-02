//! Software-id leases: released exactly once, after the read thread and the raw channel are gone.

use super::*;

static RELEASED_SW_IDS: Mutex<Vec<u8>> = Mutex::new(Vec::new());

static ORDERING_RAW_CHANNEL_DROPPED: AtomicBool = AtomicBool::new(false);

static ORDERING_RELEASE_AFTER_RAW_DROP: AtomicBool = AtomicBool::new(false);

static ORDERING_RELEASE_COUNT: AtomicUsize = AtomicUsize::new(0);

#[test]
fn replacing_and_dropping_leased_policies_releases_each_exactly_once() {
    futures::executor::block_on(async {
        RELEASED_SW_IDS.lock().unwrap().clear();
        let (raw, _handle) = MockRawHidChannel::new();
        let mut channel = channel_with_reader(raw).await;

        channel.set_sw_id_policy(leased_policy(1, record_sw_id_release));
        channel.set_sw_id_policy(leased_policy(2, record_sw_id_release));

        assert_eq!(*RELEASED_SW_IDS.lock().unwrap(), [1]);

        drop(channel);

        assert_eq!(*RELEASED_SW_IDS.lock().unwrap(), [1, 2]);
    });
}

#[test]
fn final_lease_releases_after_read_thread_and_raw_channel_stop() {
    futures::executor::block_on(async {
        ORDERING_RAW_CHANNEL_DROPPED.store(false, Ordering::SeqCst);
        ORDERING_RELEASE_AFTER_RAW_DROP.store(false, Ordering::SeqCst);
        ORDERING_RELEASE_COUNT.store(0, Ordering::SeqCst);
        let (raw, _handle) = MockRawHidChannel::with_drop_flag(Some(&ORDERING_RAW_CHANNEL_DROPPED));
        let mut channel = channel_with_reader(raw).await;
        channel.set_sw_id_policy(leased_policy(3, record_ordered_sw_id_release));

        drop(channel);

        assert!(ORDERING_RAW_CHANNEL_DROPPED.load(Ordering::SeqCst));
        assert!(ORDERING_RELEASE_AFTER_RAW_DROP.load(Ordering::SeqCst));
        assert_eq!(ORDERING_RELEASE_COUNT.load(Ordering::SeqCst), 1);
    });
}

/// A lease that reports its release to `free`, standing in for the transport's
/// table entry and OS lock.
struct RecordingLease {
    id: u8,
    free: fn(u8),
}

impl Drop for RecordingLease {
    fn drop(&mut self) {
        (self.free)(self.id);
    }
}

fn leased_policy(id: u8, free: fn(u8)) -> SwIdPolicy {
    leased_policy_with_secondary(id, None, free)
}

fn leased_policy_with_secondary(id: u8, secondary: Option<u8>, free: fn(u8)) -> SwIdPolicy {
    SwIdPolicy::Leased {
        id: RequestSwId::new(U4::from_lo(id)).unwrap(),
        secondary: secondary.map(|id| RequestSwId::new(U4::from_lo(id)).unwrap()),
        lease: Box::new(RecordingLease { id, free }),
    }
}

/// Two in-process consumers sharing one channel — inventory's own probing and
/// a capture session reusing an inventory-owned channel (PR #522) — that both
/// address the same device/feature/function get replies nothing on the wire
/// tells apart under the channel's *default* single (leased) software id, so
/// the second's request queues behind the first's in-flight one exactly like
/// [`super::same_header::a_request_waits_while_the_same_header_is_in_flight`]
/// does. #1128 reported this as capture sessions restarting ("capture session
/// ended unexpectedly") alternating with inventory retiring the very channel
/// capture was mid-setup on ("node probe keeps failing"): the queued request
/// occasionally outlasts its caller's own timeout, and each side reads that
/// as the other's channel having died.
///
/// A request stamped with the channel's *secondary* leased id — which
/// [`crate::feature::root::RootFeature::new_secondary`]/
/// [`crate::device::Device::new_secondary`] give capture's session-setup and
/// liveness calls — carries a different correlation key and must not queue
/// behind the primary id's in-flight request, closing that race.
#[test]
fn a_secondary_leased_sw_id_does_not_queue_behind_the_primary_ids_in_flight_request() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        // Writes park, so the first (primary-id) request stays in flight for
        // as long as the test wants.
        handle.park_writes();
        let mut channel = channel_with_reader(raw).await;
        channel.set_sw_id_policy(leased_policy_with_secondary(
            1,
            Some(2),
            discard_sw_id_release,
        ));

        let header = |software_id: u8| v20::MessageHeader {
            device_index: 0x01,
            feature_index: 0x00,
            function_id: U4::from_lo(0x00),
            software_id: U4::from_lo(software_id),
        };

        // Inventory's in-flight `getFeature`-shaped request, stamped with the
        // channel's primary leased id.
        let primary_id = channel.get_sw_id().to_lo();
        let mut inventory_probe =
            Box::pin(channel.send_v20(v20::Message::Short(header(primary_id), [0, 0, 0])));
        assert!(futures::poll!(inventory_probe.as_mut()).is_pending());
        assert_eq!(handle.written_reports().len(), 1);

        // Capture's session-setup request: same device/feature/function, but
        // stamped with the channel's secondary leased id.
        let secondary_id = channel.get_secondary_sw_id().to_lo();
        assert_ne!(
            primary_id, secondary_id,
            "the leased secondary id must differ from the primary one"
        );
        let mut capture_probe =
            Box::pin(channel.send_v20(v20::Message::Short(header(secondary_id), [0, 0, 0])));
        assert!(futures::poll!(capture_probe.as_mut()).is_pending());
        assert_eq!(
            handle.written_reports().len(),
            2,
            "a request stamped with the secondary leased id queued behind \
             the primary id's in-flight request"
        );
        assert_eq!(pending_len(&channel), 2);
    });
}

fn record_sw_id_release(id: u8) {
    RELEASED_SW_IDS.lock().unwrap().push(id);
}

/// A no-op release callback for tests that only care about queuing behavior:
/// the shared `RELEASED_SW_IDS`/[`record_sw_id_release`] used by
/// [`replacing_and_dropping_leased_policies_releases_each_exactly_once`]
/// races with it under parallel test execution (the channel's drop at the
/// end of an async test body would append to that same static).
fn discard_sw_id_release(_id: u8) {}

fn record_ordered_sw_id_release(_id: u8) {
    ORDERING_RELEASE_AFTER_RAW_DROP.store(
        ORDERING_RAW_CHANNEL_DROPPED.load(Ordering::SeqCst),
        Ordering::SeqCst,
    );
    ORDERING_RELEASE_COUNT.fetch_add(1, Ordering::SeqCst);
}
