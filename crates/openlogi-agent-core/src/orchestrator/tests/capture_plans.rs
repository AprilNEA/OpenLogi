//! What a config reload or an app switch publishes to the hook and the capture managers.

use super::*;

#[test]
fn config_reload_publishes_scroll_preferences_without_restarting_the_hook() {
    let mut orch = orchestrator(Config::default());
    let preferences = Arc::clone(&orch.shared.scroll_preferences);
    assert!(!preferences.smooth_scroll_enabled());
    assert_eq!(
        preferences.vertical_sensitivity(),
        VerticalScrollSensitivity::DEFAULT
    );

    let mut config = Config::default();
    config.app_settings.smooth_scroll = true;
    config.app_settings.vertical_scroll_sensitivity =
        VerticalScrollSensitivity::try_new(7).expect("valid sensitivity");
    orch.reload_config(config);

    assert!(preferences.smooth_scroll_enabled());
    assert_eq!(
        preferences.vertical_sensitivity(),
        VerticalScrollSensitivity::try_new(7).expect("valid sensitivity")
    );
}

/// The published capture plan's Back binding for the first device, if any.
fn published_back_binding(orch: &Orchestrator) -> Option<Action> {
    let plans = orch.shared.capture_plans.borrow();
    plans.first().and_then(|plan| {
        plan.dispatch
            .bindings
            .get(&ButtonId::Back)
            .map(Binding::click_action)
    })
}

#[test]
fn app_switch_republishes_capture_plans() {
    // HID++ dispatch reads `plan.dispatch.bindings` at event time, so a
    // foreground-app change must republish the capture plans — their
    // binding maps and divert sets are per-app effective — or every
    // diverted button keeps firing the previous app's actions.
    let mut config = Config::default();
    config.set_per_app_binding(
        "a",
        "com.example.editor",
        ButtonId::Back,
        Some(Action::Undo),
    );
    let mut orch = orchestrator(config);
    orch.devices = vec![dev("a", 1, true)];
    orch.rebuild();
    assert_ne!(
        published_back_binding(&orch),
        Some(Action::Undo),
        "no per-app overlay while no app is in front"
    );
    orch.set_current_app(Some(ForegroundApp::unnamed("com.example.editor".into())));
    assert_eq!(published_back_binding(&orch), Some(Action::Undo));
}

#[test]
fn hook_maps_publish_selection_and_preserve_learned_thumbwheel_polarity() {
    let mut orch = orchestrator(Config::default());
    orch.devices = vec![dev("a", 1, true)];
    orch.rebuild();

    {
        let mut maps = orch.shared.hook_maps.write().expect("hook maps");
        assert_eq!(maps.selected_device.as_deref(), Some("a"));
        maps.thumbwheel_positive_is_forward
            .insert("a".to_owned(), true);
    }

    // Config/app rebuilds replace binding maps but hardware observations must
    // remain in the same atomically published snapshot.
    orch.reload_config(Config::default());
    let maps = orch.shared.hook_maps.read().expect("hook maps");
    assert_eq!(maps.selected_device.as_deref(), Some("a"));
    assert_eq!(maps.thumbwheel_positive_is_forward.get("a"), Some(&true));
}

#[test]
fn macos_side_gesture_capture_follows_mouse_hook_availability() {
    let mut config = Config::default();
    config.set_gesture_mode("a", ButtonId::Forward, true);
    let mut orch = orchestrator(config);
    orch.devices = vec![dev("a", 1, true)];
    orch.rebuild();
    let mut capture_plans = orch.shared.capture_plans.clone();
    let _ = capture_plans.borrow_and_update();

    let side_gesture_is_armed = |orch: &Orchestrator| {
        orch.shared.capture_plans.borrow()[0]
            .target
            .spec
            .divert_gesture_buttons
            .iter()
            .any(|&(_, button)| button == ButtonId::Forward)
    };
    assert!(
        !side_gesture_is_armed(&orch),
        "HID++ diversion must wait for the movement hook"
    );

    orch.set_os_mouse_hook_available(true);
    assert_eq!(
        capture_plans
            .has_changed()
            .expect("publication remains open"),
        cfg!(target_os = "macos"),
        "only a semantic capture-plan change should wake reconciliation"
    );
    let _ = capture_plans.borrow_and_update();
    if cfg!(target_os = "macos") {
        let hook_maps = orch
            .shared
            .hook_maps
            .read()
            .expect("hook maps should not be poisoned");
        assert!(!hook_maps.bindings.contains_key(&ButtonId::Forward));
        assert!(!hook_maps.gestures.contains_key(&ButtonId::Forward));
        assert!(side_gesture_is_armed(&orch));
    } else {
        let hook_maps = orch
            .shared
            .hook_maps
            .read()
            .expect("hook maps should not be poisoned");
        assert!(hook_maps.gestures.contains_key(&ButtonId::Forward));
        assert!(!side_gesture_is_armed(&orch));
    }

    orch.set_os_mouse_hook_available(false);
    assert_eq!(
        capture_plans
            .has_changed()
            .expect("publication remains open"),
        cfg!(target_os = "macos"),
        "only a semantic capture-plan change should wake reconciliation"
    );
    assert!(
        !side_gesture_is_armed(&orch),
        "revoking the movement hook must restore native HID++ controls"
    );
}
