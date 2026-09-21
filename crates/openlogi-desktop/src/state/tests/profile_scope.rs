//! Which per-app profile a window edits and shows.

use super::*;

/// A second, unmistakably different mouse, so a test can change the active device.
fn second_mouse_inventory() -> DeviceInventory {
    let mut inventory = direct_inventory([0x11, 0x22, 0x33, 0x44]);
    inventory.receiver.name = "MX Anywhere 3S".to_string();
    inventory.receiver.product_id = 0xb037;
    inventory
}

#[test]
fn a_profile_belongs_to_the_device_it_was_opened_on() {
    // Overlays are per-device, so a scope must not follow the selection onto
    // another mouse and silently edit a profile the user never opened.
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[
            direct_inventory([0xa3, 0x93, 0xca, 0xe0]),
            second_mouse_inventory(),
        ],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    let other = state
        .devices()
        .iter()
        .position(|record| record.config_key != KNOWN_MOUSE_KEY)
        .expect("the fixture pairs a second device");
    let known = state
        .devices()
        .iter()
        .position(|record| record.config_key == KNOWN_MOUSE_KEY)
        .expect("the fixture pairs the known mouse");

    let _ = state.select_device(known);
    let _ = state.set_editing_app(Some("com.apple.Safari".into()));

    let _ = state.select_device(other);
    assert_eq!(
        state.editing_app(),
        None,
        "another device falls back to its own global profile"
    );

    let _ = state.select_device(known);
    assert_eq!(
        state.editing_app(),
        Some("com.apple.Safari"),
        "and returning restores the profile that was open here"
    );
}

#[test]
fn invalid_device_selection_preserves_the_valid_current_device() {
    let mut state = state_with_a_known_mouse();
    let selected = state.selected_device_index();

    assert!(state.select_device(usize::MAX).is_empty());
    assert_eq!(state.selected_device_index(), selected);
    assert!(state.current_record().is_some());
}

#[test]
fn the_active_profile_is_the_default_until_the_app_in_front_is_overridden() {
    let mut state = state_with_a_known_mouse();
    let safari = app("com.apple.Safari", "Safari");
    let _ = state.set_foreground(ForegroundApps {
        current: Some(safari.clone()),
        recent: vec![safari],
    });

    assert_eq!(
        state.active_profile_name(),
        None,
        "an app with no overrides runs the device's global bindings"
    );

    state.config.edit(|config| {
        config.set_per_app_binding(
            KNOWN_MOUSE_KEY,
            "com.apple.Safari",
            ButtonId::Back,
            Some(Action::Undo),
        );
    });
    assert_eq!(state.active_profile_name(), Some("Safari"));
}

#[test]
fn the_profile_shown_is_the_apps_even_while_this_window_has_focus() {
    // The frontmost application is OpenLogi whenever the user is looking at
    // this panel, so keying off `current` would report "Default profile" for
    // exactly the moment the row is on screen (issue: the row had no content
    // at all before). The recent list excludes our own windows, so its head is
    // the app the user came from.
    let mut state = state_with_a_known_mouse();
    state.config.edit(|config| {
        config.set_per_app_binding(
            KNOWN_MOUSE_KEY,
            "com.apple.Safari",
            ButtonId::Back,
            Some(Action::Undo),
        );
    });
    let _ = state.set_foreground(ForegroundApps {
        current: Some(app(openlogi_core::brand::APP_ID, "OpenLogi")),
        recent: vec![app("com.apple.Safari", "Safari")],
    });

    assert_eq!(state.active_profile_name(), Some("Safari"));
}

#[test]
fn a_host_with_no_readable_foreground_app_reports_the_default_profile() {
    let mut state = state_with_a_known_mouse();
    state.config.edit(|config| {
        config.set_per_app_binding(
            KNOWN_MOUSE_KEY,
            "com.apple.Safari",
            ButtonId::Back,
            Some(Action::Undo),
        );
    });
    // A pure-Wayland session with no usable backend, or a watcher that could
    // not start: the agent reports nothing and no profile can be in effect.
    assert!(state.set_foreground(ForegroundApps::default()).is_empty());
    assert_eq!(state.active_profile_name(), None);
}
