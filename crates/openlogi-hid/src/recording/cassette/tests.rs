use std::error::Error;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use hidpp::async_trait;
use hidpp::channel::{
    HidppChannel, HidppMessage, RawHidChannel, RequestOutcome, RequestSwId, SwIdPolicy,
};
use hidpp::nibble::U4;
use openlogi_device::backend::{NodeId, NodeInfo};
use openlogi_device::fixture::{ReplayRawHidChannel, RequestMatch};
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use super::*;
use crate::recording::{
    NativeRecorder, RecordedChannel, RecordedChannelOpenOutcome, RecordedRequestFact,
};

const DEVICE: u8 = 1;
const CAPTURE_SW_ID: u8 = 3;
const REPLAY_SW_ID: u8 = 9;
const UNIT_ID: [u8; 4] = [0x12, 0x34, 0x56, 0x78];
const DEVICE_SERIAL: &[u8; 12] = b"SERIAL123456";
const BOLT_UNIQUE_ID: &[u8; 16] = b"ABCDEF0123456789";
const UNIFYING_SERIAL: [u8; 4] = [0xaa, 0xbb, 0xcc, 0xdd];

#[tokio::test]
async fn sanitizes_every_supported_identity_and_preserves_relations_and_fifo() {
    let mut channel = record_successes(identity_transactions(CAPTURE_SW_ID)).await;
    channel.requests.reverse();

    let report = channel.build_hid_cassette(metadata());
    assert!(report.is_committable(), "{:?}", report.rejections);
    let cassette = report.cassette.expect("committable cassette");

    let replacements = &report.audit.replacements;
    assert_eq!(replacements.len(), 4);
    assert_replacement(replacements, SanitizedIdentityKind::ReceiverUniqueId, 1, 16);
    assert_replacement(
        replacements,
        SanitizedIdentityKind::ReceiverSerialNumber,
        1,
        4,
    );
    assert_replacement(replacements, SanitizedIdentityKind::DeviceUnitId, 3, 4);
    assert_replacement(
        replacements,
        SanitizedIdentityKind::DeviceSerialNumber,
        1,
        12,
    );

    let bytes: Vec<_> = cassette
        .exchanges
        .iter()
        .flat_map(|exchange| exchange.response.as_deref().unwrap_or_default())
        .copied()
        .collect();
    assert!(!contains(&bytes, UNIT_ID));
    assert!(!contains(&bytes, DEVICE_SERIAL));
    assert!(!contains(&bytes, BOLT_UNIQUE_ID));
    assert!(!contains(&bytes, UNIFYING_SERIAL));

    let unit_values: Vec<_> = cassette
        .exchanges
        .iter()
        .filter_map(|exchange| {
            let response = exchange.response.as_deref()?;
            match exchange.request.as_slice() {
                [0x10, DEVICE, 5, 0x00, ..] => Some(response[5..9].to_vec()),
                [0x10, 0xff, 0x83, 0xb5, 0x51, ..] => Some(response[8..12].to_vec()),
                _ => None,
            }
        })
        .collect();
    assert_eq!(unit_values.len(), 3);
    assert!(unit_values.windows(2).all(|pair| pair[0] == pair[1]));

    let battery_percentages: Vec<_> = cassette
        .exchanges
        .iter()
        .filter(|exchange| exchange.request[2] == 6)
        .map(|exchange| exchange.response.as_ref().unwrap()[4])
        .collect();
    assert_eq!(battery_percentages, [10, 20]);

    for exchange in &cassette.exchanges {
        if exchange.request_match == RequestMatch::Hidpp20 {
            assert_eq!(exchange.request[3] & 0x0f, 0);
            let response = exchange.response.as_ref().unwrap();
            let software_id = if response[2] == 0xff {
                response[4]
            } else {
                response[3]
            };
            assert_eq!(software_id & 0x0f, 0);
        }
    }

    replay_with_different_lease(cassette).await;
}

#[tokio::test]
async fn software_id_variance_produces_the_same_candidate() {
    let first = record_successes(identity_transactions(3))
        .await
        .build_hid_cassette(metadata());
    let second = record_successes(identity_transactions(12))
        .await
        .build_hid_cassette(metadata());

    assert!(first.is_committable());
    assert!(second.is_committable());
    assert_eq!(first.cassette, second.cassette);
    assert_eq!(first.audit, second.audit);
}

#[tokio::test]
async fn hidpp10_receiver_requests_remain_exact() {
    let request = short(0xff, 0x83, 0xfb, [0, 0, 0]);
    let response = long(0xff, 0x83, 0xfb, BOLT_UNIQUE_ID);
    let report = record_successes(vec![(request.clone(), response)])
        .await
        .build_hid_cassette(metadata());

    assert!(report.is_committable(), "{:?}", report.rejections);
    let exchange = &report.cassette.unwrap().exchanges[0];
    assert_eq!(exchange.request_match, RequestMatch::Exact);
    assert_eq!(exchange.request, request);
    assert_ne!(exchange.response.as_ref().unwrap()[4..20], *BOLT_UNIQUE_ID);

    let version_ping = short(DEVICE, 0, 0x10 | CAPTURE_SW_ID, [0; 3]);
    let hidpp10_error = short(DEVICE, 0x8f, 0, [0x10 | CAPTURE_SW_ID, 0x01, 0]);
    let cross_version = record_successes(vec![(version_ping, hidpp10_error)])
        .await
        .build_hid_cassette(metadata());
    assert_rejected(
        &cross_version,
        &CassetteRejectionReason::UnsupportedCrossVersionPing,
    );
}

#[tokio::test]
async fn feature_set_learning_classifies_high_direct_device_indices_as_hidpp20() {
    let mut device_info = [0u8; 16];
    device_info[1..5].copy_from_slice(&UNIT_ID);
    let feature_set_index = 7;
    let device_information_index = 0x83;
    let battery_index = 0x4d;
    let report = record_successes(vec![
        root_mapping_for(0xff, CAPTURE_SW_ID, 0x0001, feature_set_index),
        feature_set_mapping(0xff, CAPTURE_SW_ID, feature_set_index, 0, 0x0000),
        feature_set_mapping(
            0xff,
            CAPTURE_SW_ID,
            feature_set_index,
            device_information_index,
            0x0003,
        ),
        feature_set_mapping(
            0xff,
            CAPTURE_SW_ID,
            feature_set_index,
            battery_index,
            0x1004,
        ),
        (
            short(0xff, device_information_index, CAPTURE_SW_ID, [0; 3]),
            long(0xff, device_information_index, CAPTURE_SW_ID, &device_info),
        ),
        (
            short(0xff, battery_index, CAPTURE_SW_ID, [0; 3]),
            short(0xff, battery_index, CAPTURE_SW_ID, [0x0f, 0, 0]),
        ),
    ])
    .await
    .build_hid_cassette(metadata());

    assert!(report.is_committable(), "{:?}", report.rejections);
    assert!(
        report
            .cassette
            .as_ref()
            .unwrap()
            .exchanges
            .iter()
            .all(|exchange| exchange.request_match == RequestMatch::Hidpp20)
    );
    assert_replacement(
        &report.audit.replacements,
        SanitizedIdentityKind::DeviceUnitId,
        1,
        4,
    );
}

#[tokio::test]
async fn rejects_pairing_passkeys_unknown_and_unclassified_identity_features() {
    let pairing_request = long(0xff, 0x82, 0xc1, &[0; 16]);
    let pairing_response = long(0xff, 0x82, 0xc1, &[0; 16]);
    let pairing = record_successes(vec![(pairing_request, pairing_response)])
        .await
        .build_hid_cassette(metadata());
    assert_rejected(&pairing, &CassetteRejectionReason::PairingTraffic);

    let passkey = long(0xff, 0x4d, 0, b"123456\0ABCDEF\0\0\0");
    let passkey = record_unassociated(vec![passkey]).await;
    let passkey = passkey.build_hid_cassette(metadata());
    assert_rejected(&passkey, &CassetteRejectionReason::PairingTraffic);

    let unknown_index = record_successes(vec![(
        short(DEVICE, 0x44, 0x03, [0; 3]),
        short(DEVICE, 0x44, 0x03, [1, 2, 3]),
    )])
    .await
    .build_hid_cassette(metadata());
    assert_rejected(
        &unknown_index,
        &CassetteRejectionReason::UnknownFeatureIndex {
            device_index: DEVICE,
            feature_index: 0x44,
        },
    );

    let friendly_name = record_successes(vec![
        root_mapping(CAPTURE_SW_ID, 0x0007, 7),
        (
            short(DEVICE, 7, CAPTURE_SW_ID, [0; 3]),
            short(DEVICE, 7, CAPTURE_SW_ID, [4, 8, 8]),
        ),
    ])
    .await
    .build_hid_cassette(metadata());
    assert_rejected(
        &friendly_name,
        &CassetteRejectionReason::UnsupportedIdentityFeature { feature_id: 0x0007 },
    );

    let unknown_payload = record_successes(vec![
        root_mapping(CAPTURE_SW_ID, 0x9999, 8),
        (
            short(DEVICE, 8, CAPTURE_SW_ID, [1, 2, 3]),
            short(DEVICE, 8, CAPTURE_SW_ID, [4, 5, 6]),
        ),
    ])
    .await
    .build_hid_cassette(metadata());
    assert_rejected(
        &unknown_payload,
        &CassetteRejectionReason::UnsupportedHidpp20Function {
            feature_id: 0x9999,
            function_id: 0,
        },
    );

    let unknown_hidpp10_request = short(0xff, 0x83, 0xaa, [1, 2, 3]);
    let unknown_hidpp10_error = short(0xff, 0x8f, 0x83, [0xaa, 0x02, 0]);
    let unknown_hidpp10 = record_successes(vec![(unknown_hidpp10_request, unknown_hidpp10_error)])
        .await
        .build_hid_cassette(metadata());
    assert_rejected(
        &unknown_hidpp10,
        &CassetteRejectionReason::UnsupportedHidpp10Register,
    );

    let ambiguous_mapping = record_successes(vec![
        root_mapping(CAPTURE_SW_ID, 0x0003, 5),
        root_mapping(CAPTURE_SW_ID, 0x1004, 5),
    ])
    .await
    .build_hid_cassette(metadata());
    assert_rejected(
        &ambiguous_mapping,
        &CassetteRejectionReason::AmbiguousFeatureMapping,
    );
}

#[tokio::test]
async fn rejects_malformed_unmatched_and_unproven_fire_and_forget_evidence() {
    let evidence = record_unassociated(vec![
        vec![0x10, 0xff, 0x01],
        short(DEVICE, 0, 0x01, [1, 2, 3]),
    ])
    .await;
    let report = evidence.build_hid_cassette(metadata());
    assert_rejected(&report, &CassetteRejectionReason::MalformedIncomingReport);
    assert_rejected(&report, &CassetteRejectionReason::UnmatchedIncomingReport);

    let fire_and_forget = record_fire_and_forget(short(DEVICE, 0, 0x11, [1, 2, 3])).await;
    let report = fire_and_forget.build_hid_cassette(metadata());
    assert_rejected(&report, &CassetteRejectionReason::UnprovenFireAndForget);
}

#[tokio::test]
async fn rejects_every_unsuccessful_terminal_outcome() {
    let transaction = root_mapping(CAPTURE_SW_ID, 0x0003, 5);
    let mut channel = record_successes(vec![
        transaction.clone(),
        transaction.clone(),
        transaction.clone(),
        transaction,
    ])
    .await;
    let outcomes = [
        RequestOutcome::TimedOut,
        RequestOutcome::WriteFailed,
        RequestOutcome::NoResponse,
        RequestOutcome::Cancelled,
    ];
    for (request, replacement) in channel.requests.iter_mut().zip(outcomes) {
        for fact in &mut request.facts {
            if let RecordedRequestFact::Outcome { outcome, .. } = fact {
                *outcome = replacement;
            }
        }
    }

    let report = channel.build_hid_cassette(metadata());
    assert_rejected(&report, &CassetteRejectionReason::RequestTimedOut);
    assert_rejected(&report, &CassetteRejectionReason::RequestWriteFailed);
    assert_rejected(&report, &CassetteRejectionReason::RequestLostResponse);
    assert_rejected(&report, &CassetteRejectionReason::RequestCancelled);
}

#[tokio::test]
async fn rejects_ambiguous_request_fact_counts_and_malformed_identity() {
    let mut channel = record_successes(vec![root_mapping(CAPTURE_SW_ID, 0x0003, 5)]).await;
    let outgoing = channel.requests[0]
        .facts
        .iter()
        .find(|fact| matches!(fact, RecordedRequestFact::OutgoingReport { .. }))
        .unwrap()
        .clone();
    channel.requests[0].facts.push(outgoing);
    let report = channel.build_hid_cassette(metadata());
    assert_rejected(
        &report,
        &CassetteRejectionReason::OutgoingReportCount { actual: 2 },
    );

    let malformed_serial = record_successes(vec![
        root_mapping(CAPTURE_SW_ID, 0x0003, 5),
        (
            short(DEVICE, 5, 0x20 | CAPTURE_SW_ID, [0; 3]),
            long(DEVICE, 5, 0x20 | CAPTURE_SW_ID, &[0xff; 16]),
        ),
    ])
    .await
    .build_hid_cassette(metadata());
    assert_rejected(
        &malformed_serial,
        &CassetteRejectionReason::MalformedIdentity,
    );
}

fn identity_transactions(sw_id: u8) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut device_info = [0u8; 16];
    device_info[1..5].copy_from_slice(&UNIT_ID);
    device_info[6] = 0x0f;
    device_info[7..13].copy_from_slice(&[0x90, 0x01, 0x90, 0x02, 0x90, 0x03]);
    device_info[14] = 1;

    let mut device_serial = [0u8; 16];
    device_serial[..12].copy_from_slice(DEVICE_SERIAL);

    let mut pairing_info = [0u8; 16];
    pairing_info[0] = 0x51;
    pairing_info[1] = 0x22;
    pairing_info[2..4].copy_from_slice(&[0x34, 0x12]);
    pairing_info[4..8].copy_from_slice(&UNIT_ID);

    let mut receiver_info = [0u8; 16];
    receiver_info[0] = 0x03;
    receiver_info[1..5].copy_from_slice(&UNIFYING_SERIAL);
    receiver_info[6] = 6;

    vec![
        root_mapping(sw_id, 0x0003, 5),
        root_mapping(sw_id, 0x1004, 6),
        (
            short(DEVICE, 5, sw_id, [0; 3]),
            long(DEVICE, 5, sw_id, &device_info),
        ),
        (
            short(DEVICE, 5, sw_id, [0; 3]),
            long(DEVICE, 5, sw_id, &device_info),
        ),
        (
            short(DEVICE, 5, 0x20 | sw_id, [0; 3]),
            long(DEVICE, 5, 0x20 | sw_id, &device_serial),
        ),
        (
            short(DEVICE, 6, 0x10 | sw_id, [0; 3]),
            long(
                DEVICE,
                6,
                0x10 | sw_id,
                &[10, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            ),
        ),
        (
            short(DEVICE, 6, 0x10 | sw_id, [0; 3]),
            long(
                DEVICE,
                6,
                0x10 | sw_id,
                &[20, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            ),
        ),
        (
            short(0xff, 0x83, 0xb5, [0x51, 0, 0]),
            long(0xff, 0x83, 0xb5, &pairing_info),
        ),
        (
            short(0xff, 0x83, 0xfb, [0; 3]),
            long(0xff, 0x83, 0xfb, BOLT_UNIQUE_ID),
        ),
        (
            short(0xff, 0x83, 0xb5, [0x03, 0, 0]),
            long(0xff, 0x83, 0xb5, &receiver_info),
        ),
    ]
}

fn root_mapping(sw_id: u8, feature_id: u16, feature_index: u8) -> (Vec<u8>, Vec<u8>) {
    root_mapping_for(DEVICE, sw_id, feature_id, feature_index)
}

fn root_mapping_for(
    device: u8,
    sw_id: u8,
    feature_id: u16,
    feature_index: u8,
) -> (Vec<u8>, Vec<u8>) {
    let [high, low] = feature_id.to_be_bytes();
    (
        short(device, 0, sw_id, [high, low, 0]),
        short(device, 0, sw_id, [feature_index, 0, 4]),
    )
}

fn feature_set_mapping(
    device: u8,
    sw_id: u8,
    feature_set_index: u8,
    feature_index: u8,
    feature_id: u16,
) -> (Vec<u8>, Vec<u8>) {
    let [high, low] = feature_id.to_be_bytes();
    (
        short(
            device,
            feature_set_index,
            0x10 | sw_id,
            [feature_index, 0, 0],
        ),
        short(device, feature_set_index, 0x10 | sw_id, [high, low, 0]),
    )
}

async fn record_successes(transactions: Vec<(Vec<u8>, Vec<u8>)>) -> RecordedChannel {
    let recorder = NativeRecorder::new(256).unwrap();
    let sink = recorder.sink();
    let mut capture = sink.begin_channel(test_node()).unwrap();
    let (raw, handle) = FakeRawHidChannel::new();
    let channel = HidppChannel::from_raw_channel_with_observer(raw, capture.observer())
        .await
        .unwrap();
    capture.complete(RecordedChannelOpenOutcome::Opened {
        supports_short: true,
        supports_long: true,
    });
    drop(capture);

    for (index, (request, response)) in transactions.iter().enumerate() {
        let request = HidppMessage::read_raw(request).expect("valid synthetic request");
        let expected = HidppMessage::read_raw(response).expect("valid synthetic response");
        let send = channel.send(request, move |candidate| *candidate == expected);
        let respond = async {
            handle.wait_for_writes(index + 1).await;
            handle.send_raw(response.clone());
        };
        let (result, ()) = tokio::join!(send, respond);
        assert_eq!(result.unwrap(), expected);
    }

    wait_for_accepted(&recorder, 2 + transactions.len() * 3).await;
    drop(channel);
    wait_for_accepted(&recorder, 3 + transactions.len() * 3).await;
    recorder.finish().unwrap().channels.remove(0)
}

async fn record_unassociated(reports: Vec<Vec<u8>>) -> RecordedChannel {
    let report_count = reports.len();
    let recorder = NativeRecorder::new(32).unwrap();
    let sink = recorder.sink();
    let mut capture = sink.begin_channel(test_node()).unwrap();
    let (raw, handle) = FakeRawHidChannel::new();
    let channel = HidppChannel::from_raw_channel_with_observer(raw, capture.observer())
        .await
        .unwrap();
    capture.complete(RecordedChannelOpenOutcome::Opened {
        supports_short: true,
        supports_long: true,
    });
    drop(capture);

    for report in reports {
        handle.send_raw(report);
    }
    wait_for_accepted(&recorder, 2 + report_count).await;
    drop(channel);
    wait_for_closed(&recorder).await;
    recorder.finish().unwrap().channels.remove(0)
}

async fn record_fire_and_forget(report: Vec<u8>) -> RecordedChannel {
    let recorder = NativeRecorder::new(16).unwrap();
    let sink = recorder.sink();
    let mut capture = sink.begin_channel(test_node()).unwrap();
    let (raw, _handle) = FakeRawHidChannel::new();
    let channel = HidppChannel::from_raw_channel_with_observer(raw, capture.observer())
        .await
        .unwrap();
    capture.complete(RecordedChannelOpenOutcome::Opened {
        supports_short: true,
        supports_long: true,
    });
    drop(capture);

    channel
        .send_and_forget(HidppMessage::read_raw(&report).unwrap())
        .await
        .unwrap();
    wait_for_accepted(&recorder, 3).await;
    drop(channel);
    wait_for_accepted(&recorder, 4).await;
    recorder.finish().unwrap().channels.remove(0)
}

async fn replay_with_different_lease(cassette: openlogi_device::fixture::HidCassette) {
    let exchanges = cassette.exchanges.clone();
    let (raw, handle) = ReplayRawHidChannel::new(cassette, 0x046d, 0xc548).unwrap();
    let mut channel = HidppChannel::from_raw_channel(raw).await.unwrap();
    channel.set_sw_id_policy(SwIdPolicy::Leased {
        id: RequestSwId::new(U4::from_lo(REPLAY_SW_ID)).unwrap(),
        free: release_test_lease,
    });

    for exchange in exchanges {
        let mut request = exchange.request;
        let mut expected = exchange.response.unwrap();
        if exchange.request_match == RequestMatch::Hidpp20 {
            request[3] |= channel.get_sw_id().to_lo();
            if expected[2] == 0xff {
                expected[4] |= channel.get_sw_id().to_lo();
            } else {
                expected[3] |= channel.get_sw_id().to_lo();
            }
        }
        let request = HidppMessage::read_raw(&request).unwrap();
        let expected = HidppMessage::read_raw(&expected).unwrap();
        let actual = channel
            .send(request, move |candidate| *candidate == expected)
            .await
            .unwrap();
        assert_eq!(actual, expected);
    }

    handle.require_complete().unwrap();
}

fn release_test_lease(_id: u8) {}

fn assert_replacement(
    replacements: &[IdentityReplacement],
    kind: SanitizedIdentityKind,
    occurrences: usize,
    length: usize,
) {
    let replacement = replacements
        .iter()
        .find(|replacement| replacement.kind == kind)
        .expect("replacement kind present");
    assert_eq!(replacement.occurrences, occurrences);
    assert_eq!(replacement.synthetic_value.len(), length);
}

fn assert_rejected(report: &HidCassetteBuildReport, expected: &CassetteRejectionReason) {
    assert!(!report.is_committable());
    assert!(report.cassette.is_none());
    assert!(
        report
            .rejections
            .iter()
            .any(|rejection| rejection.reason == *expected),
        "expected {expected:?}, got {:?}",
        report.rejections
    );
}

fn contains(haystack: &[u8], needle: impl AsRef<[u8]>) -> bool {
    let needle = needle.as_ref();
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn metadata() -> HidCassetteMetadata {
    HidCassetteMetadata {
        name: "sanitized-read-only".to_string(),
        channel: "receiver".to_string(),
    }
}

fn test_node() -> NodeInfo {
    NodeInfo {
        id: NodeId::from("/private/native/path".to_owned()),
        vendor_id: 0x046d,
        product_id: 0xc548,
        usage_page: 0xff00,
        usage_id: 0x0002,
        name: "Private Host Node".to_owned(),
        manufacturer: Some("Logitech".to_owned()),
        serial_number: Some("HOST-SERIAL-PRIVATE".to_owned()),
    }
}

fn short(device: u8, feature: u8, function: u8, payload: [u8; 3]) -> Vec<u8> {
    vec![
        0x10, device, feature, function, payload[0], payload[1], payload[2],
    ]
}

fn long(device: u8, feature: u8, function: u8, payload: &[u8]) -> Vec<u8> {
    assert_eq!(payload.len(), 16);
    let mut report = vec![0x11, device, feature, function];
    report.extend_from_slice(payload);
    report
}

async fn wait_for_accepted(recorder: &NativeRecorder, expected: usize) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(1) {
        if recorder
            .shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .accepted
            >= expected
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {expected} recorder events");
}

async fn wait_for_closed(recorder: &NativeRecorder) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(1) {
        if recorder
            .shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .active_producers
            == 0
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for recorder channel close");
}

struct FakeRawHidChannel {
    incoming: AsyncMutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    written: Arc<Mutex<Vec<Vec<u8>>>>,
}

struct FakeRawHidHandle {
    incoming: mpsc::UnboundedSender<Vec<u8>>,
    written: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl FakeRawHidChannel {
    fn new() -> (Self, FakeRawHidHandle) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let written = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                incoming: AsyncMutex::new(receiver),
                written: Arc::clone(&written),
            },
            FakeRawHidHandle {
                incoming: sender,
                written,
            },
        )
    }
}

impl FakeRawHidHandle {
    fn send_raw(&self, report: Vec<u8>) {
        self.incoming.send(report).unwrap();
    }

    async fn wait_for_writes(&self, expected: usize) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(1) {
            if self
                .written
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len()
                >= expected
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for {expected} raw writes");
    }
}

#[async_trait]
impl RawHidChannel for FakeRawHidChannel {
    fn vendor_id(&self) -> u16 {
        0x046d
    }

    fn product_id(&self) -> u16 {
        0xc548
    }

    async fn write_report(&self, report: &[u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(report.to_vec());
        Ok(report.len())
    }

    async fn read_report(&self, buffer: &mut [u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        let Some(report) = self.incoming.lock().await.recv().await else {
            return std::future::pending().await;
        };
        if report.len() > buffer.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "report too long").into());
        }
        buffer[..report.len()].copy_from_slice(&report);
        Ok(report.len())
    }

    fn supports_short_long_hidpp(&self) -> Option<(bool, bool)> {
        Some((true, true))
    }

    async fn get_report_descriptor(
        &self,
        _buffer: &mut [u8],
    ) -> Result<usize, Box<dyn Error + Send + Sync>> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "descriptor not needed").into())
    }
}
