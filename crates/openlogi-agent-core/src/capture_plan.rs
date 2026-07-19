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
use std::sync::{Arc, RwLock};

use openlogi_core::binding::{Action, ButtonId, GestureDirection, default_binding};
use openlogi_core::config::Config;
use openlogi_hid::DeviceRoute;
use openlogi_hid::gesture::DIVERTABLE_STANDARD_BUTTONS;
use openlogi_hid::reprog_controls::GESTURE_BUTTON_CID;

use crate::bindings::{bindings_for, gesture_bindings_for, oshook_gestures_for};

/// Everything the capture watcher needs to run one device's session and
/// dispatch its events.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceCapturePlan {
    /// Stable per-device config key (binding / preset lookup).
    pub config_key: String,
    /// HID++ route the session opens.
    pub route: DeviceRoute,
    /// Per-button single actions for this device (per-app effective).
    pub bindings: BTreeMap<ButtonId, Action>,
    /// Per-direction map when the dedicated HID++ gesture button owns the
    /// gesture role on this device; empty otherwise.
    pub gesture_bindings: BTreeMap<GestureDirection, Action>,
    /// Standard buttons whose binding leaves the default — divert over
    /// `0x1b04`. A button at its default keeps its native HID behavior, so no
    /// re-synthesis is ever needed.
    pub divert_buttons: Vec<(u16, ButtonId)>,
    /// Whether any thumbwheel binding leaves its default. Combined with the
    /// sensitivity to decide thumb-wheel diversion.
    pub thumbwheel_bindings_nondefault: bool,
    /// This device's effective thumb-wheel sensitivity (device override or the
    /// app-wide default).
    pub thumbwheel_sensitivity: i32,
    /// Capture re-arm generation from the orchestrator. Bumps on reconnect /
    /// system wake so sessions restart even when route and divert set match.
    pub rearm_generation: u64,
}

/// Shared plan list, rewritten by the orchestrator and read by the watcher.
pub type SharedCapturePlans = Arc<RwLock<Vec<DeviceCapturePlan>>>;

/// Build one device's plan from the config (per-app effective for `app`).
#[must_use]
pub fn plan_for_device(
    config: &Config,
    config_key: &str,
    route: DeviceRoute,
    app: Option<&str>,
    rearm_generation: u64,
) -> DeviceCapturePlan {
    let bindings = bindings_for(config, Some(config_key), app);
    let gesture_bindings = gesture_bindings_for(config, Some(config_key));
    // A button acting as the OS-hook gesture owner must stay native: the hook
    // needs to see its press to run hold+swipe detection, and diverting it
    // would starve the hook of events.
    let oshook = oshook_gestures_for(config, Some(config_key), app);
    // The HID++ gesture control never reaches the OS hook, so a non-default
    // single binding on it is deliverable only via a plain HID++ divert — but
    // only while it does NOT own the gesture role (the raw-XY gesture divert
    // owns its CIDs in that case, and `gesture_bindings` is how the watcher
    // arms that divert). `GESTURE_BUTTON_CID` is a marker here: the capture
    // session expands it to whichever gesture-source CIDs the device exposes.
    let plain_gesture_button = gesture_bindings
        .is_empty()
        .then_some((GESTURE_BUTTON_CID, ButtonId::GestureButton));
    let divert_buttons: Vec<(u16, ButtonId)> = DIVERTABLE_STANDARD_BUTTONS
        .into_iter()
        .chain(plain_gesture_button)
        .filter(|(_, button)| !oshook.contains_key(button))
        .filter(|(_, button)| {
            bindings
                .get(button)
                .is_some_and(|action| *action != default_binding(*button))
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
            .is_some_and(|action| *action != default_binding(*button))
    });
    DeviceCapturePlan {
        config_key: config_key.to_owned(),
        route,
        bindings,
        gesture_bindings,
        divert_buttons,
        thumbwheel_bindings_nondefault,
        thumbwheel_sensitivity: config.thumbwheel_sensitivity(config_key),
        rearm_generation,
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::binding::Binding;
    use openlogi_hid::reprog_controls::{GESTURE_BUTTON_CID, HAPTIC_PANEL_CID};

    use super::*;

    fn route() -> DeviceRoute {
        DeviceRoute::Bolt {
            receiver_uid: "cafe".into(),
            slot: 2,
        }
    }

    #[test]
    #[ignore = "RED: the haptic panel as its own control is not implemented yet"]
    fn haptic_panel_can_own_the_gesture_role() {
        // The MX Master 4 haptic panel is a HID++ gesture source: selecting it
        // as the gesture owner must arm the raw-XY gesture divert, exactly
        // like the dedicated gesture button.
        let mut cfg = Config::default();
        cfg.set_gesture_owner("2b042", ButtonId::HapticPanel);

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0);
        assert!(
            !plan.gesture_bindings.is_empty(),
            "a panel gesture owner must arm the HID++ gesture divert"
        );
        assert!(
            !plan
                .divert_buttons
                .iter()
                .any(|&(cid, _)| cid == HAPTIC_PANEL_CID),
            "the gesture owner is delivered via raw-XY divert, never a plain one"
        );
    }

    #[test]
    #[ignore = "RED: the haptic panel as its own control is not implemented yet"]
    fn single_bound_haptic_panel_is_plain_diverted_when_not_the_owner() {
        // While the dedicated button owns gestures (the default), a single
        // action bound to the panel is deliverable only via a plain HID++
        // divert dispatching ButtonId::HapticPanel.
        let mut cfg = Config::default();
        cfg.set_binding(
            "2b042",
            ButtonId::HapticPanel,
            Binding::Single(Action::Copy),
        );

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0);
        assert!(
            plan.divert_buttons
                .contains(&(HAPTIC_PANEL_CID, ButtonId::HapticPanel)),
            "a single-bound panel must be plain-diverted, or the binding can never fire"
        );
    }

    #[test]
    #[ignore = "RED: the haptic panel as its own control is not implemented yet"]
    fn unbound_haptic_panel_stays_native() {
        // Default binding for the panel is Action::None — an untouched panel
        // must not be diverted, so its firmware behavior (haptics) survives.
        let cfg = Config::default();

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0);
        assert!(
            !plan
                .divert_buttons
                .iter()
                .any(|&(cid, _)| cid == HAPTIC_PANEL_CID),
            "an unbound panel must keep its native behavior"
        );
    }

    #[test]
    fn gestures_off_single_bound_gesture_button_is_plain_diverted() {
        // The dedicated gesture button (CID 0x00c3) never reaches the OS hook,
        // so with gestures off a non-default single binding on it is only
        // deliverable via a plain HID++ divert.
        let mut cfg = Config::default();
        cfg.disable_gestures("2b042");
        cfg.set_binding(
            "2b042",
            ButtonId::GestureButton,
            Binding::Single(Action::CycleDpiPresets),
        );

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0);
        assert!(
            plan.gesture_bindings.is_empty(),
            "gestures are off — no raw-XY gesture divert"
        );
        assert!(
            plan.divert_buttons
                .contains(&(GESTURE_BUTTON_CID, ButtonId::GestureButton)),
            "a single-bound gesture button must be plain-diverted, or the binding can never fire"
        );
    }

    #[test]
    fn gesture_owner_button_is_never_plain_diverted() {
        // When the gesture button owns the gesture role, the raw-XY gesture
        // divert owns CID 0x00c3 — a plain divert on top would strip raw-XY.
        // (Its default Click projects to a non-default single action, so only
        // the owner rule keeps it out of the plain list.)
        let mut cfg = Config::default();
        cfg.set_gesture_owner("2b042", ButtonId::GestureButton);

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0);
        assert!(
            !plan.gesture_bindings.is_empty(),
            "the gesture button owns the gesture role"
        );
        assert!(
            !plan
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
        cfg.disable_gestures("2b042");

        let plan = plan_for_device(&cfg, "2b042", route(), None, 0);
        assert!(
            !plan
                .divert_buttons
                .iter()
                .any(|&(cid, _)| cid == GESTURE_BUTTON_CID),
            "an unbound gesture button must not be captured"
        );
    }
}
