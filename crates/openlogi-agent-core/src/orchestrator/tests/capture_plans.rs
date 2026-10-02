//! What a config reload or an app switch publishes to the hook and the capture managers.

use openlogi_core::device_order::PhysicalDeviceKey;
use openlogi_hid::reprog_controls::BACK_CIDS;

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
    let published = orch.shared.capture_plans.borrow();
    published.plans().first().and_then(|plan| {
        plan.dispatch
            .bindings
            .get(&ButtonId::Back)
            .map(Binding::click_action)
    })
}

/// The published plan carries the held-wheel inversion of the device's
/// link, which `plan_for_device` alone cannot resolve.
#[test]
fn published_plans_carry_the_held_wheel_inversion_of_the_link() {
    let mut config = Config::default();
    config.set_invert_scroll("a", false);
    let mut device = dev("a", 1, true);
    device.capabilities = Some(Capabilities {
        scroll_inversion: true,
        ..Capabilities::default()
    });
    let mut link = LinkConfig::default();
    link.overrides.invert_scroll = Some(true);
    config
        .devices
        .entry("a".to_owned())
        .or_default()
        .links
        .insert(stable_id(&device).route_key(), link);
    let mut orch = orchestrator(config);
    orch.devices = vec![device];
    orch.rebuild();
    let published = orch.shared.capture_plans.borrow();
    let plan = published
        .plans()
        .first()
        .expect("the online device is planned");
    assert!(plan.dispatch.invert_wheel);
}

/// A smooth-wheel device's published plan asks its session to divert the
/// wheel once the hook may post the scrolling, and leaves it native again
/// when the hook goes away.
#[test]
fn published_plans_divert_the_main_wheel_only_while_the_hook_runs() {
    let mut config = Config::default();
    config.set_smooth_wheel("a", true);
    let mut device = dev("a", 1, true);
    device.capabilities = Some(Capabilities {
        hires_wheel: true,
        ..Capabilities::default()
    });
    let mut orch = orchestrator(config);
    orch.devices = vec![device];
    let main_wheel = |orch: &Orchestrator| {
        orch.shared
            .capture_plans
            .borrow()
            .plans()
            .first()
            .expect("the online device is planned")
            .target
            .spec
            .divert_main_wheel
    };
    orch.set_os_mouse_hook_available(true);
    orch.rebuild();
    assert_eq!(
        main_wheel(&orch),
        Some(openlogi_hid::session::gesture::MainWheelDivert {
            resolution: None,
            inverted: None,
        })
    );
    orch.set_os_mouse_hook_available(false);
    orch.rebuild();
    assert_eq!(main_wheel(&orch), None);
}

#[test]
fn app_switch_republishes_capture_plans() {
    // HID++ dispatch reads `plan.dispatch.bindings` at event time, so a
    // foreground-app change must republish the capture plans — their
    // binding maps and divert sets are per-app effective — or every
    // diverted button keeps firing the previous app's actions.
    let mut config = Config::default();
    config.app_settings.mouse_profile_target = openlogi_core::config::MouseProfileTarget::Focused;
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
fn app_switch_cancels_held_buttons_only_when_a_per_app_override_is_involved() {
    let mut config = Config::default();
    config.set_per_app_binding("a", "editor", ButtonId::Back, Some(Action::Undo));
    let mut orch = orchestrator(config);
    orch.devices = vec![dev("a", 1, true)];
    orch.rebuild();
    // No profile on either side: a desktop-switch gesture changing the front
    // app must not end its own hold (#1634).
    assert!(!orch.set_current_app(Some(ForegroundApp::unnamed("finder".into()))));
    assert!(!orch.set_current_app(Some(ForegroundApp::unnamed("safari".into()))));
    // Entering or leaving an app with a profile changes the bindings.
    assert!(orch.set_current_app(Some(ForegroundApp::unnamed("editor".into()))));
    assert!(orch.set_current_app(Some(ForegroundApp::unnamed("safari".into()))));
}

#[test]
fn pointer_profiles_switch_to_desktop_without_changing_keyboard_focus() {
    use openlogi_hook::{PointerContext, PointerTarget};

    let mut config = Config::default();
    config.set_binding(
        "a",
        ButtonId::Back,
        Binding::Single(Action::PreviousDesktop),
    );
    config.set_per_app_binding("a", "browser", ButtonId::Back, Some(Action::BrowserBack));
    config.set_per_app_binding("keyboard", "browser", ButtonId::Back, Some(Action::Copy));
    let mut orch = orchestrator(config.clone());
    let mut keyboard = dev("keyboard", 2, true);
    keyboard.kind = DeviceKind::Keyboard;
    orch.devices = vec![dev("a", 1, true), keyboard];
    orch.set_current_app(Some(ForegroundApp::unnamed("browser".into())));
    let window = PointerTarget::Window {
        process_id: 41,
        window_id: 7,
    };
    assert!(orch.set_pointer_context(PointerContext {
        app: Some(ForegroundApp::unnamed("browser".into())),
        target: window,
    }));
    assert_eq!(published_back_binding(&orch), Some(Action::BrowserBack));
    assert!(orch.set_pointer_context(PointerContext {
        app: None,
        target: PointerTarget::Desktop
    }));
    assert_eq!(published_back_binding(&orch), Some(Action::PreviousDesktop));
    {
        let maps = orch.shared.hook_maps.read().expect("hook maps");
        assert_eq!(
            maps.bindings[&ButtonId::Back].click_action(),
            Action::PreviousDesktop
        );
        assert_eq!(maps.pointer_target, Some(PointerTarget::Desktop));
    }
    let published = orch.shared.capture_plans.borrow();
    let keyboard = published
        .plans()
        .iter()
        .find(|plan| plan.dispatch.config_key == "keyboard")
        .expect("keyboard plan");
    assert_eq!(
        keyboard.dispatch.bindings[&ButtonId::Back].click_action(),
        Action::Copy
    );
    assert_eq!(keyboard.dispatch.pointer_target, None);
    drop(published);
    assert_eq!(orch.current_app.as_deref(), Some("browser"));

    // The explicit option takes effect immediately without moving the pointer.
    config.app_settings.mouse_profile_target = openlogi_core::config::MouseProfileTarget::Focused;
    orch.reload_config(config);
    assert_eq!(published_back_binding(&orch), Some(Action::BrowserBack));
    assert_eq!(
        orch.shared
            .hook_maps
            .read()
            .expect("hook maps")
            .pointer_target,
        None
    );
}

#[test]
fn unidentified_pointer_context_uses_the_focused_profile_never_the_desktop() {
    use openlogi_hook::{PointerContext, PointerTarget};
    let mut config = Config::default();
    config.set_binding(
        "a",
        ButtonId::Back,
        Binding::Single(Action::PreviousDesktop),
    );
    config.set_per_app_binding("a", "browser", ButtonId::Back, Some(Action::BrowserBack));
    let mut orch = orchestrator(config);
    orch.devices = vec![dev("a", 1, true)];
    orch.set_current_app(Some(ForegroundApp::unnamed("browser".into())));
    let published_pointer_target = |orch: &Orchestrator| {
        let hook = orch.shared.hook_maps.read().expect("maps").pointer_target;
        let plan = orch
            .shared
            .capture_plans
            .borrow()
            .plans()
            .first()
            .expect("mouse plan")
            .dispatch
            .pointer_target;
        assert_eq!(hook, plan, "OS hook and HID++ share one mouse context");
        hook
    };
    assert!(orch.set_pointer_context(PointerContext {
        app: None,
        target: PointerTarget::Desktop,
    }));
    assert_eq!(published_back_binding(&orch), Some(Action::PreviousDesktop));

    // An overlay or a failed lookup leaves the target unidentified, possibly
    // for the whole session. The mouse then uses the focused application's
    // profile, as `mouse_profile_target = "focused"` would, never the
    // desktop's, and every press stays revalidated against the pointer.
    assert!(
        orch.set_pointer_context(PointerContext {
            app: None,
            target: PointerTarget::Unavailable,
        }),
        "presses resolved against the desktop's profile must end"
    );
    assert_eq!(published_back_binding(&orch), Some(Action::BrowserBack));
    assert_eq!(
        published_pointer_target(&orch),
        Some(PointerTarget::Unavailable)
    );

    // Reaching an identified target ends presses resolved against the
    // focused profile, exactly as moving between two windows does.
    assert!(orch.set_pointer_context(PointerContext {
        app: None,
        target: PointerTarget::Desktop,
    }));
    assert_eq!(published_back_binding(&orch), Some(Action::PreviousDesktop));
    assert_eq!(
        published_pointer_target(&orch),
        Some(PointerTarget::Desktop)
    );

    // Unsupported compositors follow focus outright: nothing to revalidate.
    orch.set_pointer_context(PointerContext {
        app: None,
        target: PointerTarget::Unsupported,
    });
    assert_eq!(published_back_binding(&orch), Some(Action::BrowserBack));
    assert_eq!(published_pointer_target(&orch), None);
}

#[test]
fn unidentified_pointer_context_without_a_focused_app_uses_the_global_bindings() {
    use openlogi_hook::{PointerContext, PointerTarget};
    // Before the foreground watcher publishes an app there is no focused
    // profile to fall back on. The global bindings then apply, exactly as
    // `mouse_profile_target = "focused"` applies them in the same state, and
    // the press stays pointer-scoped so it ends once a target is identified.
    let bindings = |config: &mut Config| {
        config.set_binding(
            "a",
            ButtonId::Back,
            Binding::Single(Action::PreviousDesktop),
        );
        config.set_per_app_binding("a", "browser", ButtonId::Back, Some(Action::BrowserBack));
    };
    let mut config = Config::default();
    bindings(&mut config);
    let mut orch = orchestrator(config);
    orch.devices = vec![dev("a", 1, true)];
    orch.set_current_app(None);
    orch.rebuild();
    orch.set_pointer_context(PointerContext {
        app: None,
        target: PointerTarget::Unavailable,
    });
    assert_eq!(published_back_binding(&orch), Some(Action::PreviousDesktop));
    assert_eq!(
        orch.shared.hook_maps.read().expect("maps").pointer_target,
        Some(PointerTarget::Unavailable)
    );

    let mut config = Config::default();
    config.app_settings.mouse_profile_target = openlogi_core::config::MouseProfileTarget::Focused;
    bindings(&mut config);
    let mut orch = orchestrator(config);
    orch.devices = vec![dev("a", 1, true)];
    orch.set_current_app(None);
    orch.rebuild();
    assert_eq!(published_back_binding(&orch), Some(Action::PreviousDesktop));
    assert_eq!(
        orch.shared.hook_maps.read().expect("maps").pointer_target,
        None
    );
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

fn assert_side_gesture_capture_follows_platform_ownership_and_hook_availability(button: ButtonId) {
    let mut config = Config::default();
    config.set_gesture_mode("a", button, true);
    let mut orch = orchestrator(config);
    orch.devices = vec![dev("a", 1, true)];
    orch.rebuild();
    let mut capture_plans = orch.shared.capture_plans.clone();
    let _ = capture_plans.borrow_and_update();

    let side_gesture_is_requested = |orch: &Orchestrator| {
        orch.shared.capture_plans.borrow().plans()[0]
            .target
            .spec
            .divert_gesture_buttons
            .iter()
            .any(|&(_, b)| b == button)
    };
    assert!(
        !side_gesture_is_requested(&orch),
        "HID++ diversion must wait for the movement hook"
    );

    orch.set_os_mouse_hook_available(true);
    assert_eq!(
        capture_plans
            .has_changed()
            .expect("publication remains open"),
        cfg!(any(target_os = "macos", target_os = "windows")),
        "only a semantic capture-plan change should wake reconciliation"
    );
    let _ = capture_plans.borrow_and_update();
    let hook_maps = orch
        .shared
        .hook_maps
        .read()
        .expect("hook maps should not be poisoned");
    if cfg!(target_os = "macos") && button != ButtonId::MiddleClick {
        assert!(!hook_maps.bindings.contains_key(&button));
        assert!(!hook_maps.gestures.contains_key(&button));
        assert!(side_gesture_is_requested(&orch));
    } else if cfg!(any(target_os = "macos", target_os = "windows")) {
        assert!(hook_maps.gestures.contains_key(&button));
        assert!(
            side_gesture_is_requested(&orch),
            "{button} must request HID++ raw XY while retaining the passive hook fallback"
        );
    } else {
        assert!(hook_maps.gestures.contains_key(&button));
        assert!(!side_gesture_is_requested(&orch));
    }
    drop(hook_maps);

    orch.set_os_mouse_hook_available(false);
    assert_eq!(
        capture_plans
            .has_changed()
            .expect("publication remains open"),
        cfg!(any(target_os = "macos", target_os = "windows")),
        "only a semantic capture-plan change should wake reconciliation"
    );
    assert!(
        !side_gesture_is_requested(&orch),
        "revoking the movement hook must restore native HID++ controls"
    );
}

#[test]
fn side_gesture_capture_follows_platform_ownership_and_hook_availability() {
    assert_side_gesture_capture_follows_platform_ownership_and_hook_availability(ButtonId::Forward);
}

#[test]
fn middle_click_gesture_capture_follows_platform_ownership_and_hook_availability() {
    assert_side_gesture_capture_follows_platform_ownership_and_hook_availability(
        ButtonId::MiddleClick,
    );
}

#[test]
fn keyboard_control_bindings_never_enter_the_mouse_capture_plan() {
    // The keyboard watcher owns `0x1b04` key diversion. The gesture watcher
    // also plans a session for every online device, so a keyboard key must
    // not appear in that plan's divert set or the two sessions would each
    // divert the same control on the same channel.
    let mut config = Config::default();
    config.set_binding(
        "keyboard",
        ButtonId::control(0x010a),
        Binding::Single(Action::Screenshot),
    );
    let mut orch = orchestrator(config);
    let mut keyboard = dev("keyboard", 2, true);
    keyboard.kind = DeviceKind::Keyboard;
    orch.devices = vec![keyboard];
    orch.rebuild();

    let published = orch.shared.capture_plans.borrow();
    let plan = published.plans().first().expect("keyboard plan");
    let spec = &plan.target.spec;
    let diverted_controls: Vec<u16> = spec
        .divert_buttons
        .iter()
        .chain(&spec.divert_gesture_buttons)
        .filter(|(_, button)| button.cid().is_some())
        .map(|(cid, _)| *cid)
        .collect();
    assert!(diverted_controls.is_empty(), "{diverted_controls:#06x?}");
    assert!(!spec.divert_gesture_sources.contains(&0x010a));
    assert_eq!(
        plan.dispatch.bindings[&ButtonId::control(0x010a)].click_action(),
        Action::Screenshot,
        "the key still resolves in the plan's binding map for dispatch"
    );
}

/// One `0x1b04` control, one owner. A K380's multiplatform Back key is both
/// a Back-family member the mouse plan diverts for a non-default `Back`
/// binding and, bound by number, a key of the keyboard session. The explicit
/// key binding wins: the keyboard's plan releases exactly that control and
/// keeps diverting the rest of the family.
#[test]
fn a_control_the_keyboard_session_owns_leaves_the_keyboard_capture_plan() {
    let [
        back,
        multiplatform_back,
        multiplatform_back_alt,
        back_generic,
    ] = BACK_CIDS;
    let mut config = Config::default();
    config.set_binding("kbd", ButtonId::Back, Binding::Single(Action::Copy));
    config.set_binding(
        "kbd",
        ButtonId::control(multiplatform_back),
        Binding::Single(Action::Paste),
    );
    let mut orch = orchestrator(config);
    let mut keyboard = dev("kbd", 2, true);
    keyboard.kind = DeviceKind::Keyboard;
    orch.devices = vec![keyboard];
    orch.rebuild();

    let spec = orch
        .keyboard_spec_for()
        .expect("the bound key publishes a spec");
    assert_eq!(
        spec.wanted.keys().copied().collect::<Vec<_>>(),
        [multiplatform_back]
    );
    let published = orch.shared.capture_plans.borrow();
    let diverted: Vec<u16> = published.plans()[0]
        .target
        .spec
        .divert_buttons
        .iter()
        .map(|(cid, _)| *cid)
        .collect();
    assert!(
        !diverted.contains(&multiplatform_back),
        "{diverted:#06x?} must leave the keyboard session's control alone"
    );
    for cid in [back, multiplatform_back_alt, back_generic] {
        assert!(
            diverted.contains(&cid),
            "{cid:#06x} stays with the Back binding"
        );
    }
}

/// Without a competing key binding, a hand-edited `Back` on a keyboard keeps
/// its whole family in the mouse plan — the release is per owned control,
/// not a blanket ban on keyboards.
#[test]
fn a_back_binding_alone_keeps_its_whole_family_on_a_keyboard() {
    let mut config = Config::default();
    config.set_binding("kbd", ButtonId::Back, Binding::Single(Action::Copy));
    let mut orch = orchestrator(config);
    let mut keyboard = dev("kbd", 2, true);
    keyboard.kind = DeviceKind::Keyboard;
    orch.devices = vec![keyboard];
    orch.rebuild();

    assert!(orch.keyboard_spec_for().is_none());
    let published = orch.shared.capture_plans.borrow();
    let diverted: Vec<u16> = published.plans()[0]
        .target
        .spec
        .divert_buttons
        .iter()
        .map(|(cid, _)| *cid)
        .collect();
    for cid in BACK_CIDS {
        assert!(diverted.contains(&cid), "{cid:#06x}");
    }
}

#[test]
fn a_disabled_mouse_keeps_its_latest_route_for_restore() {
    let mut config = Config::default();
    config.set_device_enabled("mouse", false);
    let mut orch = orchestrator(config);
    let mut mouse = dev("mouse", 2, true);
    mouse.unit_id = [0x01, 0x02, 0x03, 0x04];
    mouse.route = Some(DeviceRoute::Bolt {
        receiver_uid: "receiver-b".to_owned(),
        slot: 2,
    });
    orch.devices = vec![dev("other", 1, true), mouse];
    orch.rebuild();

    let key = PhysicalDeviceKey::parse("unit:01020304").expect("unit id should be a physical key");
    {
        let published = orch.shared.capture_plans.borrow();
        assert!(
            published
                .plans()
                .iter()
                .all(|plan| plan.dispatch.config_key != "mouse"),
            "disabling capture must not arm a session"
        );
        assert!(
            published
                .plans()
                .iter()
                .any(|plan| plan.dispatch.config_key == "other"),
            "an enabled device still publishes a capture plan"
        );
        assert_eq!(
            published.restore_route(&key).cloned(),
            Some(mouse_route("receiver-b", 2))
        );
    }

    let mut publication = orch.shared.capture_plans.clone();
    let _ = publication.borrow_and_update();
    orch.devices[1].route = Some(DeviceRoute::Bolt {
        receiver_uid: "receiver-c".to_owned(),
        slot: 1,
    });
    orch.devices[1].slot = 1;
    orch.rebuild();

    assert!(
        publication.has_changed().expect("publication remains open"),
        "a route change must wake restoration even when capture stays disabled"
    );
    let published = orch.shared.capture_plans.borrow();
    assert!(
        published
            .plans()
            .iter()
            .all(|plan| plan.dispatch.config_key != "mouse")
    );
    assert_eq!(
        published.restore_route(&key).cloned(),
        Some(mouse_route("receiver-c", 1))
    );
}

/// Bolt route named by the disabled-mouse restore test.
fn mouse_route(receiver_uid: &str, slot: u8) -> DeviceRoute {
    DeviceRoute::Bolt {
        receiver_uid: receiver_uid.to_owned(),
        slot,
    }
}

#[cfg(target_os = "windows")]
#[test]
fn two_mouse_hidpp_plans_are_device_scoped_while_hook_fallback_is_selected_device_global() {
    let mut config = Config::default();
    config.set_gesture_mode("selected", ButtonId::Forward, true);
    let mut orch = orchestrator(config);
    orch.devices = vec![dev("selected", 1, true), dev("other", 2, true)];
    orch.rebuild();
    orch.set_os_mouse_hook_available(true);

    let published = orch.shared.capture_plans.borrow();
    let selected = published
        .plans()
        .iter()
        .find(|plan| plan.dispatch.config_key == "selected")
        .expect("selected device plan");
    let other = published
        .plans()
        .iter()
        .find(|plan| plan.dispatch.config_key == "other")
        .expect("other device plan");
    assert!(
        selected
            .dispatch
            .side_gesture_bindings
            .contains_key(&ButtonId::Forward),
        "HID++ input keeps the selected device's dispatch plan"
    );
    assert!(other.dispatch.side_gesture_bindings.is_empty());
    drop(published);

    let hook_maps = orch
        .shared
        .hook_maps
        .read()
        .expect("hook maps should not be poisoned");
    assert_eq!(hook_maps.selected_device.as_deref(), Some("selected"));
    assert!(
        hook_maps.gestures.contains_key(&ButtonId::Forward),
        "the passive Windows fallback is one selected-device map, not a per-source map"
    );
}
