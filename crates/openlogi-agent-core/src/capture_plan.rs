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
use openlogi_hid::session::gesture::{
    CaptureSpec, DIVERTABLE_STANDARD_BUTTONS, GESTURE_SOURCE_BUTTONS, spy_buttons_for_model,
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

/// Whether `button`'s stored binding leaves its default — the general-case
/// half of the `0x1b04` divert-list predicate below (everything except its
/// `HapticPanel` special case, which does not apply to `0x8110` spy
/// buttons), reused as the whole predicate for the spy arm. A `LongPress`
/// binding is always non-default (its short-press action alone might equal
/// the default, but the binding as a whole is a real customization).
fn is_bound_nondefault(bindings: &BTreeMap<ButtonId, Binding>, button: ButtonId) -> bool {
    bindings.get(&button).is_some_and(|binding| {
        matches!(binding, Binding::LongPress(_))
            || binding.click_action() != default_binding(button)
    })
}

/// A device's identity and route — the facts [`plan_for_device`] needs about
/// *which* device it's planning for, grouped so the function stays under the
/// arg-count lint rather than growing a ninth positional parameter.
pub struct DeviceCaptureIdentity<'a> {
    /// Identity used to serialize firmware ownership even when the config
    /// entry carrying this device's settings is adopted or renamed.
    pub physical_key: PhysicalDeviceKey,
    /// The user-config namespace — can change on adoption/rename.
    pub config_key: &'a str,
    /// The device *model's* identity (`DeviceModelInfo::config_key()`),
    /// distinct from `config_key` — selects a `0x8110` spy button map, since
    /// the mapping is inherent to the hardware model, not the user's config.
    pub model_key: &'a str,
    /// HID++ route the session opens.
    pub route: DeviceRoute,
}

/// Build one device's plan from the config (per-app effective for `app`).
#[must_use]
pub fn plan_for_device(
    config: &Config,
    device: DeviceCaptureIdentity<'_>,
    app: Option<&str>,
    rearm_generation: u64,
    os_mouse_hook_available: bool,
) -> DeviceCapturePlan {
    let DeviceCaptureIdentity {
        physical_key,
        config_key,
        model_key,
        route,
    } = device;
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
    let divert_gesture_buttons = if os_mouse_hook_available {
        DIVERTABLE_STANDARD_BUTTONS
            .into_iter()
            .filter(|(_, button)| side_gesture_bindings.contains_key(button))
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
    let thumbwheel_bindings_nondefault = [
        ButtonId::Thumbwheel,
        ButtonId::ThumbwheelScrollUp,
        ButtonId::ThumbwheelScrollDown,
    ]
    .iter()
    .any(|button| {
        bindings
            .get(button)
            .is_some_and(|binding| binding.click_action() != default_binding(*button))
    });
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
                divert_buttons,
                // Arm the spy only when the user actually bound one of the
                // model's gaming buttons — an untouched device must never be
                // told to stream events (the spy reports every press,
                // including left click, while armed).
                spy_buttons: spy_buttons_for_model(model_key)
                    .filter(|map| {
                        map.iter()
                            .any(|&(_, button)| is_bound_nondefault(&bindings, button))
                    })
                    .unwrap_or(&[]),
            },
            rearm_generation,
        },
        dispatch: DispatchPlan {
            config_key: config_key.to_owned(),
            bindings,
            gesture_bindings,
            side_gesture_bindings,
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

    /// Test shim keeping the pre-existing 6-parameter call sites unchanged.
    /// `model_key` is fixed to a value with no [`spy_buttons_for_model`]
    /// entry — none of these tests target a G-series device, so every plan
    /// built through this shim gets an empty `spy_buttons`, matching the
    /// pre-spy behavior exactly. [`plan_for_device_with_model`] below is for
    /// the spy-specific tests that need a real model key.
    fn plan_for_device(
        config: &Config,
        config_key: &str,
        route: DeviceRoute,
        app: Option<&str>,
        rearm_generation: u64,
        os_mouse_hook_available: bool,
    ) -> DeviceCapturePlan {
        plan_for_device_with_model(
            config,
            config_key,
            "not-a-gaming-model",
            route,
            app,
            rearm_generation,
            os_mouse_hook_available,
        )
    }

    fn plan_for_device_with_model(
        config: &Config,
        config_key: &str,
        model_key: &str,
        route: DeviceRoute,
        app: Option<&str>,
        rearm_generation: u64,
        os_mouse_hook_available: bool,
    ) -> DeviceCapturePlan {
        super::plan_for_device(
            config,
            DeviceCaptureIdentity {
                physical_key: PhysicalDeviceKey::parse("receiver:cafe:slot:2")
                    .expect("fixture should be a physical key"),
                config_key,
                model_key,
                route,
            },
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

    #[test]
    fn unbound_gaming_model_arms_no_spy_buttons() {
        let cfg = Config::default();
        let plan = plan_for_device_with_model(&cfg, "2b042", "04099", route(), None, 0, true);
        assert!(
            plan.target.spec.spy_buttons.is_empty(),
            "an untouched G-series device must never be told to stream spy events"
        );
    }

    #[test]
    fn one_bound_gaming_button_arms_the_full_model_map() {
        let mut cfg = Config::default();
        cfg.set_binding(
            "2b042",
            ButtonId::ProfileCycle,
            Binding::Single(Action::MissionControl),
        );
        let plan = plan_for_device_with_model(&cfg, "2b042", "04099", route(), None, 0, true);
        assert_eq!(
            plan.target.spec.spy_buttons,
            [
                (5, ButtonId::DpiShift),
                (9, ButtonId::ProfileCycle),
                (10, ButtonId::DpiUp),
                (11, ButtonId::DpiDown),
            ],
            "binding one gaming button must arm the model's whole spy map, not just the bound one"
        );

        // Binding a second gaming button must not change the spec — this is
        // what keeps the spy from restarting on every binding edit.
        let mut cfg2 = cfg.clone();
        cfg2.set_binding("2b042", ButtonId::DpiUp, Binding::Single(Action::Undo));
        let plan2 = plan_for_device_with_model(&cfg2, "2b042", "04099", route(), None, 0, true);
        assert_eq!(plan.target.spec.spy_buttons, plan2.target.spec.spy_buttons);
    }

    #[test]
    fn non_gaming_model_never_arms_spy_buttons() {
        let mut cfg = Config::default();
        cfg.set_binding(
            "2b042",
            ButtonId::ProfileCycle,
            Binding::Single(Action::MissionControl),
        );
        // "2b042" is a real (non-gaming) model key used elsewhere in this
        // file's fixtures; a ProfileCycle binding on it is meaningless data
        // (the device has no such control), which is exactly the point: the
        // spy only arms for a model `spy_buttons_for_model` actually knows.
        let plan = plan_for_device(&cfg, "2b042", route(), None, 0, true);
        assert!(plan.target.spec.spy_buttons.is_empty());
    }
}
