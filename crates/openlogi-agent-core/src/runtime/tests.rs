//! Regression tests for action dispatch and lifecycle-owned output.

mod mouse_hold;
mod output;

use super::*;
use output::{OutputEvent, RecordingOutput};

static BROWSER_NAV_TEST_LOCK: Mutex<()> = Mutex::new(());

fn executor_with(pointer_speeds: Arc<PointerSpeeds>) -> ActionExecutor {
    let (_signal, device_io) = openlogi_hid::device_io_channel();
    let (requests, _requests_rx) = tokio::sync::mpsc::unbounded_channel();
    ActionExecutor {
        dpi_cycle: Arc::default(),
        pointer_speeds,
        access: DeviceAccess {
            channel: openlogi_hid::CaptureChannelSlot::default(),
            registry: openlogi_hid::ChannelRegistry::default(),
            receiver_access: crate::receiver_access::ReceiverAccess::default(),
            device_io,
            transport_turns: crate::hardware::TransportTurns::default(),
        },
        requests,
    }
}

/// The MX Ergo's precision button keeps its default Cycle DPI Presets
/// binding. The mouse has no DPI presets and scales its pointer with
/// `0x2205`, so each press switches precision mode instead of being
/// dropped (#1372). A mouse that is not scaled still drops the press.
#[test]
fn cycle_dpi_presets_toggles_precision_mode_on_a_scaled_mouse() {
    let pointer_speeds = Arc::new(PointerSpeeds::default());
    pointer_speeds.publish(
        None,
        [crate::pointer_speed::SpeedMouse {
            key: "ergo".to_string(),
            route: openlogi_core::hid::DeviceRoute::Unifying {
                receiver_uid: "UNIFYING".to_string(),
                slot: 1,
            },
            saved: crate::pointer_speed::SavedSpeed::Scaled(None),
        }],
    );
    let executor = executor_with(pointer_speeds.clone());

    executor.dispatch(&Action::CycleDpiPresets, Some("ergo"));
    assert!(pointer_speeds.precision("ergo"));
    executor.dispatch(&Action::CycleDpiPresets, Some("ergo"));
    assert!(!pointer_speeds.precision("ergo"));

    executor.dispatch(&Action::CycleDpiPresets, Some("master"));
    assert!(!pointer_speeds.precision("master"));
}

/// Presets saved for a scaled mouse, by an older panel or by hand, do not
/// take the press: the mouse has no DPI to write them to.
#[test]
fn cycle_dpi_presets_prefers_precision_mode_over_saved_presets() {
    let route = openlogi_core::hid::DeviceRoute::Unifying {
        receiver_uid: "UNIFYING".to_string(),
        slot: 1,
    };
    let pointer_speeds = Arc::new(PointerSpeeds::default());
    pointer_speeds.publish(
        None,
        [crate::pointer_speed::SpeedMouse {
            key: "ergo".to_string(),
            route: route.clone(),
            saved: crate::pointer_speed::SavedSpeed::Scaled(None),
        }],
    );
    let mut executor = executor_with(pointer_speeds.clone());
    let mut cycles = DpiCycles::default();
    cycles.by_key.insert(
        "ergo".to_string(),
        DpiCycleState {
            presets: vec![
                openlogi_core::hid::Dpi::new(800),
                openlogi_core::hid::Dpi::new(1600),
            ],
            index: 0,
            target: Some(route),
            capabilities: None,
            ..DpiCycleState::default()
        },
    );
    executor.dpi_cycle = Arc::new(RwLock::new(cycles));

    executor.dispatch(&Action::CycleDpiPresets, Some("ergo"));

    assert!(pointer_speeds.precision("ergo"));
    let cycles = executor.dpi_cycle.read().expect("dpi_cycle lock");
    assert_eq!(cycles.by_key["ergo"].index, 0, "the presets did not cycle");
}

/// A DPI mouse has DPI to cycle, so its DPI button keeps cycling the
/// presets (#1931). Its precision button sets the lowest DPI, and a
/// preset press ends precision mode: the mouse leaves the lowest DPI.
#[test]
fn a_dpi_mouse_cycles_its_presets_and_a_preset_ends_precision_mode() {
    let route = openlogi_core::hid::DeviceRoute::Unifying {
        receiver_uid: "UNIFYING".to_string(),
        slot: 2,
    };
    let pointer_speeds = Arc::new(PointerSpeeds::default());
    pointer_speeds.publish(
        None,
        [crate::pointer_speed::SpeedMouse {
            key: "master".to_string(),
            route: route.clone(),
            saved: crate::pointer_speed::SavedSpeed::Dpi(None),
        }],
    );
    let mut executor = executor_with(pointer_speeds.clone());
    let mut cycles = DpiCycles::default();
    cycles.by_key.insert(
        "master".to_string(),
        DpiCycleState {
            presets: vec![
                openlogi_core::hid::Dpi::new(800),
                openlogi_core::hid::Dpi::new(1600),
            ],
            index: 0,
            target: Some(route),
            capabilities: None,
            ..DpiCycleState::default()
        },
    );
    executor.dpi_cycle = Arc::new(RwLock::new(cycles));

    executor.dispatch(&Action::TogglePrecisionMode, Some("master"));
    assert!(pointer_speeds.precision("master"));

    executor.dispatch(&Action::CycleDpiPresets, Some("master"));
    assert!(!pointer_speeds.precision("master"));
    // No mouse answers in this test, so the preset write never lands
    // and the restore stays owed.
    assert!(pointer_speeds.restore_pending("master"));
    let cycles = executor.dpi_cycle.read().expect("dpi_cycle lock");
    assert_eq!(cycles.by_key["master"].index, 1, "the presets cycled");
}

#[test]
fn ring_trigger_reaches_the_ring_over_an_unavailable_pointer_target() {
    // The second press that toggles the ring closed lands on the ring's
    // own overlay window, which the pointer hit test reports as
    // unavailable.
    let (_signal, device_io) = openlogi_hid::device_io_channel();
    let (requests, mut ring_rx) = tokio::sync::mpsc::unbounded_channel();
    let executor = ActionExecutor {
        dpi_cycle: Arc::default(),
        pointer_speeds: Arc::default(),
        access: DeviceAccess {
            channel: openlogi_hid::CaptureChannelSlot::default(),
            registry: openlogi_hid::ChannelRegistry::default(),
            receiver_access: crate::receiver_access::ReceiverAccess::default(),
            device_io,
            transport_turns: crate::hardware::TransportTurns::default(),
        },
        requests,
    };

    executor.dispatch_to(
        &Action::ShowActionsRing,
        Some("mouse"),
        ActionDispatchTarget::for_pointer(
            Some(PointerPress::distinct(
                openlogi_hook::PointerTarget::Unavailable,
            )),
            || None,
        ),
    );

    assert_eq!(
        ring_rx.try_recv(),
        Ok(ActionRequest::ShowActionsRing {
            device_key: Some("mouse".to_owned())
        })
    );
}
/// Page actions go to the agent, keyed by the pressing keypad, and — like
/// the ring — no pointer target gates them: the press acts on the keypad.
#[test]
fn page_actions_reach_the_agent_as_page_turns() {
    let (_signal, device_io) = openlogi_hid::device_io_channel();
    let (requests, mut received) = tokio::sync::mpsc::unbounded_channel();
    let executor = ActionExecutor {
        dpi_cycle: Arc::default(),
        pointer_speeds: Arc::default(),
        access: DeviceAccess {
            channel: openlogi_hid::CaptureChannelSlot::default(),
            registry: openlogi_hid::ChannelRegistry::default(),
            receiver_access: crate::receiver_access::ReceiverAccess::default(),
            device_io,
            transport_turns: crate::hardware::TransportTurns::default(),
        },
        requests,
    };
    let unavailable = || {
        ActionDispatchTarget::for_pointer(
            Some(PointerPress::distinct(
                openlogi_hook::PointerTarget::Unavailable,
            )),
            || None,
        )
    };

    executor.dispatch_to(&Action::NextKeypadPage, Some("keypad"), unavailable());
    executor.dispatch_to(&Action::PreviousKeypadPage, Some("keypad"), unavailable());

    for turn in [PageTurn::Next, PageTurn::Previous] {
        assert_eq!(
            received.try_recv(),
            Ok(ActionRequest::TurnKeypadPage {
                device_key: Some("keypad".to_owned()),
                turn,
            })
        );
    }
}

#[test]
fn instantaneous_actions_do_not_enter_held_state() {
    let press = PressToken::hook_for_test(1, ButtonId::Back);
    let mut held = HeldActions::new(RecordingOutput::default());

    assert!(!held.start(&press, &Action::Copy, ActionDispatchTarget::Keyboard));
    held.end(&press);
}

#[test]
fn remapped_middle_click_enters_held_state_until_release() {
    use openlogi_core::binding::MouseButton;
    let press = PressToken::hook_for_test(1, ButtonId::Back);
    let output = RecordingOutput::default();
    let mut held = HeldActions::new(output.clone());

    assert!(
        held.start(&press, &Action::MiddleClick, ActionDispatchTarget::Keyboard),
        "a remapped middle click must remain down until its physical press ends"
    );
    assert!(held.contains(&press));
    assert_eq!(
        output.events(),
        [OutputEvent::MouseDown(MouseButton::Middle)]
    );
    held.end(&press);
    assert!(!held.contains(&press));
    assert_eq!(
        output.events(),
        [
            OutputEvent::MouseDown(MouseButton::Middle),
            OutputEvent::MouseUp(MouseButton::Middle)
        ]
    );
    held.end(&press);
    assert_eq!(
        output.events().len(),
        2,
        "a duplicate terminal event cannot release twice"
    );
}

#[test]
fn an_unavailable_middle_click_target_does_not_create_an_output_owner() {
    let press = PressToken::hook_for_test(1, ButtonId::Back);
    let mut held = HeldActions::new(RecordingOutput::default());
    let target = ActionDispatchTarget::Pointer {
        target: openlogi_hook::PointerTarget::Unsupported,
        focus: FocusBinding::Distinct,
        fallback_safari_pid: None,
    };

    assert!(held.start(&press, &Action::MiddleClick, target));
    assert!(!held.contains(&press));
}

#[test]
fn triggered_middle_clicks_remain_instantaneous() {
    let press = PressToken::hook_for_test(1, ButtonId::Back);
    let mut held = HeldActions::new(RecordingOutput::default());

    assert!(!held.start_shortcut(&press, &Action::MiddleClick, ActionDispatchTarget::Keyboard,));
    assert!(!held.contains(&press));
}

/// An executor for one mouse with presets and no reachable device.
fn dpi_shift_executor(current: Option<Dpi>) -> ActionExecutor {
    let mut cycles = DpiCycles::default();
    cycles.by_key.insert(
        "mouse".into(),
        DpiCycleState {
            presets: vec![Dpi::new(400), Dpi::new(1600)],
            target: Some(DeviceRoute::Bolt {
                receiver_uid: "AA00".into(),
                slot: 1,
            }),
            current,
            ..DpiCycleState::default()
        },
    );
    let mut executor = executor_with(Arc::default());
    executor.dpi_cycle = Arc::new(RwLock::new(cycles));
    executor
}

/// A preset step reads the mouse first only while OpenLogi never set its
/// DPI; once it did, the index is where the mouse is.
#[test]
fn a_dpi_step_reads_the_mouse_only_while_its_dpi_is_unknown() {
    let unknown = dpi_shift_executor(None);
    assert!(unknown.step_dpi_from_reading(Some("mouse"), &Action::NextDpiPreset));
    assert!(unknown.step_dpi_from_reading(Some("mouse"), &Action::PreviousDpiPreset));
    assert!(!unknown.step_dpi_from_reading(Some("missing"), &Action::NextDpiPreset));

    let known = dpi_shift_executor(Some(Dpi::new(1600)));
    assert!(!known.step_dpi_from_reading(Some("mouse"), &Action::NextDpiPreset));
}

/// A preset step caches its DPI when the write is requested. When the
/// mouse rejects it, the next DPI shift must not return to that DPI: the
/// rejection forgets it, so the shift reads the mouse and returns there.
#[test]
fn a_rejected_preset_step_leaves_the_dpi_shift_returning_to_the_old_dpi() {
    let executor = dpi_shift_executor(Some(Dpi::new(400)));
    let route = DeviceRoute::Bolt {
        receiver_uid: "AA00".into(),
        slot: 1,
    };
    let stepped = executor.step_dpi(Some("mouse"), &Action::NextDpiPreset);
    assert_eq!(stepped.map(|(dpi, _)| dpi), Some(Dpi::new(1600)));
    executor.preset_rejected(route)(Dpi::new(1600));
    assert_eq!(executor.dpi_cycle.read().unwrap().current("mouse"), None);

    let press = PressToken::hook_for_test(1, ButtonId::G6);
    let mut held = HeldDpiShifts::default();
    held.start(&executor, &press, Some("mouse"));
    let shift = held.by_device.values().next().unwrap();
    // What the shift's own write reads from the mouse, which kept 400.
    shift.before.set(Dpi::new(400));
    let cycles = executor.dpi_cycle.read().unwrap();
    assert_eq!(shift.restore_to(&cycles), Some(Dpi::new(400)));
}

/// A rejection that arrives after a newer preset pick leaves that pick.
#[test]
fn a_rejected_preset_step_keeps_a_newer_pick() {
    let executor = dpi_shift_executor(Some(Dpi::new(400)));
    let route = DeviceRoute::Bolt {
        receiver_uid: "AA00".into(),
        slot: 1,
    };
    executor.step_dpi(Some("mouse"), &Action::NextDpiPreset);
    executor.step_dpi(Some("mouse"), &Action::PreviousDpiPreset);
    executor.preset_rejected(route)(Dpi::new(1600));
    assert_eq!(
        executor.dpi_cycle.read().unwrap().current("mouse"),
        Some(Dpi::new(400))
    );
}

#[test]
fn dpi_shift_ends_only_with_the_last_held_press() {
    let executor = dpi_shift_executor(Some(Dpi::new(1200)));
    let (first, second) = (
        PressToken::hook_for_test(1, ButtonId::G6),
        PressToken::hook_for_test(2, ButtonId::G7),
    );
    let mut held = HeldDpiShifts::default();
    held.start(&executor, &first, Some("mouse"));
    held.start(&executor, &second, Some("mouse"));
    held.end(&executor, &first);
    assert_eq!(held.by_device.values().next().map(|h| h.presses), Some(1));
    held.end(&executor, &second);
    assert!(held.by_device.is_empty() && held.by_press.is_empty());
}

/// A press right after a release can supersede the pending restore while
/// the sensor is still at the shifted DPI, so it must not read again.
#[test]
fn dpi_shift_keeps_its_reading_until_a_restore_is_written() {
    let executor = dpi_shift_executor(None);
    let (first, second) = (
        PressToken::hook_for_test(1, ButtonId::G6),
        PressToken::hook_for_test(2, ButtonId::G6),
    );
    let mut held = HeldDpiShifts::default();
    held.start(&executor, &first, Some("mouse"));
    let before = held.by_device.values().next().unwrap().before.clone();
    before.set(Dpi::new(1200));
    held.end(&executor, &first);

    held.start(&executor, &second, Some("mouse"));
    let again = &held.by_device.values().next().unwrap().before;
    assert_eq!(again.get(), Some(Dpi::new(1200)));
    before.clear();
    assert_eq!(again.get(), None, "a written restore forgets the reading");
}

/// A failed restore keeps the reading, but a reconnect puts the mouse at
/// its onboard DPI, so the next hold must read it again.
#[test]
fn a_dpi_shift_after_a_reconnect_reads_the_mouse_again() {
    let executor = dpi_shift_executor(None);
    let press = PressToken::hook_for_test(1, ButtonId::G6);
    executor.dpi_cycle.read().unwrap().by_key["mouse"]
        .before_shift
        .set(Dpi::new(1200));
    executor.dpi_cycle.write().unwrap().reconnected("mouse");

    let mut held = HeldDpiShifts::default();
    held.start(&executor, &press, Some("mouse"));
    assert_eq!(held.by_device.values().next().unwrap().before.get(), None);
}

/// A panel write the mouse rejects during a hold clears `current`, so
/// the release falls back to the DPI the shift started from.
#[test]
fn a_rejected_panel_dpi_during_a_shift_still_returns_to_the_start() {
    let executor = dpi_shift_executor(Some(Dpi::new(1200)));
    let press = PressToken::hook_for_test(1, ButtonId::G6);
    let mut held = HeldDpiShifts::default();
    held.start(&executor, &press, Some("mouse"));

    let route = DeviceRoute::Bolt {
        receiver_uid: "AA00".into(),
        slot: 1,
    };
    let mut cycles = executor.dpi_cycle.write().unwrap();
    cycles.reapplied_at(&route, Dpi::new(1600));
    cycles.rejected_at(&route, Dpi::new(1600));
    let shift = held.by_device.values().next().unwrap();
    assert_eq!(shift.restore_to(&cycles), Some(Dpi::new(1200)));
}

#[test]
fn dpi_shift_returns_to_the_dpi_last_set_or_else_the_one_it_read() {
    let mut cycles = DpiCycles::default();
    cycles.by_key.insert(
        "mouse".into(),
        DpiCycleState {
            presets: vec![Dpi::new(400), Dpi::new(1600)],
            ..DpiCycleState::default()
        },
    );
    let held = HeldDpiShift {
        presses: 1,
        key: "mouse".into(),
        route: DeviceRoute::Bolt {
            receiver_uid: "AA00".into(),
            slot: 1,
        },
        before: DpiBeforeShift::default(),
        own_restore: None,
    };
    // Never set and not read yet: a preset is not the DPI to return to.
    assert_eq!(held.restore_to(&cycles), None);
    held.before.set(Dpi::new(1200));
    assert_eq!(held.restore_to(&cycles), Some(Dpi::new(1200)));
    // A preset picked during the hold stays after release.
    cycles.state_for(Some("mouse")).unwrap().step_up();
    assert_eq!(held.restore_to(&cycles), Some(Dpi::new(1600)));
}

/// Precision mode turned on during the hold sets the DPI, and its write
/// may still be waiting, so the release writes nothing after it and
/// forgets its reading.
#[test]
fn dpi_shift_release_leaves_precision_mode_alone() {
    let speeds = Arc::new(PointerSpeeds::default());
    speeds.publish(
        None,
        [crate::pointer_speed::SpeedMouse {
            key: "mouse".to_string(),
            route: DeviceRoute::Bolt {
                receiver_uid: "AA00".into(),
                slot: 1,
            },
            saved: crate::pointer_speed::SavedSpeed::Dpi(Some(Dpi::new(1600))),
        }],
    );
    let mut executor = dpi_shift_executor(None);
    executor.pointer_speeds = Arc::clone(&speeds);
    let press = PressToken::hook_for_test(1, ButtonId::G7);
    let mut held = HeldDpiShifts::default();
    held.start(&executor, &press, Some("mouse"));
    let before = held.by_device.values().next().unwrap().before.clone();
    before.set(Dpi::new(1200));
    let (on, _) = speeds
        .toggle_precision(Some("mouse"), PrecisionPress::Assigned)
        .expect("a DPI mouse toggles precision mode");
    assert!(on);

    held.end(&executor, &press);
    assert!(held.by_device.is_empty());
    assert_eq!(before.get(), None, "the reading goes with the restore");
}

/// A quick press while precision mode is on: the shift's own low write
/// ended the mode and still waits when the press ends. The release must
/// restore anyway, and owe nothing once it has.
#[test]
fn a_quick_dpi_shift_in_precision_mode_still_restores() {
    let speeds = Arc::new(PointerSpeeds::default());
    speeds.publish(
        None,
        [crate::pointer_speed::SpeedMouse {
            key: "mouse".to_string(),
            route: DeviceRoute::Bolt {
                receiver_uid: "AA00".into(),
                slot: 1,
            },
            saved: crate::pointer_speed::SavedSpeed::Dpi(Some(Dpi::new(1600))),
        }],
    );
    speeds
        .toggle_precision(Some("mouse"), PrecisionPress::Assigned)
        .expect("precision on");
    let mut executor = dpi_shift_executor(None);
    executor.pointer_speeds = Arc::clone(&speeds);
    let press = PressToken::hook_for_test(1, ButtonId::G7);
    let mut held = HeldDpiShifts::default();
    held.start(&executor, &press, Some("mouse"));
    let before = held.by_device.values().next().unwrap().before.clone();
    before.set(Dpi::new(1200));

    held.end(&executor, &press);
    assert!(held.by_device.is_empty());
    assert_eq!(
        before.get(),
        Some(Dpi::new(1200)),
        "the release restores, so it keeps the reading until it lands"
    );
}

/// After a session enters host mode the saved DPI is written again, and
/// a DPI shift returns to it rather than to a preset picked before.
#[test]
fn entering_host_mode_writes_the_saved_dpi_again() {
    let speeds = Arc::new(PointerSpeeds::default());
    speeds.publish(
        None,
        [crate::pointer_speed::SpeedMouse {
            key: "mouse".to_string(),
            route: DeviceRoute::Bolt {
                receiver_uid: "AA00".into(),
                slot: 1,
            },
            saved: crate::pointer_speed::SavedSpeed::Dpi(Some(Dpi::new(1600))),
        }],
    );
    let mut executor = dpi_shift_executor(Some(Dpi::new(400)));
    executor.pointer_speeds = speeds;

    executor.host_mode_entered("mouse");
    let cycles = executor.dpi_cycle.read().expect("dpi_cycle lock");
    assert_eq!(cycles.current("mouse"), Some(Dpi::new(1600)));
}

#[test]
fn captured_safari_navigation_keeps_press_time_pid_and_never_falls_back() {
    for (action, forward) in [(Action::BrowserBack, false), (Action::BrowserForward, true)] {
        for navigated in [false, true] {
            let mut pid_and_direction = None;
            let mut keyboard = false;
            assert_eq!(
                dispatch_browser_navigation(
                    &action,
                    ActionDispatchTarget::SafariProcess(417),
                    |pid, direction| {
                        pid_and_direction = Some((pid, direction));
                        navigated
                    },
                    || keyboard = true,
                ),
                navigated
            );
            assert_eq!(pid_and_direction, Some((417, forward)));
            assert!(!keyboard);
        }
    }
}

#[test]
fn keyboard_navigation_never_reinterprets_a_later_safari_focus() {
    let mut ax_called = false;
    let mut keyboard = false;

    assert!(dispatch_browser_navigation(
        &Action::BrowserForward,
        ActionDispatchTarget::Keyboard,
        |_, _| {
            ax_called = true;
            true
        },
        || keyboard = true,
    ));

    assert!(!ax_called);
    assert!(keyboard);
}

#[test]
fn browser_navigation_debounce_is_per_direction_and_expires() {
    let _guard = BROWSER_NAV_TEST_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let now = Instant::now();
    *BROWSER_NAV_LAST
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = (Some(now), None);

    assert!(browser_nav_debounce_begin(&Action::BrowserBack).is_none());
    assert!(browser_nav_debounce_begin(&Action::BrowserForward).is_some());

    let expired = Instant::now()
        .checked_sub(BROWSER_NAV_DEBOUNCE)
        .expect("the debounce interval fits before the current instant");
    BROWSER_NAV_LAST
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .0 = Some(expired);

    assert!(browser_nav_debounce_begin(&Action::BrowserBack).is_some());
}

#[test]
fn slow_successful_navigation_still_debounces_the_queued_duplicate() {
    let _guard = BROWSER_NAV_TEST_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let started = Instant::now()
        .checked_sub(BROWSER_NAV_DEBOUNCE * 2)
        .expect("the simulated AX duration fits before the current instant");
    *BROWSER_NAV_LAST
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = (None, Some(started));

    browser_nav_debounce_commit(BrowserNavDebounceReservation {
        forward: true,
        timestamp: started,
    });

    assert!(browser_nav_debounce_begin(&Action::BrowserForward).is_none());
    assert!(browser_nav_debounce_begin(&Action::BrowserBack).is_some());
}

#[test]
fn failed_captured_navigation_releases_its_debounce_reservation() {
    let _guard = BROWSER_NAV_TEST_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    *BROWSER_NAV_LAST
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = (None, None);
    let reservation = browser_nav_debounce_begin(&Action::BrowserBack)
        .expect("the first attempt should reserve the direction");

    assert!(!dispatch_browser_navigation(
        &Action::BrowserBack,
        ActionDispatchTarget::SafariProcess(417),
        |_, _| false,
        || panic!("stale Safari targets must not fall back"),
    ));
    browser_nav_debounce_cancel(reservation);

    assert!(browser_nav_debounce_begin(&Action::BrowserBack).is_some());
}

#[test]
fn canceling_a_failed_attempt_never_clears_a_newer_reservation() {
    let _guard = BROWSER_NAV_TEST_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let original = Instant::now();
    let newer = original + Duration::from_millis(1);
    *BROWSER_NAV_LAST
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = (Some(newer), None);
    browser_nav_debounce_cancel(BrowserNavDebounceReservation {
        forward: false,
        timestamp: original,
    });

    assert_eq!(
        BROWSER_NAV_LAST
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .0,
        Some(newer)
    );
}
