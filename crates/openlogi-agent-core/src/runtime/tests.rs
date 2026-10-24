//! Regression tests for action dispatch and lifecycle-owned output.

mod mouse_hold;
mod output;

use super::*;
use output::{OutputEvent, RecordingOutput};

static BROWSER_NAV_TEST_LOCK: Mutex<()> = Mutex::new(());

fn executor_with(pointer_speeds: Arc<PointerSpeeds>) -> ActionExecutor {
    let (_signal, device_io) = openlogi_hid::device_io_channel();
    let (action_ring, _ring_rx) = tokio::sync::mpsc::unbounded_channel();
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
        action_ring,
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
    let (action_ring, mut ring_rx) = tokio::sync::mpsc::unbounded_channel();
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
        action_ring,
    };

    executor.dispatch_to(
        &Action::ShowActionsRing,
        Some("mouse"),
        ActionDispatchTarget::for_pointer(Some(openlogi_hook::PointerTarget::Unavailable), || None),
    );

    assert_eq!(ring_rx.try_recv(), Ok(Some("mouse".to_owned())));
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
