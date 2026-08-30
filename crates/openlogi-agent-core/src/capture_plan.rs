//! Per-device capture plans: what each online device's HID++ capture session
//! should divert, plus the device's own binding maps for dispatch.
//!
//! The orchestrator rebuilds the shared plan list from config + inventory for
//! *every* online device (not just the GUI's selection), and the capture
//! watcher diffs it into running sessions. Keeping the binding maps inside the
//! plan is what makes dispatch per-device: an input is resolved against the
//! plan of the session it arrived on, never against a global selected-device
//! map.

mod hold;

#[cfg(test)]
pub(crate) use hold::FALLBACK_HOLD_SENSOR_DPI;

use std::collections::BTreeMap;
use std::sync::Arc;

use openlogi_core::binding::{Action, Binding, ButtonId, GestureDirection, default_binding};
use openlogi_core::bindings::{
    button_bindings_for, hidpp_gesture_maps_for, hold_mode_bindings_for, oshook_gestures_for,
};
use openlogi_core::config::{Config, ThumbwheelSensitivity, ZoomSensitivity};
use openlogi_core::device_order::PhysicalDeviceKey;
use openlogi_core::hid::Dpi;
use openlogi_hid::DeviceRoute;
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
    /// This device's effective thumb-wheel sensitivity (device override or the
    /// app-wide default).
    pub thumbwheel_sensitivity: ThumbwheelSensitivity,
    /// Hold-mode (`Pan` / `Zoom`) button bindings. The OS-hook map must omit
    /// these keys so HID++ is the only dispatch path.
    pub hold_bindings: BTreeMap<ButtonId, Action>,
    /// Live sensor DPI, for converting raw counts to millimetres. Unlike
    /// [`CaptureSpec::sensor_dpi`] this tracks the real reading, so the felt
    /// speed of a pan is right on a device whose sensor is not at the
    /// fallback.
    pub sensor_dpi: Option<Dpi>,
    /// App-wide hold-mode zoom responsiveness.
    pub zoom_sensitivity: ZoomSensitivity,
    /// App-wide hold-mode pan direction. `false` is content-follows-hand.
    pub invert_pan: bool,
}

/// Host facts that decide whether hold-mode raw-XY may be armed.
///
/// [`plan_for_device`] fail-closes on injection only: it stays unavailable
/// until the orchestrator calls [`plan_for_device_with`]. A missing DPI no
/// longer disables hold-mode — see [`hold::FALLBACK_HOLD_SENSOR_DPI`].
#[derive(Clone, Copy, Debug)]
pub struct CaptureHostAbility {
    /// Whether the OS movement hook is currently usable.
    pub os_mouse_hook_available: bool,
    /// Whether synthesised events can be delivered (macOS Accessibility).
    /// Arming a raw-XY divert without this freezes the cursor for a gesture
    /// that can never happen.
    pub injection_available: bool,
    /// Live sensor DPI. Falls back to the committed config DPI, then to a
    /// named factory default, so a missing reading never disables hold-mode.
    pub sensor_dpi: Option<Dpi>,
}

impl CaptureHostAbility {
    /// Hook availability only — hold-mode stays unarmed.
    #[must_use]
    pub const fn hook_only(os_mouse_hook_available: bool) -> Self {
        Self {
            os_mouse_hook_available,
            injection_available: false,
            sensor_dpi: None,
        }
    }
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
    plan_for_device_with(
        config,
        physical_key,
        config_key,
        route,
        app,
        rearm_generation,
        CaptureHostAbility::hook_only(os_mouse_hook_available),
    )
}

/// Whether any thumb-wheel control carries a non-default binding. That alone
/// is reason to capture the wheel, independent of its sensitivity.
fn thumbwheel_bindings_customized(bindings: &BTreeMap<ButtonId, Binding>) -> bool {
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

/// Build one device's plan with explicit injection and DPI facts.
#[must_use]
pub fn plan_for_device_with(
    config: &Config,
    physical_key: PhysicalDeviceKey,
    config_key: &str,
    route: DeviceRoute,
    app: Option<&str>,
    rearm_generation: u64,
    ability: CaptureHostAbility,
) -> DeviceCapturePlan {
    let os_mouse_hook_available = ability.os_mouse_hook_available;
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
    let gesture_bindings = hidpp_gesture_maps_for(config, Some(config_key));
    let hold_bindings = hold_mode_bindings_for(config, Some(config_key), app);
    let resolved_dpi = hold::resolve_hold_sensor_dpi(ability.sensor_dpi, config.dpi(config_key));
    hold::warn_if_hold_dpi_is_approximate(
        config_key,
        !hold_bindings.is_empty(),
        ability.injection_available,
        &resolved_dpi,
    );
    let sensor_dpi = Some(resolved_dpi.dpi);
    // The armed spec carries the fallback only, never a live sensor reading.
    // `CaptureSpec` is part of `CaptureTarget`'s identity, so folding the live
    // value in would retire and re-arm the session the moment the first
    // `getSensorDpi` lands, seconds after connect, tearing down any hold the
    // user had already started. The device layer prefers the process-wide
    // sensor cache at press time anyway, so this value only ever matters on a
    // device whose DPI can never be read.
    let armed_dpi = Some(hold::resolve_hold_sensor_dpi(None, config.dpi(config_key)).dpi);
    let divert_hold_buttons = hold::raw_xy_hold_diverts(
        &hold_bindings,
        &gesture_bindings,
        &oshook,
        ability.injection_available,
    );
    let divert_gesture_buttons = if os_mouse_hook_available {
        DIVERTABLE_STANDARD_BUTTONS
            .into_iter()
            .filter(|(_, button)| side_gesture_bindings.contains_key(button))
            .filter(|(_, button)| !hold_bindings.contains_key(button))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
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
        .filter(|(_, button)| {
            // Raw-XY hold owns these CIDs when injection can deliver. When it
            // cannot, keep them on the plain-divert list so the button still
            // runs its binding as a click instead of its native action.
            !hold_bindings.contains_key(button) || !ability.injection_available
        })
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
    let thumbwheel_bindings_nondefault = thumbwheel_bindings_customized(&bindings);
    let thumbwheel_sensitivity = config.thumbwheel_sensitivity(config_key);
    DeviceCapturePlan {
        target: CaptureTarget {
            physical_key,
            route,
            spec: CaptureSpec {
                capture_thumbwheel: thumbwheel_sensitivity != ThumbwheelSensitivity::DEFAULT
                    || thumbwheel_bindings_nondefault,
                divert_gesture_sources: GESTURE_SOURCE_BUTTONS
                    .into_iter()
                    .filter(|(_, button)| gesture_bindings.contains_key(button))
                    .map(|(cid, _)| cid)
                    .collect(),
                divert_gesture_buttons,
                divert_hold_buttons,
                divert_buttons,
                sensor_dpi: armed_dpi,
                hold_requested: hold_bindings.len(),
                injection_available: ability.injection_available,
            },
            rearm_generation,
        },
        dispatch: DispatchPlan {
            config_key: config_key.to_owned(),
            bindings,
            gesture_bindings,
            side_gesture_bindings,
            thumbwheel_sensitivity,
            hold_bindings,
            sensor_dpi,
            zoom_sensitivity: config.app_settings.zoom_sensitivity,
            invert_pan: config.app_settings.invert_pan,
        },
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::binding::{Binding, LongPressBinding};
    use openlogi_core::hid::Dpi;
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
    fn macos_side_gesture_requests_hidpp_raw_xy_capture() {
        let mut cfg = Config::default();
        cfg.set_gesture_mode("2b042", ButtonId::Forward, true);

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0, true);
        if cfg!(target_os = "macos") {
            assert!(
                plan.dispatch
                    .side_gesture_bindings
                    .contains_key(&ButtonId::Forward)
            );
            assert!(
                plan.target
                    .spec
                    .divert_gesture_buttons
                    .contains(&(0x0056, ButtonId::Forward)),
                "Forward must be requested as a HID++ raw-XY gesture source"
            );
            assert!(
                !plan
                    .target
                    .spec
                    .divert_buttons
                    .iter()
                    .any(|&(_, button)| button == ButtonId::Forward),
                "a gesture hold must not also be a plain divert"
            );
        } else {
            assert!(plan.dispatch.side_gesture_bindings.is_empty());
            assert!(plan.target.spec.divert_gesture_buttons.is_empty());
        }
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

    fn plan_with(
        config: &Config,
        config_key: &str,
        ability: CaptureHostAbility,
    ) -> DeviceCapturePlan {
        super::plan_for_device_with(
            config,
            PhysicalDeviceKey::parse("receiver:cafe:slot:2")
                .expect("fixture should be a physical key"),
            config_key,
            route(),
            None,
            0,
            ability,
        )
    }

    fn hold_ready(dpi: Dpi) -> CaptureHostAbility {
        CaptureHostAbility {
            os_mouse_hook_available: true,
            injection_available: true,
            sensor_dpi: Some(dpi),
        }
    }

    #[test]
    fn hold_mode_button_is_raw_xy_diverted_and_not_plain_or_os_hook() {
        let mut cfg = Config::default();
        cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Pan));

        let plan = plan_with(&cfg, "2b042", hold_ready(Dpi::new(1000)));
        assert_eq!(
            plan.dispatch.hold_bindings.get(&ButtonId::Back),
            Some(&Action::Pan)
        );
        assert!(
            plan.target
                .spec
                .divert_hold_buttons
                .iter()
                .any(|&(_, button)| button == ButtonId::Back),
            "Pan must be a raw-XY hold divert: {:?}",
            plan.target.spec.divert_hold_buttons
        );
        assert!(
            !plan
                .target
                .spec
                .divert_buttons
                .iter()
                .any(|&(_, button)| button == ButtonId::Back),
            "a hold-mode button must not also be a plain divert"
        );
        assert!(
            !plan
                .target
                .spec
                .divert_gesture_buttons
                .iter()
                .any(|&(_, button)| button == ButtonId::Back),
            "a hold-mode button must not stay on the swipe-gesture divert list"
        );
    }

    #[test]
    fn hold_mode_is_not_armed_without_injection() {
        let mut cfg = Config::default();
        cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Zoom));

        let plan = plan_with(
            &cfg,
            "2b042",
            CaptureHostAbility {
                os_mouse_hook_available: true,
                injection_available: false,
                sensor_dpi: Some(Dpi::new(1000)),
            },
        );
        assert!(
            plan.dispatch.hold_bindings.contains_key(&ButtonId::Back),
            "dispatch still names the hold so the hook map can strip it"
        );
        assert!(
            plan.target.spec.divert_hold_buttons.is_empty(),
            "arming without injection would freeze the cursor for a dropped gesture"
        );
    }

    #[test]
    fn hold_mode_arms_on_a_fallback_dpi_when_the_sensor_read_is_missing() {
        let mut cfg = Config::default();
        cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Pan));

        let plan = plan_with(
            &cfg,
            "2b042",
            CaptureHostAbility {
                os_mouse_hook_available: true,
                injection_available: true,
                sensor_dpi: None,
            },
        );
        assert_eq!(
            plan.target.spec.sensor_dpi,
            Some(hold::FALLBACK_HOLD_SENSOR_DPI)
        );
        assert!(
            plan.target
                .spec
                .divert_hold_buttons
                .iter()
                .any(|&(_, button)| button == ButtonId::Back),
            "a missing DPI must not disable the feature"
        );
    }

    #[test]
    fn hold_mode_scale_dpi_is_the_live_sensor_reading_not_a_constant() {
        let mut cfg = Config::default();
        cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Pan));
        cfg.set_dpi("2b042", Dpi::new(400));

        let low = plan_with(&cfg, "2b042", hold_ready(Dpi::new(400)));
        let high = plan_with(&cfg, "2b042", hold_ready(Dpi::new(1600)));
        assert_eq!(low.dispatch.sensor_dpi, Some(Dpi::new(400)));
        assert_eq!(high.dispatch.sensor_dpi, Some(Dpi::new(1600)));
        assert_ne!(
            low.dispatch.sensor_dpi, high.dispatch.sensor_dpi,
            "if this were the DPI-blind path both plans would carry the same scale"
        );
    }

    #[test]
    fn a_live_dpi_reading_never_changes_the_armed_capture_target() {
        // `CaptureSpec` is part of `CaptureTarget`'s identity. Folding the
        // live reading in retired and re-armed the session the moment the
        // first `getSensorDpi` landed, which tore down a hold the user had
        // already started and, on a re-armed accumulator, turned the eventual
        // release into a fresh click.
        let mut cfg = Config::default();
        cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Pan));

        let unread = plan_with(
            &cfg,
            "2b042",
            CaptureHostAbility {
                os_mouse_hook_available: true,
                injection_available: true,
                sensor_dpi: None,
            },
        );
        let read = plan_with(&cfg, "2b042", hold_ready(Dpi::new(950)));

        assert_eq!(
            unread.target, read.target,
            "a completed sensor read must not cycle the firmware diverts"
        );
        assert_ne!(
            unread.dispatch.sensor_dpi, read.dispatch.sensor_dpi,
            "the reading still has to reach the millimetre conversion"
        );
    }

    #[test]
    fn gesture_mode_takes_precedence_over_a_hold_mode_single() {
        let mut cfg = Config::default();
        cfg.set_gesture_mode("2b042", ButtonId::GestureButton, true);
        cfg.set_per_app_binding(
            "2b042",
            "com.apple.Safari",
            ButtonId::GestureButton,
            Some(Action::Pan),
        );

        let plan = super::plan_for_device_with(
            &cfg,
            PhysicalDeviceKey::parse("receiver:cafe:slot:2")
                .expect("fixture should be a physical key"),
            "2b042",
            route(),
            Some("com.apple.Safari"),
            0,
            hold_ready(Dpi::new(1000)),
        );
        assert_eq!(
            plan.dispatch.hold_bindings.get(&ButtonId::GestureButton),
            Some(&Action::Pan),
            "the per-app overlay is a hold-mode Single"
        );
        assert!(
            plan.dispatch
                .gesture_bindings
                .contains_key(&ButtonId::GestureButton),
            "device-level gesture mode still owns the HID++ source"
        );
        assert!(
            !plan
                .target
                .spec
                .divert_hold_buttons
                .iter()
                .any(|&(_, button)| button == ButtonId::GestureButton),
            "a gesturing HID++ source must keep the swipe divert, not a hold-mode stream"
        );
    }

    #[test]
    fn hold_mode_uses_committed_config_dpi_when_the_sensor_is_unread() {
        let mut cfg = Config::default();
        cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Pan));
        cfg.set_dpi("2b042", Dpi::new(800));

        let plan = plan_with(
            &cfg,
            "2b042",
            CaptureHostAbility {
                os_mouse_hook_available: true,
                injection_available: true,
                sensor_dpi: None,
            },
        );
        assert_eq!(plan.target.spec.sensor_dpi, Some(Dpi::new(800)));
        assert!(
            plan.target
                .spec
                .divert_hold_buttons
                .iter()
                .any(|&(_, button)| button == ButtonId::Back),
            "config DPI must be enough to arm; a missing live reading is not a count-blind fallback"
        );
    }

    #[test]
    fn plan_for_device_fail_closes_hold_mode_until_the_host_opts_in() {
        let mut cfg = Config::default();
        cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Pan));
        cfg.set_dpi("2b042", Dpi::new(1000));

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0, true);
        assert!(
            plan.target.spec.divert_hold_buttons.is_empty(),
            "the compatibility entry point must not arm hold-mode without injection"
        );
    }
}
