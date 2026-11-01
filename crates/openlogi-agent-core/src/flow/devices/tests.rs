use openlogi_core::device::DeviceKind;
use openlogi_core::device_order::PhysicalDeviceKey;
use openlogi_flow::generated as proto;
use openlogi_flow::identity::CanonicalDeviceIdentifier;
use openlogi_hid::DeviceRoute;

use super::LocalDevices;
use crate::flow::test_support::{
    KEYBOARD, MOUSE, PRESENTER, config, configured, peer, record, slot, unit_identity,
};

fn keys(devices: &LocalDevices) -> Vec<&str> {
    devices.iter().map(|device| device.key().as_str()).collect()
}

#[test]
fn join_orders_pointing_devices_first_and_keyboards_last() {
    let config = config(vec![
        configured(KEYBOARD, Some(1)),
        configured(PRESENTER, Some(1)),
        configured(MOUSE, Some(1)),
    ]);
    let inventory = [
        record(KEYBOARD, DeviceKind::Keyboard, 0x0b, true),
        record(PRESENTER, DeviceKind::Presenter, 0x0c, true),
        record(MOUSE, DeviceKind::Mouse, 0x0a, true),
    ];

    let devices = LocalDevices::join(&config, &inventory);

    assert_eq!(keys(&devices), [MOUSE, PRESENTER, KEYBOARD]);
}

#[test]
fn join_drops_unlisted_and_unidentifiable_devices() {
    let config = config(vec![
        configured(MOUSE, Some(1)),
        configured(KEYBOARD, Some(1)),
    ]);
    let mut keyboard = record(KEYBOARD, DeviceKind::Keyboard, 0x0b, true);
    keyboard.unit_id = [0; 4];
    // The mouse is configured but absent; the presenter is present but not
    // configured; the keyboard has no serial and no unit id.
    let inventory = [
        keyboard,
        record(PRESENTER, DeviceKind::Presenter, 0x0c, true),
    ];

    let devices = LocalDevices::join(&config, &inventory);

    assert!(keys(&devices).is_empty());
}

#[test]
fn outgoing_lists_online_routed_devices_with_a_slot_for_the_peer() {
    let config = config(vec![
        configured(MOUSE, Some(2)),
        configured(KEYBOARD, None),
        configured(PRESENTER, Some(1)),
    ]);
    let mut presenter = record(PRESENTER, DeviceKind::Presenter, 0x0c, true);
    presenter.route = None;
    let inventory = [
        record(MOUSE, DeviceKind::Mouse, 0x0a, true),
        record(KEYBOARD, DeviceKind::Keyboard, 0x0b, true),
        presenter,
    ];
    let devices = LocalDevices::join(&config, &inventory);

    let outgoing = devices.outgoing(&peer());

    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].slot, slot(2));
    assert_eq!(outgoing[0].identity.ids, unit_identity(0x0a).ids);

    let offline = LocalDevices::join(&config, &[record(MOUSE, DeviceKind::Mouse, 0x0a, false)]);
    assert!(offline.outgoing(&peer()).is_empty());
}

#[test]
fn find_matches_on_any_shared_identifier_and_rejects_malformed_ones() {
    let config = config(vec![configured(MOUSE, Some(1))]);
    let mut mouse = record(MOUSE, DeviceKind::Mouse, 0x0a, true);
    mouse.serial = Some("2C1A".to_owned());
    let devices = LocalDevices::join(&config, &[mouse]);

    let by_serial = proto::DeviceIdentity {
        ids: vec![CanonicalDeviceIdentifier::serial("2C1A").into()],
        ..Default::default()
    };
    assert!(devices.find(&by_serial).is_some());
    assert!(devices.find(&unit_identity(0x0a)).is_some());
    assert!(devices.find(&unit_identity(0x0b)).is_none());

    let mut malformed = unit_identity(0x0a);
    malformed.ids.push(proto::DeviceIdentifier {
        kind: proto::IdentifierKind::UnitId.into(),
        value: vec![1, 2],
        ..Default::default()
    });
    assert!(devices.find(&malformed).is_none());
}

#[test]
fn announce_reports_the_self_slot_and_held_lists_online_devices() {
    let config = config(vec![
        configured(MOUSE, Some(1)),
        configured(KEYBOARD, Some(1)),
    ]);
    let inventory = [
        record(MOUSE, DeviceKind::Mouse, 0x0a, true),
        record(KEYBOARD, DeviceKind::Keyboard, 0x0b, false),
    ];
    let devices = LocalDevices::join(&config, &inventory);

    let announce = devices.announce(9);

    assert_eq!(announce.revision, 9);
    assert_eq!(announce.devices.len(), 2);
    assert_eq!(announce.devices[0].channel_to_me, 0);
    assert!(announce.devices[0].connected);
    assert!(!announce.devices[1].connected);
    assert_eq!(
        announce.devices[0]
            .identity
            .as_option()
            .map(|identity| identity.name.as_str()),
        Some(MOUSE)
    );
    assert_eq!(devices.held().len(), 1);
}

/// The inventory keys a device with a serial by that serial, but a device
/// configured by its unit id is the same device.
#[test]
fn join_matches_a_configured_unit_id_on_a_device_keyed_by_its_serial() {
    let config = config(vec![configured(MOUSE, Some(1))]);
    let mut mouse = record(MOUSE, DeviceKind::Mouse, 0x0a, true);
    mouse.serial = Some("2C1A".to_owned());
    mouse.key = PhysicalDeviceKey::parse("serial:2c1a").expect("a serial key");

    let devices = LocalDevices::join(&config, &[mouse]);

    assert_eq!(keys(&devices), [MOUSE]);
    assert_eq!(devices.outgoing(&peer()).len(), 1);
}

/// A mouse paired to a receiver and connected over Bluetooth has a record for
/// each route. The online, addressable one is the device Flow moves.
#[test]
fn join_prefers_the_online_record_of_a_device_seen_on_two_routes() {
    let config = config(vec![configured(MOUSE, Some(1))]);
    let mut receiver = record(MOUSE, DeviceKind::Mouse, 0x0a, false);
    receiver.serial = Some("2c1a".to_owned());
    let mut bluetooth = record(MOUSE, DeviceKind::Mouse, 0x0a, true);
    bluetooth.serial = Some("2c1a".to_owned());
    bluetooth.route = Some(DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb023,
    });

    let devices = LocalDevices::join(&config, &[receiver, bluetooth.clone()]);

    assert_eq!(keys(&devices), [MOUSE]);
    let outgoing = devices.outgoing(&peer());
    assert_eq!(outgoing.len(), 1);
    assert_eq!(Some(outgoing[0].route.clone()), bluetooth.route);
}

#[test]
fn find_unlisted_keys_a_peer_identity_like_the_inventory() {
    const SERIAL_MOUSE: &str = "serial:2c1a";
    let config = config(vec![
        configured(SERIAL_MOUSE, Some(1)),
        configured(KEYBOARD, Some(1)),
        configured(PRESENTER, Some(1)),
    ]);
    let devices = LocalDevices::join(
        &config,
        &[record(PRESENTER, DeviceKind::Presenter, 0x0c, false)],
    );

    // The inventory folds a serial's case into its key; the wire does not.
    let by_serial = proto::DeviceIdentity {
        ids: vec![
            CanonicalDeviceIdentifier::serial("2C1A").into(),
            CanonicalDeviceIdentifier::unit_id(0x99).into(),
        ],
        ..Default::default()
    };
    assert_eq!(
        devices
            .find_unlisted(&by_serial)
            .map(PhysicalDeviceKey::as_str),
        Some(SERIAL_MOUSE)
    );
    assert_eq!(
        devices
            .find_unlisted(&unit_identity(0x0b))
            .map(PhysicalDeviceKey::as_str),
        Some(KEYBOARD)
    );
    assert!(
        devices.find_unlisted(&unit_identity(0x0c)).is_none(),
        "the presenter is listed"
    );
    assert!(devices.find_unlisted(&unit_identity(0x0d)).is_none());
    assert_eq!(keys(&devices), [PRESENTER]);
}
