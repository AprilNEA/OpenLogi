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
    SwIdPolicy::Leased {
        id: RequestSwId::new(U4::from_lo(id)).unwrap(),
        lease: Box::new(RecordingLease { id, free }),
    }
}

fn record_sw_id_release(id: u8) {
    RELEASED_SW_IDS.lock().unwrap().push(id);
}

fn record_ordered_sw_id_release(_id: u8) {
    ORDERING_RELEASE_AFTER_RAW_DROP.store(
        ORDERING_RAW_CHANNEL_DROPPED.load(Ordering::SeqCst),
        Ordering::SeqCst,
    );
    ORDERING_RELEASE_COUNT.fetch_add(1, Ordering::SeqCst);
}
