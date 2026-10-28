//! Unit tests for `PersistentRemappableAction` payload parsing.

use std::assert_matches;

use super::{
    ActionId, HostMask, ModifierMask, PersistentAction, PersistentActionStatus,
    RemappableCapabilities,
};
use crate::feature::{hosts_info::HostIndex, reprog_controls::ControlId};

#[test]
fn parses_feature_info_flags() {
    // flags MSB (byte0) carries the power-key bit (bit 8); flags LSB (byte1) the
    // rest. Here: power key + keyboard + consumer control.
    let payload = {
        let mut p = [0; 16];
        p[0] = 0x01; // power key (bit 8)
        p[1] = 0b0100_0001; // consumer ctrl (bit6) + keyboard (bit0)
        p
    };
    let caps =
        RemappableCapabilities::from_bits_retain(u16::from_be_bytes([payload[0], payload[1]]));
    assert!(caps.contains(RemappableCapabilities::POWER_KEY));
    assert!(caps.contains(RemappableCapabilities::KEYBOARD_REPORT));
    assert!(caps.contains(RemappableCapabilities::CONSUMER_CONTROL));
    assert!(!caps.contains(RemappableCapabilities::MOUSE_BUTTONS));
}

#[test]
fn parses_persistent_action() {
    let mut payload = [0; 16];
    payload[0..2].copy_from_slice(&0x00c3u16.to_be_bytes()); // cid
    payload[2] = 0xff; // current host
    payload[3] = 0x01; // SendKeyboard
    payload[4..6].copy_from_slice(&0x001eu16.to_be_bytes()); // value
    payload[6] = ModifierMask::LEFT_CTRL.bits() | ModifierMask::LEFT_GUI.bits();
    payload[7] = 0x01; // remapped

    let action = PersistentAction::from_payload(&payload).unwrap();
    assert_eq!(action.cid, ControlId(0x00c3));
    assert_eq!(action.host, HostIndex::Current);
    assert_eq!(action.action_id, ActionId::SendKeyboard);
    assert_eq!(action.value, 0x001e);
    assert!(action.modifier_mask.contains(ModifierMask::LEFT_CTRL));
    assert!(action.modifier_mask.contains(ModifierMask::LEFT_GUI));
    assert!(!action.modifier_mask.contains(ModifierMask::RIGHT_ALT));
    assert!(action.is_remapped());
    assert_eq!(action.status, PersistentActionStatus::REMAPPED);
}

#[test]
fn reports_default_mapping_as_not_remapped() {
    let mut payload = [0; 16];
    payload[3] = 0x08; // ExecuteInternalFunction
    payload[7] = 0x00; // not remapped

    let action = PersistentAction::from_payload(&payload).unwrap();
    assert_eq!(action.action_id, ActionId::ExecuteInternalFunction);
    assert!(!action.is_remapped());
}

#[test]
fn rejects_unknown_action_id() {
    let mut payload = [0; 16];
    payload[3] = 0x55;

    assert_matches!(
        PersistentAction::from_payload(&payload),
        Err(crate::protocol::v20::Hidpp20Error::UnsupportedResponse)
    );
}

#[test]
fn maps_action_id_wire_values() {
    assert_eq!(ActionId::try_from(0x01u8).unwrap(), ActionId::SendKeyboard);
    assert_eq!(ActionId::try_from(0x09u8).unwrap(), ActionId::SendPowerKey);
    assert_eq!(ActionId::try_from(0x00u8).unwrap(), ActionId::Empty);
    ActionId::try_from(0x0au8).unwrap_err();
    assert_eq!(u8::from(ActionId::SendConsumerControl), 0x07);
}

#[test]
fn host_mask_bits() {
    let mask = HostMask::HOST_1 | HostMask::HOST_3;
    assert_eq!(mask.bits(), 0b101);
}

#[test]
fn decodes_an_empty_action() {
    // A control mapped to no output reports action 0 with a zero value and
    // no modifiers. It must decode, not fail as an unknown action.
    let mut payload = [0; 16];
    payload[0..2].copy_from_slice(&0x0052u16.to_be_bytes()); // cid
    payload[2] = 0x00; // host 1
    payload[3] = 0x00; // Empty
    payload[7] = 0x01; // remapped away from the default
    let action = PersistentAction::from_payload(&payload).unwrap();
    assert_eq!(action.cid, ControlId(0x0052));
    assert_eq!(action.action_id, ActionId::Empty);
    assert_eq!(action.value, 0);
    assert_eq!(action.modifier_mask, ModifierMask::empty());
    assert!(action.is_remapped());
}

#[test]
fn keeps_every_status_bit() {
    // Only bit 0 has a known meaning; the other bits must survive decoding.
    let mut payload = [0; 16];
    payload[3] = 0x07; // SendConsumerControl
    payload[7] = 0b1010_0000;
    let action = PersistentAction::from_payload(&payload).unwrap();
    assert_eq!(action.status.bits(), 0b1010_0000);
    assert!(!action.is_remapped());

    payload[7] = 0b1000_0001;
    let action = PersistentAction::from_payload(&payload).unwrap();
    assert_eq!(action.status.bits(), 0b1000_0001);
    assert!(action.is_remapped());
}
