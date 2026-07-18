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
    /// Whether any thumbwheel binding leaves its default. The watcher combines
    /// this with the live sensitivity to decide thumb-wheel diversion.
    pub thumbwheel_bindings_nondefault: bool,
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
) -> DeviceCapturePlan {
    let bindings = bindings_for(config, Some(config_key), app);
    let gesture_bindings = gesture_bindings_for(config, Some(config_key));
    // A button acting as the OS-hook gesture owner must stay native: the hook
    // needs to see its press to run hold+swipe detection, and diverting it
    // would starve the hook of events.
    let oshook = oshook_gestures_for(config, Some(config_key), app);
    let divert_buttons: Vec<(u16, ButtonId)> = DIVERTABLE_STANDARD_BUTTONS
        .into_iter()
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
    }
}
