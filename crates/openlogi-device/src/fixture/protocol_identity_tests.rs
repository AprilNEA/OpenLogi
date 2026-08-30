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
