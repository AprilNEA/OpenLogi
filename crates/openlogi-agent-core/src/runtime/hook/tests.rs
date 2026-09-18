//! Regression tests for OS-hook state and dispatch policy.

use super::*;
use openlogi_core::binding::{GESTURE_SWIPE_THRESHOLD, LongPressBinding};

fn token(id: u64, button: ButtonId) -> PressToken {
    PressToken::hook_for_test(id, button)
}

#[test]
fn senderless_buttons_follow_the_platform_source_policy() {
    assert_eq!(button_source_may_remap(None), !cfg!(target_os = "macos"));
}

#[test]
fn attributed_sources_still_follow_the_device_policy() {
    let trackpad = EventDevice {
        product_name: Some("Apple Internal Keyboard / Trackpad".into()),
        ..EventDevice::default()
    };
    let logitech_mouse = EventDevice {
        product_name: Some("Logitech MX Master 3".into()),
        ..EventDevice::default()
    };

    assert!(!button_source_may_remap(Some(&trackpad)));
    assert!(button_source_may_remap(Some(&logitech_mouse)));
}

fn test_dispatcher() -> (
    ActionDispatcher,
    super::super::button::ButtonRuntimeOwner,
    mpsc::Receiver<super::super::button::ButtonRuntimeEvent>,
) {
    let (events, received) = mpsc::channel();
    let owner = super::super::button::ButtonRuntimeOwner::spawn(move |event| {
        events
            .send(event)
            .expect("test receiver should stay connected");
    })
    .expect("button worker should start");
    let (action_ring, _ring_events) = tokio::sync::mpsc::unbounded_channel();
    let dispatcher = ActionDispatcher {
        executor: super::super::ActionExecutor {
            dpi_cycle: Arc::new(RwLock::new(crate::DpiCycles::default())),
            capture: Arc::new(RwLock::new(None)),
            registry: openlogi_hid::ChannelRegistry::default(),
            receiver_access: crate::receiver_access::ReceiverAccess::default(),
            device_io: openlogi_hid::device_io_channel().1,
            action_ring,
        },
        buttons: owner.input(),
    };
    (dispatcher, owner, received)
}

// The mid-swipe gate itself is unit-tested on `SwipeAccumulator` in
// `openlogi-core`; these cover only what `HoldState` adds on top — tagging a
// commit with the exact press and held button, and matching the release.

#[test]
fn accumulate_tags_a_committed_swipe_with_the_held_press() {
    let mut hold = HoldState::default();
    let press = token(1, ButtonId::Back);
    hold.begin(ButtonId::Back, press.clone());
    hold.swipe.backdate_hold_for_test();

    assert_eq!(
        hold.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
        Some((press.clone(), ButtonId::Back, GestureDirection::Right))
    );
    assert_eq!(
        hold.accumulate(50, 0),
        None,
        "commits at most once per hold"
    );
    assert_eq!(hold.end(ButtonId::Back), Some((press, false)));
}

#[test]
fn a_same_button_repress_restarts_the_stale_hold() {
    let mut hold = HoldState::default();
    let old = token(1, ButtonId::Back);
    assert!(matches!(
        hold.prepare_begin(ButtonId::Back),
        HoldAdmission::Begin
    ));
    hold.begin(ButtonId::Back, old);

    let replacement = token(2, ButtonId::Back);
    assert!(
        matches!(hold.prepare_begin(ButtonId::Back), HoldAdmission::Begin),
        "a same-button re-press is proof of a lost release"
    );
    hold.begin(ButtonId::Back, replacement.clone());
    hold.swipe.backdate_hold_for_test();
    assert_eq!(
        hold.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
        Some((replacement, ButtonId::Back, GestureDirection::Right))
    );
}

#[test]
fn an_aged_hold_yields_to_a_new_buttons_press() {
    let mut hold = HoldState::default();
    hold.begin(ButtonId::Back, token(1, ButtonId::Back));
    hold.backdate_for_test();

    let replacement = token(2, ButtonId::Forward);
    let HoldAdmission::Replace(stale) = hold.prepare_begin(ButtonId::Forward) else {
        panic!("an aged hold must yield to a new press");
    };
    assert_eq!(stale, token(1, ButtonId::Back));
    hold.begin(ButtonId::Forward, replacement.clone());
    hold.swipe.backdate_hold_for_test();
    assert_eq!(
        hold.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
        Some((replacement, ButtonId::Forward, GestureDirection::Right))
    );
}

#[test]
fn begin_is_first_wins_while_a_hold_is_active() {
    let mut hold = HoldState::default();
    let first = token(1, ButtonId::Back);
    hold.begin(ButtonId::Back, first.clone());
    hold.swipe.backdate_hold_for_test();
    assert!(
        matches!(hold.prepare_begin(ButtonId::Forward), HoldAdmission::Refuse),
        "a second press must not hijack the active hold"
    );

    assert_eq!(
        hold.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
        Some((first.clone(), ButtonId::Back, GestureDirection::Right))
    );
    assert_eq!(hold.end(ButtonId::Forward), None);
    assert_eq!(hold.end(ButtonId::Back), Some((first, false)));
}

#[test]
fn end_matches_the_held_button_and_returns_its_token() {
    let mut hold = HoldState::default();
    let press = token(1, ButtonId::Back);
    hold.begin(ButtonId::Back, press.clone());
    assert_eq!(hold.end(ButtonId::Forward), None);
    assert_eq!(hold.end(ButtonId::Back), Some((press, true)));
}

#[test]
fn resolve_gesture_click_prefers_explicit_then_falls_back_to_default() {
    let gestures = BTreeMap::from([(
        ButtonId::Back,
        BTreeMap::from([(GestureDirection::Click, Action::Copy)]),
    )]);
    assert_eq!(
        resolve_gesture_click(&gestures, ButtonId::Back),
        Action::Copy
    );

    let off = BTreeMap::from([(
        ButtonId::Back,
        BTreeMap::from([(GestureDirection::Click, Action::None)]),
    )]);
    assert_eq!(resolve_gesture_click(&off, ButtonId::Back), Action::None);
}

#[test]
fn fail_open_press_pairs_release() {
    let mut fail_open = HashSet::new();
    assert_eq!(
        remapped_press_disposition(ButtonId::Back, true, &mut fail_open),
        EventDisposition::Suppress
    );
    assert_eq!(
        remapped_release_disposition(ButtonId::Back, &mut fail_open),
        EventDisposition::Suppress
    );
    assert_eq!(
        remapped_press_disposition(ButtonId::Forward, false, &mut fail_open),
        EventDisposition::PassThrough
    );
    assert_eq!(
        remapped_release_disposition(ButtonId::Forward, &mut fail_open),
        EventDisposition::PassThrough
    );
    assert_eq!(
        remapped_release_disposition(ButtonId::Forward, &mut fail_open),
        EventDisposition::Suppress
    );
}

#[test]
fn rejected_key_edges_fail_open() {
    assert_eq!(queued_event_disposition(true), EventDisposition::Suppress);
    assert_eq!(
        queued_event_disposition(false),
        EventDisposition::PassThrough
    );
}

#[test]
fn queued_key_action_retains_its_press_time_target() {
    let (dispatcher, mut owner, _events) = test_dispatcher();
    let keycode = 0x7a;
    let modifiers = KeyModifiers::default();
    let bindings = Arc::new(RwLock::new(BTreeMap::from([(
        KeyTrigger { keycode, modifiers },
        Action::BrowserBack,
    )])));
    let (actions, queued) = mpsc::sync_channel(1);
    let target = ActionDispatchTarget::SafariProcess(417);

    assert_eq!(
        handle_key(
            KeyEvent {
                keycode,
                pressed: true,
                modifiers: openlogi_hook::KeyModifiers::default(),
            },
            &bindings,
            &actions,
            &dispatcher,
            || target,
        ),
        EventDisposition::Suppress
    );
    assert_eq!(
        queued.recv().expect("action should be queued"),
        (Action::BrowserBack, target)
    );
    assert!(owner.shutdown());
}

#[test]
fn safari_target_never_relaxes_device_isolation() {
    let (dispatcher, mut owner, events) = test_dispatcher();
    let hooks = Arc::new(RwLock::new(HookMaps {
        bindings: BTreeMap::from([
            (ButtonId::Back, Action::BrowserBack.into()),
            (ButtonId::Forward, Action::BrowserForward.into()),
        ]),
        ..HookMaps::default()
    }));
    let sources = [
        Some(EventDevice {
            vendor_id: Some(0x045e),
            product_name: Some("Microsoft Mouse".into()),
            ..EventDevice::default()
        }),
        Some(EventDevice {
            product_name: Some("Magic Trackpad".into()),
            ..EventDevice::default()
        }),
        None,
    ];
    for source in &sources {
        // Linux/Windows filter attachment upstream and permit unknown senders.
        if source.is_none() && !cfg!(target_os = "macos") {
            continue;
        }
        let (actions, _queued) = mpsc::sync_channel(4);
        for id in [ButtonId::Back, ButtonId::Forward] {
            for pressed in [true, false] {
                assert_eq!(
                    handle_button(
                        id,
                        pressed,
                        source.as_ref(),
                        &hooks,
                        &dispatcher,
                        &actions,
                        || { ActionDispatchTarget::SafariProcess(417) }
                    ),
                    EventDisposition::PassThrough
                );
            }
        }
    }
    assert!(owner.shutdown());
    assert!(matches!(
        events.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
}

#[test]
fn scroll_interception_uses_the_button_source_safety_policy_and_skips_trackpads() {
    let logitech = EventDevice {
        vendor_id: Some(openlogi_hook::LOGITECH_VENDOR_ID),
        product_name: Some("Logitech MX Master".to_string()),
        ..EventDevice::default()
    };
    let trackpad = EventDevice {
        product_name: Some("Magic Trackpad".to_string()),
        ..EventDevice::default()
    };

    assert!(scroll_source_may_intercept(false, Some(&logitech)));
    assert!(!scroll_source_may_intercept(true, Some(&logitech)));
    assert!(!scroll_source_may_intercept(false, Some(&trackpad)));
    assert_eq!(
        scroll_source_may_intercept(false, None),
        !cfg!(target_os = "macos"),
        "only macOS requires callback-time device attribution"
    );
}

#[test]
fn rebound_horizontal_wheel_maps_to_thumbwheel_directions() {
    let maps = HookMaps {
        bindings: BTreeMap::from([
            (ButtonId::ThumbwheelScrollUp, Action::NextTab.into()),
            (ButtonId::ThumbwheelScrollDown, Action::PrevTab.into()),
        ]),
        ..HookMaps::default()
    };
    assert_eq!(
        rebound_thumbwheel_action(&maps, 1.0),
        Some((ButtonId::ThumbwheelScrollDown, Action::PrevTab))
    );
    assert_eq!(
        rebound_thumbwheel_action(&maps, -1.0),
        Some((ButtonId::ThumbwheelScrollUp, Action::NextTab))
    );
    assert_eq!(rebound_thumbwheel_action(&maps, 0.0), None);
}

#[test]
fn rebound_horizontal_wheel_uses_the_selected_devices_polarity() {
    let maps = HookMaps {
        bindings: BTreeMap::from([
            (ButtonId::ThumbwheelScrollUp, Action::NextTab.into()),
            (ButtonId::ThumbwheelScrollDown, Action::PrevTab.into()),
        ]),
        selected_device: Some("mx3".to_owned()),
        thumbwheel_positive_is_forward: BTreeMap::from([("mx3".to_owned(), true)]),
        ..HookMaps::default()
    };
    assert_eq!(
        rebound_thumbwheel_action(&maps, 1.0),
        Some((ButtonId::ThumbwheelScrollUp, Action::NextTab))
    );
    assert_eq!(
        rebound_thumbwheel_action(&maps, -1.0),
        Some((ButtonId::ThumbwheelScrollDown, Action::PrevTab))
    );
}

#[test]
fn rebound_horizontal_wheel_does_not_guess_a_selected_devices_polarity() {
    let maps = HookMaps {
        bindings: BTreeMap::from([
            (ButtonId::ThumbwheelScrollUp, Action::NextTab.into()),
            (ButtonId::ThumbwheelScrollDown, Action::PrevTab.into()),
        ]),
        selected_device: Some("not-learned-yet".to_owned()),
        ..HookMaps::default()
    };
    assert_eq!(rebound_thumbwheel_action(&maps, 1.0), None);
    assert_eq!(rebound_thumbwheel_action(&maps, -1.0), None);
}

#[test]
fn native_thumbwheel_scroll_stays_os_native() {
    let maps = HookMaps {
        bindings: BTreeMap::from([
            (
                ButtonId::ThumbwheelScrollUp,
                default_binding(ButtonId::ThumbwheelScrollUp).into(),
            ),
            (
                ButtonId::ThumbwheelScrollDown,
                default_binding(ButtonId::ThumbwheelScrollDown).into(),
            ),
        ]),
        ..HookMaps::default()
    };
    assert_eq!(rebound_thumbwheel_action(&maps, 1.0), None);
    assert_eq!(rebound_thumbwheel_action(&maps, -1.0), None);
}

#[test]
fn long_press_never_passes_through_as_a_native_click() {
    let binding = Binding::LongPress(LongPressBinding::new(
        default_binding(ButtonId::Back),
        Action::MissionControl,
    ));
    assert!(!binding_is_native_click(ButtonId::Back, &binding));
}

/// Clear the thread-local side-button hold so redirect tests stay hermetic
/// no matter which pooled test thread they land on.
fn clear_hscroll_hold() {
    HSCROLL.with_borrow_mut(|hold| *hold = None);
}

fn logitech_device() -> EventDevice {
    EventDevice {
        vendor_id: Some(openlogi_hook::LOGITECH_VENDOR_ID),
        product_name: Some("Logitech Signature M650L".into()),
        ..EventDevice::default()
    }
}

#[test]
fn side_button_replay_lookup_serves_only_side_buttons() {
    let maps = HookMaps {
        side_button_hscroll: BTreeMap::from([
            (ButtonId::Back, Action::MouseBack),
            (ButtonId::Forward, Action::Copy),
            // A malformed snapshot must never hijack the middle button.
            (ButtonId::MiddleClick, Action::Copy),
        ]),
        ..HookMaps::default()
    };
    assert_eq!(
        hscroll_replay_action(&maps, ButtonId::Back),
        Some(Action::MouseBack)
    );
    assert_eq!(
        hscroll_replay_action(&maps, ButtonId::Forward),
        Some(Action::Copy)
    );
    assert_eq!(hscroll_replay_action(&maps, ButtonId::MiddleClick), None);

    let disarmed = HookMaps::default();
    assert_eq!(hscroll_replay_action(&disarmed, ButtonId::Back), None);
    assert_eq!(hscroll_replay_action(&disarmed, ButtonId::Forward), None);
}

#[test]
fn redirect_conversion_needs_an_open_hold_and_pure_vertical_ticks() {
    use openlogi_core::scroll::ScrollDelta;

    clear_hscroll_hold();
    assert_eq!(
        hscroll_redirect_delta(ScrollDelta::wheel_ticks(0.0, 1.0)),
        None
    );

    HSCROLL.with_borrow_mut(|hold| {
        *hold = Some(HScrollHold {
            button: ButtonId::Back,
            click: Action::MouseBack,
            scrolled: false,
            target: ActionDispatchTarget::Keyboard,
        });
    });
    // Wheel-up maps to scroll-right; wheel-down to scroll-left. The polarity
    // is MUST-VERIFY on hardware (issue #1053).
    assert_eq!(
        hscroll_redirect_delta(ScrollDelta::wheel_ticks(0.0, 1.0)),
        Some(ScrollDelta::wheel_ticks(1.0, 0.0))
    );
    assert_eq!(
        hscroll_redirect_delta(ScrollDelta::wheel_ticks(0.0, -2.0)),
        Some(ScrollDelta::wheel_ticks(-2.0, 0.0))
    );
    // Native horizontal, mixed two-axis, and pixel-precise input keep their
    // normal paths — horizontal is never swallowed.
    assert_eq!(
        hscroll_redirect_delta(ScrollDelta::wheel_ticks(1.0, 0.0)),
        None
    );
    assert_eq!(
        hscroll_redirect_delta(ScrollDelta::wheel_ticks(1.0, 1.0)),
        None
    );
    assert_eq!(hscroll_redirect_delta(ScrollDelta::pixels(0.0, 10.0)), None);
    assert_eq!(
        hscroll_redirect_delta(ScrollDelta::wheel_ticks(0.0, 0.0)),
        None
    );
    clear_hscroll_hold();
}

#[test]
fn side_button_hold_without_wheel_replays_the_native_click() {
    use openlogi_core::scroll::ScrollDelta;

    clear_hscroll_hold();
    let (dispatcher, mut owner, _events) = test_dispatcher();
    let hooks = Arc::new(RwLock::new(HookMaps {
        side_button_hscroll: BTreeMap::from([(ButtonId::Back, Action::MouseBack)]),
        ..HookMaps::default()
    }));
    let (actions, queued) = mpsc::sync_channel(4);
    let device = logitech_device();

    assert_eq!(
        handle_button(
            ButtonId::Back,
            true,
            Some(&device),
            &hooks,
            &dispatcher,
            &actions,
            || ActionDispatchTarget::Keyboard
        ),
        EventDisposition::Suppress
    );
    // No wheel motion: the release replays the native click off-thread.
    assert_eq!(
        hscroll_redirect_delta(ScrollDelta::wheel_ticks(0.0, 0.0)),
        None,
        "zero motion must not arm the swallow"
    );
    assert_eq!(
        handle_button(
            ButtonId::Back,
            false,
            Some(&device),
            &hooks,
            &dispatcher,
            &actions,
            || ActionDispatchTarget::Keyboard
        ),
        EventDisposition::Suppress
    );
    assert_eq!(
        queued.recv().expect("replayed click should be queued"),
        (Action::MouseBack, ActionDispatchTarget::Keyboard)
    );
    assert!(owner.shutdown());
    clear_hscroll_hold();
}

#[test]
fn side_button_hold_without_wheel_replays_the_rebound_action() {
    // A rebound side button redirects while keeping its remap: a clean
    // release fires the bound action (here PreviousDesktop, the reporter's
    // binding) instead of the native click.
    clear_hscroll_hold();
    let (dispatcher, mut owner, _events) = test_dispatcher();
    let hooks = Arc::new(RwLock::new(HookMaps {
        side_button_hscroll: BTreeMap::from([(ButtonId::Back, Action::PreviousDesktop)]),
        ..HookMaps::default()
    }));
    let (actions, queued) = mpsc::sync_channel(4);
    let device = logitech_device();

    assert_eq!(
        handle_button(
            ButtonId::Back,
            true,
            Some(&device),
            &hooks,
            &dispatcher,
            &actions,
            || ActionDispatchTarget::Keyboard
        ),
        EventDisposition::Suppress
    );
    assert_eq!(
        handle_button(
            ButtonId::Back,
            false,
            Some(&device),
            &hooks,
            &dispatcher,
            &actions,
            || ActionDispatchTarget::Keyboard
        ),
        EventDisposition::Suppress
    );
    assert_eq!(
        queued.recv().expect("replayed action should be queued"),
        (Action::PreviousDesktop, ActionDispatchTarget::Keyboard)
    );
    assert!(owner.shutdown());
    clear_hscroll_hold();
}

#[test]
fn side_button_hold_replays_the_press_time_action_across_a_reload() {
    // The replay action snapshots at press: a config change mid-hold must
    // not retarget the release.
    clear_hscroll_hold();
    let (dispatcher, mut owner, _events) = test_dispatcher();
    let hooks = Arc::new(RwLock::new(HookMaps {
        side_button_hscroll: BTreeMap::from([(ButtonId::Back, Action::MouseBack)]),
        ..HookMaps::default()
    }));
    let (actions, queued) = mpsc::sync_channel(4);
    let device = logitech_device();

    assert_eq!(
        handle_button(
            ButtonId::Back,
            true,
            Some(&device),
            &hooks,
            &dispatcher,
            &actions,
            || ActionDispatchTarget::Keyboard
        ),
        EventDisposition::Suppress
    );
    // A reload rebinds Back mid-hold; the release still replays MouseBack.
    hooks
        .write()
        .expect("hook maps")
        .side_button_hscroll
        .insert(ButtonId::Back, Action::Copy);
    assert_eq!(
        handle_button(
            ButtonId::Back,
            false,
            Some(&device),
            &hooks,
            &dispatcher,
            &actions,
            || ActionDispatchTarget::Keyboard
        ),
        EventDisposition::Suppress
    );
    assert_eq!(
        queued.recv().expect("replayed click should be queued"),
        (Action::MouseBack, ActionDispatchTarget::Keyboard)
    );
    assert!(owner.shutdown());
    clear_hscroll_hold();
}

#[test]
fn hidpp_redirect_admits_unattributed_and_own_device_wheels() {
    use openlogi_core::scroll::ScrollDelta;

    let park = HidppHscrollPark {
        device_key: Arc::from("serial:2302lz00fn58"),
        button: ButtonId::Back,
        source_ids: Some((0x046d, 0xb02a)),
    };
    let one = [park.clone()];
    let two = [
        park.clone(),
        HidppHscrollPark {
            device_key: Arc::from("receiver:cafe:slot:2"),
            button: ButtonId::Forward,
            source_ids: None,
        },
    ];
    let own_device = EventDevice {
        vendor_id: Some(0x046d),
        product_id: Some(0xb02a),
        ..EventDevice::default()
    };
    let other_mouse = EventDevice {
        vendor_id: Some(0x046d),
        product_id: Some(0xc52b),
        ..EventDevice::default()
    };
    let microsoft = EventDevice {
        vendor_id: Some(0x045e),
        product_name: Some("Microsoft Mouse".into()),
        ..EventDevice::default()
    };
    // The reporter's exact shape: BLE-direct devices expose a product name
    // but no vendor id ("Signature M650 L"), so `is_logitech` fails and the
    // vendor-less wheel cannot be attributed to anyone.
    let named_without_vendor = EventDevice {
        vendor_id: None,
        product_name: Some("Signature M650 L".into()),
        ..EventDevice::default()
    };
    let empty_identity = EventDevice::default();

    // Unattributed sources: senderless, empty identity, and — the fix — a
    // product name without vendor id. All redirect while parked.
    for device in [None, Some(&empty_identity), Some(&named_without_vendor)] {
        assert_eq!(
            hidpp_redirect_delta(ScrollDelta::wheel_ticks(0.0, 2.0), device, &one),
            Some(ScrollDelta::wheel_ticks(2.0, 0.0)),
            "device {device:?} should redirect while parked"
        );
    }
    // The parked device's own attributed wheel (wired links) matches its
    // carried route ids.
    assert_eq!(
        hidpp_redirect_delta(ScrollDelta::wheel_ticks(0.0, 2.0), Some(&own_device), &one),
        Some(ScrollDelta::wheel_ticks(2.0, 0.0))
    );
    // Never hijack: no park, ambiguous parks, someone else's wheel.
    assert_eq!(
        hidpp_redirect_delta(ScrollDelta::wheel_ticks(0.0, 2.0), None, &[]),
        None
    );
    assert_eq!(
        hidpp_redirect_delta(ScrollDelta::wheel_ticks(0.0, 2.0), None, &two),
        None
    );
    for device in [&other_mouse, &microsoft] {
        assert_eq!(
            hidpp_redirect_delta(ScrollDelta::wheel_ticks(0.0, 2.0), Some(device), &one),
            None,
            "device {device:?} must never redirect"
        );
    }
    // A vendor-bearing wheel with a park that carries no ids (receiver
    // route) cannot be matched: refuse.
    assert_eq!(
        hidpp_redirect_delta(ScrollDelta::wheel_ticks(0.0, 2.0), Some(&own_device), &two),
        None
    );
    // Axis and unit guards are unchanged.
    assert_eq!(
        hidpp_redirect_delta(ScrollDelta::wheel_ticks(2.0, 0.0), None, &one),
        None
    );
    assert_eq!(
        hidpp_redirect_delta(ScrollDelta::wheel_ticks(1.0, 1.0), None, &one),
        None
    );
    assert_eq!(
        hidpp_redirect_delta(ScrollDelta::pixels(0.0, 10.0), None, &one),
        None
    );
    assert_eq!(
        hidpp_redirect_delta(ScrollDelta::wheel_ticks(0.0, 0.0), None, &one),
        None
    );
}

#[test]
fn native_horizontal_motion_while_held_swallows_the_click() {
    use openlogi_core::scroll::ScrollDelta;

    // Firmware that converts hold+wheel itself (e.g. M650L) emits real
    // horizontal ticks: they pass through untouched, but the hold must
    // still swallow the release.
    clear_hscroll_hold();
    HSCROLL.with_borrow_mut(|hold| {
        *hold = Some(HScrollHold {
            button: ButtonId::Back,
            click: Action::PreviousDesktop,
            scrolled: false,
            target: ActionDispatchTarget::Keyboard,
        });
    });
    mark_hscroll_native_horizontal(ScrollDelta::wheel_ticks(2.0, 0.0));
    assert!(
        HSCROLL.with_borrow(|hold| hold.as_ref().is_some_and(|held| held.scrolled)),
        "native horizontal must arm the swallow"
    );
    // Pure vertical and pixel-precise motion never mark the hold.
    clear_hscroll_hold();
    HSCROLL.with_borrow_mut(|hold| {
        *hold = Some(HScrollHold {
            button: ButtonId::Back,
            click: Action::PreviousDesktop,
            scrolled: false,
            target: ActionDispatchTarget::Keyboard,
        });
    });
    mark_hscroll_native_horizontal(ScrollDelta::wheel_ticks(0.0, 1.0));
    mark_hscroll_native_horizontal(ScrollDelta::pixels(10.0, 0.0));
    assert!(
        HSCROLL.with_borrow(|hold| hold.as_ref().is_some_and(|held| !held.scrolled)),
        "vertical and pixel motion must not arm the swallow"
    );
    clear_hscroll_hold();
}

#[test]
fn side_button_hold_with_wheel_swallows_the_click() {
    clear_hscroll_hold();
    let (dispatcher, mut owner, _events) = test_dispatcher();
    let hooks = Arc::new(RwLock::new(HookMaps {
        side_button_hscroll: BTreeMap::from([(ButtonId::Forward, Action::MouseForward)]),
        ..HookMaps::default()
    }));
    let (actions, queued) = mpsc::sync_channel(4);
    let device = logitech_device();

    assert_eq!(
        handle_button(
            ButtonId::Forward,
            true,
            Some(&device),
            &hooks,
            &dispatcher,
            &actions,
            || ActionDispatchTarget::Keyboard
        ),
        EventDisposition::Suppress
    );
    // Simulate the scroll arm accepting one redirected tick.
    HSCROLL.with_borrow_mut(|hold| {
        hold.as_mut().expect("hold should be open").scrolled = true;
    });
    assert_eq!(
        handle_button(
            ButtonId::Forward,
            false,
            Some(&device),
            &hooks,
            &dispatcher,
            &actions,
            || ActionDispatchTarget::Keyboard
        ),
        EventDisposition::Suppress
    );
    assert!(
        matches!(queued.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "a scrolled hold must queue no click"
    );
    assert!(owner.shutdown());
    clear_hscroll_hold();
}

#[test]
fn resolve_gesture_click_falls_back_when_click_is_absent() {
    let no_click = BTreeMap::from([(
        ButtonId::Back,
        BTreeMap::from([(GestureDirection::Up, Action::Copy)]),
    )]);
    assert_eq!(
        resolve_gesture_click(&no_click, ButtonId::Back),
        default_binding(ButtonId::Back)
    );

    let empty = BTreeMap::new();
    assert_eq!(
        resolve_gesture_click(&empty, ButtonId::Forward),
        default_binding(ButtonId::Forward)
    );
}
