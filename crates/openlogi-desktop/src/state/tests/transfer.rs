//! Copying settings onto a device from another device or from a file.

use super::*;

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
