//! Regression tests for OS-hook state and dispatch policy.

use super::*;
use openlogi_core::binding::{GESTURE_SWIPE_THRESHOLD, LongPressBinding};
use openlogi_core::config::KeyModifiers;

use super::super::button::{ButtonRuntimeEvent, CancelReason, EndReason};

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
            access: crate::hardware::DeviceAccess {
                channel: Arc::new(RwLock::new(None)),
                registry: openlogi_hid::ChannelRegistry::default(),
                receiver_access: crate::receiver_access::ReceiverAccess::default(),
                device_io: openlogi_hid::device_io_channel().1,
                transport_turns: crate::hardware::TransportTurns::default(),
            },
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
    hold.begin(ButtonId::Back, None, press.clone());
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
    assert_eq!(hold.end(ButtonId::Back, None), Some((press, false)));
}

#[test]
fn a_same_button_repress_restarts_the_stale_hold() {
    let mut hold = HoldState::default();
    let old = token(1, ButtonId::Back);
    assert!(matches!(
        hold.prepare_begin(ButtonId::Back, None),
        HoldAdmission::Begin
    ));
    hold.begin(ButtonId::Back, None, old);

    let replacement = token(2, ButtonId::Back);
    assert!(
        matches!(
            hold.prepare_begin(ButtonId::Back, None),
            HoldAdmission::Begin
        ),
        "a same-button re-press is proof of a lost release"
    );
    hold.begin(ButtonId::Back, None, replacement.clone());
    hold.swipe.backdate_hold_for_test();
    assert_eq!(
        hold.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
        Some((replacement, ButtonId::Back, GestureDirection::Right))
    );
}

#[test]
fn an_aged_hold_yields_to_a_new_buttons_press() {
    let mut hold = HoldState::default();
    hold.begin(ButtonId::Back, None, token(1, ButtonId::Back));
    hold.backdate_for_test();

    let replacement = token(2, ButtonId::Forward);
    let HoldAdmission::Replace(stale) = hold.prepare_begin(ButtonId::Forward, None) else {
        panic!("an aged hold must yield to a new press");
    };
    assert_eq!(stale, token(1, ButtonId::Back));
    hold.begin(ButtonId::Forward, None, replacement.clone());
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
    hold.begin(ButtonId::Back, None, first.clone());
    hold.swipe.backdate_hold_for_test();
    assert!(
        matches!(
            hold.prepare_begin(ButtonId::Forward, None),
            HoldAdmission::Refuse
        ),
        "a second press must not hijack the active hold"
    );

    assert_eq!(
        hold.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
        Some((first.clone(), ButtonId::Back, GestureDirection::Right))
    );
    assert_eq!(hold.end(ButtonId::Forward, None), None);
    assert_eq!(hold.end(ButtonId::Back, None), Some((first, false)));
}

#[test]
fn end_matches_the_held_button_and_returns_its_token() {
    let mut hold = HoldState::default();
    let press = token(1, ButtonId::Back);
    hold.begin(ButtonId::Back, None, press.clone());
    assert_eq!(hold.end(ButtonId::Forward, None), None);
    assert_eq!(hold.end(ButtonId::Back, None), Some((press, true)));
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
    let mut fail_open = HashMap::new();
    assert_eq!(
        remapped_press_disposition(
            (ButtonId::Back, None),
            Some(token(1, ButtonId::Back)),
            &mut fail_open
        ),
        EventDisposition::Suppress
    );
    assert_eq!(
        remapped_release_disposition(&(ButtonId::Back, None), &mut fail_open),
        EventDisposition::Suppress
    );
    assert_eq!(
        remapped_press_disposition((ButtonId::Forward, None), None, &mut fail_open),
        EventDisposition::PassThrough
    );
    assert_eq!(
        remapped_release_disposition(&(ButtonId::Forward, None), &mut fail_open),
        EventDisposition::PassThrough
    );
    assert_eq!(
        remapped_release_disposition(&(ButtonId::Forward, None), &mut fail_open),
        EventDisposition::PassThrough
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
                modifiers: KeyModifiers::default(),
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
fn mouse_press_uses_the_target_published_with_its_binding_not_frontmost_safari() {
    use super::super::button::ButtonRuntimeEvent;
    use openlogi_hook::PointerTarget;

    let (dispatcher, mut owner, events) = test_dispatcher();
    let hooks = Arc::new(RwLock::new(HookMaps {
        bindings: BTreeMap::from([(ButtonId::Back, Action::PreviousDesktop.into())]),
        pointer_target: Some(PointerTarget::Desktop),
        ..HookMaps::default()
    }));
    let mouse = EventDevice {
        vendor_id: Some(0x046d),
        product_name: Some("Logitech MX Master 3".into()),
        ..EventDevice::default()
    };
    assert_eq!(
        handle_button(
            ButtonId::Back,
            true,
            Some(&mouse),
            &[],
            &hooks,
            &dispatcher,
            || { panic!("pointer-scoped binding must not capture foreground Safari") }
        ),
        EventDisposition::Suppress
    );
    let ButtonRuntimeEvent::Started(press) =
        events.recv_timeout(Duration::from_secs(1)).expect("down")
    else {
        panic!("started");
    };
    assert_eq!(
        press.target(),
        ActionDispatchTarget::Pointer {
            target: PointerTarget::Desktop,
            fallback_safari_pid: None,
        }
    );
    assert_eq!(press.start_action(), Some(&Action::PreviousDesktop));
    // Pointer movement can restore the native binding before release. The
    // suppressed down must still have a suppressed up and end its lifecycle.
    hooks
        .write()
        .expect("maps")
        .bindings
        .insert(ButtonId::Back, Action::MouseBack.into());
    assert_eq!(
        handle_button(
            ButtonId::Back,
            false,
            Some(&mouse),
            &[],
            &hooks,
            &dispatcher,
            || { panic!("release does not retarget") }
        ),
        EventDisposition::Suppress
    );
    let ButtonRuntimeEvent::Ended { press: ended, .. } =
        events.recv_timeout(Duration::from_secs(1)).expect("up")
    else {
        panic!("ended");
    };
    assert_eq!(ended.token(), press.token());

    // The inverse crossing must not swallow the release of a native press.
    assert_eq!(
        handle_button(
            ButtonId::Back,
            true,
            Some(&mouse),
            &[],
            &hooks,
            &dispatcher,
            || None
        ),
        EventDisposition::PassThrough
    );
    hooks
        .write()
        .expect("maps")
        .bindings
        .insert(ButtonId::Back, Action::PreviousDesktop.into());
    assert_eq!(
        handle_button(
            ButtonId::Back,
            false,
            Some(&mouse),
            &[],
            &hooks,
            &dispatcher,
            || { panic!("release does not retarget") }
        ),
        EventDisposition::PassThrough
    );
    assert!(owner.shutdown());
}

#[test]
fn queued_unidentified_pointer_action_retains_its_press_time_target() {
    use super::super::button::ButtonRuntimeEvent;
    use openlogi_hook::PointerTarget;
    use std::cell::Cell;

    let mouse = EventDevice {
        vendor_id: Some(0x046d),
        product_name: Some("Logitech MX Master 3".into()),
        ..EventDevice::default()
    };
    let mut routes = Vec::new();
    for (pressed_safari, later_safari, current_pointer) in [
        (Some(417), None, PointerTarget::Unavailable),
        (None, Some(518), PointerTarget::Unavailable),
        (Some(417), Some(518), PointerTarget::Unavailable),
        (Some(417), None, PointerTarget::Desktop),
    ] {
        let (dispatcher, mut owner, events) = test_dispatcher();
        let hooks = Arc::new(RwLock::new(HookMaps {
            bindings: BTreeMap::from([(ButtonId::Back, Action::BrowserBack.into())]),
            pointer_target: Some(PointerTarget::Unavailable),
            ..HookMaps::default()
        }));
        let focused_safari = Cell::new(pressed_safari);
        assert_eq!(
            handle_button(
                ButtonId::Back,
                true,
                Some(&mouse),
                &[],
                &hooks,
                &dispatcher,
                || focused_safari.get(),
            ),
            EventDisposition::Suppress
        );

        focused_safari.set(later_safari);
        let ButtonRuntimeEvent::Started(press) =
            events.recv_timeout(Duration::from_secs(1)).expect("down")
        else {
            panic!("started");
        };
        let action = press.start_action().expect("browser binding");
        let resolved = press.target().resolve_with(
            action,
            || current_pointer,
            |_| panic!("an unidentified target has no window to check"),
            || focused_safari.get(),
        );
        let mut navigation = (None, false);
        if let Some(target) = resolved {
            assert!(super::super::dispatch_browser_navigation(
                action,
                target,
                |pid, forward| {
                    assert!(!forward);
                    navigation.0 = Some(pid);
                    true
                },
                || navigation.1 = true,
            ));
        }
        routes.push(navigation);
        assert_eq!(
            handle_button(
                ButtonId::Back,
                false,
                Some(&mouse),
                &[],
                &hooks,
                &dispatcher,
                || panic!("release does not retarget"),
            ),
            EventDisposition::Suppress
        );
        assert!(owner.shutdown());
    }
    assert_eq!(
        routes,
        [
            (Some(417), false),
            (None, true),
            (Some(417), false),
            (None, false)
        ]
    );
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
        for id in [ButtonId::Back, ButtonId::Forward] {
            for pressed in [true, false] {
                assert_eq!(
                    handle_button(
                        id,
                        pressed,
                        source.as_ref(),
                        &[],
                        &hooks,
                        &dispatcher,
                        || Some(417)
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

/// Reproduces the sender-less-attribution hazard: a hold begins under a known
/// source, a later release of the same button arrives with its attribution
/// invalidated (the resolver's unreadable- or lost-holder case), and the hold must be
/// cancelled there and then — not left for a stray pointer move to turn into
/// a phantom swipe (the downstream state `HoldState::cancel_for` guards, not
/// just the resolver's own cache checked by `senderless_button`'s tests).
#[test]
fn an_invalidated_attribution_cancels_the_hold_it_began_under() {
    let (dispatcher, mut owner, events) = test_dispatcher();
    let hooks = Arc::new(RwLock::new(HookMaps {
        gestures: BTreeMap::from([(ButtonId::Back, BTreeMap::new())]),
        ..HookMaps::default()
    }));
    let logitech = EventDevice {
        product_name: Some("Logitech MX Master 3".into()),
        ..EventDevice::default()
    };
    // `invalidated_sources` is only ever set on macOS, usually
    // alongside `device: None` (see `MouseEvent::Button`'s doc), but
    // `handle_button`'s policy split is platform-neutral: any source
    // `button_source_may_remap` rejects takes the same branch. A
    // non-remappable *known* device (a trackpad, rejected on every platform)
    // drives that branch without relying on macOS's `None`-fails-closed
    // rule, so this test is not vacuous on the Linux/Windows CI hosts that
    // run it.
    let trackpad = EventDevice {
        product_name: Some("Apple Internal Keyboard / Trackpad".into()),
        ..EventDevice::default()
    };
    let target = || Some(417);

    // A hold begins under a known, remappable source.
    assert_eq!(
        handle_button(
            ButtonId::Back,
            true,
            Some(&logitech),
            &[],
            &hooks,
            &dispatcher,
            target
        ),
        EventDisposition::Suppress
    );
    assert!(
        HOLD.with_borrow(|h| h.current.is_some()),
        "hold must be active"
    );
    assert!(
        matches!(
            events.recv_timeout(Duration::from_secs(1)),
            Ok(ButtonRuntimeEvent::Started(_))
        ),
        "the dispatcher must have accepted the lifecycle"
    );

    // The hold's attribution is lost. The event itself still can't be
    // remapped, but it must cancel the hold it silently invalidated rather
    // than leave it dangling.
    assert_eq!(
        handle_button(
            ButtonId::Back,
            false,
            Some(&trackpad),
            std::slice::from_ref(&logitech),
            &hooks,
            &dispatcher,
            target
        ),
        EventDisposition::PassThrough
    );
    assert!(
        HOLD.with_borrow(|h| h.current.is_none()),
        "the stale hold must not survive the invalidation"
    );
    assert!(
        matches!(
            events.recv_timeout(Duration::from_secs(1)),
            Ok(ButtonRuntimeEvent::Ended {
                reason: EndReason::Canceled(CancelReason::StaleHold),
                ..
            })
        ),
        "the dispatcher-owned lifecycle must be released, not just the thread-local pointer"
    );

    // The exact reported symptom: stray pointer motion after both physical
    // releases must not resurrect a swipe from the cancelled hold.
    assert_eq!(
        HOLD.with_borrow_mut(|h| h.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0)),
        None
    );

    assert!(owner.shutdown());
}

/// The held-binding half of the same hazard: a long-press accepted under a
/// known source must not keep timing after a lost holder invalidates
/// its attribution, because its own release will arrive unattributed and pass
/// through without ever reaching the dispatcher.
#[test]
fn an_invalidated_attribution_cancels_the_held_binding_it_began_under() {
    let (dispatcher, mut owner, events) = test_dispatcher();
    let hooks = Arc::new(RwLock::new(HookMaps {
        bindings: BTreeMap::from([(
            ButtonId::Back,
            Binding::LongPress(LongPressBinding::new(Action::Copy, Action::MissionControl)),
        )]),
        ..HookMaps::default()
    }));
    let logitech = EventDevice {
        product_name: Some("Logitech MX Master 3".into()),
        ..EventDevice::default()
    };
    // A non-remappable known source drives the rejected-source branch on
    // every platform, as in the gesture-hold test above.
    let trackpad = EventDevice {
        product_name: Some("Apple Internal Keyboard / Trackpad".into()),
        ..EventDevice::default()
    };

    assert_eq!(
        handle_button(
            ButtonId::Back,
            true,
            Some(&logitech),
            &[],
            &hooks,
            &dispatcher,
            || None
        ),
        EventDisposition::Suppress
    );
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(1)),
        Ok(ButtonRuntimeEvent::Started(_))
    ));

    assert_eq!(
        handle_button(
            ButtonId::Back,
            false,
            Some(&trackpad),
            std::slice::from_ref(&logitech),
            &hooks,
            &dispatcher,
            || None
        ),
        EventDisposition::PassThrough
    );
    assert!(
        matches!(
            events.recv_timeout(Duration::from_secs(1)),
            Ok(ButtonRuntimeEvent::Ended {
                reason: EndReason::Canceled(CancelReason::StaleHold),
                ..
            })
        ),
        "the held binding's lifecycle must be released"
    );
    assert!(
        SUPPRESSED_PRESSES.with_borrow(HashMap::is_empty),
        "a later attributed release must not be suppressed for the cancelled press"
    );

    assert!(owner.shutdown());
}

/// A press attributed in the same event that drops a lost holder's
/// attribution cancels the stale hold even when the button is no longer a
/// gesture, so nothing else would replace it.
#[test]
fn an_attributed_press_that_invalidates_a_lost_hold_cancels_it() {
    let (dispatcher, mut owner, events) = test_dispatcher();
    let hooks = Arc::new(RwLock::new(HookMaps {
        gestures: BTreeMap::from([(ButtonId::Back, BTreeMap::new())]),
        ..HookMaps::default()
    }));
    let logitech = EventDevice {
        product_name: Some("Logitech MX Master 3".into()),
        ..EventDevice::default()
    };
    let press = |invalidated: &[EventDevice]| {
        handle_button(
            ButtonId::Back,
            true,
            Some(&logitech),
            invalidated,
            &hooks,
            &dispatcher,
            || None,
        )
    };

    assert_eq!(press(&[]), EventDisposition::Suppress);
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(1)),
        Ok(ButtonRuntimeEvent::Started(_))
    ));

    *hooks.write().unwrap() = HookMaps::default();
    assert_eq!(
        press(std::slice::from_ref(&logitech)),
        EventDisposition::PassThrough
    );
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(1)),
        Ok(ButtonRuntimeEvent::Ended {
            reason: EndReason::Canceled(CancelReason::StaleHold),
            ..
        })
    ));
    assert!(
        HOLD.with_borrow(|h| h.current.is_none()),
        "the stale hold must not survive the invalidation"
    );

    assert!(owner.shutdown());
}

fn two_logitech_mice() -> (EventDevice, EventDevice) {
    let first = EventDevice {
        product_id: Some(1),
        product_name: Some("Logitech MX Master 3".into()),
        ..EventDevice::default()
    };
    let second = EventDevice {
        product_id: Some(2),
        ..first.clone()
    };
    (first, second)
}

/// One mouse's lost attribution cancels only the hold that mouse began.
#[test]
fn an_invalidation_cancels_only_its_own_sources_hold() {
    let (dispatcher, mut owner, events) = test_dispatcher();
    let hooks = Arc::new(RwLock::new(HookMaps {
        gestures: BTreeMap::from([(ButtonId::Back, BTreeMap::new())]),
        ..HookMaps::default()
    }));
    let (holder, other) = two_logitech_mice();
    let event = |pressed, device: Option<&EventDevice>, invalidated: &[EventDevice]| {
        handle_button(
            ButtonId::Back,
            pressed,
            device,
            invalidated,
            &hooks,
            &dispatcher,
            || None,
        )
    };

    assert_eq!(event(true, Some(&holder), &[]), EventDisposition::Suppress);
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(1)),
        Ok(ButtonRuntimeEvent::Started(_))
    ));
    assert_eq!(
        event(false, Some(&other), std::slice::from_ref(&other)),
        EventDisposition::PassThrough,
        "another mouse's release must not end or suppress the hold"
    );
    assert!(HOLD.with_borrow(|h| h.current.is_some()));
    assert!(
        events.recv_timeout(Duration::from_millis(200)).is_err(),
        "the hold's lifecycle must not be cancelled"
    );
    assert_eq!(event(false, Some(&holder), &[]), EventDisposition::Suppress);
    assert!(HOLD.with_borrow(|h| h.current.is_none()));

    assert!(owner.shutdown());
}

/// A release is suppressed only when that same mouse's press was: another
/// mouse's raw press would otherwise stay down at the OS.
#[test]
fn a_release_is_suppressed_only_for_the_source_whose_press_was() {
    let (dispatcher, mut owner, _events) = test_dispatcher();
    let (first, second) = two_logitech_mice();
    for hooks in [
        HookMaps {
            bindings: BTreeMap::from([(ButtonId::Back, Action::Copy.into())]),
            ..HookMaps::default()
        },
        HookMaps {
            gestures: BTreeMap::from([(ButtonId::Back, BTreeMap::new())]),
            ..HookMaps::default()
        },
    ] {
        let hooks = Arc::new(RwLock::new(hooks));
        let event = |pressed, device: &EventDevice| {
            handle_button(
                ButtonId::Back,
                pressed,
                Some(device),
                &[],
                &hooks,
                &dispatcher,
                || None,
            )
        };

        assert_eq!(event(true, &first), EventDisposition::Suppress);
        // The gesture hold refuses the second press and no binding remaps it.
        if hooks.read().unwrap().bindings.is_empty() {
            assert_eq!(event(true, &second), EventDisposition::PassThrough);
            assert_eq!(event(false, &second), EventDisposition::PassThrough);
        } else {
            assert_eq!(
                event(false, &second),
                EventDisposition::PassThrough,
                "a release whose press was never seen passes through"
            );
        }
        assert_eq!(event(false, &first), EventDisposition::Suppress);
        assert_eq!(event(false, &first), EventDisposition::PassThrough);
    }

    assert!(owner.shutdown());
}

/// The worker's events until `started` presses have all ended. Shutdown
/// rides its own channel, so it would otherwise overtake queued edges.
fn events_until_ended(
    events: &mpsc::Receiver<ButtonRuntimeEvent>,
    started: usize,
) -> Vec<ButtonRuntimeEvent> {
    let mut received = Vec::new();
    let mut open = 0_usize;
    let mut seen = 0;
    while seen < started || open > 0 {
        let event = events
            .recv_timeout(Duration::from_secs(1))
            .expect("every accepted press must end");
        match &event {
            ButtonRuntimeEvent::Started(_) => {
                seen += 1;
                open += 1;
            }
            ButtonRuntimeEvent::Ended { .. } => open -= 1,
            ButtonRuntimeEvent::Triggered { .. } => {}
        }
        received.push(event);
    }
    received
}

/// Back is a gesture button with a fallback binding, Forward a plain binding.
fn gesture_and_bindings() -> SharedHookMaps {
    Arc::new(RwLock::new(HookMaps {
        gestures: BTreeMap::from([(
            ButtonId::Back,
            BTreeMap::from([(GestureDirection::Right, Action::Paste)]),
        )]),
        bindings: BTreeMap::from([
            (ButtonId::Back, Action::Copy.into()),
            (ButtonId::Forward, Action::Cut.into()),
        ]),
        ..HookMaps::default()
    }))
}

/// A second mouse's press of the held gesture button runs its own binding
/// without cancelling the first mouse's hold, whose swipe still fires.
#[test]
fn another_mouses_press_leaves_the_gesture_hold_alone() {
    let (dispatcher, mut owner, events) = test_dispatcher();
    let hooks = gesture_and_bindings();
    let (first, second) = two_logitech_mice();
    let event = |pressed, device: &EventDevice| {
        handle_button(
            ButtonId::Back,
            pressed,
            Some(device),
            &[],
            &hooks,
            &dispatcher,
            || None,
        )
    };

    assert_eq!(event(true, &first), EventDisposition::Suppress);
    let Ok(ButtonRuntimeEvent::Started(held)) = events.recv_timeout(Duration::from_secs(1)) else {
        panic!("the gesture hold must start");
    };
    assert_eq!(event(true, &second), EventDisposition::Suppress);
    HOLD.with_borrow_mut(|h| h.swipe.backdate_hold_for_test());
    handle_moved(GESTURE_SWIPE_THRESHOLD + 10, 0, &hooks, &dispatcher);
    assert_eq!(event(false, &second), EventDisposition::Suppress);
    assert_eq!(event(false, &first), EventDisposition::Suppress);

    let mut events = events_until_ended(&events, 1);
    events.insert(0, ButtonRuntimeEvent::Started(held.clone()));
    assert!(owner.shutdown());
    assert!(
        events.iter().any(|event| matches!(
            event,
            ButtonRuntimeEvent::Triggered { press, action: Action::Paste }
                if press.token() == held.token()
        )),
        "the first mouse's swipe must still fire"
    );
    assert!(
        events.iter().all(|event| !matches!(
            event,
            ButtonRuntimeEvent::Ended {
                reason: EndReason::Canceled(_),
                ..
            }
        )),
        "no press may be cancelled by another mouse's"
    );
}

/// Every order of three actors each pressing then releasing, plus one swipe
/// (`None`).
fn interleave(
    order: &mut Vec<Option<(usize, bool)>>,
    progress: &mut [u8; 3],
    swiped: bool,
    out: &mut Vec<Vec<Option<(usize, bool)>>>,
) {
    if swiped && progress.iter().all(|&done| done == 2) {
        out.push(order.clone());
        return;
    }
    if !swiped {
        order.push(None);
        interleave(order, progress, true, out);
        order.pop();
    }
    for actor in 0..3 {
        if progress[actor] < 2 {
            order.push(Some((actor, progress[actor] == 0)));
            progress[actor] += 1;
            interleave(order, progress, swiped, out);
            progress[actor] -= 1;
            order.pop();
        }
    }
}

/// Every ordering of two identified mice pressing Back, the second also
/// Forward, and one swipe, through the real lifecycle worker: no press
/// cancels another, every accepted press ends on its own release, and each
/// release is suppressed exactly when its press was.
#[test]
fn every_two_mouse_interleaving_keeps_each_press_its_own() {
    let (first, second) = two_logitech_mice();
    let actors = [
        (ButtonId::Back, first),
        (ButtonId::Back, second.clone()),
        (ButtonId::Forward, second),
    ];
    let mut orders = Vec::new();
    let mut order = Vec::new();
    interleave(&mut order, &mut [0; 3], false, &mut orders);
    assert_eq!(orders.len(), 630);

    let hooks = gesture_and_bindings();
    for order in orders {
        HOLD.with_borrow_mut(HoldState::cancel);
        SUPPRESSED_PRESSES.with_borrow_mut(HashMap::clear);
        let (dispatcher, mut owner, events) = test_dispatcher();
        let mut pressed_as = [None; 3];
        for step in &order {
            let Some((actor, pressed)) = *step else {
                HOLD.with_borrow_mut(|h| h.swipe.backdate_hold_for_test());
                handle_moved(GESTURE_SWIPE_THRESHOLD + 10, 0, &hooks, &dispatcher);
                continue;
            };
            let (id, device) = &actors[actor];
            let disposition =
                handle_button(*id, pressed, Some(device), &[], &hooks, &dispatcher, || {
                    None
                });
            if pressed {
                pressed_as[actor] = Some(disposition);
            } else {
                assert_eq!(
                    Some(disposition),
                    pressed_as[actor],
                    "a release must share its press's disposition: {order:?}"
                );
            }
        }
        let started = pressed_as
            .iter()
            .filter(|&&disposition| disposition == Some(EventDisposition::Suppress))
            .count();
        let received = events_until_ended(&events, started);
        assert!(owner.shutdown());
        let mut open = Vec::new();
        for event in received {
            match event {
                ButtonRuntimeEvent::Started(press) => open.push(press.token().clone()),
                ButtonRuntimeEvent::Ended { press, reason } => {
                    assert_eq!(
                        reason,
                        EndReason::Released,
                        "a press ended other than by its release: {order:?}"
                    );
                    open.retain(|token| token != press.token());
                }
                ButtonRuntimeEvent::Triggered { .. } => {}
            }
        }
        assert!(open.is_empty(), "a press never ended: {order:?}");
    }
    HOLD.with_borrow_mut(HoldState::cancel);
}
