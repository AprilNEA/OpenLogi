//! Button, thumb-wheel, gesture and Actions Ring edits, global and per-app.

use super::*;
use openlogi_core::binding::LongPressBinding;

#[test]
fn picking_a_short_action_preserves_the_long_press_binding() {
    use openlogi_core::config::ConfigFile;

    for button in [
        ButtonId::Back,
        ButtonId::Forward,
        ButtonId::DpiToggle,
        ButtonId::HapticPanel,
        ButtonId::GestureButton,
        ButtonId::MiddleClick,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        let (mut config, mut file) = ConfigFile::load_from_path(&path).unwrap();
        config.set_binding(
            KNOWN_MOUSE_KEY,
            button,
            Binding::LongPress(LongPressBinding::new(Action::Copy, Action::MissionControl)),
        );
        file.save(&config).unwrap();
        let resolver = AssetResolver::new();
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState::new(Sources {
            inventories: &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
            persistence: ConfigPersistence::UserFile(file),
            ..Sources::in_memory(config, &resolver, commands)
        });
        super::fn_lock::drain(&mut receiver);

        for action in [Action::Screenshot, Action::None] {
            let events = state.commit_binding(button, action.clone());

            let expected = Binding::LongPress(LongPressBinding::new(
                action.clone(),
                Action::MissionControl,
            ));
            assert_eq!(
                state.config.stored_bindings(KNOWN_MOUSE_KEY).get(&button),
                Some(&expected),
                "editing {button:?}'s short action must retain its long action"
            );
            assert_eq!(state.button_bindings()[&button], action);
            assert_eq!(
                Config::load_from_path(&path)
                    .unwrap()
                    .stored_bindings(KNOWN_MOUSE_KEY)
                    .get(&button),
                Some(&expected)
            );
            assert_eq!(
                events,
                [StateEvent::BindingsChanged(DeviceKey::from(
                    KNOWN_MOUSE_KEY
                ))]
            );
            let commands = super::fn_lock::drain(&mut receiver);
            assert!(matches!(
                commands.as_slice(),
                [crate::services::ipc::Command::ReloadConfig(_)]
            ));
        }
    }
}

#[test]
fn per_app_action_edits_leave_the_global_long_press_intact() {
    let mut config = Config::ephemeral();
    let binding = Binding::LongPress(LongPressBinding::new(Action::Copy, Action::MissionControl));
    config.set_binding(KNOWN_MOUSE_KEY, ButtonId::HapticPanel, binding.clone());
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
        ..Sources::in_memory(config, &resolver, commands)
    });
    let _ = state.set_editing_app(Some("com.apple.Safari".into()));

    let _ = state.commit_binding(ButtonId::HapticPanel, Action::Paste);

    assert_eq!(
        state
            .config
            .stored_bindings(KNOWN_MOUSE_KEY)
            .get(&ButtonId::HapticPanel),
        Some(&binding)
    );
    assert_eq!(
        state.button_bindings()[&ButtonId::HapticPanel],
        Action::Paste
    );
    let _ = state.clear_app_binding(ButtonId::HapticPanel);
    assert_eq!(
        state.button_bindings()[&ButtonId::HapticPanel],
        Action::Copy
    );
    assert_eq!(
        state
            .config
            .stored_bindings(KNOWN_MOUSE_KEY)
            .get(&ButtonId::HapticPanel),
        Some(&binding)
    );
}

#[test]
fn failed_short_action_save_restores_both_long_press_actions() {
    let mut config = Config::ephemeral();
    let binding = Binding::LongPress(LongPressBinding::new(Action::Copy, Action::MissionControl));
    config.set_binding(KNOWN_MOUSE_KEY, ButtonId::HapticPanel, binding.clone());
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
        persistence: ConfigPersistence::ReadOnly("read-only".into()),
        ..Sources::in_memory(config, &resolver, commands)
    });
    super::fn_lock::drain(&mut receiver);

    let _ = state.commit_binding(ButtonId::HapticPanel, Action::Paste);

    assert_eq!(
        state
            .config
            .stored_bindings(KNOWN_MOUSE_KEY)
            .get(&ButtonId::HapticPanel),
        Some(&binding)
    );
    assert_eq!(
        state.button_bindings()[&ButtonId::HapticPanel],
        Action::Copy
    );
    assert_eq!(state.config_issue(), Some("read-only"));
    assert!(
        receiver.try_recv().is_err(),
        "a failed save must not reload the agent"
    );
}

#[test]
fn thumbwheel_pair_updates_both_memory_and_config_entries() {
    let mut bindings = std::collections::BTreeMap::new();
    let mut config = Config::ephemeral();
    let key = "2b034";

    assert!(apply_thumbwheel_pair(
        &mut bindings,
        &mut config,
        Some(key),
        None,
        ThumbwheelPreset::Volume.pair(),
    ));
    assert_eq!(
        bindings.get(&ButtonId::ThumbwheelScrollDown),
        Some(&Action::VolumeDown)
    );
    assert_eq!(
        bindings.get(&ButtonId::ThumbwheelScrollUp),
        Some(&Action::VolumeUp)
    );
    let persisted = config.stored_bindings(key);
    assert_eq!(
        persisted.get(&ButtonId::ThumbwheelScrollDown),
        Some(&Binding::Single(Action::VolumeDown))
    );
    assert_eq!(
        persisted.get(&ButtonId::ThumbwheelScrollUp),
        Some(&Binding::Single(Action::VolumeUp))
    );
}

#[test]
fn transient_thumbwheel_pair_stays_in_memory_without_persistence() {
    let mut bindings = std::collections::BTreeMap::new();
    let mut config = Config::ephemeral();

    assert!(!apply_thumbwheel_pair(
        &mut bindings,
        &mut config,
        None,
        None,
        ThumbwheelPreset::CycleDpi.pair(),
    ));
    assert_eq!(bindings.len(), 2);
    assert!(config.stored_bindings("missing").is_empty());
}

/// A known mouse with `app`'s profile open for editing.
fn state_editing(app: &str) -> AppState {
    let mut state = state_with_a_known_mouse();
    let _ = state.set_editing_app(Some(app.to_string()));
    assert_eq!(state.editing_app(), Some(app), "scope did not take");
    state
}

#[test]
fn a_binding_committed_in_a_per_app_profile_leaves_the_global_one_alone() {
    let mut state = state_editing("com.apple.Safari");
    let _ = state.commit_binding(ButtonId::Back, Action::Undo);

    assert_eq!(
        state
            .config
            .per_app_overrides(KNOWN_MOUSE_KEY, "com.apple.Safari"),
        Some(&BTreeMap::from([(ButtonId::Back, Action::Undo)]))
    );
    assert!(
        state.config.stored_bindings(KNOWN_MOUSE_KEY).is_empty(),
        "the device's global bindings must be untouched"
    );
}

fn ring_action(action: Action) -> RingAction {
    RingAction::new(action).expect("test action must be valid in the Actions Ring")
}

#[test]
fn an_unsaved_action_ring_profile_inherits_default_until_its_first_edit() {
    let mut state = state_with_a_known_mouse();
    let inherited = state.current_action_ring_layout();

    let _ = state.set_editing_action_ring_app(Some("com.apple.Safari".into()));

    assert_eq!(state.current_action_ring_layout(), inherited);
    assert!(
        state.current_action_ring().per_app.is_empty(),
        "selecting an application must not persist an unchanged layout"
    );

    let _ = state.commit_action_ring_slot(ActionRingSlot::Top, Some(ring_action(Action::NewTab)));

    let ring = state.current_action_ring();
    let safari = ring
        .per_app
        .get("com.apple.Safari")
        .expect("the first edit creates the application layout");
    assert_eq!(
        ring.default, inherited,
        "the default layout stays unchanged"
    );
    assert_eq!(safari.slots[&ActionRingSlot::Top].action(), &Action::NewTab);
    assert_eq!(
        safari.slots[&ActionRingSlot::Bottom],
        inherited.slots[&ActionRingSlot::Bottom],
        "the application profile starts as a complete copy of Default"
    );
}

#[test]
fn an_action_ring_icon_edit_targets_the_open_application_layout() {
    let mut state = state_with_a_known_mouse();
    let _ = state.set_editing_action_ring_app(Some("com.apple.Safari".into()));

    let _ = state.commit_action_ring_icon(ActionRingSlot::Top, Some(ActionRingIcon::Keyboard));

    let ring = state.current_action_ring();
    assert_eq!(ring.default.slots[&ActionRingSlot::Top].custom_icon(), None);
    assert_eq!(
        ring.per_app["com.apple.Safari"].slots[&ActionRingSlot::Top].custom_icon(),
        Some(ActionRingIcon::Keyboard)
    );
}

#[test]
fn removing_an_action_ring_profile_leaves_button_overrides_untouched() {
    let mut state = state_with_a_known_mouse();
    let _ = state.set_editing_app(Some("com.apple.Safari".into()));
    let _ = state.commit_binding(ButtonId::Back, Action::Undo);
    let _ = state.set_editing_action_ring_app(Some("com.apple.Safari".into()));
    let _ = state.commit_action_ring_slot(ActionRingSlot::Top, Some(ring_action(Action::NewTab)));

    let _ = state.remove_action_ring_profile(&DeviceKey::from(KNOWN_MOUSE_KEY), "com.apple.Safari");

    assert_eq!(state.editing_action_ring_app(), None);
    assert!(state.current_action_ring().per_app.is_empty());
    assert_eq!(
        state
            .config
            .per_app_overrides(KNOWN_MOUSE_KEY, "com.apple.Safari"),
        Some(&BTreeMap::from([(ButtonId::Back, Action::Undo)]))
    );
    assert_eq!(
        state.editing_app(),
        Some("com.apple.Safari"),
        "the Buttons editor keeps its independent scope"
    );
}

#[test]
fn clearing_an_override_falls_back_to_the_global_binding() {
    let mut state = state_with_a_known_mouse();
    let _ = state.commit_binding(ButtonId::Back, Action::Copy);
    let _ = state.set_editing_app(Some("com.apple.Safari".into()));
    let _ = state.commit_binding(ButtonId::Back, Action::Undo);
    assert_eq!(
        state.button_bindings().get(&ButtonId::Back),
        Some(&Action::Undo)
    );

    let _ = state.clear_app_binding(ButtonId::Back);

    assert_eq!(
        state.button_bindings().get(&ButtonId::Back),
        Some(&Action::Copy),
        "the panel falls back to what the default profile binds"
    );
    assert!(
        state
            .config
            .per_app_overrides(KNOWN_MOUSE_KEY, "com.apple.Safari")
            .is_none(),
        "an emptied profile is pruned, not left behind"
    );
}

#[test]
fn clearing_a_thumbwheel_override_drops_both_directions() {
    let mut state = state_with_a_known_mouse();
    let _ = state.commit_thumbwheel_preset(ThumbwheelPreset::Volume);
    let _ = state.set_editing_app(Some("com.apple.Safari".into()));
    let _ = state.commit_thumbwheel_preset(ThumbwheelPreset::CycleDpi);

    let _ = state.clear_app_thumbwheel();

    assert_eq!(
        state.button_bindings().get(&ButtonId::ThumbwheelScrollDown),
        Some(&Action::VolumeDown)
    );
    assert_eq!(
        state.button_bindings().get(&ButtonId::ThumbwheelScrollUp),
        Some(&Action::VolumeUp)
    );
    assert!(
        state
            .config
            .per_app_overrides(KNOWN_MOUSE_KEY, "com.apple.Safari")
            .is_none(),
        "both halves must be cleared so the empty profile is pruned"
    );
}

#[test]
fn gesture_mode_is_not_editable_from_inside_a_per_app_profile() {
    // The trap this guards: `set_gesture_mode` writes the device's global
    // bindings, so honouring it here would change every application from a
    // panel labelled with one. A per-app entry is `Action`-valued and has no
    // per-direction shape to promote into.
    let mut state = state_editing("com.apple.Safari");

    let _ = state.commit_gesture_mode(ButtonId::DpiToggle, true);

    assert!(
        !state
            .config
            .is_gesture_mode(KNOWN_MOUSE_KEY, ButtonId::DpiToggle),
        "a per-app profile must not promote a button globally"
    );
    assert!(
        state.current_gesture_maps().is_empty(),
        "and no gesture menu is offered in that scope"
    );
}

#[test]
fn a_gesture_button_stays_one_when_the_scope_returns_to_the_default_profile() {
    let mut state = state_with_a_known_mouse();
    let _ = state.commit_gesture_mode(ButtonId::DpiToggle, true);
    let global = state.current_gesture_maps();
    assert!(global.contains_key(&ButtonId::DpiToggle));

    let _ = state.set_editing_app(Some("com.apple.Safari".into()));
    assert!(state.current_gesture_maps().is_empty());
    assert_eq!(
        state.gesture_bindings(),
        &global,
        "the inspector cache keeps inherited gestures while per-app editing hides their controls"
    );
    // The device still has its gestures — only the open profile cannot show
    // them, which is what the device card must keep reporting.
    assert_eq!(
        state.device_gesture_binding_count(),
        global.values().map(BTreeMap::len).sum::<usize>()
    );

    let _ = state.set_editing_app(None);
    assert_eq!(state.current_gesture_maps(), global);
}

#[test]
fn dpi_gesture_enablement_requires_measured_support_but_preserves_stored_maps() {
    for capabilities in [
        None,
        Some(Capabilities::presumed_from_kind(DeviceKind::Mouse)),
    ] {
        let mut state = state_with_a_known_mouse();
        let _ = state.commit_binding(ButtonId::DpiToggle, Action::Paste);
        state.devices.records[0].capabilities = capabilities;
        let _ = state.commit_gesture_mode(ButtonId::DpiToggle, true);
        assert!(
            !state
                .config
                .is_gesture_mode(KNOWN_MOUSE_KEY, ButtonId::DpiToggle)
        );
        assert_eq!(
            state.button_bindings().get(&ButtonId::DpiToggle),
            Some(&Action::Paste)
        );

        // A stored map survives a missing/negative capability snapshot and
        // remains editable and removable, but cannot be newly enabled.
        state
            .config
            .edit(|config| config.set_gesture_mode(KNOWN_MOUSE_KEY, ButtonId::DpiToggle, true));
        let _ =
            state.commit_gesture_binding(ButtonId::DpiToggle, GestureDirection::Up, Action::Copy);
        assert_eq!(
            state.current_gesture_maps()[&ButtonId::DpiToggle][&GestureDirection::Up],
            Action::Copy
        );
        let _ = state.commit_gesture_mode(ButtonId::DpiToggle, false);
        assert!(
            !state
                .config
                .is_gesture_mode(KNOWN_MOUSE_KEY, ButtonId::DpiToggle)
        );
        let _ = state.commit_gesture_mode(ButtonId::DpiToggle, true);
        assert!(
            !state
                .config
                .is_gesture_mode(KNOWN_MOUSE_KEY, ButtonId::DpiToggle)
        );
    }
}

#[test]
fn unsupported_controls_cannot_enter_gesture_mode_through_the_ui_state() {
    let mut state = state_with_a_known_mouse();

    for button in [
        ButtonId::LeftClick,
        ButtonId::RightClick,
        ButtonId::WheelTiltLeft,
        ButtonId::WheelTiltRight,
        ButtonId::Thumbwheel,
        ButtonId::ThumbwheelScrollUp,
        ButtonId::ThumbwheelScrollDown,
    ] {
        let _ = state.commit_gesture_mode(button, true);
        assert!(
            !state.config.is_gesture_mode(KNOWN_MOUSE_KEY, button),
            "unsupported control {button:?} entered gesture mode"
        );
    }
}

#[test]
fn middle_click_can_enter_gesture_mode_from_a_fresh_binding() {
    let mut state = state_with_a_known_mouse();

    let _ = state.commit_gesture_mode(ButtonId::MiddleClick, true);

    assert!(
        state
            .config
            .is_gesture_mode(KNOWN_MOUSE_KEY, ButtonId::MiddleClick)
    );
    assert_eq!(
        state
            .current_gesture_maps()
            .get(&ButtonId::MiddleClick)
            .and_then(|map| map.get(&GestureDirection::Click)),
        Some(&Action::MiddleClick),
        "a tap must retain native middle-click behavior"
    );
}

#[test]
fn middle_click_gesture_can_be_edited_disabled_and_reenabled() {
    let middle = BTreeMap::from([
        (GestureDirection::Click, Action::MiddleClick),
        (GestureDirection::Up, Action::Copy),
    ]);
    let mut config = Config::ephemeral();
    config.set_binding(
        KNOWN_MOUSE_KEY,
        ButtonId::MiddleClick,
        Binding::Gesture(middle.clone()),
    );
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
        ..Sources::in_memory(config, &resolver, commands)
    });

    assert_eq!(
        state.current_gesture_maps().get(&ButtonId::MiddleClick),
        Some(&middle),
        "the inspector must expose the persisted directions"
    );

    let _ =
        state.commit_gesture_binding(ButtonId::MiddleClick, GestureDirection::Down, Action::Paste);
    assert_eq!(
        state
            .current_gesture_maps()
            .get(&ButtonId::MiddleClick)
            .and_then(|map| map.get(&GestureDirection::Down)),
        Some(&Action::Paste),
        "an existing Middle Click gesture must remain editable"
    );

    let _ = state.commit_gesture_mode(ButtonId::MiddleClick, false);
    assert!(
        !state
            .config
            .is_gesture_mode(KNOWN_MOUSE_KEY, ButtonId::MiddleClick)
    );
    assert!(
        !state
            .current_gesture_maps()
            .contains_key(&ButtonId::MiddleClick)
    );

    let _ = state.commit_gesture_mode(ButtonId::MiddleClick, true);
    assert!(
        state
            .config
            .is_gesture_mode(KNOWN_MOUSE_KEY, ButtonId::MiddleClick)
    );
    assert_eq!(
        state
            .current_gesture_maps()
            .get(&ButtonId::MiddleClick)
            .and_then(|map| map.get(&GestureDirection::Down)),
        Some(&Action::Paste),
        "re-enabling must restore the edited directions"
    );
}

#[test]
fn gesture_maps_cover_every_gesture_mode_button() {
    // With per-button gesture mode, the GUI's display maps carry one entry
    // per gesture-mode button: the dedicated button's seeded default map
    // plus a promoted OS-hook button's stored map — simultaneously.
    use openlogi_core::binding::{ButtonId, GestureDirection};

    let mut config = Config::default();
    config.set_device_identity(
        "2b042",
        DeviceIdentity {
            display_name: "MX Master 4".to_string(),
            kind: DeviceKind::Mouse,
            capabilities: Capabilities::presumed_from_kind(DeviceKind::Mouse),
            light_capabilities: None,
            model_info: None,
            codename: None,
            driver_id: None,
            registry_model_id: None,
        },
    );
    config.set_gesture_mode("2b042", ButtonId::Back, true);
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources::in_memory(config, &AssetResolver::new(), commands));

    let maps = state.current_gesture_maps();
    let dedicated = maps
        .get(&ButtonId::GestureButton)
        .expect("the dedicated button's default gesture mode must be shown");
    assert!(
        dedicated.contains_key(&GestureDirection::Up),
        "HID++ maps are shown seeded, matching watcher dispatch"
    );
    assert!(
        maps.contains_key(&ButtonId::Back),
        "a promoted OS-hook button gets its own menu simultaneously"
    );
}

/// Excel as the Windows foreground producer names it: a lower-cased path.
const EXCEL: &str = r"c:\program files\microsoft office\root\office16\excel.exe";

fn review_scroll_alias(identifier: &str) -> Config {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    std::fs::write(
        &path,
        format!(
            r#"schema_version = {}
[apps.excel]
macos = '{identifier}'
linux = '{identifier}'
windows = '{identifier}'
[thumbwheel_apps."app:excel"]
delivery = "shift_wheel"
"#,
            openlogi_core::config::SCHEMA_VERSION
        ),
    )
    .unwrap();
    Config::load_from_path(&path).unwrap()
}

#[test]
fn review_scroll_selector_choices_override_the_runtime_rule() {
    use openlogi_core::config::ThumbwheelAppRule;
    use openlogi_core::scroll::ScrollDelivery;

    for identifier in [EXCEL, "exe:excel.exe"] {
        for editor in ["exe:excel.exe", "app:excel"] {
            for delivery in [ScrollDelivery::Native, ScrollDelivery::CtrlShiftWheel] {
                let mut config = review_scroll_alias(identifier);
                let alias = config.thumbwheel_apps["app:excel"].clone();
                for key in [EXCEL, "exe:excel.exe"] {
                    config.thumbwheel_apps.insert(
                        key.into(),
                        ThumbwheelAppRule {
                            delivery: Some(ScrollDelivery::AltWheel),
                        },
                    );
                }
                assert!(set_app_scroll_delivery(&mut config, editor, delivery));
                assert_eq!(
                    config.scroll_delivery(Some(EXCEL)),
                    delivery,
                    "{identifier}: {editor}"
                );
                assert_eq!(
                    config.thumbwheel_apps["app:excel"], alias,
                    "portable alias stays intact"
                );
                assert!(!config.thumbwheel_apps.contains_key(EXCEL));
                assert!(!set_app_scroll_delivery(&mut config, editor, delivery));
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
                let (_, mut file) =
                    openlogi_core::config::ConfigFile::load_from_path(&path).unwrap();
                file.save(&config).unwrap();
                let reloaded = Config::load_from_path(&path).unwrap();
                assert_eq!(reloaded.scroll_delivery(Some(EXCEL)), delivery);
            }
        }
    }
}

#[test]
#[cfg(target_os = "windows")]
fn review_scroll_alias_profiles_are_not_duplicated_or_listed_after_native_override() {
    use openlogi_core::config::ThumbwheelAppRule;
    use openlogi_core::scroll::ScrollDelivery;

    for identifier in [EXCEL, "exe:excel.exe"] {
        let mut state = state_with_a_known_mouse();
        state.config.edit(|config| {
            let alias = review_scroll_alias(identifier);
            config.apps = alias.apps;
            config.thumbwheel_apps = alias.thumbwheel_apps;
        });
        let _ = state.set_editing_app(Some(EXCEL.into()));
        let _ = state.commit_binding(ButtonId::Back, Action::Undo);
        assert_eq!(state.app_profiles().collect::<Vec<_>>(), [(EXCEL, 1)]);

        state.config.edit(|config| {
            config.remove_app_profile(KNOWN_MOUSE_KEY, EXCEL);
            config.thumbwheel_apps.insert(
                "exe:excel.exe".into(),
                ThumbwheelAppRule {
                    delivery: Some(ScrollDelivery::Native),
                },
            );
        });
        assert!(
            state.app_profiles().next().is_none(),
            "an overridden alias is not a profile"
        );
        let _ = state.set_editing_app(None);
        assert!(
            state.app_profiles().next().is_none(),
            "a closed editor must not revive an alias"
        );
    }
}

#[test]
#[cfg(target_os = "windows")]
fn review_scroll_exact_native_override_does_not_list_a_recent_app() {
    use openlogi_core::config::ThumbwheelAppRule;
    use openlogi_core::scroll::ScrollDelivery;

    let mut state = state_with_a_known_mouse();
    state.config.edit(|config| {
        for (key, delivery) in [
            (EXCEL, ScrollDelivery::Native),
            ("exe:excel.exe", ScrollDelivery::ShiftWheel),
        ] {
            config.thumbwheel_apps.insert(
                key.into(),
                ThumbwheelAppRule {
                    delivery: Some(delivery),
                },
            );
        }
    });
    let _ = state.set_editing_app(Some(EXCEL.into()));
    assert!(
        state.app_profiles().next().is_none(),
        "the effective Native rule wins"
    );
    let _ = state.set_editing_app(None);
    let seen = ForegroundApp::unnamed(EXCEL.into());
    let _ = state.set_foreground(ForegroundApps {
        current: Some(seen.clone()),
        recent: vec![seen],
    });
    assert!(
        state.app_profiles().next().is_none(),
        "a recent identifier also uses the effective Native rule"
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn review_scroll_profiles_preserve_windows_settings_on_unsupported_platforms() {
    for command in [
        AppState::reset_app_profile,
        AppState::remove_app_profile,
        AppState::remove_all_app_profiles,
    ] {
        let mut state = state_with_a_known_mouse();
        let _ = state.set_editing_app(Some(EXCEL.into()));
        let _ = state.commit_binding(ButtonId::Back, Action::Undo);
        state.config.edit(|config| {
            let alias = review_scroll_alias(EXCEL);
            config.apps = alias.apps;
            config.thumbwheel_apps = alias.thumbwheel_apps;
        });
        let rules = state.config.thumbwheel_apps.clone();
        assert!(!state.app_has_scroll_method(EXCEL));
        let _ = command(&mut state, &DeviceKey::from(KNOWN_MOUSE_KEY), EXCEL);
        assert_eq!(state.config.thumbwheel_apps, rules);
        assert!(state.app_profiles().next().is_none());
    }
}

#[test]
fn a_scroll_method_is_written_under_the_executable_selector() {
    use openlogi_core::config::ThumbwheelAppRule;
    use openlogi_core::scroll::ScrollDelivery;

    let mut config = Config::ephemeral();
    assert!(set_app_scroll_delivery(
        &mut config,
        EXCEL,
        ScrollDelivery::CtrlShiftWheel
    ));
    assert_eq!(
        config.thumbwheel_apps,
        BTreeMap::from([(
            "exe:excel.exe".to_string(),
            ThumbwheelAppRule {
                delivery: Some(ScrollDelivery::CtrlShiftWheel),
            },
        )]),
        "the rule survives a versioned install directory"
    );
    assert!(
        !set_app_scroll_delivery(&mut config, EXCEL, ScrollDelivery::CtrlShiftWheel),
        "re-picking the current method changes nothing"
    );

    assert!(set_app_scroll_delivery(
        &mut config,
        EXCEL,
        ScrollDelivery::ShiftWheel
    ));
    assert_eq!(
        config.scroll_delivery(Some(EXCEL)),
        ScrollDelivery::ShiftWheel
    );

    assert!(set_app_scroll_delivery(
        &mut config,
        EXCEL,
        ScrollDelivery::Native
    ));
    assert!(
        config.thumbwheel_apps.is_empty(),
        "native removes the rule rather than storing the default"
    );
    assert!(
        !set_app_scroll_delivery(&mut config, EXCEL, ScrollDelivery::Native),
        "there is nothing left to remove"
    );
}

#[test]
fn a_hand_written_exact_rule_cannot_mask_the_choice() {
    use openlogi_core::config::ThumbwheelAppRule;
    use openlogi_core::scroll::ScrollDelivery;

    let exact = |delivery| ThumbwheelAppRule {
        delivery: Some(delivery),
    };
    let mut config = Config::ephemeral();
    config
        .thumbwheel_apps
        .insert(EXCEL.to_string(), exact(ScrollDelivery::AltWheel));
    config.thumbwheel_apps.insert(
        r"c:\tools\other.exe".to_string(),
        exact(ScrollDelivery::AltWheel),
    );

    assert!(set_app_scroll_delivery(
        &mut config,
        EXCEL,
        ScrollDelivery::VerticalWheel
    ));
    assert_eq!(
        config.scroll_delivery(Some(EXCEL)),
        ScrollDelivery::VerticalWheel,
        "the exact key outranks `exe:`, so it must not survive the choice"
    );
    assert!(!config.thumbwheel_apps.contains_key(EXCEL));

    config
        .thumbwheel_apps
        .insert(EXCEL.to_string(), exact(ScrollDelivery::AltWheel));
    assert!(set_app_scroll_delivery(
        &mut config,
        EXCEL,
        ScrollDelivery::Native
    ));
    assert_eq!(config.scroll_delivery(Some(EXCEL)), ScrollDelivery::Native);
    assert_eq!(
        config.thumbwheel_apps.keys().collect::<Vec<_>>(),
        [r"c:\tools\other.exe"],
        "another application's rule is left alone"
    );
}

#[test]
fn a_scroll_method_choice_overrides_an_alias_and_round_trips() {
    use openlogi_core::config::{ConfigFile, SCHEMA_VERSION, ThumbwheelAppRule};
    use openlogi_core::scroll::ScrollDelivery;

    for delivery in [ScrollDelivery::Native, ScrollDelivery::CtrlShiftWheel] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        std::fs::write(
            &path,
            format!(
                r#"schema_version = {SCHEMA_VERSION}
[apps.excel]
macos = "exe:excel.exe"
linux = "exe:excel.exe"
windows = "exe:excel.exe"
[thumbwheel_apps."app:excel"]
delivery = "shift_wheel"
"#,
            ),
        )
        .unwrap();
        let (mut config, mut file) = ConfigFile::load_from_path(&path).unwrap();
        let alias = config.thumbwheel_apps["app:excel"].clone();
        assert_eq!(
            config.scroll_delivery(Some(EXCEL)),
            ScrollDelivery::ShiftWheel
        );
        for key in [EXCEL, "exe:excel.exe"] {
            config.thumbwheel_apps.insert(
                key.to_string(),
                ThumbwheelAppRule {
                    delivery: Some(ScrollDelivery::AltWheel),
                },
            );
        }

        assert!(set_app_scroll_delivery(&mut config, EXCEL, delivery));
        assert_eq!(config.scroll_delivery(Some(EXCEL)), delivery);
        assert!(!config.thumbwheel_apps.contains_key(EXCEL));
        assert_eq!(config.thumbwheel_apps["app:excel"], alias);
        assert_eq!(
            config.thumbwheel_apps["exe:excel.exe"].delivery,
            Some(delivery),
            "native must be explicit when the alias would otherwise win"
        );
        assert!(
            !set_app_scroll_delivery(&mut config, EXCEL, delivery),
            "re-picking the effective method changes nothing"
        );

        file.save(&config).unwrap();
        let reloaded = Config::load_from_path(&path).unwrap();
        assert_eq!(reloaded.scroll_delivery(Some(EXCEL)), delivery);
        assert_eq!(reloaded.thumbwheel_apps, config.thumbwheel_apps);
        assert_eq!(reloaded.apps, config.apps);
    }
}

#[test]
fn the_scroll_method_is_offered_only_in_a_per_app_profile_on_windows() {
    use openlogi_core::scroll::ScrollDelivery;

    use crate::features::mouse::thumbwheel::{
        SCROLL_METHODS_SUPPORTED, ScrollMethodChoice, ScrollMethodSuggestion,
    };

    let mut state = state_with_a_known_mouse();
    assert_eq!(state.editing_scroll_method(), None);
    let _ = state.commit_scroll_delivery(ScrollDelivery::ShiftWheel);
    assert!(
        state.config.thumbwheel_apps.is_empty(),
        "the global profile names no application to write a rule for"
    );

    let _ = state.set_editing_app(Some(EXCEL.into()));
    assert_eq!(
        state.editing_scroll_method(),
        SCROLL_METHODS_SUPPORTED.then_some(ScrollMethodChoice {
            current: ScrollDelivery::Native,
            suggestion: Some(ScrollMethodSuggestion {
                delivery: ScrollDelivery::CtrlShiftWheel,
                needs_verification: true,
            }),
        }),
        "a suggestion is offered, never applied"
    );
    assert!(state.config.thumbwheel_apps.is_empty());

    let events = state.commit_scroll_delivery(ScrollDelivery::CtrlShiftWheel);
    assert_eq!(
        events,
        [StateEvent::BindingsChanged(DeviceKey::from(
            KNOWN_MOUSE_KEY
        ))]
    );
    assert_eq!(
        state.editing_scroll_method().map(|choice| choice.current),
        SCROLL_METHODS_SUPPORTED.then_some(ScrollDelivery::CtrlShiftWheel)
    );
    assert!(
        state
            .config
            .per_app_overrides(KNOWN_MOUSE_KEY, EXCEL)
            .is_none(),
        "the method is an application rule, not a button override"
    );
}

#[test]
fn a_scroll_method_is_saved_and_reloaded_and_native_deletes_it() {
    use openlogi_core::config::ConfigFile;
    use openlogi_core::scroll::ScrollDelivery;

    use crate::services::ipc::Command;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    let (config, file) = ConfigFile::load_from_path(&path).unwrap();
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
        persistence: ConfigPersistence::UserFile(file),
        ..Sources::in_memory(config, &resolver, commands)
    });
    // Discard only the initial load.
    receiver.try_recv().unwrap();
    let _ = state.set_editing_app(Some(EXCEL.into()));

    let _ = state.commit_scroll_delivery(ScrollDelivery::CtrlShiftWheel);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("[thumbwheel_apps.\"exe:excel.exe\"]")
            && text.contains("delivery = \"ctrl_shift_wheel\""),
        "{text}"
    );
    assert_eq!(
        Config::load_from_path(&path)
            .unwrap()
            .scroll_delivery(Some(EXCEL)),
        ScrollDelivery::CtrlShiftWheel
    );
    assert!(matches!(receiver.try_recv(), Ok(Command::ReloadConfig(_))));

    let _ = state.commit_scroll_delivery(ScrollDelivery::CtrlShiftWheel);
    assert!(
        receiver.try_recv().is_err(),
        "an unchanged method is neither saved nor reloaded"
    );

    let _ = state.commit_scroll_delivery(ScrollDelivery::Native);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("thumbwheel_apps"), "{text}");
    assert!(matches!(receiver.try_recv(), Ok(Command::ReloadConfig(_))));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn a_failed_save_leaves_no_scroll_method_behind() {
    use openlogi_core::scroll::ScrollDelivery;

    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
        persistence: ConfigPersistence::ReadOnly("read-only".into()),
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    while receiver.try_recv().is_ok() {}
    let _ = state.set_editing_app(Some(EXCEL.into()));

    let _ = state.commit_scroll_delivery(ScrollDelivery::ShiftWheel);

    assert_eq!(
        state.config.scroll_delivery(Some(EXCEL)),
        ScrollDelivery::Native
    );
    assert_eq!(state.config_issue(), Some("read-only"));
    assert!(
        receiver.try_recv().is_err(),
        "failed saves must not reload the agent"
    );
}

#[test]
fn a_scroll_method_alone_keeps_the_app_listed_as_a_profile() {
    use openlogi_core::scroll::ScrollDelivery;

    use crate::features::mouse::thumbwheel::SCROLL_METHODS_SUPPORTED;

    let listed = |state: &AppState| {
        state
            .app_profiles()
            .map(|(app, count)| (app.to_string(), count))
            .collect::<Vec<_>>()
    };
    let rule_only = |app: &str| {
        if SCROLL_METHODS_SUPPORTED {
            vec![(app.to_string(), 0)]
        } else {
            Vec::new()
        }
    };
    let mut state = state_with_a_known_mouse();
    let _ = state.set_editing_app(Some(EXCEL.into()));
    let _ = state.commit_scroll_delivery(ScrollDelivery::CtrlShiftWheel);
    assert_eq!(
        listed(&state),
        rule_only(EXCEL),
        "the open profile is saved, and listed once"
    );

    let _ = state.set_editing_app(None);
    assert_eq!(
        listed(&state),
        rule_only("exe:excel.exe"),
        "the profile outlives its editor"
    );

    let _ = state.set_editing_app(Some(EXCEL.into()));
    let _ = state.commit_binding(ButtonId::Back, Action::Undo);
    let _ = state.set_editing_app(None);
    assert_eq!(
        listed(&state),
        [(EXCEL.to_string(), 1)],
        "a button profile the rule applies to already lists the app"
    );
}

#[test]
fn resetting_or_removing_an_app_profile_also_clears_its_scroll_method() {
    use crate::features::mouse::thumbwheel::SCROLL_METHODS_SUPPORTED;
    use openlogi_core::scroll::ScrollDelivery;

    let key = DeviceKey::from(KNOWN_MOUSE_KEY);
    for command in [
        AppState::reset_app_profile,
        AppState::remove_app_profile,
        AppState::remove_all_app_profiles,
    ] {
        for buttons in [false, true] {
            let mut state = state_with_a_known_mouse();
            let _ = state.set_editing_app(Some(EXCEL.into()));
            let _ = state.commit_scroll_delivery(ScrollDelivery::ShiftWheel);
            if buttons {
                let _ = state.commit_binding(ButtonId::Back, Action::Undo);
            }
            let _ = state.set_editing_app(None);
            assert_eq!(state.app_has_scroll_method(EXCEL), SCROLL_METHODS_SUPPORTED);

            // The identifier the profile list offers it under.
            let app = if buttons { EXCEL } else { "exe:excel.exe" };
            let _ = command(&mut state, &key, app);
            assert_eq!(
                state.config.thumbwheel_apps.is_empty(),
                SCROLL_METHODS_SUPPORTED,
                "buttons {buttons}: Windows clears the rule; other platforms preserve it"
            );
            assert!(!state.app_has_scroll_method(EXCEL));
            assert!(state.app_profiles().next().is_none(), "buttons {buttons}");
        }
    }
}
