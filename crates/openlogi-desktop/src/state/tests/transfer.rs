//! Copying settings onto a device from another device or from a file.

use openlogi_core::config::SmartShift;

use super::fn_lock::drain;
use super::*;
use crate::services::ipc::{Command, SetDpi, SetSmartShift};

const OLD_MOUSE_KEY: &str = "serial:old-mouse";

fn old_mouse() -> openlogi_core::config::DeviceConfig {
    let mut device = openlogi_core::config::DeviceConfig::default();
    device.identity = Some(DeviceIdentity {
        display_name: "MX Master 2S".to_string(),
        model_info: None,
        codename: None,
        kind: DeviceKind::Mouse,
        capabilities: Capabilities::presumed_from_kind(DeviceKind::Mouse),
        light_capabilities: None,
        driver_id: None,
        registry_model_id: None,
    });
    device
        .bindings
        .insert(ButtonId::Back, Binding::Single(Action::Copy));
    device
}

/// The known mouse online, plus an offline mouse with settings of its own.
fn state_with_an_old_mouse() -> AppState {
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut config = Config::ephemeral();
    config.devices.insert(OLD_MOUSE_KEY.to_owned(), old_mouse());
    AppState::new(Sources {
        inventories: &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
        ..Sources::in_memory(config, &resolver, commands)
    })
}

fn back_binding(state: &AppState) -> Option<&Binding> {
    state
        .config
        .devices
        .get(KNOWN_MOUSE_KEY)
        .and_then(|device| device.bindings.get(&ButtonId::Back))
}

#[test]
fn a_device_copies_another_devices_settings_once() {
    let mut state = state_with_an_old_mouse();
    let sources = state.transfer_sources(KNOWN_MOUSE_KEY);
    let names: Vec<&str> = sources.iter().map(|source| source.name.as_str()).collect();
    assert_eq!(
        names,
        ["MX Master 2S"],
        "the old mouse, offline, never itself"
    );

    let preview = state
        .settings_transfer_preview(KNOWN_MOUSE_KEY, &sources[0])
        .expect("both devices are known");
    assert!(!preview.is_empty());
    assert_eq!(back_binding(&state), None, "a preview writes nothing");

    let _ = state.commit_settings_transfer(KNOWN_MOUSE_KEY, &sources[0]);

    assert_eq!(back_binding(&state), Some(&Binding::Single(Action::Copy)));
    let again = state
        .settings_transfer_preview(KNOWN_MOUSE_KEY, &sources[0])
        .expect("both devices are known");
    assert!(again.is_empty());
}

#[test]
fn a_device_copies_settings_from_an_exported_file() {
    let exporting = state_with_an_old_mouse();
    let file = exporting
        .config
        .device_export(OLD_MOUSE_KEY)
        .expect("the old mouse has settings");

    let mut state = state_with_a_known_mouse();
    let sources = state.file_transfer_sources(KNOWN_MOUSE_KEY, &file);
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].name, "MX Master 2S");

    let _ = state.commit_settings_transfer(KNOWN_MOUSE_KEY, &sources[0]);

    assert_eq!(back_binding(&state), Some(&Binding::Single(Action::Copy)));
    assert!(
        state
            .device_export(KNOWN_MOUSE_KEY)
            .is_some_and(|export| export.devices.len() == 1),
        "the copied settings export as the one device"
    );
}

#[test]
fn a_file_without_a_device_of_this_kind_offers_nothing() {
    let state = state_with_a_known_mouse();
    let mut file = Config::ephemeral();
    let mut keyboard = old_mouse();
    if let Some(identity) = keyboard.identity.as_mut() {
        identity.kind = DeviceKind::Keyboard;
    }
    file.devices.insert("serial:keyboard".to_owned(), keyboard);

    assert!(
        state
            .file_transfer_sources(KNOWN_MOUSE_KEY, &file)
            .is_empty()
    );
}

/// The DPI and SmartShift the old mouse has, which the known mouse does not.
const COPIED_DPI: Dpi = Dpi::new(2400);
const SAVED_DPI: Dpi = Dpi::new(800);

fn copied_smartshift() -> SmartShift {
    SmartShift::from(SmartShiftStatus {
        mode: SmartShiftMode::Ratchet,
        auto_disengage: SmartShiftAutoDisengage::Threshold(SmartShiftThreshold::from_rounded(20.0)),
        tunable_torque: None,
    })
}

/// The known mouse, `online` or not, with a saved DPI of its own, and the
/// old mouse with another DPI and a SmartShift. Commands sent while the
/// state was built are drained.
fn known_mouse_and_an_old_one(
    online: bool,
) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut config = Config::ephemeral();
    let mut old = old_mouse();
    old.dpi = Some(COPIED_DPI);
    old.smartshift = Some(copied_smartshift());
    config.devices.insert(OLD_MOUSE_KEY.to_owned(), old);
    config.set_dpi(KNOWN_MOUSE_KEY, SAVED_DPI);
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    inventory.paired[0].online = online;
    let state = AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(config, &resolver, commands)
    });
    drain(&mut receiver);
    (state, receiver)
}

/// The mouse the inventory lists, as opposed to the old one only the config
/// remembers, with the key its card and its settings go by.
fn listed_mouse(state: &AppState) -> (&DeviceRecord, String) {
    let record = state
        .devices()
        .iter()
        .find(|record| record.record_key() != OLD_MOUSE_KEY)
        .expect("the inventory lists a mouse");
    let key = record
        .persistent_config_key()
        .expect("the mouse's settings persist")
        .to_owned();
    (record, key)
}

#[test]
fn copying_onto_an_online_mouse_writes_its_dpi_and_smartshift() {
    let (mut state, mut receiver) = known_mouse_and_an_old_one(true);
    let (record, target) = listed_mouse(&state);
    assert_eq!(target, KNOWN_MOUSE_KEY);
    let key = record.device_key();
    let route = record.route.clone().expect("the mouse is reachable");
    let sources = state.transfer_sources(&target);

    let _ = state.commit_settings_transfer(&target, &sources[0]);

    let commands = drain(&mut receiver);
    let dpi_writes: Vec<&SetDpi> = commands
        .iter()
        .filter_map(|command| match command {
            Command::SetDpi(write) => Some(write),
            _ => None,
        })
        .collect();
    let [write] = dpi_writes.as_slice() else {
        panic!("one DPI write, not {}", dpi_writes.len());
    };
    assert_eq!(
        (&write.route, write.dpi, &write.key),
        (&route, COPIED_DPI, &key)
    );
    let smartshift_writes: Vec<SmartShiftStatus> = commands
        .iter()
        .filter_map(|command| match command {
            Command::SetSmartShift(SetSmartShift {
                route: written,
                status,
            }) if *written == route => Some(*status),
            _ => None,
        })
        .collect();
    assert_eq!(
        smartshift_writes,
        [SmartShiftStatus::from(copied_smartshift())]
    );
    assert_eq!(state.config.smartshift(&target), Some(copied_smartshift()));
    assert_eq!(
        state.config.dpi(&target),
        Some(SAVED_DPI),
        "a DPI is saved once the mouse takes it, as from its panel"
    );

    let _ = state.apply_dpi_written(&key, write.write, COPIED_DPI, Ok(()));

    assert_eq!(state.config.dpi(&target), Some(COPIED_DPI));
}

#[test]
fn copying_onto_an_offline_mouse_saves_its_dpi_for_the_next_connection() {
    let (mut state, mut receiver) = known_mouse_and_an_old_one(false);
    let (record, target) = listed_mouse(&state);
    assert!(!record.is_online());
    let sources = state.transfer_sources(&target);

    let _ = state.commit_settings_transfer(&target, &sources[0]);

    let writes: Vec<Command> = drain(&mut receiver)
        .into_iter()
        .filter(|command| matches!(command, Command::SetDpi(_) | Command::SetSmartShift(_)))
        .collect();
    assert!(
        writes.is_empty(),
        "{} writes to a mouse that is off",
        writes.len()
    );
    assert_eq!(state.config.dpi(&target), Some(COPIED_DPI));
    assert_eq!(state.config.smartshift(&target), Some(copied_smartshift()));
}
