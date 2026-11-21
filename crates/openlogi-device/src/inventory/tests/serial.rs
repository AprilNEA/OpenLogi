//! A paired device's serial-number read, and the identity each scan gives it.

use std::sync::atomic::{AtomicUsize, Ordering};

use hidpp::channel::HidppChannel;

use super::super::cache::{ProbeTarget, probe_or_reuse};
use super::super::identity::SerialRead;
use super::*;
use crate::channel::scripted::{feature_error, within_deadline};

const SLOT: u8 = 1;
const UNIT_ID: [u8; 4] = [0xa3, 0x93, 0xca, 0xe0];
const SERIAL: &str = "1234ABCD";
const MODEL_ID: u16 = 0xc09d;
const EXTENDED_MODEL_ID: u8 = 1;
const RECEIVER_UID: &str = "RECEIVER";

/// How a scripted device answers the reads of its serial number.
enum SerialReads {
    Answer,
    FailOnce,
    FailAlways,
}

impl SerialReads {
    fn failures(self) -> usize {
        match self {
            Self::Answer => 0,
            Self::FailOnce => 1,
            Self::FailAlways => usize::MAX,
        }
    }
}

/// What [`paired_device`] answers DeviceInformation with, beside `serial`.
fn answered_model(serial: Option<&str>) -> DeviceModelInfo {
    DeviceModelInfo {
        entity_count: 3,
        serial_number: serial.map(str::to_string),
        unit_id: UNIT_ID,
        transports: DeviceTransports {
            equad: true,
            ..DeviceTransports::default()
        },
        model_ids: [MODEL_ID, 0, 0],
        extended_model_id: EXTENDED_MODEL_ID,
    }
}

/// A HID++ 2.0 device whose DeviceInformation reports [`UNIT_ID`] and a serial
/// number, read as `reads` says.
async fn paired_device(reads: SerialReads) -> Arc<HidppChannel> {
    const NAME: &[u8] = b"Paired mouse";
    let failures_left = AtomicUsize::new(reads.failures());
    let (raw, _) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        let mut response = vec![0; 20];
        response[..4].copy_from_slice(&request[..4]);
        response[0] = 0x11;
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            (0, 0) => {
                response[4] = match u16::from_be_bytes([request[4], request[5]]) {
                    0x0001 => 1,
                    0x0003 => 2,
                    _ => 0,
                };
            }
            (1, 0) => response[4] = 2,
            (1, 1) => response[4..6]
                .copy_from_slice(&[0x0001u16, 0x0003][usize::from(request[4]) - 1].to_be_bytes()),
            (2, 0) => {
                response[4] = 3;
                response[5..9].copy_from_slice(&UNIT_ID);
                response[10] = 1 << 2;
                response[11..13].copy_from_slice(&MODEL_ID.to_be_bytes());
                response[17] = EXTENDED_MODEL_ID;
                response[18] = 0x01;
            }
            (2, 2) => {
                let lost = failures_left
                    .try_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                        left.checked_sub(1)
                    })
                    .is_ok();
                if lost {
                    return Some(feature_error(request, 0x08));
                }
                response[4..4 + SERIAL.len()].copy_from_slice(SERIAL.as_bytes());
            }
            // The Unifying receiver's name register, read for a slot whose
            // device exposes no marketing name.
            (0x83, 0xb) => {
                response[4] = request[4];
                response[5] = u8::try_from(NAME.len()).unwrap();
                response[6..6 + NAME.len()].copy_from_slice(NAME);
            }
            _ => panic!("unexpected request: {request:02x?}"),
        }
        Some(response)
    });
    within_deadline("opening the channel", scripted_channel(raw)).await
}

/// What one inventory scan learned about a paired device.
struct Scan {
    model_info: Option<DeviceModelInfo>,
    outcome: CacheOutcome,
}

/// The receiver a device is paired on, which decides how a scan reaches it.
#[derive(Clone, Copy)]
enum Receiver {
    Bolt,
    Unifying,
}

impl Receiver {
    /// One inventory scan of the slot with nothing memoized for it yet.
    async fn scan(self, channel: &Arc<HidppChannel>) -> Scan {
        match self {
            Self::Bolt => scan_bolt_slot(channel, None).await,
            Self::Unifying => scan_unifying_slot(channel, None).await,
        }
    }
}

/// One inventory scan of the Bolt slot, with `known` memoized for it.
async fn scan_bolt_slot(channel: &Arc<HidppChannel>, known: Option<&Cached>) -> Scan {
    let (probe, outcome) = within_deadline(
        "the Bolt slot scan",
        probe_or_reuse(
            channel,
            ProbeTarget {
                index: SLOT,
                registers: None,
            },
            Some(CacheKey::Bolt { unit_id: UNIT_ID }),
            known,
            true,
            Instant::now(),
            None,
        ),
    )
    .await;
    Scan {
        model_info: probe.identity.into_model(),
        outcome,
    }
}

/// One inventory scan of the Unifying slot, with `known` memoized for it.
async fn scan_unifying_slot(channel: &Arc<HidppChannel>, known: Option<Cached>) -> Scan {
    let key = CacheKey::UnifyingSlot {
        receiver_uid: RECEIVER_UID.to_string(),
        slot: SLOT,
    };
    let cache: HashMap<_, _> = known.map(|entry| (key, entry)).into_iter().collect();
    let pass = PassContext {
        cache: &cache,
        now: Instant::now(),
        subscriptions: None,
        timeouts: &ProbeTimeouts::DEFAULT,
    };
    let (device, outcome) = within_deadline(
        "the Unifying slot scan",
        probe_unifying_slot(
            channel,
            &online_unifying_arrival(),
            RECEIVER_UID,
            &host_lock::ReceiverRegisterPhase::uncontended(),
            pass,
        ),
    )
    .await
    .expect("an online slot surfaces");
    Scan {
        model_info: device.model_info,
        outcome,
    }
}

/// A cache entry holding `identity`, due for a refresh.
fn entry_due_for_refresh(identity: ProbedIdentity) -> Cached {
    let mut entry = cache_entry();
    entry.probe = probed(identity);
    entry.probed_at = Instant::now()
        .checked_sub(REFRESH_INTERVAL + Duration::from_nanos(1))
        .expect("the process has been up longer than the refresh interval");
    entry
}

/// An online encrypted mouse announcing itself in [`SLOT`] of a Unifying
/// receiver.
fn online_unifying_arrival() -> hidpp::receiver::unifying::DeviceConnection {
    let message = Message::Short(
        MessageHeader {
            device_index: SLOT,
            sub_id: 0x41,
        },
        [0x04, 0x22, 0x69, 0x40],
    );
    let Some(UnifyingEvent::DeviceConnection(event)) = decode_notification(&message) else {
        panic!("expected a device-connection event");
    };
    assert!(event.online, "bit 6 clear must decode as online");
    event
}

async fn a_serial_that_reads_gives_the_device_its_identity(receiver: Receiver) {
    let channel = paired_device(SerialReads::Answer).await;

    let scan = receiver.scan(&channel).await;

    assert_eq!(scan.model_info, Some(answered_model(Some(SERIAL))));
    assert!(
        matches!(scan.outcome, CacheOutcome::Fresh(..)),
        "a complete identity is memoized"
    );
}

async fn a_serial_lost_once_is_read_again_by_the_next_scan(receiver: Receiver) {
    let channel = paired_device(SerialReads::FailOnce).await;

    let scan = receiver.scan(&channel).await;
    assert_eq!(scan.model_info, None, "no identity was read");
    assert!(
        matches!(scan.outcome, CacheOutcome::Seen(..)),
        "a probe that lost the serial is not memoized"
    );

    let scan = receiver.scan(&channel).await;
    assert_eq!(scan.model_info, Some(answered_model(Some(SERIAL))));
    assert!(matches!(scan.outcome, CacheOutcome::Fresh(..)));
}

#[tokio::test]
async fn a_bolt_device_whose_serial_reads_keeps_its_identity() {
    a_serial_that_reads_gives_the_device_its_identity(Receiver::Bolt).await;
}

#[tokio::test]
async fn a_unifying_device_whose_serial_reads_keeps_its_identity() {
    a_serial_that_reads_gives_the_device_its_identity(Receiver::Unifying).await;
}

#[tokio::test]
async fn a_bolt_serial_read_lost_once_is_read_again_on_the_next_scan() {
    a_serial_lost_once_is_read_again_by_the_next_scan(Receiver::Bolt).await;
}

#[tokio::test]
async fn a_unifying_serial_read_lost_once_is_read_again_on_the_next_scan() {
    a_serial_lost_once_is_read_again_by_the_next_scan(Receiver::Unifying).await;
}

async fn a_serial_that_never_reads_leaves_the_device_unidentified(receiver: Receiver) {
    // Its unit id would key it `unit:<hex>`, a key it stops answering to once
    // its serial is read. It gets no identity for as long as the read keeps
    // failing.
    let channel = paired_device(SerialReads::FailAlways).await;

    for _ in 0..3 {
        let scan = receiver.scan(&channel).await;
        assert_eq!(scan.model_info, None, "no identity was read");
        assert!(
            matches!(scan.outcome, CacheOutcome::Seen(..)),
            "a probe with no identity is never memoized"
        );
    }
}

#[tokio::test]
async fn a_bolt_device_whose_serial_never_reads_stays_unidentified() {
    a_serial_that_never_reads_leaves_the_device_unidentified(Receiver::Bolt).await;
}

#[tokio::test]
async fn a_unifying_device_whose_serial_never_reads_stays_unidentified() {
    a_serial_that_never_reads_leaves_the_device_unidentified(Receiver::Unifying).await;
}

/// The identity a scripted device read in full before losing its serial. It
/// differs from what [`answered_model`] gives, so a model spliced from the
/// refresh would not compare equal, but the unit and model are the device's.
fn earlier_reading() -> ProbedIdentity {
    ProbedIdentity::Known(DeviceModelInfo {
        model_ids: [MODEL_ID, 0, 0],
        extended_model_id: EXTENDED_MODEL_ID,
        ..model(UNIT_ID, Some(SERIAL))
    })
}

/// A due refresh that loses the serial read is answered by the identity the
/// cache read in full, rather than by no identity: the device keeps the key
/// it was managed under (#482).
#[tokio::test]
async fn a_known_bolt_device_that_loses_its_serial_keeps_its_cached_identity() {
    let channel = paired_device(SerialReads::FailAlways).await;
    let known = entry_due_for_refresh(earlier_reading());

    let scan = scan_bolt_slot(&channel, Some(&known)).await;

    assert_eq!(
        scan.model_info.as_ref(),
        known.probe.identity.model(),
        "the cache answers for the lost read"
    );
    let CacheOutcome::Fresh(_, memoized) = scan.outcome else {
        panic!("a backfilled identity is complete, so the refresh is memoized");
    };
    assert_eq!(memoized.probe.identity, known.probe.identity);
}

/// The same recovery through a slot key, which proves nothing by itself: the
/// unit the lost read did get is what lets the cache answer.
#[tokio::test]
async fn a_known_unifying_device_that_loses_its_serial_keeps_its_cached_identity() {
    let channel = paired_device(SerialReads::FailAlways).await;
    let known = entry_due_for_refresh(earlier_reading());
    let expected = known.probe.identity.clone();

    let scan = scan_unifying_slot(&channel, Some(known)).await;

    assert_eq!(
        scan.model_info.as_ref(),
        expected.model(),
        "the cache answers for the lost read"
    );
    let CacheOutcome::Fresh(_, memoized) = scan.outcome else {
        panic!("a backfilled identity is complete, so the refresh is memoized");
    };
    assert_eq!(memoized.probe.identity, expected);
}

/// A Unifying cache entry is keyed by its slot, so it does not prove the
/// device answering there is the one it read. A replacement of the same model
/// whose serial read fails said which unit it is, and must not be published
/// under the previous occupant's key. The refused read takes over the entry,
/// so a later walk that fails cannot serve the previous occupant either.
#[tokio::test]
async fn unifying_replacement_with_lost_serial() {
    let channel = paired_device(SerialReads::FailAlways).await;
    let previous = entry_due_for_refresh(ProbedIdentity::Known(DeviceModelInfo {
        unit_id: [9, 9, 9, 9],
        serial_number: None,
        ..answered_model(None)
    }));

    let scan = scan_unifying_slot(&channel, Some(previous)).await;

    assert_eq!(scan.model_info, None, "no identity was read");
    let CacheOutcome::Fresh(_, memoized) = scan.outcome else {
        panic!("the slot's entry describes its new occupant from now on");
    };
    assert_eq!(
        memoized.probe.identity,
        ProbedIdentity::answered(answered_model(None), SerialRead::Lost)
    );
}
