//! Per-application profiles: the button overrides a device stores for one
//! application, and how the application in front is matched to them.
//!
//! An override replaces a whole button with a [`Binding`] of any shape,
//! including a gesture direction map. The profile mirrors the device-wide one:
//! one dispatched overlay
//! ([`DeviceConfig::per_app_bindings`](super::DeviceConfig::per_app_bindings))
//! and one stash of direction maps whose gesture mode is off
//! ([`DeviceConfig::per_app_disabled_gestures`](super::DeviceConfig::per_app_disabled_gestures)),
//! which is never dispatched. Editing addresses a profile by its exact key;
//! matching goes through [`overlay_for`], which also falls back from a Windows
//! executable path to that executable's `exe:<filename>` key, and then to the
//! `app:<alias>` key of the [`Config::apps`] alias that lists the application.

use std::collections::BTreeMap;

use super::Config;
use crate::app::{overlay_for, profile_key_for};
use crate::binding::{
    Action, Binding, ButtonId, GestureDirection, default_binding, default_binding_for,
};

impl Config {
    /// Resolve the effective binding map for `device_key`, overlaying the
    /// per-app entry for `bundle_id` (if any) on top of the global per-device
    /// `bindings`. A per-app override replaces the whole button; everything
    /// else falls through.
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
        if let Some(bid) = bundle_id
            && let Some(overlay) = overlay_for(&device.per_app_bindings, &self.apps, bid)
        {
            out.extend(overlay.iter().map(|(k, v)| (*k, v.clone())));
        }
        out
    }

    /// Records a single-action per-app override. Creates the device + app
    /// entries as needed. A gesture map the app profile held for `button` is
    /// stashed, so [`Self::enable_per_app_gestures`] restores it.
    ///
    /// Passing `None` restores inheritance: it removes the override and the
    /// stashed map, and prunes the empty app maps.
    pub fn set_per_app_binding(
        &mut self,
        device_key: &str,
        bundle_id: &str,
        button: ButtonId,
        action: Option<Action>,
    ) {
        match action {
            Some(action) => {
                self.store_per_app_binding(device_key, bundle_id, button, Binding::Single(action));
            }
            None => {
                if let Some(device) = self.devices.get_mut(device_key) {
                    if let Some(overlay) = device.per_app_bindings.get_mut(bundle_id) {
                        overlay.remove(&button);
                    }
                    if let Some(stash) = device.per_app_disabled_gestures.get_mut(bundle_id) {
                        stash.remove(&button);
                    }
                    device.per_app_bindings.retain(|_, m| !m.is_empty());
                    device
                        .per_app_disabled_gestures
                        .retain(|_, m| !m.is_empty());
                }
            }
        }
    }

    /// Turn gesture mode on for `button` in `app`'s profile only, leaving the
    /// device-wide binding alone.
    ///
    /// Restores the map stashed by [`Self::disable_per_app_gestures`] when there
    /// is one. Otherwise the binding the app currently resolves to — its own
    /// override or the inherited one — is promoted as
    /// [`Binding::upgrade_to_gesture`] promotes it, with unbound directions
    /// seeded from their defaults.
    pub fn enable_per_app_gestures(&mut self, device_key: &str, app: &str, button: ButtonId) {
        let stashed = self.devices.get_mut(device_key).and_then(|device| {
            let stash = device.per_app_disabled_gestures.get_mut(app)?;
            let map = stash.remove(&button);
            device
                .per_app_disabled_gestures
                .retain(|_, m| !m.is_empty());
            map
        });
        let binding = if let Some(map) = stashed {
            Binding::Gesture(map)
        } else {
            let mut binding = self.app_binding(device_key, app, button);
            binding.upgrade_to_gesture();
            binding.fill_gesture_defaults();
            binding
        };
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .per_app_bindings
            .entry(app.to_string())
            .or_default()
            .insert(button, binding);
    }

    /// Turn gesture mode off for `button` in `app`'s profile only: stash the
    /// direction map the app resolves to and override the button with a
    /// [`Binding::Single`] of its `Click` action, falling back to the button's
    /// canonical [`default_binding`]. The device-wide binding is untouched.
    pub fn disable_per_app_gestures(&mut self, device_key: &str, app: &str, button: ButtonId) {
        let Binding::Gesture(map) = self.app_binding(device_key, app, button) else {
            return;
        };
        let mut single = Binding::Gesture(map.clone());
        single.demote_to_single(default_binding(button));
        let device = self.devices.entry(device_key.to_string()).or_default();
        device
            .per_app_bindings
            .entry(app.to_string())
            .or_default()
            .insert(button, single);
        device
            .per_app_disabled_gestures
            .entry(app.to_string())
            .or_default()
            .insert(button, map);
    }

    /// Records `action` for one `direction` of `button` in `app`'s profile.
    /// The first edit snapshots the map the app inherits, so later edits to
    /// the device-wide map no longer reach this app. Nothing happens when
    /// the app does not resolve `button` to a gesture map: a stale direction
    /// edit never silently promotes a single action.
    pub fn set_per_app_gesture_direction(
        &mut self,
        device_key: &str,
        app: &str,
        button: ButtonId,
        direction: GestureDirection,
        action: Action,
    ) {
        let mut binding = self.app_binding(device_key, app, button);
        binding.fill_gesture_defaults();
        let Binding::Gesture(map) = &mut binding else {
            return;
        };
        map.insert(direction, action);
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .per_app_bindings
            .entry(app.to_string())
            .or_default()
            .insert(button, binding);
    }

    /// The binding `app`'s profile resolves `button` to: its own override, or
    /// the inherited one, or the button's canonical default.
    fn app_binding(&self, device_key: &str, app: &str, button: ButtonId) -> Binding {
        let mut binding = self
            .devices
            .get(device_key)
            .and_then(|device| {
                device
                    .per_app_bindings
                    .get(app)
                    .and_then(|overlay| overlay.get(&button))
                    .or_else(|| device.bindings.get(&button))
            })
            .cloned()
            .unwrap_or_else(|| default_binding_for(button));
        if let Binding::Gesture(map) = &mut binding {
            map.entry(GestureDirection::Click)
                .or_insert_with(|| default_binding(button));
        }
        binding
    }

    /// Store a non-gesture override for `button`, stashing the gesture map
    /// it replaces.
    fn store_per_app_binding(
        &mut self,
        device_key: &str,
        app: &str,
        button: ButtonId,
        binding: Binding,
    ) {
        let device = self.devices.entry(device_key.to_string()).or_default();
        let replaced = device
            .per_app_bindings
            .entry(app.to_string())
            .or_default()
            .insert(button, binding);
        if let Some(Binding::Gesture(map)) = replaced {
            device
                .per_app_disabled_gestures
                .entry(app.to_string())
                .or_default()
                .insert(button, map);
        }
    }

    /// The overrides `device_key` stores for the application key `app`,
    /// or `None` when it has no profile for it.
    ///
    /// Exact key, deliberately: this answers "what did the user author under
    /// this key", which is what an editor needs to show and to clear. The
    /// question [`Self::has_app_override`] answers — "will the app in front hit
    /// a profile" — is the matcher's, and goes through the same `exe:` and
    /// alias fallbacks the matcher does. The two look interchangeable and are
    /// not.
    #[must_use]
    pub fn per_app_overrides(
        &self,
        device_key: &str,
        app: &str,
    ) -> Option<&BTreeMap<ButtonId, Binding>> {
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
            .flat_map(|device| device.per_app_bindings.keys().map(String::as_str))
    }

    /// Drop `device_key`'s whole profile for `app`, stashed direction maps
    /// included. Nothing happens when there is none.
    pub fn remove_app_profile(&mut self, device_key: &str, app: &str) {
        if let Some(device) = self.devices.get_mut(device_key) {
            device.per_app_bindings.remove(app);
            device.per_app_disabled_gestures.remove(app);
        }
    }

    /// Whether `device_key` has a non-empty per-app binding overlay for the
    /// foreground app `app`. Drives the menu-bar popover's "override
    /// active" badge — when the current app has its own bindings for this
    /// device, the global bindings are (partly) overridden.
    #[must_use]
    pub fn has_app_override(&self, device_key: &str, app: &str) -> bool {
        self.devices.get(device_key).is_some_and(|d| {
            overlay_for(&d.per_app_bindings, &self.apps, app)
                .is_some_and(|overlay| !overlay.is_empty())
        })
    }

    /// The key of the profile `device_key` runs the foreground application
    /// `app` under, matched as [`Self::effective_bindings`] matches it — the
    /// exact key, an `exe:<filename>` key, or an `app:<alias>` key — or `None`
    /// when the global bindings apply.
    #[must_use]
    pub fn app_profile_for(&self, device_key: &str, app: &str) -> Option<&str> {
        profile_key_for(
            &self.devices.get(device_key)?.per_app_bindings,
            &self.apps,
            app,
        )
    }

    /// The first per-app profile, by device and then key, whose `app:` key
    /// names an alias [`Self::apps`] does not define, as `(device, key)`.
    /// Such a profile could never apply, so loading rejects it.
    #[cfg(feature = "fs")]
    pub(super) fn undefined_app_alias(&self) -> Option<(&str, &str)> {
        self.devices.iter().find_map(|(device_key, device)| {
            device
                .per_app_bindings
                .keys()
                .chain(device.per_app_disabled_gestures.keys())
                .chain(device.action_ring.per_app.keys())
                .find(|key| self.apps.is_undefined_reference(key))
                .map(|key| (device_key.as_str(), key.as_str()))
        })
    }
}
