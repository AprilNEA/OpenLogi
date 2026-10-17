//! Wheel-hold mode: whether holding Back or Forward turns the main wheel into
//! a virtual thumb wheel, and the edit that switches it.
//!
//! Like gesture mode, the mode is the binding's shape
//! ([`Binding::WheelHold`]), never a separate flag. Switching it keeps the
//! button's click action, so a press without scrolling does what it did.
use super::Config;
use crate::binding::{Binding, ButtonId, WheelHoldBinding, default_binding};

impl Config {
    /// Whether `button` of `device_key` is stored as a wheel-hold binding.
    /// No button holds the wheel by default.
    #[must_use]
    pub fn is_wheel_hold(&self, device_key: &str, button: ButtonId) -> bool {
        self.stored_binding(device_key, button)
            .is_some_and(Binding::is_wheel_hold)
    }

    /// Turn wheel-hold on or off for `button` of `device_key`, keeping the
    /// action its press runs.
    ///
    /// On: a button in gesture mode first leaves it through
    /// [`Self::set_gesture_mode`], which stashes its direction map so turning
    /// gestures on again restores it, and keeps its `Click` action. The
    /// resulting click action becomes the wheel-hold press; a long press
    /// keeps its short action and loses its long one, because the same
    /// release cannot decide both.
    ///
    /// Off: demote to a [`Binding::Single`] of the click action.
    ///
    /// A no-op for a button that cannot hold the wheel
    /// ([`ButtonId::supports_wheel_hold`]) and when the mode already matches.
    pub fn set_wheel_hold(&mut self, device_key: &str, button: ButtonId, enabled: bool) {
        if !button.supports_wheel_hold() || self.is_wheel_hold(device_key, button) == enabled {
            return;
        }
        if enabled && self.is_gesture_mode(device_key, button) {
            self.set_gesture_mode(device_key, button, false);
        }
        let click = self
            .stored_binding(device_key, button)
            .map_or_else(|| default_binding(button), Binding::click_action);
        let binding = if enabled {
            Binding::WheelHold(WheelHoldBinding::horizontal_scroll(click))
        } else {
            Binding::Single(click)
        };
        self.set_binding(device_key, button, binding);
    }

    fn stored_binding(&self, device_key: &str, button: ButtonId) -> Option<&Binding> {
        self.devices
            .get(device_key)
            .and_then(|device| device.bindings.get(&button))
    }
}
