//! Button, thumb-wheel, gesture and Actions Ring edits, global and per-app.

use super::*;

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
        ButtonId::MiddleClick,
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
fn a_stored_middle_click_gesture_remains_editable_until_it_is_disabled() {
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
        !state
            .config
            .is_gesture_mode(KNOWN_MOUSE_KEY, ButtonId::MiddleClick),
        "once disabled, unsupported Middle Click gestures must not be re-enabled"
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
