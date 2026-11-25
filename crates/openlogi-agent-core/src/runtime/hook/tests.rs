//! Regression tests for OS-hook state and dispatch policy.

use super::*;
use crate::runtime::FocusBinding;
use openlogi_core::binding::{GESTURE_SWIPE_THRESHOLD, LongPressBinding};
use openlogi_core::config::KeyModifiers;

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
            pointer_speeds: Arc::default(),
            access: crate::hardware::DeviceAccess {
                channel: Arc::new(RwLock::new(None)),
                registry: openlogi_hid::ChannelRegistry::default(),
                receiver_access: crate::receiver_access::ReceiverAccess::default(),
                device_io: openlogi_hid::device_io_channel().1,
                transport_turns: crate::hardware::TransportTurns::default(),
            },
            requests: action_ring,
        },
        buttons: owner.input(),
    };
    (dispatcher, owner, received)
}

/// A hold of `press` begun now.
fn held(press: PressToken) -> PressPhase {
    PressPhase::HeldAsGesture {
        hold: GestureHold::new(press, Instant::now()),
    }
}

/// A hold of `press` already past the swipe gate, so its next long-enough
/// motion commits.
fn held_for_swipe(press: PressToken) -> PressPhase {
    let mut hold = GestureHold::new(press, Instant::now());
    hold.swipe.backdate_hold_for_test();
    PressPhase::HeldAsGesture { hold }
}

/// The token of a released hold and whether it was a click; `None` for any
/// other release.
fn ended_hold(released: Option<PressPhase>) -> Option<(PressToken, bool)> {
    match released? {
        PressPhase::HeldAsGesture { hold } => Some(hold.end()),
        _ => None,
    }
}

/// Every recorded phase against every event: a press enters its own phase
/// whatever was recorded, a release ends the press, and an interrupt ends only
/// a hold.
#[test]
fn press_transition_truth_table() {
    use EventDisposition::{PassThrough, Suppress};
    let phases = || {
        [
            (held(token(1, ButtonId::Back)), Suppress),
            (PressPhase::Suppressed, Suppress),
            (PressPhase::Diverted(DivertedPress::Echo), Suppress),
            (
                PressPhase::Diverted(DivertedPress::OtherDevice),
                PassThrough,
            ),
            (PressPhase::Passthrough, PassThrough),
        ]
    };
    let recorded = || {
        [None]
            .into_iter()
            .chain(phases().map(|(phase, _)| Some(phase)))
    };
    for phase in recorded() {
        for (pressed, disposition) in phases() {
            assert_eq!(pressed.disposition(), disposition, "{pressed:?}");
            let kind = std::mem::discriminant(&pressed);
            let NextPhase::Enter(next) =
                press_transition(phase.as_ref(), PressEvent::Pressed(pressed))
            else {
                panic!("a press over {phase:?} enters its own phase");
            };
            assert_eq!(std::mem::discriminant(&next), kind, "over {phase:?}");
            assert_eq!(next.disposition(), disposition, "over {phase:?}");
        }
        assert!(
            matches!(
                press_transition(phase.as_ref(), PressEvent::Released),
                NextPhase::Up
            ),
            "{phase:?}"
        );
        let interrupted = press_transition(phase.as_ref(), PressEvent::CaptureInterrupted);
        match phase {
            Some(PressPhase::HeldAsGesture { .. }) => {
                assert!(matches!(interrupted, NextPhase::Up), "{interrupted:?}");
            }
            _ => assert!(matches!(interrupted, NextPhase::Keep), "{phase:?}"),
        }
    }
}

// The mid-swipe gate itself is unit-tested on `SwipeAccumulator` in
// `openlogi-core`; these cover only what a hold adds on top — tagging a
// commit with the exact press and held button, and matching the release.

#[test]
fn accumulate_tags_a_committed_swipe_with_the_held_press() {
    let mut presses = ButtonPresses::default();
    let press = token(1, ButtonId::Back);
    presses.press(ButtonId::Back, held_for_swipe(press.clone()));

    assert_eq!(
        presses.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
        Some((press.clone(), ButtonId::Back, GestureDirection::Right))
    );
    assert_eq!(
        presses.accumulate(50, 0),
        None,
        "continuing the same swipe does not repeat"
    );
    assert_eq!(
        ended_hold(presses.release(ButtonId::Back)),
        Some((press, false))
    );
}

#[test]
fn a_same_button_repress_restarts_the_stale_hold() {
    let mut presses = ButtonPresses::default();
    let now = Instant::now();
    assert!(matches!(
        presses.admit_gesture(ButtonId::Back, now),
        HoldAdmission::Begin
    ));
    presses.press(ButtonId::Back, held(token(1, ButtonId::Back)));

    let replacement = token(2, ButtonId::Back);
    assert!(
        matches!(
            presses.admit_gesture(ButtonId::Back, now),
            HoldAdmission::Begin
        ),
        "a same-button re-press is proof of a lost release"
    );
    presses.press(ButtonId::Back, held_for_swipe(replacement.clone()));
    assert_eq!(
        presses.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
        Some((replacement, ButtonId::Back, GestureDirection::Right))
    );
}

#[test]
fn an_aged_hold_yields_to_a_new_buttons_press() {
    let mut presses = ButtonPresses::default();
    let pressed_at = Instant::now();
    presses.press(
        ButtonId::Back,
        PressPhase::HeldAsGesture {
            hold: GestureHold::new(token(1, ButtonId::Back), pressed_at),
        },
    );
    assert!(
        matches!(
            presses.admit_gesture(
                ButtonId::Forward,
                pressed_at + HOLD_STALE_AFTER.saturating_sub(Duration::from_millis(1))
            ),
            HoldAdmission::Refuse
        ),
        "a hold short of the horizon is still live"
    );

    let replacement = token(2, ButtonId::Forward);
    let HoldAdmission::Replace(stale) =
        presses.admit_gesture(ButtonId::Forward, pressed_at + HOLD_STALE_AFTER)
    else {
        panic!("an aged hold must yield to a new press");
    };
    assert_eq!(stale, token(1, ButtonId::Back));
    presses.press(ButtonId::Forward, held_for_swipe(replacement.clone()));
    assert_eq!(
        presses.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
        Some((replacement, ButtonId::Forward, GestureDirection::Right))
    );
    assert!(
        presses.release(ButtonId::Back).is_none(),
        "the replaced hold's late release is a stray"
    );
}

#[test]
fn begin_is_first_wins_while_a_hold_is_active() {
    let mut presses = ButtonPresses::default();
    let first = token(1, ButtonId::Back);
    presses.press(ButtonId::Back, held_for_swipe(first.clone()));
    assert!(
        matches!(
            presses.admit_gesture(ButtonId::Forward, Instant::now()),
            HoldAdmission::Refuse
        ),
        "a second press must not hijack the active hold"
    );
    // The refused press takes the single-action path instead.
    presses.press(ButtonId::Forward, PressPhase::Suppressed);

    assert_eq!(
        presses.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
        Some((first.clone(), ButtonId::Back, GestureDirection::Right))
    );
    assert!(matches!(
        presses.release(ButtonId::Forward),
        Some(PressPhase::Suppressed)
    ));
    assert_eq!(
        ended_hold(presses.release(ButtonId::Back)),
        Some((first, false))
    );
}

#[test]
fn end_matches_the_held_button_and_returns_its_token() {
    let mut presses = ButtonPresses::default();
    let press = token(1, ButtonId::Back);
    presses.press(ButtonId::Back, held(press.clone()));
    assert_eq!(ended_hold(presses.release(ButtonId::Forward)), None);
    assert_eq!(
        ended_hold(presses.release(ButtonId::Back)),
        Some((press, true))
    );
}

/// The OS may drop a hold's release with the interrupt. The hold ends with
/// it, so the next stray pointer move cannot commit a phantom swipe, while
/// other presses keep their release's disposition.
#[test]
fn an_interrupt_ends_the_hold_before_it_commits_a_phantom_swipe() {
    let mut presses = ButtonPresses::default();
    presses.press(ButtonId::Back, held_for_swipe(token(1, ButtonId::Back)));
    presses.press(ButtonId::Forward, PressPhase::Suppressed);
    presses.interrupt();

    assert_eq!(presses.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0), None);
    assert!(
        presses.release(ButtonId::Back).is_none(),
        "the hold's late release is a stray"
    );
    assert!(matches!(
        presses.release(ButtonId::Forward),
        Some(PressPhase::Suppressed)
    ));
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
    let release = |presses: &mut ButtonPresses, id| {
        presses
            .release(id)
            .map_or(EventDisposition::PassThrough, |press| press.disposition())
    };
    let mut presses = ButtonPresses::default();
    presses.press(ButtonId::Back, PressPhase::Suppressed);
    assert_eq!(
        release(&mut presses, ButtonId::Back),
        EventDisposition::Suppress
    );
    presses.press(ButtonId::Forward, PressPhase::Passthrough);
    assert_eq!(
        release(&mut presses, ButtonId::Forward),
        EventDisposition::PassThrough
    );
    assert_eq!(
        release(&mut presses, ButtonId::Forward),
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
        (Action::BrowserBack, target, PressControl::Key(keycode))
    );
    assert!(owner.shutdown());
}

/// A held action needs the key's release, which the one-shot action queue
/// never delivers.
#[test]
fn held_actions_on_a_key_run_as_held_presses() {
    use super::super::button::ButtonRuntimeEvent;

    for action in [
        Action::DpiShift,
        Action::HoldShortcut("Ctrl+F18".parse().expect("valid chord")),
        Action::TapKeyHoldingModifiers("Alt+F19".parse().expect("valid chord")),
        Action::HoldModifier("Ctrl+Shift".parse().expect("valid modifiers")),
    ] {
        let (dispatcher, mut owner, events) = test_dispatcher();
        let keycode = 0x7a;
        let modifiers = KeyModifiers::default();
        let bindings = Arc::new(RwLock::new(BTreeMap::from([(
            KeyTrigger { keycode, modifiers },
            action,
        )])));
        let (actions, queued) = mpsc::sync_channel(1);
        let key = |pressed| {
            handle_key(
                KeyEvent {
                    keycode,
                    pressed,
                    modifiers,
                },
                &bindings,
                &actions,
                &dispatcher,
                ActionDispatchTarget::capture,
            )
        };

        assert_eq!(key(true), EventDisposition::Suppress);
        queued.try_recv().unwrap_err();
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(1)),
            Ok(ButtonRuntimeEvent::Started(_))
        ));
        assert_eq!(key(false), EventDisposition::Suppress);
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(1)),
            Ok(ButtonRuntimeEvent::Ended { .. })
        ));
        assert!(owner.shutdown());
    }
}

#[test]
fn key_bound_to_easy_switch_passes_through_without_a_firing_device() {
    // A function key carries no device, so dispatching an Easy-Switch action
    // from it would move the selected device instead of the keyboard pressed.
    let (dispatcher, mut owner, _events) = test_dispatcher();
    let keycode = 0x7a;
    let modifiers = KeyModifiers::default();
    let bindings = Arc::new(RwLock::new(BTreeMap::from([(
        KeyTrigger { keycode, modifiers },
        Action::EasySwitchChannel2,
    )])));
    let (actions, queued) = mpsc::sync_channel(1);

    assert_eq!(
        handle_key(
            KeyEvent {
                keycode,
                pressed: true,
                modifiers,
            },
            &bindings,
            &actions,
            &dispatcher,
            ActionDispatchTarget::capture,
        ),
        EventDisposition::PassThrough
    );
    queued.try_recv().expect_err("no action should be queued");
    assert!(owner.shutdown());
}

#[test]
fn mouse_press_uses_the_target_published_with_its_binding_not_frontmost_safari() {
    use super::super::button::ButtonRuntimeEvent;
    use openlogi_hook::PointerTarget;

    let (dispatcher, mut owner, events) = test_dispatcher();
    let hooks = Arc::new(RwLock::new(HookMaps {
        bindings: BTreeMap::from([(ButtonId::Back, Action::PreviousDesktop.into())]),
        pointer: Some(PointerSelection::new(
            PointerTarget::Desktop,
            BTreeSet::from([ButtonId::Back]),
        )),
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
            focus: FocusBinding::Distinct,
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
            &hooks,
            &dispatcher,
            || { panic!("release does not retarget") }
        ),
        EventDisposition::PassThrough
    );
    assert!(owner.shutdown());
}

#[test]
fn pointer_press_judges_the_binding_layer_that_the_press_reads() {
    use openlogi_hook::PointerTarget;

    let mut maps = HookMaps {
        bindings: BTreeMap::from([(ButtonId::Back, Action::BrowserBack.into())]),
        shift_bindings: BTreeMap::from([(ButtonId::Back, Action::Copy.into())]),
        selected_device: Some("mouse".into()),
        pointer: Some(PointerSelection::new(
            PointerTarget::Desktop,
            BTreeSet::from([ButtonId::Back]),
        )),
        ..HookMaps::default()
    };
    let focus = |maps: &HookMaps, id| maps.pointer_press(id).map(|press| press.focus);
    assert_eq!(focus(&maps, ButtonId::Back), Some(FocusBinding::Distinct));
    assert_eq!(focus(&maps, ButtonId::Forward), Some(FocusBinding::Shared));
    // No app profile changes the G-Shift layer, so focus shares its binding
    // even where the base layer differs.
    maps.shift_held.insert("mouse".into());
    assert_eq!(
        maps.press_binding(ButtonId::Back).0,
        Some(Action::Copy.into())
    );
    assert_eq!(focus(&maps, ButtonId::Back), Some(FocusBinding::Shared));
    maps.pointer = None;
    assert_eq!(maps.pointer_press(ButtonId::Back), None);
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
            pointer: Some(PointerSelection::new(
                PointerTarget::Unavailable,
                BTreeSet::new(),
            )),
            ..HookMaps::default()
        }));
        let focused_safari = Cell::new(pressed_safari);
        assert_eq!(
            handle_button(
                ButtonId::Back,
                true,
                Some(&mouse),
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
                    handle_button(id, pressed, source.as_ref(), &hooks, &dispatcher, || {
                        Some(417)
                    }),
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

#[test]
fn held_gshift_replaces_a_gesture_button_with_its_shifted_binding() {
    let mut maps = HookMaps {
        selected_device: Some("g502".into()),
        ..HookMaps::default()
    };
    maps.gestures.insert(ButtonId::Back, BTreeMap::new());
    maps.shift_bindings
        .insert(ButtonId::Back, Binding::Single(Action::Copy));

    let (binding, is_gesture) = maps.press_binding(ButtonId::Back);
    assert!(is_gesture && binding.is_none());

    maps.shift_held.insert("g502".into());
    let (binding, is_gesture) = maps.press_binding(ButtonId::Back);
    assert!(!is_gesture);
    assert_eq!(binding, Some(Binding::Single(Action::Copy)));

    // A button with no shifted binding keeps its gestures under G-Shift.
    maps.gestures.insert(ButtonId::Forward, BTreeMap::new());
    assert!(maps.press_binding(ButtonId::Forward).1);
}

/// A Signature M650 L connected over Bluetooth, as evdev reports it (#2158).
fn m650_bluetooth() -> EventDevice {
    EventDevice {
        vendor_id: Some(0x046d),
        product_id: Some(0xb02a),
        product_name: Some("Signature M650 L Mouse".into()),
    }
}

fn m650_route() -> openlogi_hid::DeviceRoute {
    openlogi_hid::DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb02a,
    }
}

fn m650_session(epoch: u64) -> crate::runtime::HidppSessionId {
    crate::runtime::HidppSessionId::with_epoch("m650", epoch)
}

#[test]
fn a_diverted_button_is_owned_only_on_its_own_direct_device() {
    let mut diverted = DivertedButtons::default();
    diverted.record(&m650_session(7), &m650_route(), ButtonId::Back);
    assert!(diverted.owns(&m650_bluetooth(), ButtonId::Back));
    assert!(
        !diverted.owns(&m650_bluetooth(), ButtonId::Forward),
        "only what the session diverted"
    );
    let other_mouse = EventDevice {
        product_id: Some(0xb034),
        ..m650_bluetooth()
    };
    assert!(
        !diverted.owns(&other_mouse, ButtonId::Back),
        "another mouse's Back is a real press"
    );
    assert!(
        !diverted.owns(&EventDevice::default(), ButtonId::Back),
        "an unattributed source is never an echo"
    );
}

#[test]
fn a_receiver_route_names_no_echo_source_and_hidpp_only_buttons_are_not_recorded() {
    let mut diverted = DivertedButtons::default();
    let bolt = bolt_route(1);
    diverted.record(&m650_session(7), &bolt, ButtonId::Back);
    diverted.record(&m650_session(7), &m650_route(), ButtonId::DpiToggle);
    assert!(
        !diverted.owns(&m650_bluetooth(), ButtonId::Back),
        "a receiver route names no OS input identity"
    );
    assert!(
        diverted.diverted_on(&bolt, ButtonId::Back),
        "but its device's presses still never reach the OS"
    );
    assert!(!diverted.owns(&m650_bluetooth(), ButtonId::DpiToggle));
    assert!(!diverted.diverted_on(&m650_route(), ButtonId::DpiToggle));
}

#[test]
fn a_diversion_is_reported_only_for_its_own_route_while_its_session_lives() {
    let mut diverted = DivertedButtons::default();
    diverted.record(&m650_session(7), &bolt_route(2), ButtonId::MiddleClick);
    assert!(diverted.diverted_on(&bolt_route(2), ButtonId::MiddleClick));
    assert!(
        !diverted.diverted_on(&bolt_route(2), ButtonId::Back),
        "only what the session diverted"
    );
    assert!(
        !diverted.diverted_on(&bolt_route(1), ButtonId::MiddleClick),
        "another slot is another device"
    );
    diverted.release(&m650_session(7));
    assert!(!diverted.diverted_on(&bolt_route(2), ButtonId::MiddleClick));
}

#[test]
fn releasing_a_stale_epoch_keeps_its_successors_diversion() {
    let mut diverted = DivertedButtons::default();
    diverted.record(&m650_session(7), &m650_route(), ButtonId::Back);
    diverted.record(&m650_session(8), &m650_route(), ButtonId::Back);
    diverted.release(&m650_session(7));
    assert!(diverted.owns(&m650_bluetooth(), ButtonId::Back));
    diverted.release(&m650_session(8));
    assert!(!diverted.owns(&m650_bluetooth(), ButtonId::Back));
}

/// #2158: Linux 7.3 re-reports a Back the M650's capture session diverted as
/// an evdev press. The session dispatches it, so the hook must drop both
/// edges instead of running the binding a second time.
#[test]
fn an_echo_of_a_hidpp_diverted_press_is_dropped_on_both_edges() {
    use super::super::button::ButtonRuntimeEvent;
    let (dispatcher, mut owner, events) = test_dispatcher();
    let mut maps = HookMaps {
        bindings: BTreeMap::from([(ButtonId::Back, Action::NextTab.into())]),
        ..HookMaps::default()
    };
    maps.diverted_buttons
        .record(&m650_session(7), &m650_route(), ButtonId::Back);
    let hooks = Arc::new(RwLock::new(maps));
    let m650 = m650_bluetooth();
    for pressed in [true, false] {
        assert_eq!(
            handle_button(
                ButtonId::Back,
                pressed,
                Some(&m650),
                &hooks,
                &dispatcher,
                || None
            ),
            EventDisposition::Suppress,
            "pressed {pressed}"
        );
    }
    assert!(
        events.recv_timeout(Duration::from_millis(200)).is_err(),
        "the capture session dispatches this press, not the hook"
    );

    let other_mouse = EventDevice {
        product_id: Some(0xb034),
        ..m650_bluetooth()
    };
    assert_eq!(
        handle_button(
            ButtonId::Back,
            true,
            Some(&other_mouse),
            &hooks,
            &dispatcher,
            || None
        ),
        EventDisposition::Suppress
    );
    let ButtonRuntimeEvent::Started(press) =
        events.recv_timeout(Duration::from_secs(1)).expect("down")
    else {
        panic!("started");
    };
    assert_eq!(press.start_action(), Some(&Action::NextTab));
    assert_eq!(
        handle_button(
            ButtonId::Back,
            false,
            Some(&other_mouse),
            &hooks,
            &dispatcher,
            || None
        ),
        EventDisposition::Suppress
    );
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(1)).expect("up"),
        ButtonRuntimeEvent::Ended { .. }
    ));
    assert!(owner.shutdown());
}

/// A release follows its press: an echo whose session ends mid-press still
/// has its release dropped, rather than a stray native Back release reaching
/// the desktop.
#[test]
fn an_echoed_press_keeps_its_release_after_the_session_ends() {
    let (dispatcher, mut owner, events) = test_dispatcher();
    let mut maps = HookMaps {
        bindings: BTreeMap::from([(ButtonId::Back, Action::NextTab.into())]),
        ..HookMaps::default()
    };
    maps.diverted_buttons
        .record(&m650_session(7), &m650_route(), ButtonId::Back);
    let hooks = Arc::new(RwLock::new(maps));
    let m650 = m650_bluetooth();
    assert_eq!(
        handle_button(
            ButtonId::Back,
            true,
            Some(&m650),
            &hooks,
            &dispatcher,
            || None
        ),
        EventDisposition::Suppress
    );
    hooks
        .write()
        .expect("maps")
        .diverted_buttons
        .release(&m650_session(7));
    assert_eq!(
        handle_button(
            ButtonId::Back,
            false,
            Some(&m650),
            &hooks,
            &dispatcher,
            || None
        ),
        EventDisposition::Suppress
    );
    assert!(
        events.recv_timeout(Duration::from_millis(200)).is_err(),
        "neither edge reached the button runtime"
    );
    assert!(owner.shutdown());
}

/// A Bolt receiver slot, the route of a mouse whose OS input names no device.
fn bolt_route(slot: u8) -> openlogi_hid::DeviceRoute {
    openlogi_hid::DeviceRoute::Bolt {
        receiver_uid: "C0FFEE".into(),
        slot,
    }
}

/// Hook maps for an MX Master 3S on Bolt slot 2 whose Middle opens the Actions
/// Ring, with its capture session having diverted Middle.
fn mx_master_3s_diverting_middle() -> HookMaps {
    let mut maps = HookMaps {
        bindings: BTreeMap::from([(ButtonId::MiddleClick, Action::ShowActionsRing.into())]),
        selected_device: Some("mx-master-3s".into()),
        selected_route: Some(bolt_route(2)),
        ..HookMaps::default()
    };
    maps.diverted_buttons.record(
        &crate::runtime::HidppSessionId::with_epoch("mx-master-3s", 7),
        &bolt_route(2),
        ButtonId::MiddleClick,
    );
    maps
}

/// #2381: Windows names no source for a Middle click. Once the selected MX
/// Master 3S has diverted its Middle, an OS Middle click cannot be its own, so
/// the G305 that sent it keeps its native click on both edges instead of
/// opening the 3S's Actions Ring.
#[test]
fn an_unattributed_press_of_a_button_the_selected_device_diverted_stays_native() {
    let (dispatcher, mut owner, events) = test_dispatcher();
    let hooks = Arc::new(RwLock::new(mx_master_3s_diverting_middle()));
    assert_eq!(
        handle_button(
            ButtonId::MiddleClick,
            true,
            None,
            &hooks,
            &dispatcher,
            || None
        ),
        EventDisposition::PassThrough
    );
    // The release follows its press even when the session ends in between.
    hooks.write().expect("maps").diverted_buttons.release(
        &crate::runtime::HidppSessionId::with_epoch("mx-master-3s", 7),
    );
    assert_eq!(
        handle_button(
            ButtonId::MiddleClick,
            false,
            None,
            &hooks,
            &dispatcher,
            || None
        ),
        EventDisposition::PassThrough
    );
    assert!(
        events.recv_timeout(Duration::from_millis(200)).is_err(),
        "the selected device's binding never ran"
    );
    assert!(owner.shutdown());
}

/// Without a diversion on the selected device the unattributed press may be
/// its own, so the binding still applies — through a release that arrives
/// after a session diverted the control mid-press.
#[cfg(not(target_os = "macos"))]
#[test]
fn an_unattributed_press_keeps_the_selected_binding_while_that_device_has_not_diverted_it() {
    use super::super::button::ButtonRuntimeEvent;
    let (dispatcher, mut owner, events) = test_dispatcher();
    let mut maps = mx_master_3s_diverting_middle();
    maps.selected_route = Some(bolt_route(1));
    let hooks = Arc::new(RwLock::new(maps));
    assert_eq!(
        handle_button(
            ButtonId::MiddleClick,
            true,
            None,
            &hooks,
            &dispatcher,
            || None
        ),
        EventDisposition::Suppress,
        "another device's diversion says nothing about the selected one"
    );
    let ButtonRuntimeEvent::Started(press) =
        events.recv_timeout(Duration::from_secs(1)).expect("down")
    else {
        panic!("started");
    };
    assert_eq!(press.start_action(), Some(&Action::ShowActionsRing));
    hooks.write().expect("maps").selected_route = Some(bolt_route(2));
    assert_eq!(
        handle_button(
            ButtonId::MiddleClick,
            false,
            None,
            &hooks,
            &dispatcher,
            || None
        ),
        EventDisposition::Suppress
    );
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(1)).expect("up"),
        ButtonRuntimeEvent::Ended { .. }
    ));
    assert!(owner.shutdown());
}

/// Wait for the runtime to end `hold`'s lifecycle as a stale hold.
fn assert_stale_hold_ended(
    events: &mpsc::Receiver<super::super::button::ButtonRuntimeEvent>,
    hold: &super::super::button::ActivePress,
) {
    use super::super::button::{ButtonRuntimeEvent, CancelReason, EndReason};
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(1)).expect("ended"),
        ButtonRuntimeEvent::Ended {
            press,
            reason: EndReason::Canceled(CancelReason::StaleHold),
        } if press.token() == hold.token()
    ));
}

/// A press the runtime refused as a gesture went native, so its release must
/// too, even though an earlier press of the button was suppressed and lost its
/// release. Suppressing this release would leave the button stuck down on the
/// desktop.
#[test]
fn a_refused_gesture_press_after_a_lost_release_passes_both_edges() {
    let (dispatcher, mut owner, _events) = test_dispatcher();
    let hooks = Arc::new(RwLock::new(HookMaps {
        bindings: BTreeMap::from([(ButtonId::Back, Action::NextTab.into())]),
        ..HookMaps::default()
    }));
    let mouse = m650_bluetooth();
    let back = |pressed| {
        handle_button(
            ButtonId::Back,
            pressed,
            Some(&mouse),
            &hooks,
            &dispatcher,
            || None,
        )
    };
    assert_eq!(back(true), EventDisposition::Suppress);
    // That release is lost. Back becomes a gesture button, and the runtime
    // refuses the next lifecycle.
    hooks
        .write()
        .expect("maps")
        .gestures
        .insert(ButtonId::Back, BTreeMap::new());
    assert!(owner.shutdown());
    assert_eq!(back(true), EventDisposition::PassThrough);
    assert_eq!(back(false), EventDisposition::PassThrough);
}

/// The same for a gesture hold that lost its release: it must not claim the
/// release of the button's next press, which went native once the button
/// stopped being a gesture button. The hold's lifecycle, which may hold a
/// shortcut down, ends at that press instead.
#[test]
fn a_native_press_after_a_lost_hold_passes_both_edges() {
    use super::super::button::ButtonRuntimeEvent;
    let (dispatcher, mut owner, events) = test_dispatcher();
    let hooks = Arc::new(RwLock::new(HookMaps {
        gestures: BTreeMap::from([(ButtonId::Back, BTreeMap::new())]),
        ..HookMaps::default()
    }));
    let mouse = m650_bluetooth();
    let back = |pressed| {
        handle_button(
            ButtonId::Back,
            pressed,
            Some(&mouse),
            &hooks,
            &dispatcher,
            || None,
        )
    };
    assert_eq!(back(true), EventDisposition::Suppress);
    let ButtonRuntimeEvent::Started(hold) =
        events.recv_timeout(Duration::from_secs(1)).expect("down")
    else {
        panic!("started");
    };
    // That release is lost, and Back goes back to its native click.
    hooks.write().expect("maps").gestures.clear();
    assert_eq!(back(true), EventDisposition::PassThrough);
    assert_stale_hold_ended(&events, &hold);
    assert_eq!(back(false), EventDisposition::PassThrough);
    assert!(owner.shutdown());
}

/// A gesture hold that lost its release ends at the button's next press, even
/// one that is no gesture: here an echo of a HID++ diversion recorded since.
/// Left behind, the hold would turn that press's motion into a phantom swipe
/// and dispatch it on the still-live lifecycle, which ends at the press instead.
#[test]
fn a_re_press_ends_a_lost_hold_before_its_motion_commits_a_phantom_swipe() {
    use super::super::button::ButtonRuntimeEvent;
    use openlogi_core::binding::GESTURE_HOLD_FOR_SWIPE;

    let (dispatcher, mut owner, events) = test_dispatcher();
    let hooks = Arc::new(RwLock::new(HookMaps {
        gestures: BTreeMap::from([(ButtonId::Back, BTreeMap::new())]),
        ..HookMaps::default()
    }));
    let m650 = m650_bluetooth();
    let back = |pressed| {
        handle_button(
            ButtonId::Back,
            pressed,
            Some(&m650),
            &hooks,
            &dispatcher,
            || None,
        )
    };
    assert_eq!(back(true), EventDisposition::Suppress);
    let ButtonRuntimeEvent::Started(hold) =
        events.recv_timeout(Duration::from_secs(1)).expect("down")
    else {
        panic!("started");
    };
    // That release is lost, and a capture session diverts Back.
    hooks.write().expect("maps").diverted_buttons.record(
        &m650_session(7),
        &m650_route(),
        ButtonId::Back,
    );
    assert_eq!(back(true), EventDisposition::Suppress);
    assert_stale_hold_ended(&events, &hold);

    thread::sleep(GESTURE_HOLD_FOR_SWIPE);
    assert_eq!(
        handle_moved(GESTURE_SWIPE_THRESHOLD + 10, 0, &hooks, &dispatcher),
        EventDisposition::PassThrough
    );
    assert!(
        events.recv_timeout(Duration::from_millis(200)).is_err(),
        "no swipe fired"
    );
    assert_eq!(back(false), EventDisposition::Suppress);
    assert!(owner.shutdown());
}
