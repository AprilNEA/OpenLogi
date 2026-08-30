use super::*;

#[test]
fn synthetic_identity_policy_has_golden_values_for_every_representation() {
    let ordinal = SyntheticIdentityOrdinal::new(1).expect("one is a valid ordinal");
    let values = [
        (
            SyntheticIdentityKind::BoltReceiverUid,
            SyntheticIdentityValue::BoltReceiverUid(*b"OL-BOLT-UID-0001"),
        ),
        (
            SyntheticIdentityKind::UnifyingReceiverSerial,
            SyntheticIdentityValue::UnifyingReceiverSerial([b'O', b'L', b'R', 1]),
        ),
        (
            SyntheticIdentityKind::UnifyingReceiverRoute,
            SyntheticIdentityValue::UnifyingReceiverRoute("4F4C5201".to_string()),
        ),
        (
            SyntheticIdentityKind::DeviceUnitId,
            SyntheticIdentityValue::DeviceUnitId([b'O', b'L', b'D', 1]),
        ),
        (
            SyntheticIdentityKind::DeviceSerialNumber,
            SyntheticIdentityValue::DeviceSerialNumber(*b"OL-SER-00001"),
        ),
        (
            SyntheticIdentityKind::RawHidProfileIdentity,
            SyntheticIdentityValue::RawHidProfileIdentity(
                "OPENLOGI-FIXTURE-RAWHID-001".to_string(),
            ),
        ),
    ];

    for (kind, expected) in values {
        let generated = generate_synthetic_identity(kind, ordinal);
        assert_eq!(generated, expected);
        let classified = match &generated {
            SyntheticIdentityValue::UnifyingReceiverRoute(value)
            | SyntheticIdentityValue::RawHidProfileIdentity(value) => {
                classify_synthetic_profile_identity(kind, value)
            }
            _ => classify_synthetic_identity_bytes(
                kind,
                generated.as_bytes().expect("byte-backed golden value"),
            ),
        }
        .expect("generated value classifies");
        assert_eq!(classified, ordinal);
    }
}

#[test]
fn synthetic_identity_policy_enforces_bounds_and_canonical_formatting() {
    SyntheticIdentityOrdinal::new(0).expect_err("zero ordinal must fail");
    let maximum = SyntheticIdentityOrdinal::new(MAX_SYNTHETIC_IDENTITY_ORDINAL)
        .expect("maximum ordinal is valid");
    assert_eq!(maximum.get(), u8::MAX);
    SyntheticIdentityOrdinal::new(MAX_SYNTHETIC_IDENTITY_ORDINAL + 1)
        .expect_err("ordinal beyond the common representation range must fail");
    assert_eq!(
        generate_synthetic_identity(SyntheticIdentityKind::BoltReceiverUid, maximum),
        SyntheticIdentityValue::BoltReceiverUid(*b"OL-BOLT-UID-0255")
    );
    assert_eq!(
        generate_synthetic_identity(SyntheticIdentityKind::DeviceSerialNumber, maximum),
        SyntheticIdentityValue::DeviceSerialNumber(*b"OL-SER-00255")
    );

    classify_synthetic_profile_identity(SyntheticIdentityKind::BoltReceiverUid, "OL-BOLT-UID-0000")
        .expect_err("zero-valued Bolt ordinal must fail");
    classify_synthetic_profile_identity(SyntheticIdentityKind::UnifyingReceiverRoute, "4f4c5201")
        .expect_err("lowercase Unifying route must fail");
    classify_synthetic_profile_identity(
        SyntheticIdentityKind::RawHidProfileIdentity,
        "OPENLOGI-FIXTURE-RAWHID-01",
    )
    .expect_err("short raw-HID identity must fail");
}

#[test]
fn four_byte_identity_magics_are_kind_separated() {
    let ordinal = SyntheticIdentityOrdinal::new(7).expect("valid ordinal");
    let receiver =
        generate_synthetic_identity(SyntheticIdentityKind::UnifyingReceiverSerial, ordinal);
    let device = generate_synthetic_identity(SyntheticIdentityKind::DeviceUnitId, ordinal);
    assert_ne!(receiver.as_bytes(), device.as_bytes());
    classify_synthetic_identity_bytes(
        SyntheticIdentityKind::DeviceUnitId,
        receiver.as_bytes().expect("binary receiver serial"),
    )
    .expect_err("receiver magic must not classify as a device unit ID");
    classify_synthetic_identity_bytes(
        SyntheticIdentityKind::UnifyingReceiverSerial,
        device.as_bytes().expect("binary device unit ID"),
    )
    .expect_err("device magic must not classify as a receiver serial");
}

#[test]
fn unifying_binary_serial_and_profile_route_are_one_exact_relation() {
    let serial = [b'O', b'L', b'R', 42];
    assert_eq!(unifying_receiver_route(serial), "4F4C522A");
    assert_eq!(
        classify_synthetic_identity_bytes(SyntheticIdentityKind::UnifyingReceiverSerial, &serial)
            .expect("binary serial classifies")
            .get(),
        42
    );
    assert_eq!(
        classify_synthetic_profile_identity(
            SyntheticIdentityKind::UnifyingReceiverRoute,
            "4F4C522A"
        )
        .expect("route classifies")
        .get(),
        42
    );
}
