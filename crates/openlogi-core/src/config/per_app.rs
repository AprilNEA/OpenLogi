//! Per-application profiles: the button overrides a device stores for one
//! application, and how the application in front is matched to them.
//!
//! An override replaces a whole button with a single action or a direction map. Editing addresses
//! a profile by its exact key; matching goes through [`overlay_for`], which
//! also falls back from a Windows executable path to that executable's
//! `exe:<filename>` key.

use std::collections::BTreeMap;

use super::Config;
use crate::app::overlay_for;
use crate::binding::{
    Action, Binding, ButtonId, GestureDirection, default_binding, default_binding_for,
};

impl Config {
    /// Resolve the effective binding map for `device_key`, overlaying the
    /// per-app entry for `bundle_id` (if any) on top of the global per-device
    /// `bindings`. A per-app override replaces the whole button with a
    /// [`Binding::Single`]; everything else falls through.
    ///
    /// Returns an empty map when the device has no recorded bindings yet.
    /// Callers (the GUI / hook) layer their own defaults on top.
    #[must_use]
    pub fn effective_bindings(
        &self,
        device_key: &str,
        bundle_id: Option<&str>,
    ) -> BTreeMap<ButtonId, Binding> {
        let Some(device) = self.devices.get(device_key) else {
            return BTreeMap::new();
        };
        let mut out = device.bindings.clone();
        if let Some(bid) = bundle_id {
            if let Some(overlay) = overlay_for(&device.per_app_gesture_bindings, bid) {
                for (button, directions) in overlay {
                    out.insert(*button, Binding::Gesture(directions.clone()));
                }
            }
            // A single action suspends the app's map without discarding its swipes.
            if let Some(overlay) = overlay_for(&device.per_app_bindings, bid) {
                for (button, action) in overlay {
                    out.insert(*button, Binding::Single(action.clone()));
                }
            }
        }
        out
    }

    /// Records a per-app override. Creates the device + app entries as
    /// needed; passing an action of `None` removes the override and prunes
    /// the empty app map. A single action suspends a stored app direction map;
    /// clearing the override removes both shapes and restores live inheritance.
    pub fn set_per_app_binding(
        &mut self,
        device_key: &str,
        bundle_id: &str,
        button: ButtonId,
        action: Option<Action>,
    ) {
        if action.is_none()
            && let Some(device) = self.devices.get_mut(device_key)
            && let Some(gestures) = device.per_app_gesture_bindings.get_mut(bundle_id)
        {
            gestures.remove(&button);
            device
                .per_app_gesture_bindings
                .retain(|_, map| !map.is_empty());
        }
        let entry = self
            .devices
            .entry(device_key.to_string())
            .or_default()
            .per_app_bindings
            .entry(bundle_id.to_string())
            .or_default();
        match action {
            Some(a) => {
                entry.insert(button, a);
            }
            None => {
                entry.remove(&button);
            }
        }
        if let Some(d) = self.devices.get_mut(device_key) {
            d.per_app_bindings.retain(|_, m| !m.is_empty());
        }
    }

    /// Enter or leave gesture mode only in `app`'s profile, preserving the global binding.
    pub fn set_per_app_gesture_mode(
        &mut self,
        device_key: &str,
        app: &str,
        button: ButtonId,
        enabled: bool,
    ) {
        let mut binding = self.app_gesture_binding(device_key, app, button);
        if enabled {
            if let Some(map) = self
                .per_app_gesture_overrides(device_key, app)
                .and_then(|maps| maps.get(&button))
            {
                binding = Binding::Gesture(map.clone());
            }
            binding.upgrade_to_gesture();
            binding.fill_gesture_defaults();
            if let Binding::Gesture(map) = binding {
                self.store_per_app_gestures(device_key, app, button, map);
            }
        } else {
            let click = binding.click_action();
            if let Binding::Gesture(map) = binding {
                self.store_per_app_gestures(device_key, app, button, map);
            }
            self.set_per_app_binding(device_key, app, button, Some(click));
        }
    }

    /// Edit one app-specific direction, snapshotting the inherited map before its first edit.
    /// A single-action binding is not silently promoted by a stale direction edit.
    pub fn set_per_app_gesture_direction(
        &mut self,
        device_key: &str,
        app: &str,
        button: ButtonId,
        direction: GestureDirection,
        action: Action,
    ) {
        let mut binding = self.app_gesture_binding(device_key, app, button);
        if !binding.is_gesture() {
            return;
        }
        binding.fill_gesture_defaults();
        if let Binding::Gesture(mut map) = binding {
            map.insert(direction, action);
            self.store_per_app_gestures(device_key, app, button, map);
        }
    }

    fn app_gesture_binding(&self, device_key: &str, app: &str, button: ButtonId) -> Binding {
        let mut binding = self
            .effective_bindings(device_key, Some(app))
            .remove(&button)
            .unwrap_or_else(|| default_binding_for(button));
        if let Binding::Gesture(map) = &mut binding {
            map.entry(GestureDirection::Click)
                .or_insert_with(|| default_binding(button));
        }
        binding
    }

    fn store_per_app_gestures(
        &mut self,
        device_key: &str,
        app: &str,
        button: ButtonId,
        map: BTreeMap<GestureDirection, Action>,
    ) {
        self.set_per_app_binding(device_key, app, button, None);
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .per_app_gesture_bindings
            .entry(app.to_string())
            .or_default()
            .insert(button, map);
    }

    /// App-specific directional overrides for the exact editor profile key.
    #[must_use]
    pub fn per_app_gesture_overrides(
        &self,
        device_key: &str,
        app: &str,
    ) -> Option<&BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>> {
        self.devices
            .get(device_key)?
            .per_app_gesture_bindings
            .get(app)
            .filter(|overrides| !overrides.is_empty())
    }

    /// The overrides `device_key` stores for the application key `app`,
    /// or `None` when it has no profile for it.
    ///
    /// Exact key, deliberately: this answers "what did the user author under
    /// this key", which is what an editor needs to show and to clear. The
    /// question [`Self::has_app_override`] answers — "will the app in front hit
    /// a profile" — is the matcher's, and goes through the same `exe:` fallback
    /// the matcher does. The two look interchangeable and are not.
    #[must_use]
    pub fn per_app_overrides(
        &self,
        device_key: &str,
        app: &str,
    ) -> Option<&BTreeMap<ButtonId, Action>> {
        self.devices
            .get(device_key)?
            .per_app_bindings
            .get(app)
            .filter(|overrides| !overrides.is_empty())
    }

    /// Every application key `device_key` has a profile for, in key order.
    pub fn app_profiles(&self, device_key: &str) -> impl Iterator<Item = &str> {
        self.devices
            .get(device_key)
            .into_iter()
            .flat_map(|device| {
                device
                    .per_app_bindings
                    .keys()
                    .chain(device.per_app_gesture_bindings.keys())
                    .map(String::as_str)
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
    }

    /// Drop `device_key`'s whole profile for `app`. Nothing happens when there
    /// is none.
    pub fn remove_app_profile(&mut self, device_key: &str, app: &str) {
        if let Some(device) = self.devices.get_mut(device_key) {
            device.per_app_bindings.remove(app);
            device.per_app_gesture_bindings.remove(app);
        }
    }

    /// Whether `device_key` has a non-empty per-app binding overlay for the
    /// foreground app `app`. Drives the menu-bar popover's "override
    /// active" badge — when the current app has its own bindings for this
    /// device, the global bindings are (partly) overridden.
    #[must_use]
    pub fn has_app_override(&self, device_key: &str, app: &str) -> bool {
        self.devices.get(device_key).is_some_and(|d| {
            overlay_for(&d.per_app_bindings, app).is_some_and(|overlay| !overlay.is_empty())
                || overlay_for(&d.per_app_gesture_bindings, app)
                    .is_some_and(|overlay| !overlay.is_empty())
        })
    }
}
