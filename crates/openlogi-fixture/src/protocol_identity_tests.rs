//! Protocol identity extraction tests.

use super::protocol_identity::ProtocolRequestTarget;
use super::*;

const DEVICE: u8 = 1;
const SW_ID: u8 = 3;

#[test]
fn extractor_classifies_bolt_and_unifying_receiver_identities() {
    let mut extractor = ProtocolIdentityExtractor::default();
    let bolt = extractor
        .classify_exchange(
            &short(0xff, 0x83, 0xfb, [0; 3]),
            &long(0xff, 0x83, 0xfb, b"OL-BOLT-UID-0001"),
        )
        .expect("tagged Bolt UID classifies");
    assert_eq!(
        bolt,
        vec![(SyntheticIdentityKind::BoltReceiverUid, ordinal(1))]
    );

    let mut receiver_info = [0; 16];
    receiver_info[0] = 0x03;
    receiver_info[1..5].copy_from_slice(&[b'O', b'L', b'R', 2]);
    let unifying = extractor
        .classify_exchange(
            &short(0xff, 0x83, 0xb5, [0x03, 0, 0]),
            &long(0xff, 0x83, 0xb5, &receiver_info),
        )
        .expect("tagged Unifying serial classifies");
    assert_eq!(
        unifying,
        vec![(SyntheticIdentityKind::UnifyingReceiverSerial, ordinal(2))]
    );
}

#[test]
fn extractor_learns_root_and_feature_set_for_repeated_device_identities() {
    let mut extractor = ProtocolIdentityExtractor::default();
    extractor
        .inspect_exchange(
            &short(DEVICE, 0, SW_ID, [0, 1, 0]),
            &short(DEVICE, 0, SW_ID, [7, 0, 0]),
        )
        .expect("Root maps FeatureSet");
    extractor
        .inspect_exchange(
            &short(DEVICE, 7, 0x10 | SW_ID, [5, 0, 0]),
            &short(DEVICE, 7, 0x10 | SW_ID, [0, 3, 0]),
        )
        .expect("FeatureSet maps DeviceInformation");

    let mut info = [0; 16];
    info[1..5].copy_from_slice(&[b'O', b'L', b'D', 9]);
    let unit_request = short(DEVICE, 5, SW_ID, [0; 3]);
    let unit_response = long(DEVICE, 5, SW_ID, &info);
    for _ in 0..2 {
        assert_eq!(
            extractor
                .classify_exchange(&unit_request, &unit_response)
                .expect("repeated unit ID classifies"),
            vec![(SyntheticIdentityKind::DeviceUnitId, ordinal(9))]
        );
    }

    let mut serial = [0; 16];
    serial[..12].copy_from_slice(b"OL-SER-00009");
    assert_eq!(
        extractor
            .classify_exchange(
                &short(DEVICE, 5, 0x20 | SW_ID, [0; 3]),
                &long(DEVICE, 5, 0x20 | SW_ID, &serial),
            )
            .expect("device serial classifies"),
        vec![(SyntheticIdentityKind::DeviceSerialNumber, ordinal(9))]
    );
}

#[test]
fn extractor_rejects_real_looking_malformed_pairing_and_unknown_identity_traffic() {
    let mut extractor = ProtocolIdentityExtractor::default();
    let real = extractor
        .classify_exchange(
            &short(0xff, 0x83, 0xfb, [0; 3]),
            &long(0xff, 0x83, 0xfb, b"ABCDEF0123456789"),
        )
        .expect_err("real-looking UID is not synthetic");
    assert!(matches!(
        real,
        ProtocolIdentityError::NonSyntheticIdentity { .. }
    ));

    let malformed = extractor
        .inspect_exchange(
            &short(0xff, 0x83, 0xfb, [0; 3]),
            &long(0xff, 0x83, 0xfb, &[0xff; 16]),
        )
        .expect_err("non-UTF-8 fixed ASCII identity is malformed");
    assert_eq!(malformed, ProtocolIdentityError::MalformedIdentity);

    let pairing = extractor
        .inspect_exchange(
            &long(0xff, 0x82, 0xc1, &[0; 16]),
            &long(0xff, 0x82, 0xc1, &[0; 16]),
        )
        .expect_err("pairing traffic is never fixture evidence");
    assert_eq!(pairing, ProtocolIdentityError::PairingTraffic);

    extractor
        .inspect_exchange(
            &short(DEVICE, 0, SW_ID, [0, 7, 0]),
            &short(DEVICE, 0, SW_ID, [8, 0, 0]),
        )
        .expect("Root discovery itself is classified");
    let friendly_name = extractor
        .inspect_exchange(
            &short(DEVICE, 8, SW_ID, [0; 3]),
            &short(DEVICE, 8, SW_ID, [1, 2, 3]),
        )
        .expect_err("unsupported identity feature fails closed");
    assert_eq!(
        friendly_name,
        ProtocolIdentityError::UnsupportedIdentityFeature { feature_id: 0x0007 }
    );
}

#[test]
fn extractor_rejects_unknown_device_information_functions() {
    let mut extractor = ProtocolIdentityExtractor::default();
    extractor
        .inspect_exchange(
            &short(DEVICE, 0, SW_ID, [0, 3, 0]),
            &short(DEVICE, 0, SW_ID, [5, 0, 0]),
        )
        .expect("Root maps DeviceInformation");
    let error = extractor
        .inspect_exchange(
            &short(DEVICE, 5, 0x30 | SW_ID, [0; 3]),
            &short(DEVICE, 5, 0x30 | SW_ID, [0; 3]),
        )
        .expect_err("unknown identity-capable function fails closed");
    assert_eq!(
        error,
        ProtocolIdentityError::UnsupportedHidpp20Function {
            feature_id: 0x0003,
            function_id: 3,
        }
    );
}

#[test]
fn receiver_control_reads_are_allowed_but_writes_are_not() {
    for register in [0x00, 0x02] {
        let mut extractor = ProtocolIdentityExtractor::default();
        let read = short(0xff, 0x81, register, [0; 3]);
        let response = short(0xff, 0x81, register, [0, 1, 0]);
        let inspection = extractor.inspect_exchange(&read, &response).unwrap();
        assert_eq!(inspection.request_match, RequestMatch::Exact);
        assert!(inspection.fields.is_empty());

        let write = short(0xff, 0x80, register, [0, 1, 0]);
        for response in [
            short(0xff, 0x80, register, [0; 3]),
            short(0xff, 0x8f, 0x80, [register, 2, 0]),
        ] {
            assert_eq!(
                extractor.inspect_exchange(&write, &response).unwrap_err(),
                ProtocolIdentityError::UnsupportedHidpp10Register,
                "an ACK or device error does not make a write read-only"
            );
        }
    }
}

#[test]
fn hidpp20_errors_preserve_correlation_in_both_report_widths() {
    for long_only in [false, true] {
        let report = |device, feature, function, payload: [u8; 3]| {
            if long_only {
                long(device, feature, function, &payload)
            } else {
                short(device, feature, function, payload)
            }
        };
        let mut extractor = ProtocolIdentityExtractor::default();
        extractor
            .inspect_exchange(
                &report(0xff, 0, SW_ID, [0x22, 1, 0]),
                &report(0xff, 0, SW_ID, [5, 0, 0]),
            )
            .expect("Root maps adjustable DPI");
        let request = report(0xff, 5, 0x20 | SW_ID, [0; 3]);
        let mut response = report(0xff, 0xff, 5, [0x20 | SW_ID, 7, 0]);
        let inspection = extractor.inspect_exchange(&request, &response).unwrap();
        assert_eq!(inspection.request_match, RequestMatch::Hidpp20);
        assert!(inspection.fields.is_empty());

        response[4] ^= 1;
        assert_eq!(
            extractor.inspect_exchange(&request, &response).unwrap_err(),
            ProtocolIdentityError::CorrelationMismatch,
            "the echoed function/software ID must still match"
        );
    }
}

#[test]
fn request_targets_distinguish_receiver_selectors_from_direct_feature_indices() {
    let mut extractor = ProtocolIdentityExtractor::default();
    for (selector, slot) in [
        (0x51, 1),
        (0x56, 6),
        (0x40, 1),
        (0x45, 6),
        (0x61, 1),
        (0x66, 6),
    ] {
        let request = short(0xff, 0x83, 0xb5, [selector, 0, 0]);
        extractor
            .inspect_exchange(&request, &short(0xff, 0x8f, 0x83, [0xb5, 2, 0]))
            .unwrap();
        assert_eq!(
            extractor.request_target(&request).unwrap(),
            ProtocolRequestTarget::Device(slot)
        );
    }
    let receiver_read = short(0xff, 0x81, 0, [0; 3]);
    extractor
        .inspect_exchange(&receiver_read, &receiver_read)
        .unwrap();
    assert_eq!(
        extractor.request_target(&receiver_read).unwrap(),
        ProtocolRequestTarget::Receiver
    );

    extractor
        .inspect_exchange(
            &short(0xff, 0, SW_ID, [0x10, 0, 0]),
            &short(0xff, 0, SW_ID, [0x81, 0, 0]),
        )
        .expect("a direct device may assign a feature index resembling a receiver command");
    let direct_read = short(0xff, 0x81, SW_ID, [0; 3]);
    extractor
        .inspect_exchange(&direct_read, &direct_read)
        .unwrap();
    assert_eq!(
        extractor.request_target(&direct_read).unwrap(),
        ProtocolRequestTarget::Device(0xff)
    );
}

fn ordinal(value: u16) -> SyntheticIdentityOrdinal {
    SyntheticIdentityOrdinal::new(value).expect("test ordinal is valid")
}

fn short(device: u8, feature: u8, function: u8, payload: [u8; 3]) -> Vec<u8> {
    vec![
        0x10, device, feature, function, payload[0], payload[1], payload[2],
    ]
}

fn long(device: u8, feature: u8, function: u8, payload: &[u8]) -> Vec<u8> {
    let mut report = vec![0x11, device, feature, function];
    report.extend_from_slice(payload);
    report.resize(20, 0);
    report
}
