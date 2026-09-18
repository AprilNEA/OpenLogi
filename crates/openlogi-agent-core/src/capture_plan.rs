//! Per-device capture plans: what each online device's HID++ capture session
//! should divert, plus the device's own binding maps for dispatch.
//!
//! The orchestrator rebuilds the shared plan list from config + inventory for
//! *every* online device (not just the GUI's selection), and the capture
//! watcher diffs it into running sessions. Keeping the binding maps inside the
//! plan is what makes dispatch per-device: an input is resolved against the
//! plan of the session it arrived on, never against a global selected-device
//! map.

use std::collections::BTreeMap;
use std::sync::Arc;

use openlogi_core::binding::{Action, Binding, ButtonId, GestureDirection, default_binding};
use openlogi_core::bindings::{button_bindings_for, hidpp_gesture_maps_for, oshook_gestures_for};
use openlogi_core::config::{Config, ThumbwheelSensitivity};
use openlogi_core::device_order::PhysicalDeviceKey;
use openlogi_hid::DeviceRoute;
use openlogi_hid::reprog_controls::DPI_MODE_SHIFT_CIDS;
use openlogi_hid::session::gesture::{
    CaptureSpec, DIVERTABLE_STANDARD_BUTTONS, GESTURE_SOURCE_BUTTONS,
};
use tokio::sync::watch;

/// Hardware identity of one HID++ capture session.
///
/// Equality is the rearm contract: changing any field requires restoring the
/// old firmware diversion before a replacement session may start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureTarget {
    /// Physical identity used to serialize firmware ownership even when the
    /// config entry carrying this device's settings is adopted or renamed.
    pub physical_key: PhysicalDeviceKey,
    /// HID++ route the session opens.
    pub route: DeviceRoute,
    /// Exact controls and reporting modes the session owns in firmware.
    pub spec: CaptureSpec,
    /// Orchestrator generation bumped after reconnect or system wake, forcing
    /// a rearm even when route and diversion still compare equal.
    pub rearm_generation: u64,
}

/// Action resolution and stateful dispatch configuration for captured input.
///
/// This may be hot-replaced while [`CaptureTarget`] stays armed. The manager
/// cancels input lifecycles admitted under the previous value before using the
/// replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchPlan {
    /// Current config namespace for actions from this physical device. Unlike
    /// [`CaptureTarget::physical_key`], this may change when settings are
    /// adopted and therefore hot-refreshes without touching firmware.
    pub config_key: String,
    /// Per-button immediate or threshold bindings for this device (per-app effective).
    pub bindings: BTreeMap<ButtonId, Binding>,
    /// Per-direction map for each HID++ gesture source (the dedicated gesture
    /// button, the MX Master 4 haptic panel) in gesture mode on this device,
    /// keyed by the button its captured swipes dispatch as; empty when none
    /// gestures.
    pub gesture_bindings: BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    /// macOS Back/Forward gesture maps resolved from device-owned HID++ raw XY.
    /// These remain available while an old diversion is draining.
    pub side_gesture_bindings: BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    /// Hold-to-scroll-horizontally replay map (issue #1053): each eligible
    /// side button mapped to the click action a clean release replays. The
    /// HID++ dispatch path parks these buttons instead of dispatching on
    /// down; empty unless the redirect is armed (see `hscroll_replay_map`).
    pub side_button_hscroll: BTreeMap<ButtonId, Action>,
    /// VID:PID of this device's route when it is direct (`None` for
    /// receiver, raw, and camera routes) — carried into
    /// [`Self::side_button_hscroll`] parks as the attribution anchor the
    /// OS hook compares unattributed wheel events against.
    pub park_source_ids: Option<(u16, u16)>,
    /// This device's effective thumb-wheel sensitivity (device override or the
    /// app-wide default).
    pub thumbwheel_sensitivity: ThumbwheelSensitivity,
}

/// One device's independently versioned hardware target and dispatch plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCapturePlan {
    /// Hardware state whose changes require a capture-session restart.
    pub target: CaptureTarget,
    /// Hot-replaceable action resolution for input from that target.
    pub dispatch: DispatchPlan,
}

/// Read-only, lossless, coalescing view of the latest capture-plan snapshot.
pub type SharedCapturePlans = watch::Receiver<Arc<Vec<DeviceCapturePlan>>>;

/// Back/Forward gesture maps that macOS must own through device-specific HID++
/// capture because Bluetooth-direct CGEvents may carry no sender identity.
#[must_use]
pub(crate) fn hidpp_side_gesture_maps_for(
    config: &Config,
    config_key: &str,
    app: Option<&str>,
) -> BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>> {
    if !cfg!(target_os = "macos") || !config.app_settings.capture_mouse_events {
        return BTreeMap::new();
    }
    oshook_gestures_for(config, Some(config_key), app)
        .into_iter()
        .filter(|(button, _)| matches!(button, ButtonId::Back | ButtonId::Forward))
        .collect()
}

/// Whether a rebound side button kept for the redirect stays OS-visible on
/// `route`: the hook must be able to attribute its presses. macOS
/// Bluetooth-direct senders arrive unattributed and fail the hook's closed
/// source policy — and `Direct` conflates Bluetooth with wired USB, so macOS
/// keeps every direct route diverted. Receiver routes are attributed (OS-hook
/// gesture mode already relies on it there); other platforms have no sender
/// concept, so the carve-out applies on all their routes.
fn hscroll_route_keeps_visible(route: &DeviceRoute) -> bool {
    !cfg!(target_os = "macos") || !matches!(route, DeviceRoute::Direct { .. })
}

/// Hold-to-scroll-horizontally replay map (issue #1053): each eligible side
/// button mapped to the click action a clean release replays.
///
/// Same eligibility the orchestrator resolves into the OS-hook snapshot: the
/// toggle (or its per-app override for `app`) is on, the button is not
/// gesture-owned here or by a macOS HID++ side-gesture session, and its
/// binding replays as one shot (no long-press pair, no held shortcut — their
/// hold-while-pressed semantics cannot replay). An unset binding replays the
/// canonical native click.
///
/// Two consumers, split by visibility: the divert filter below keeps the
/// map's buttons OS-visible only where the hook can attribute them (see
/// `hscroll_route_keeps_visible`) — a diverted button reports via capture
/// instead of HID, so no hook-level hold could open for it. The HID++
/// dispatch path parks the map's buttons wherever they stay diverted
/// instead (notably Bluetooth-direct on macOS): a quick release replays,
/// a hold swallows. One predicate, both consumers.
///
/// Returns empty unless the hook path can actually deliver — the capture
/// opt-out already leaves rebound OS-hook buttons native, so the carve-out
/// only matters while capture is on.
fn hscroll_replay_map(
    config: &Config,
    config_key: &str,
    app: Option<&str>,
    bindings: &BTreeMap<ButtonId, Binding>,
    oshook: &BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    side_gestures: &BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
) -> BTreeMap<ButtonId, Action> {
    if !config.app_settings.capture_mouse_events
        || !config.effective_side_button_horizontal_scroll(config_key, app)
    {
        return BTreeMap::new();
    }
    [ButtonId::Back, ButtonId::Forward]
        .into_iter()
        .filter(|button| {
            !oshook.contains_key(button)
                && !side_gestures.contains_key(button)
                && bindings.get(button).is_none_or(|binding| {
                    !matches!(binding, Binding::LongPress(_))
                        && binding.click_action().held_combo().is_none()
                })
        })
        .map(|button| {
            let replay = bindings
                .get(&button)
                .map_or_else(|| default_binding(button), Binding::click_action);
            (button, replay)
        })
        .collect()
}

/// The park's attribution anchor: a direct route's VID:PID, which the OS
/// hook compares unattributed wheel events against. Receiver routes name
/// the receiver (not the mouse) on their wheel senders, so they carry
/// none; raw and camera routes never produce wheel events.
fn park_source_ids_for(route: &DeviceRoute) -> Option<(u16, u16)> {
    match route {
        DeviceRoute::Direct {
            vendor_id,
            product_id,
        } => Some((*vendor_id, *product_id)),
        _ => None,
    }
}

/// Whether any thumb-wheel control carries a non-default single binding:
/// what arms the wheel's `0x2150` capture alongside the sensitivity check.
fn thumbwheel_bindings_nondefault(bindings: &BTreeMap<ButtonId, Binding>) -> bool {
    [
        ButtonId::Thumbwheel,
        ButtonId::ThumbwheelScrollUp,
        ButtonId::ThumbwheelScrollDown,
    ]
    .iter()
    .any(|button| {
        bindings
            .get(button)
            .is_some_and(|binding| binding.click_action() != default_binding(*button))
    })
}

/// Build one device's plan from the config (per-app effective for `app`).
#[must_use]
pub fn plan_for_device(
    config: &Config,
    physical_key: PhysicalDeviceKey,
    config_key: &str,
    route: DeviceRoute,
    app: Option<&str>,
    rearm_generation: u64,
    os_mouse_hook_available: bool,
) -> DeviceCapturePlan {
    let bindings = button_bindings_for(config, Some(config_key), app);
    // Gesture-mode OS-hook controls normally stay native so the hook sees the
    // press. macOS Back/Forward are the exception below: HID++ owns their
    // button and motion reports because Bluetooth-direct CGEvents may be
    // unattributed.
    let oshook = oshook_gestures_for(config, Some(config_key), app);
    let side_gesture_bindings = hidpp_side_gesture_maps_for(config, config_key, app);
    // One direction map per HID++ source in gesture mode — several may
    // gesture at once, each armed with its own raw-XY divert (the capture
    // target below derives the CIDs to divert from this map's keys).
    let gesture_bindings = hidpp_gesture_maps_for(config, Some(config_key), app);
    let mut divert_gesture_buttons = Vec::new();
    if os_mouse_hook_available {
        divert_gesture_buttons.extend(
            DIVERTABLE_STANDARD_BUTTONS
                .into_iter()
                .filter(|(_, button)| side_gesture_bindings.contains_key(button)),
        );
    }
    // Hold-to-scroll-horizontally: armed buttons park on the HID++ path
    // (see `hscroll_replay_map`), and the divert filter keeps them
    // OS-visible only where the hook can attribute the press
    // (see `hscroll_route_keeps_visible`); both stay empty without a hook.
    let hscroll_replay = if os_mouse_hook_available {
        hscroll_replay_map(
            config,
            config_key,
            app,
            &bindings,
            &oshook,
            &side_gesture_bindings,
        )
    } else {
        BTreeMap::new()
    };
    let hscroll_visible = if hscroll_route_keeps_visible(&route) {
        hscroll_replay.clone()
    } else {
        BTreeMap::new()
    };
    let park_source_ids = park_source_ids_for(&route);
    if gesture_bindings.contains_key(&ButtonId::DpiToggle) {
        divert_gesture_buttons.extend(
            DPI_MODE_SHIFT_CIDS
                .into_iter()
                .map(|cid| (cid, ButtonId::DpiToggle)),
        );
    }
    // The HID++ gesture sources never reach the OS hook, so a non-default
    // single binding on one is deliverable only via a plain HID++ divert — but
    // only while the source is NOT in gesture mode (the raw-XY gesture divert
    // owns a gesturing source's CID).
    let plain_sources = GESTURE_SOURCE_BUTTONS
        .into_iter()
        .filter(|(_, button)| !gesture_bindings.contains_key(button));
    let divert_buttons: Vec<(u16, ButtonId)> = DIVERTABLE_STANDARD_BUTTONS
        .into_iter()
        .chain(plain_sources)
        // These controls are owned by the OS-hook path. The capture opt-out
        // must leave them native even when they carry a non-default binding;
        // HID++-only controls remain independently remappable.
        .filter(|(_, button)| {
            config.app_settings.capture_mouse_events || !button.is_os_hook_button()
        })
        .filter(|(_, button)| !oshook.contains_key(button))
        .filter(|(_, button)| !hscroll_visible.contains_key(button))
        .filter(|(_, button)| {
            bindings.get(button).is_some_and(|binding| {
                if matches!(binding, Binding::LongPress(_)) {
                    return true;
                }
                let action = binding.click_action();
                // The panel's default is ShowActionsRing, which must be
                // diverted to open the ring. Action::None means "leave native
                // firmware haptics alone", so treat None as the only non-divert.
                if *button == ButtonId::HapticPanel {
                    action != Action::None
                } else {
                    action != default_binding(*button)
                }
            })
        })
        .collect();
    let thumbwheel_nondefault = thumbwheel_bindings_nondefault(&bindings);
    let thumbwheel_sensitivity = config.thumbwheel_sensitivity(config_key);
    DeviceCapturePlan {
        target: CaptureTarget {
            physical_key,
            route,
            spec: CaptureSpec {
                capture_thumbwheel: thumbwheel_sensitivity != ThumbwheelSensitivity::DEFAULT
                    || thumbwheel_nondefault,
                divert_gesture_sources: GESTURE_SOURCE_BUTTONS
                    .into_iter()
                    .filter(|(_, button)| gesture_bindings.contains_key(button))
                    .map(|(cid, _)| cid)
                    .collect(),
                divert_gesture_buttons,
                divert_buttons,
            },
            rearm_generation,
        },
        dispatch: DispatchPlan {
            config_key: config_key.to_owned(),
            bindings,
            gesture_bindings,
            side_gesture_bindings,
            side_button_hscroll: hscroll_replay,
            park_source_ids,
            thumbwheel_sensitivity,
        },
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::binding::{Binding, LongPressBinding};
    use openlogi_hid::reprog_controls::{GESTURE_BUTTON_CID, HAPTIC_PANEL_CID};

    use super::*;

    fn route() -> DeviceRoute {
        DeviceRoute::Bolt {
            receiver_uid: "cafe".into(),
            slot: 2,
        }
    }

    fn plan_for_device(
        config: &Config,
        config_key: &str,
        route: DeviceRoute,
        app: Option<&str>,
        rearm_generation: u64,
        os_mouse_hook_available: bool,
    ) -> DeviceCapturePlan {
        super::plan_for_device(
            config,
            PhysicalDeviceKey::parse("receiver:cafe:slot:2")
                .expect("fixture should be a physical key"),
            config_key,
            route,
            app,
            rearm_generation,
            os_mouse_hook_available,
        )
    }

    /// Whether `plan` diverts `button` at all. The plan filters the divert
    /// list per button, so every CID a button maps to is in or out together;
    /// which CIDs those are is the device layer's table, not this crate's
    /// concern.
    fn diverts(plan: &DeviceCapturePlan, button: ButtonId) -> bool {
        plan.target
            .spec
            .divert_buttons
            .iter()
            .any(|&(_, diverted)| diverted == button)
    }

    #[test]
    fn both_hidpp_sources_gesture_when_both_are_in_gesture_mode() {
        // On MX Master 4 the dedicated button and the haptic panel can gesture
        // at the same time: the plan arms a raw-XY divert for each and keeps
        // both out of the plain-divert list.
        let mut cfg = Config::default();
        cfg.set_gesture_mode("2b042", ButtonId::GestureButton, true);
        cfg.set_gesture_mode("2b042", ButtonId::HapticPanel, true);

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0, true);
        assert!(
            plan.dispatch
                .gesture_bindings
                .contains_key(&ButtonId::GestureButton)
                && plan
                    .dispatch
                    .gesture_bindings
                    .contains_key(&ButtonId::HapticPanel),
            "both sources need their own dispatch map, got: {:?}",
            plan.dispatch.gesture_bindings.keys().collect::<Vec<_>>()
        );
        assert!(
            !plan
                .target
                .spec
                .divert_buttons
                .iter()
                .any(|&(cid, _)| cid == GESTURE_BUTTON_CID || cid == HAPTIC_PANEL_CID),
            "a raw-XY-diverted source must never also be plain-diverted"
        );
    }

    #[test]
    fn bound_wheel_tilt_is_diverted_but_an_untouched_one_stays_native() {
        // The main wheel's tilt scrolls horizontally in firmware, so the
        // default binding must leave it native — diverting an untouched tilt
        // would silently kill horizontal scrolling. Binding one side to a real
        // action is what arms its `0x1b04` divert.
        let mut cfg = Config::default();
        cfg.set_binding(
            "2b01a",
            ButtonId::WheelTiltLeft,
            Binding::Single(Action::PrevTab),
        );

        let plan = plan_for_device(&cfg, "2b01a", route(), None, 0, true);
        assert!(
            plan.target
                .spec
                .divert_buttons
                .contains(&(0x005b, ButtonId::WheelTiltLeft)),
            "a bound tilt must be diverted, or the binding can never fire: {:?}",
            plan.target.spec.divert_buttons
        );
        assert!(
            !plan
                .target
                .spec
                .divert_buttons
                .iter()
                .any(|&(_, button)| button == ButtonId::WheelTiltRight),
            "the untouched right tilt must keep its native horizontal scroll"
        );
    }

    #[test]
    fn long_press_is_diverted_even_when_its_short_action_matches_the_native_default() {
        let mut cfg = Config::default();
        cfg.set_binding(
            "2b01a",
            ButtonId::Back,
            Binding::LongPress(LongPressBinding::new(
                default_binding(ButtonId::Back),
                Action::MissionControl,
            )),
        );

        let plan = plan_for_device(&cfg, "2b01a", route(), None, 0, true);
        assert!(
            plan.target
                .spec
                .divert_buttons
                .iter()
                .any(|&(_, button)| button == ButtonId::Back),
            "the runtime needs both edges even when the short action is native"
        );
    }

    #[test]
    fn thumb_button_capture_distinguishes_native_and_browser_actions() {
        for (button, native, browser) in [
            (ButtonId::Back, Action::MouseBack, Action::BrowserBack),
            (
                ButtonId::Forward,
                Action::MouseForward,
                Action::BrowserForward,
            ),
        ] {
            for (stored, expected_action, diverted) in [
                (None, native.clone(), false),
                (Some(native.clone()), native, false),
                (Some(browser.clone()), browser, true),
            ] {
                let mut cfg = Config::default();
                if let Some(action) = stored {
                    cfg.set_binding("2b023", button, Binding::Single(action));
                }

                let plan = plan_for_device(&cfg, "2b023", route(), None, 0, true);
                assert_eq!(
                    plan.dispatch.bindings.get(&button),
                    Some(&Binding::Single(expected_action)),
                    "{button:?} must resolve unset bindings to native clicks"
                );
                for side in [ButtonId::Back, ButtonId::Forward] {
                    assert_eq!(
                        diverts(&plan, side),
                        diverted && side == button,
                        "only an explicitly browser-bound {button:?} should be diverted"
                    );
                }
            }
        }
    }

    #[test]
    fn thumb_button_capture_follows_per_app_overrides_and_inheritance() {
        for (button, native, browser) in [
            (ButtonId::Back, Action::MouseBack, Action::BrowserBack),
            (
                ButtonId::Forward,
                Action::MouseForward,
                Action::BrowserForward,
            ),
        ] {
            for (global, overridden, global_diverted) in [
                (native.clone(), browser.clone(), false),
                (browser, native, true),
            ] {
                let mut cfg = Config::default();
                cfg.set_binding("2b023", button, Binding::Single(global.clone()));
                cfg.set_per_app_binding(
                    "2b023",
                    "com.apple.Safari",
                    button,
                    Some(overridden.clone()),
                );

                for (app, expected_action, diverted) in [
                    (None, &global, global_diverted),
                    (Some("com.apple.Safari"), &overridden, !global_diverted),
                    (Some("com.example.Other"), &global, global_diverted),
                ] {
                    let plan = plan_for_device(&cfg, "2b023", route(), app, 0, true);
                    assert_eq!(
                        plan.dispatch.bindings.get(&button),
                        Some(&Binding::Single(expected_action.clone())),
                        "{button:?} dispatch must resolve the profile for {app:?}"
                    );
                    assert_eq!(
                        diverts(&plan, button),
                        diverted,
                        "{button:?} capture must follow its effective binding for {app:?}"
                    );
                }

                cfg.set_per_app_binding("2b023", "com.apple.Safari", button, None);
                let inherited =
                    plan_for_device(&cfg, "2b023", route(), Some("com.apple.Safari"), 0, true);
                assert_eq!(
                    inherited.dispatch.bindings.get(&button),
                    Some(&Binding::Single(global))
                );
                assert_eq!(
                    diverts(&inherited, button),
                    global_diverted,
                    "clearing {button:?}'s app override must restore global capture"
                );
            }
        }
    }

    #[test]
    fn haptic_panel_gestures_when_promoted() {
        // The MX Master 4 haptic panel is a HID++ gesture source: promoting it
        // into gesture mode must arm the raw-XY gesture divert, exactly like
        // the dedicated gesture button.
        let mut cfg = Config::default();
        cfg.set_gesture_mode("2b042", ButtonId::HapticPanel, true);

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0, true);
        assert!(
            plan.dispatch
                .gesture_bindings
                .contains_key(&ButtonId::HapticPanel),
            "a gesture-mode panel must arm the HID++ gesture divert"
        );
        assert!(
            !plan
                .target
                .spec
                .divert_buttons
                .iter()
                .any(|&(cid, _)| cid == HAPTIC_PANEL_CID),
            "a gesture-mode source is delivered via raw-XY divert, never a plain one"
        );
    }

    #[test]
    fn single_bound_haptic_panel_is_plain_diverted_when_not_in_gesture_mode() {
        // While only the dedicated button gestures (the default), a single
        // action bound to the panel is deliverable only via a plain HID++
        // divert dispatching ButtonId::HapticPanel.
        let mut cfg = Config::default();
        cfg.set_binding(
            "2b042",
            ButtonId::HapticPanel,
            Binding::Single(Action::Copy),
        );

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0, true);
        assert!(
            plan.target
                .spec
                .divert_buttons
                .contains(&(HAPTIC_PANEL_CID, ButtonId::HapticPanel)),
            "a single-bound panel must be plain-diverted, or the binding can never fire"
        );
    }

    #[test]
    fn haptic_panel_default_is_diverted_for_actions_ring() {
        // Default binding is ShowActionsRing — the panel has no native OS path
        // and must be HID++-diverted so the ring can open.
        let plan = plan_for_device(&Config::default(), "2b042", route(), None, 0, true);

        assert!(
            plan.target
                .spec
                .divert_buttons
                .contains(&(HAPTIC_PANEL_CID, ButtonId::HapticPanel)),
            "the panel's default Actions Ring binding must be HID++-diverted"
        );
    }

    #[test]
    fn explicit_none_haptic_panel_stays_native() {
        // Action::None means leave firmware haptics alone — do not divert.
        let mut cfg = Config::default();
        cfg.set_binding(
            "2b042",
            ButtonId::HapticPanel,
            Binding::Single(Action::None),
        );

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0, true);
        assert!(
            !plan
                .target
                .spec
                .divert_buttons
                .iter()
                .any(|&(cid, _)| cid == HAPTIC_PANEL_CID),
            "an explicitly unbound panel must keep its native behavior"
        );
    }

    #[test]
    fn gestures_off_single_bound_gesture_button_is_plain_diverted() {
        // The dedicated gesture button (CID 0x00c3) never reaches the OS hook,
        // so with gestures off a non-default single binding on it is only
        // deliverable via a plain HID++ divert.
        let mut cfg = Config::default();
        cfg.set_binding(
            "2b042",
            ButtonId::GestureButton,
            Binding::Single(Action::CycleDpiPresets),
        );

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0, true);
        assert!(
            plan.dispatch.gesture_bindings.is_empty(),
            "gestures are off — no raw-XY gesture divert"
        );
        assert!(
            plan.target
                .spec
                .divert_buttons
                .contains(&(GESTURE_BUTTON_CID, ButtonId::GestureButton)),
            "a single-bound gesture button must be plain-diverted, or the binding can never fire"
        );
    }

    #[test]
    fn gesture_mode_button_is_never_plain_diverted() {
        // While the gesture button is in gesture mode, the raw-XY gesture
        // divert owns CID 0x00c3 — a plain divert on top would strip raw-XY.
        // (Its default Click projects to a non-default single action, so only
        // the gesture-mode rule keeps it out of the plain list.)
        let mut cfg = Config::default();
        cfg.set_gesture_mode("2b042", ButtonId::GestureButton, true);

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0, true);
        assert!(
            !plan.dispatch.gesture_bindings.is_empty(),
            "the gesture button owns the gesture role"
        );
        assert!(
            !plan
                .target
                .spec
                .divert_buttons
                .iter()
                .any(|&(cid, _)| cid == GESTURE_BUTTON_CID),
            "the gesture owner is delivered via raw-XY divert, never a plain one"
        );
    }

    #[test]
    fn gestures_off_default_gesture_button_stays_native() {
        // With gestures off and no explicit binding, the gesture button keeps
        // its native HID behavior — same contract as the standard buttons.
        let mut cfg = Config::default();
        cfg.set_gesture_mode("2b042", ButtonId::GestureButton, false);

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0, true);
        assert!(
            !plan
                .target
                .spec
                .divert_buttons
                .iter()
                .any(|&(cid, _)| cid == GESTURE_BUTTON_CID),
            "an unbound gesture button must not be captured"
        );
    }

    #[test]
    fn macos_side_gestures_request_hidpp_raw_xy_capture() {
        let mut cfg = Config::default();
        cfg.set_gesture_mode("2b042", ButtonId::Back, true);
        cfg.set_gesture_mode("2b042", ButtonId::Forward, true);
        cfg.set_gesture_mode("2b042", ButtonId::MiddleClick, true);

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0, true);
        if cfg!(target_os = "macos") {
            assert_eq!(
                plan.dispatch
                    .side_gesture_bindings
                    .keys()
                    .copied()
                    .collect::<Vec<_>>(),
                vec![ButtonId::Back, ButtonId::Forward],
                "only the senderless side buttons use device-owned gesture dispatch"
            );
            let expected: Vec<_> = DIVERTABLE_STANDARD_BUTTONS
                .into_iter()
                .filter(|(_, button)| matches!(button, ButtonId::Back | ButtonId::Forward))
                .collect();
            assert_eq!(
                plan.target.spec.divert_gesture_buttons, expected,
                "every known Back/Forward CID must be requested as a HID++ raw-XY gesture source"
            );
            assert!(
                !plan
                    .target
                    .spec
                    .divert_buttons
                    .iter()
                    .any(|&(_, button)| matches!(button, ButtonId::Back | ButtonId::Forward)),
                "a side-button gesture hold must not also be a plain divert"
            );
            assert!(
                !plan
                    .target
                    .spec
                    .divert_gesture_buttons
                    .iter()
                    .any(|&(_, button)| button == ButtonId::MiddleClick)
            );
        } else {
            assert!(plan.dispatch.side_gesture_bindings.is_empty());
            assert!(plan.target.spec.divert_gesture_buttons.is_empty());
        }
    }

    #[test]
    fn dpi_gesture_requests_every_modeshift_cid_without_the_os_hook() {
        let mut cfg = Config::default();
        cfg.set_gesture_mode("2b042", ButtonId::DpiToggle, true);

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0, false);
        assert!(
            plan.dispatch
                .gesture_bindings
                .contains_key(&ButtonId::DpiToggle)
        );
        assert_eq!(
            plan.target.spec.divert_gesture_buttons,
            DPI_MODE_SHIFT_CIDS
                .into_iter()
                .map(|cid| (cid, ButtonId::DpiToggle))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn dpi_gesture_capture_follows_the_app_override() {
        let mut cfg = Config::default();
        cfg.set_gesture_mode("2b042", ButtonId::DpiToggle, true);
        for action in [Action::Paste, Action::None] {
            cfg.set_per_app_binding(
                "2b042",
                "com.example.Editor",
                ButtonId::DpiToggle,
                Some(action.clone()),
            );
            for app in [None, Some("com.example.Editor"), Some("com.example.Other")] {
                let overridden = app == Some("com.example.Editor");
                let plan = plan_for_device(&cfg, "2b042", route(), app, 0, false);
                assert_eq!(
                    plan.dispatch
                        .gesture_bindings
                        .contains_key(&ButtonId::DpiToggle),
                    !overridden
                );
                assert_eq!(
                    plan.target
                        .spec
                        .divert_gesture_buttons
                        .iter()
                        .any(|&(_, button)| button == ButtonId::DpiToggle),
                    !overridden
                );
                if overridden {
                    assert_eq!(
                        plan.dispatch.bindings.get(&ButtonId::DpiToggle),
                        Some(&Binding::Single(action.clone()))
                    );
                }
            }
        }
        cfg.set_per_app_binding("2b042", "com.example.Editor", ButtonId::DpiToggle, None);
        let restored =
            plan_for_device(&cfg, "2b042", route(), Some("com.example.Editor"), 0, false);
        assert!(
            restored
                .dispatch
                .gesture_bindings
                .contains_key(&ButtonId::DpiToggle)
        );
        assert_eq!(
            restored.target.spec.divert_gesture_buttons.len(),
            DPI_MODE_SHIFT_CIDS.len()
        );
    }

    #[test]
    fn mouse_capture_opt_out_keeps_side_gesture_buttons_native() {
        let mut cfg = Config::default();
        cfg.app_settings.capture_mouse_events = false;
        cfg.set_gesture_mode("2b042", ButtonId::Forward, true);

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0, true);
        assert!(plan.dispatch.side_gesture_bindings.is_empty());
        assert!(plan.target.spec.divert_gesture_buttons.is_empty());
        assert!(
            !plan
                .target
                .spec
                .divert_buttons
                .iter()
                .any(|&(_, button)| button == ButtonId::Forward),
            "capture opt-out must leave Forward entirely native"
        );
    }

    #[test]
    fn mouse_capture_opt_out_keeps_single_os_hook_buttons_native() {
        let mut cfg = Config::default();
        cfg.app_settings.capture_mouse_events = false;
        cfg.set_binding("2b042", ButtonId::Forward, Binding::Single(Action::Copy));
        cfg.set_binding(
            "2b042",
            ButtonId::MiddleClick,
            Binding::Single(Action::Paste),
        );
        cfg.set_gesture_mode("2b042", ButtonId::GestureButton, false);
        cfg.set_binding(
            "2b042",
            ButtonId::GestureButton,
            Binding::Single(Action::Undo),
        );

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0, true);
        assert!(
            !plan
                .target
                .spec
                .divert_buttons
                .iter()
                .any(|&(_, button)| button.is_os_hook_button()),
            "capture opt-out must leave all OS-hook buttons native"
        );
        assert!(
            plan.target
                .spec
                .divert_buttons
                .iter()
                .any(|&(_, button)| button == ButtonId::GestureButton),
            "HID++-only controls must remain remappable without the OS hook"
        );
    }

    #[test]
    fn armed_hscroll_keeps_rebound_side_buttons_os_visible() {
        // The redirect needs the OS hook to see the press: a diverted button
        // reports via HID++ capture instead, so no hook-level hold could open.
        // While armed, rebound side buttons therefore stay OS-visible and
        // remap through the hook path.
        let mut cfg = Config::default();
        cfg.set_side_button_horizontal_scroll("2b023", true);
        cfg.set_binding(
            "2b023",
            ButtonId::Back,
            Binding::Single(Action::PreviousDesktop),
        );
        cfg.set_binding(
            "2b023",
            ButtonId::Forward,
            Binding::Single(Action::NextDesktop),
        );

        let plan = plan_for_device(&cfg, "2b023", route(), None, 0, true);
        for button in [ButtonId::Back, ButtonId::Forward] {
            assert!(
                !diverts(&plan, button),
                "{button:?} must stay OS-visible while the redirect is armed"
            );
        }
        // The HID++ dispatch path parks the same buttons when they stay
        // diverted (notably Bluetooth-direct, whose senderless events fail
        // the hook's source policy): a quick release replays the bound
        // action, a hold swallows it.
        assert_eq!(
            plan.dispatch.side_button_hscroll,
            BTreeMap::from([
                (ButtonId::Back, Action::PreviousDesktop),
                (ButtonId::Forward, Action::NextDesktop),
            ]),
            "the dispatch replay map must mirror the carve-out"
        );

        // Same bindings, redirect off: the classic HID++ divert applies.
        let mut off = Config::default();
        off.set_binding(
            "2b023",
            ButtonId::Back,
            Binding::Single(Action::PreviousDesktop),
        );
        off.set_binding(
            "2b023",
            ButtonId::Forward,
            Binding::Single(Action::NextDesktop),
        );
        let plan = plan_for_device(&off, "2b023", route(), None, 0, true);
        for button in [ButtonId::Back, ButtonId::Forward] {
            assert!(
                diverts(&plan, button),
                "{button:?} must divert while the redirect is off"
            );
        }
    }

    #[test]
    fn armed_hscroll_needs_an_available_hook_to_keep_buttons_visible() {
        // Without a hook the OS-visible carve-out could never redirect, so
        // the buttons stay diverted and remap the way they always have.
        let mut cfg = Config::default();
        cfg.set_side_button_horizontal_scroll("2b023", true);
        cfg.set_binding(
            "2b023",
            ButtonId::Back,
            Binding::Single(Action::PreviousDesktop),
        );

        let plan = plan_for_device(&cfg, "2b023", route(), None, 0, false);
        assert!(
            diverts(&plan, ButtonId::Back),
            "no hook — the rebound button must stay diverted"
        );
    }

    #[test]
    fn direct_routes_keep_diversion_where_senders_are_unattributed() {
        // macOS Bluetooth-direct senders arrive unattributed and fail the
        // hook's closed source policy; `Direct` conflates Bluetooth with
        // wired USB, so macOS keeps every direct route diverted. The dispatch
        // replay map still parks: the remap survives through tap-replay and
        // hold-swallow wherever diversion remains.
        let direct = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb034,
        };
        let mut cfg = Config::default();
        cfg.set_side_button_horizontal_scroll("2b023", true);
        cfg.set_binding(
            "2b023",
            ButtonId::Back,
            Binding::Single(Action::PreviousDesktop),
        );

        let plan = plan_for_device(&cfg, "2b023", direct, None, 0, true);
        assert_eq!(
            diverts(&plan, ButtonId::Back),
            cfg!(target_os = "macos"),
            "only macOS keeps direct routes diverted"
        );
        assert_eq!(
            plan.dispatch.side_button_hscroll.get(&ButtonId::Back),
            Some(&Action::PreviousDesktop),
            "the park must apply wherever diversion remains"
        );
    }

    #[test]
    fn armed_hscroll_follows_the_per_app_override_in_capture() {
        let mut cfg = Config::default();
        cfg.set_side_button_horizontal_scroll("2b023", true);
        cfg.set_binding(
            "2b023",
            ButtonId::Back,
            Binding::Single(Action::PreviousDesktop),
        );
        cfg.set_per_app_side_button_hscroll("2b023", "com.example.Editor", Some(false));

        let disarmed = plan_for_device(&cfg, "2b023", route(), Some("com.example.Editor"), 0, true);
        assert!(
            diverts(&disarmed, ButtonId::Back),
            "per-app override off must restore the divert for that app"
        );
        let armed = plan_for_device(&cfg, "2b023", route(), Some("com.example.Other"), 0, true);
        assert!(
            !diverts(&armed, ButtonId::Back),
            "other apps inherit the armed device default"
        );
    }

    #[test]
    fn armed_hscroll_yields_gesture_and_hold_semantics_buttons_to_capture() {
        // Gesture-mode and long-press/held-shortcut buttons can never replay
        // as one shot, so they stay on their existing capture paths even
        // while armed.
        let mut cfg = Config::default();
        cfg.set_side_button_horizontal_scroll("2b023", true);
        cfg.set_gesture_mode("2b023", ButtonId::Back, true);
        cfg.set_binding(
            "2b023",
            ButtonId::Forward,
            Binding::LongPress(LongPressBinding::new(
                default_binding(ButtonId::Forward),
                Action::MissionControl,
            )),
        );

        let plan = plan_for_device(&cfg, "2b023", route(), None, 0, true);
        assert!(
            plan.target
                .spec
                .divert_buttons
                .iter()
                .any(|&(_, button)| button == ButtonId::Forward),
            "a long-press button keeps its divert: the hook could never replay it"
        );
        if cfg!(target_os = "macos") {
            assert!(
                plan.target
                    .spec
                    .divert_gesture_buttons
                    .iter()
                    .any(|&(_, button)| button == ButtonId::Back),
                "a gesture-mode side button keeps its raw-XY divert on macOS"
            );
        }
    }

    #[test]
    fn unavailable_mouse_hook_keeps_side_gesture_buttons_native() {
        let mut cfg = Config::default();
        cfg.set_gesture_mode("2b042", ButtonId::Forward, true);

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0, false);
        assert!(plan.target.spec.divert_gesture_buttons.is_empty());
        if cfg!(target_os = "macos") {
            assert!(
                plan.dispatch
                    .side_gesture_bindings
                    .contains_key(&ButtonId::Forward),
                "a draining session must retain its dispatch map until disarm completes"
            );
        } else {
            assert!(plan.dispatch.side_gesture_bindings.is_empty());
        }
    }
}
