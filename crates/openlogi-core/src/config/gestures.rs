//! Gesture mode: which buttons of a device dispatch a direction map, and the
//! edits that take a button into and out of that mode.
//!
//! The mode is a per-button fact read from the binding shape, never a separate
//! flag. Leaving it stashes the button's map in
//! [`DeviceConfig::disabled_gestures`](super::DeviceConfig::disabled_gestures),
//! so entering it again restores what the user had.

use super::Config;
use crate::binding::{
    Action, Binding, ButtonId, GestureDirection, default_binding, default_binding_for,
    default_gesture_binding,
};

impl Config {
    /// Records `action` for one `direction` of `button`'s gesture binding,
    /// creating the device entry if needed.
    ///
    /// A button with no binding yet is seeded from its canonical
    /// [`default_binding_for`] — for [`ButtonId::GestureButton`] that is the full
    /// default direction map (including a [`GestureDirection::Click`]), so the
    /// merged map never persists a gesture binding whose click projection is a
    /// no-op. A prior [`Binding::Single`] is upgraded to [`Binding::Gesture`],
    /// preserving its action as the `Click` entry.
    pub fn set_gesture_direction(
        &mut self,
        device_key: &str,
        button: ButtonId,
        direction: GestureDirection,
        action: Action,
    ) {
        if let Binding::Gesture(map) = self.ensure_gesture_binding(device_key, button) {
            map.insert(direction, action);
        }
    }

    /// Ensure `button` on `device_key` is a [`Binding::Gesture`], creating the
    /// device + a default binding if needed and upgrading a [`Binding::Single`]
    /// in place (its action kept as the [`GestureDirection::Click`]). Returns the
    /// entry so the caller can finish it — seed every direction
    /// ([`Binding::fill_gesture_defaults`]) or set just one. Shared by
    /// [`Self::set_gesture_mode`] and [`Self::set_gesture_direction`] so the two
    /// promote a button into gesture mode identically.
    fn ensure_gesture_binding(&mut self, device_key: &str, button: ButtonId) -> &mut Binding {
        let entry = self
            .devices
            .entry(device_key.to_string())
            .or_default()
            .bindings
            .entry(button)
            .or_insert_with(|| default_binding_for(button));
        entry.upgrade_to_gesture();
        entry
    }

    /// Whether `button` on `device_key` is in gesture mode — a per-button fact
    /// read straight from the binding shape: a stored [`Binding::Gesture`], or
    /// no stored binding on a button whose canonical default
    /// ([`default_binding_for`]) is gesture-shaped (the dedicated HID++ gesture
    /// button starts in gesture mode).
    ///
    /// Gesture mode is not exclusive: any number of buttons may gesture at
    /// once, each with its own direction map. This replaces the former
    /// one-gesture-button-per-device owner lock — see [`Self::set_gesture_mode`].
    #[must_use]
    pub fn is_gesture_mode(&self, device_key: &str, button: ButtonId) -> bool {
        self.devices
            .get(device_key)
            .and_then(|d| d.bindings.get(&button))
            .map_or_else(
                || default_binding_for(button).is_gesture(),
                Binding::is_gesture,
            )
    }

    /// Every button of `device_key` currently in gesture mode, in [`ButtonId`]
    /// declaration order. Purely config-derived: callers cross it with the
    /// device's actual controls (a model without the dedicated gesture button
    /// simply never captures it).
    #[must_use]
    pub fn gesture_mode_buttons(&self, device_key: &str) -> Vec<ButtonId> {
        ButtonId::ALL
            .iter()
            .copied()
            .filter(|b| self.is_gesture_mode(device_key, *b))
            .collect()
    }

    /// Turn gesture mode on or off for one button, independently of every
    /// other button.
    ///
    /// On: restore the button's stashed map when one exists (see
    /// [`DeviceConfig::disabled_gestures`]) — an off/on round trip hands back
    /// the user's customized arms exactly. Otherwise promote the stored
    /// binding in place ([`Binding::upgrade_to_gesture`] keeps a prior single
    /// action as the [`GestureDirection::Click`] entry) and seed unbound
    /// directions from [`default_gesture_binding`].
    ///
    /// Off: stash the live map, then demote to a [`Binding::Single`] of the
    /// map's `Click` action, falling back to the button's canonical
    /// [`default_binding`] when the map has no explicit `Click` — a demoted
    /// button always keeps a meaningful press. A button gesturing only by
    /// default (no stored binding) stashes its seeded default map and is
    /// pinned off with an explicit `Single` at its canonical default.
    ///
    /// [`DeviceConfig::disabled_gestures`]: super::DeviceConfig::disabled_gestures
    pub fn set_gesture_mode(&mut self, device_key: &str, button: ButtonId, enabled: bool) {
        if enabled {
            let device = self.devices.entry(device_key.to_string()).or_default();
            if let Some(map) = device.disabled_gestures.remove(&button) {
                device.bindings.insert(button, Binding::Gesture(map));
            } else {
                self.ensure_gesture_binding(device_key, button)
                    .fill_gesture_defaults();
            }
            return;
        }
        let device = self.devices.entry(device_key.to_string()).or_default();
        match device.bindings.get_mut(&button) {
            Some(binding) => {
                if let Binding::Gesture(map) = binding {
                    device.disabled_gestures.insert(button, map.clone());
                }
                binding.demote_to_single(default_binding(button));
            }
            None => {
                if default_binding_for(button).is_gesture() {
                    device.disabled_gestures.insert(
                        button,
                        GestureDirection::ALL
                            .iter()
                            .copied()
                            .map(|d| (d, default_gesture_binding(d)))
                            .collect(),
                    );
                    device
                        .bindings
                        .insert(button, Binding::Single(default_binding(button)));
                }
            }
        }
    }
}
