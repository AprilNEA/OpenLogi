//! The MX Creative Console's per-device settings the Keys view shows: which
//! device is the dialpad, and the keypad's LCD brightness, key faces and
//! live keys.

use super::*;
use crate::services::ipc::Command;
use openlogi_core::config::{DeviceIdentity, KeyDisplayBrightness};
use openlogi_core::device_order::PhysicalDeviceKey;

/// A state whose one device is the MX Creative Dialpad, reached directly
/// over Bluetooth.
pub(crate) fn dialpad_state() -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let inventory = DeviceInventory {
        receiver: ReceiverInfo {
            name: "MX Creative Dialpad".to_string(),
            vendor_id: 0x046d,
            product_id: 0xbc00,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: openlogi_core::hid::DIRECT_DEVICE_INDEX,
            codename: Some("MX Creative Dialpad".to_string()),
            wpid: None,
            kind: DeviceKind::Dialpad,
            online: true,
            battery: None,
            model_info: Some(DeviceModelInfo {
                entity_count: 3,
                serial_number: Some("DIALPAD-0001".to_string()),
                unit_id: [0x4f, 0x4c, 0x44, 0x04],
                transports: DeviceTransports::default(),
                model_ids: [0xbc00, 0, 0],
                extended_model_id: 0,
            }),
            capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Dialpad)),
            settings_revision: 0,
            hidpp10: false,
        }],
    };
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    (state, receiver)
}

fn reloads(receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>) -> usize {
    std::iter::from_fn(|| receiver.try_recv().ok())
        .filter(|command| matches!(command, Command::ReloadConfig(_)))
        .count()
}

#[test]
fn the_dialpad_is_the_dialpad_and_the_keypad_is_not() {
    let (dialpad, _commands) = dialpad_state();
    assert_eq!(dialpad.current_record().unwrap().kind, DeviceKind::Dialpad);
    assert_eq!(
        dialpad.current_dialpad().map(|dialpad| dialpad.name),
        Some("MX Creative Dialpad")
    );
    assert_eq!(dialpad.current_keypad(), None);

    let (keypad, _commands) = super::keypad_pages::keypad_state();
    assert_eq!(keypad.current_dialpad(), None);
}

/// A sleeping dialpad has no live route; the route its saved entry records
/// still names it, so its dial, roller, and keys stay bindable.
#[test]
fn an_offline_dialpad_is_still_the_dialpad() {
    let offline = |route_key: &str| {
        let mut config = Config::ephemeral();
        config.set_device_identity(
            "serial:dialpad-0001",
            DeviceIdentity {
                display_name: "MX Creative Dialpad".to_string(),
                kind: DeviceKind::Dialpad,
                capabilities: Capabilities::presumed_from_kind(DeviceKind::Dialpad),
                light_capabilities: None,
                model_info: None,
                codename: None,
                driver_id: None,
                registry_model_id: None,
            },
        );
        let canonical = PhysicalDeviceKey::parse("serial:dialpad-0001").unwrap();
        assert!(config.adopt_route(&canonical, route_key, None));
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        AppState::new(Sources::in_memory(config, &AssetResolver::new(), commands))
    };

    let state = offline("direct:046d:bc00");
    assert_eq!(state.current_record().unwrap().route, None);
    assert_eq!(
        state.current_dialpad().map(|dialpad| dialpad.name),
        Some("MX Creative Dialpad")
    );
    assert_eq!(offline("direct:046d:c548").current_dialpad(), None);
}

/// The keypad's brightness stays unmanaged until the user sets one; setting
/// it saves it and has the agent reload, and setting it again is a no-op.
#[test]
fn setting_the_key_brightness_saves_it_once() {
    let (mut state, mut receiver) = super::keypad_pages::keypad_state();
    let record = state.current_record().unwrap().clone();
    let key = record.device_key();
    assert_eq!(
        state.device_key_display_brightness(&record.config_key),
        None
    );
    let _ = reloads(&mut receiver);

    let dim = KeyDisplayBrightness::try_new(30).unwrap();
    let events = state.commit_device_key_display_brightness(&key, dim);

    assert_eq!(events, [StateEvent::DeviceConfigChanged(key.clone())]);
    assert_eq!(
        state.device_key_display_brightness(&record.config_key),
        Some(dim)
    );
    assert_eq!(reloads(&mut receiver), 1, "the agent writes the new level");

    let events = state.commit_device_key_display_brightness(&key, dim);
    assert_eq!(events, [StateEvent::DeviceConfigChanged(key)]);
    assert_eq!(
        reloads(&mut receiver),
        0,
        "an unchanged level writes nothing"
    );
}

/// A key face is saved per grid key and page; restyling it the same way
/// writes nothing, and resetting it to the default removes the entry.
#[test]
fn a_key_face_saves_per_key_and_page_and_resets_away() {
    use openlogi_core::binding::{GridKey, GridSlot, KeypadPage};
    use openlogi_core::config::KeyFaceStyle;

    let (mut state, mut receiver) = super::keypad_pages::keypad_state();
    let record = state.current_record().unwrap().clone();
    let slot = GridSlot {
        page: KeypadPage::FIRST,
        key: GridKey::TopLeft,
    };
    assert!(state.current_key_face(slot).is_default());
    let _ = reloads(&mut receiver);

    let style = KeyFaceStyle {
        label: Some("Export\nPNG".into()),
        background: Some(openlogi_core::color::Rgb::new(0x20, 0x40, 0x80)),
        ..KeyFaceStyle::default()
    };
    let events = state.commit_key_face(slot, style.clone());

    assert_eq!(
        events,
        [StateEvent::DeviceConfigChanged(record.device_key())]
    );
    assert_eq!(state.current_key_face(slot), style);
    assert!(
        state
            .current_key_face(GridSlot {
                key: GridKey::Middle,
                ..slot
            })
            .is_default(),
        "another key keeps its own face"
    );
    assert_eq!(reloads(&mut receiver), 1, "the agent repaints the key");

    let _ = state.commit_key_face(slot, style);
    assert_eq!(
        reloads(&mut receiver),
        0,
        "an unchanged face writes nothing"
    );

    let _ = state.commit_key_face(slot, KeyFaceStyle::default());
    assert!(state.current_key_face(slot).is_default());
    assert_eq!(
        state
            .config
            .key_face_style(&record.config_key, ButtonId::Grid(slot)),
        None,
        "a default face leaves no entry behind"
    );
    assert_eq!(reloads(&mut receiver), 1);
}

/// A live key is saved per grid key: making it live has the agent reload,
/// the same live key again writes nothing, and `None` makes it a plain key
/// again without leaving an entry behind.
#[test]
fn a_live_key_saves_once_and_turns_back_into_a_plain_key() {
    use openlogi_core::binding::{GridKey, GridSlot, KeypadPage};
    use openlogi_core::config::{LiveKey, LiveSource};

    let (mut state, mut receiver) = super::keypad_pages::keypad_state();
    let record = state.current_record().unwrap().clone();
    let slot = GridSlot {
        page: KeypadPage::FIRST,
        key: GridKey::Middle,
    };
    assert_eq!(state.current_live_key(slot), None);
    let _ = reloads(&mut receiver);

    let clock = LiveKey::new(LiveSource::Clock);
    let events = state.commit_live_key(slot, Some(clock.clone()));
    assert_eq!(
        events,
        [StateEvent::DeviceConfigChanged(record.device_key())]
    );
    assert_eq!(state.current_live_key(slot), Some(clock.clone()));
    assert_eq!(reloads(&mut receiver), 1, "the agent starts the live key");

    let _ = state.commit_live_key(slot, Some(clock));
    assert_eq!(reloads(&mut receiver), 0, "an unchanged key writes nothing");

    let _ = state.commit_live_key(slot, None);
    assert_eq!(state.current_live_key(slot), None);
    assert!(
        state.config.devices[&record.config_key]
            .live_keys
            .is_empty(),
        "a plain key leaves no entry behind"
    );
    assert_eq!(reloads(&mut receiver), 1);
}
