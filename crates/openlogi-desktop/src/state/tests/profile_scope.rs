//! Which per-app profile a window edits and shows.

use super::*;

#[test]
fn failed_profile_reset_or_removal_keeps_both_profiles_and_editors() {
    let mut config = Config::ephemeral();
    config.set_per_app_binding(
        KNOWN_MOUSE_KEY,
        "com.apple.Safari",
        ButtonId::Back,
        Some(Action::Undo),
    );
    let ring = &mut config.devices.get_mut(KNOWN_MOUSE_KEY).unwrap().action_ring;
    let mut layout = ring.default.clone();
    layout.set_action(
        ActionRingSlot::Top,
        Some(RingAction::new(Action::Redo).unwrap()),
    );
    ring.per_app
        .insert("com.apple.Safari".into(), layout.clone());
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
        persistence: ConfigPersistence::ReadOnly("read-only".into()),
        ..Sources::in_memory(config, &resolver, commands)
    });
    let _ = state.set_editing_app(Some("com.apple.Safari".into()));
    let _ = state.set_editing_action_ring_app(Some("com.apple.Safari".into()));
    for command in [
        AppState::reset_app_profile,
        AppState::remove_app_profile,
        AppState::reset_action_ring_profile,
        AppState::remove_action_ring_profile,
        AppState::remove_all_app_profiles,
    ] {
        let _ = command(
            &mut state,
            &DeviceKey::from(KNOWN_MOUSE_KEY),
            "com.apple.Safari",
        );
        assert_eq!(state.editing_app(), Some("com.apple.Safari"));
        assert_eq!(state.button_bindings()[&ButtonId::Back], Action::Undo);
        assert_eq!(state.editing_action_ring_app(), Some("com.apple.Safari"));
        assert_eq!(state.current_action_ring_layout(), layout);
        assert!(
            state
                .current_action_ring()
                .per_app
                .contains_key("com.apple.Safari")
        );
        assert_eq!(state.config_issue(), Some("read-only"));
        assert!(
            receiver.try_recv().is_err(),
            "failed saves must not reload the agent"
        );
    }
}

#[test]
fn removing_all_app_profiles_saves_both_sections_once_and_preserves_other_settings() {
    use openlogi_core::config::ConfigFile;

    for (buttons, ring) in [(true, true), (true, false), (false, true), (false, false)] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        let (mut config, mut file) = ConfigFile::load_from_path(&path).unwrap();
        for key in [KNOWN_MOUSE_KEY, "other-device"] {
            for app in ["Safari", "Chrome"] {
                config.set_per_app_binding(key, app, ButtonId::Back, Some(Action::Undo));
                let device = config.devices.get_mut(key).unwrap();
                let mut layout = device.action_ring.default.clone();
                layout.set_action(
                    ActionRingSlot::Top,
                    Some(RingAction::new(Action::Redo).unwrap()),
                );
                device.action_ring.per_app.insert(app.into(), layout);
            }
        }
        let device = config.devices.get_mut(KNOWN_MOUSE_KEY).unwrap();
        if !buttons {
            device.per_app_bindings.remove("Safari");
        }
        if !ring {
            device.action_ring.per_app.remove("Safari");
        }
        file.save(&config).unwrap();
        let before = config.clone();
        let resolver = AssetResolver::new();
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState::new(Sources {
            inventories: &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
            persistence: ConfigPersistence::UserFile(file),
            ..Sources::in_memory(config, &resolver, commands)
        });
        // Discard only the initial load; the command itself must reload at most once.
        receiver.try_recv().unwrap();
        let _ = state.set_editing_app(Some("Safari".into()));
        let _ = state.set_editing_action_ring_app(Some("Safari".into()));
        let inherited = state.config.action_ring(KNOWN_MOUSE_KEY).default;

        let events = state.remove_all_app_profiles(&DeviceKey::from(KNOWN_MOUSE_KEY), "Safari");
        assert_eq!(
            events,
            [StateEvent::BindingsChanged(DeviceKey::from(
                KNOWN_MOUSE_KEY
            ))]
        );
        assert_eq!(state.editing_app(), None);
        assert_eq!(state.editing_action_ring_app(), None);
        assert_eq!(state.current_action_ring_layout(), inherited);
        assert_eq!(state.button_bindings()[&ButtonId::Back], Action::MouseBack);
        let saved = Config::load_from_path(&path).unwrap();
        assert!(saved.per_app_overrides(KNOWN_MOUSE_KEY, "Safari").is_none());
        assert!(
            !saved
                .action_ring(KNOWN_MOUSE_KEY)
                .per_app
                .contains_key("Safari")
        );
        assert_eq!(
            saved.devices["other-device"],
            before.devices["other-device"]
        );
        assert_eq!(
            saved.per_app_overrides(KNOWN_MOUSE_KEY, "Chrome"),
            before.per_app_overrides(KNOWN_MOUSE_KEY, "Chrome")
        );
        assert_eq!(
            saved.action_ring(KNOWN_MOUSE_KEY).per_app["Chrome"],
            before.action_ring(KNOWN_MOUSE_KEY).per_app["Chrome"]
        );
        assert_eq!(saved.action_ring(KNOWN_MOUSE_KEY).default, inherited);
        assert_eq!(
            saved.stored_bindings(KNOWN_MOUSE_KEY),
            before.stored_bindings(KNOWN_MOUSE_KEY)
        );
        if buttons || ring {
            assert!(matches!(
                receiver.try_recv(),
                Ok(crate::services::ipc::Command::ReloadConfig(_))
            ));
        }
        assert!(
            receiver.try_recv().is_err(),
            "one atomic deletion must not send duplicate reloads"
        );
    }
}

#[test]
fn resetting_profiles_restores_inheritance_without_closing_the_editors() {
    let mut state = state_with_a_known_mouse();
    let safari = "com.apple.Safari";
    let chrome = "com.google.Chrome";
    let _ = state.commit_binding(ButtonId::Back, Action::Copy);
    let default_ring = state.current_action_ring_layout();
    for (app, action) in [(safari, Action::Undo), (chrome, Action::Paste)] {
        let _ = state.set_editing_app(Some(app.into()));
        let _ = state.commit_binding(ButtonId::Back, action.clone());
        let _ = state.set_editing_action_ring_app(Some(app.into()));
        let _ = state
            .commit_action_ring_slot(ActionRingSlot::Top, Some(RingAction::new(action).unwrap()));
    }
    let chrome_ring = state.current_action_ring_layout();
    let _ = state.set_editing_app(Some(safari.into()));
    let _ = state.set_editing_action_ring_app(Some(safari.into()));
    let safari_ring = state.current_action_ring_layout();

    let _ = state.reset_app_profile(&DeviceKey::from(KNOWN_MOUSE_KEY), safari);
    assert_eq!(state.editing_app(), Some(safari));
    assert_eq!(state.button_bindings()[&ButtonId::Back], Action::Copy);
    assert!(state.editing_app_overrides().is_none());
    assert_eq!(state.current_action_ring_layout(), safari_ring);
    assert_eq!(
        state
            .config
            .per_app_overrides(KNOWN_MOUSE_KEY, chrome)
            .unwrap()[&ButtonId::Back],
        Action::Paste
    );

    let _ = state.reset_action_ring_profile(&DeviceKey::from(KNOWN_MOUSE_KEY), safari);
    assert_eq!(state.editing_action_ring_app(), Some(safari));
    assert_eq!(state.current_action_ring_layout(), default_ring);
    assert_eq!(state.current_action_ring().per_app[chrome], chrome_ring);
    // A reset must remove the snapshot, not save a copy of today's default.
    let _ = state.set_editing_action_ring_app(None);
    let _ = state.commit_action_ring_slot(
        ActionRingSlot::Top,
        Some(RingAction::new(Action::Redo).unwrap()),
    );
    let _ = state.set_editing_action_ring_app(Some(safari.into()));
    assert_eq!(
        state.current_action_ring_layout().slots[&ActionRingSlot::Top].action(),
        &Action::Redo
    );

    let _ = state.remove_app_profile(&DeviceKey::from(KNOWN_MOUSE_KEY), safari);
    let _ = state.remove_action_ring_profile(&DeviceKey::from(KNOWN_MOUSE_KEY), safari);
    assert_eq!(state.editing_app(), None);
    assert_eq!(state.editing_action_ring_app(), None);
}

#[test]
fn profile_commands_keep_the_named_target_after_selection_changes() {
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[
            direct_inventory([0xa3, 0x93, 0xca, 0xe0]),
            second_mouse_inventory(),
        ],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    let known = state
        .devices()
        .iter()
        .position(|record| record.config_key == KNOWN_MOUSE_KEY)
        .unwrap();
    let other = state
        .devices()
        .iter()
        .position(|record| record.config_key != KNOWN_MOUSE_KEY)
        .unwrap();
    let _ = state.select_device(known);
    let _ = state.set_editing_app(Some("Safari".into()));
    let _ = state.commit_binding(ButtonId::Back, Action::Undo);
    let _ = state.set_editing_action_ring_app(Some("Safari".into()));
    let _ = state.commit_action_ring_slot(
        ActionRingSlot::Top,
        Some(RingAction::new(Action::Copy).unwrap()),
    );

    let _ = state.select_device(other);
    let _ = state.set_editing_app(Some("Chrome".into()));
    let _ = state.commit_binding(ButtonId::Back, Action::Paste);
    let _ = state.set_editing_action_ring_app(Some("Chrome".into()));
    let _ = state.commit_action_ring_slot(
        ActionRingSlot::Top,
        Some(RingAction::new(Action::Redo).unwrap()),
    );
    let other_ring = state.current_action_ring();

    let events = state.remove_all_app_profiles(&DeviceKey::from(KNOWN_MOUSE_KEY), "Safari");
    assert_eq!(
        events,
        [StateEvent::BindingsChanged(DeviceKey::from(
            KNOWN_MOUSE_KEY
        ))]
    );
    assert_eq!(state.editing_app(), Some("Chrome"));
    assert_eq!(state.button_bindings()[&ButtonId::Back], Action::Paste);
    assert_eq!(state.editing_action_ring_app(), Some("Chrome"));
    assert_eq!(state.current_action_ring(), other_ring);
    let _ = state.select_device(known);
    assert!(state.app_profiles().next().is_none());
    assert!(state.current_action_ring().per_app.is_empty());
    assert_eq!(state.editing_app(), None);
    assert_eq!(state.editing_action_ring_app(), None);
}

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
