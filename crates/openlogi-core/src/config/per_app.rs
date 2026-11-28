//! Per-application profiles: device button overrides, software wheel
//! settings, and how the application in front is matched to them.
//!
//! An override replaces a whole button with a single action. Editing addresses
//! a profile by its exact key; matching goes through [`overlay_for`], which
//! also falls back from a Windows executable path to that executable's
//! `exe:<filename>` key, and then to the `app:<alias>` key of the
//! [`Config::apps`] alias that lists the application.

use std::collections::BTreeMap;

use super::{Config, ThumbwheelSensitivity, VerticalScrollSensitivity};
use crate::app::{overlay_for, profile_key_for};
use crate::binding::{Action, Binding, ButtonId};

/// Which effective settings came from matching application overrides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WheelScrollOverrides {
    /// An app profile supplied the smoothing switch.
    pub smooth_scroll: bool,
    /// An app profile supplied the vertical distance.
    pub vertical_sensitivity: bool,
}

/// Effective software wheel smoothing and ordinary vertical sensitivity in one app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WheelScrollProfile<'a> {
    /// The current mouse-target app when any software wheel profiles exist.
    /// Even two apps inheriting the same defaults own separate wheel motion.
    /// A configuration without profiles retains the global wheel lifetime.
    pub app: Option<&'a str>,
    /// The fields this app overrides independently of the global settings.
    pub overrides: WheelScrollOverrides,
    /// Whether eligible OS wheel ticks and captured thumb-wheel scrolling are eased.
    pub smooth_scroll: bool,
    /// Distance setting for the vertical component of ordinary OS wheel ticks.
    pub vertical_sensitivity: VerticalScrollSensitivity,
}

/// Effective thumb-wheel settings for one device and mouse-target application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThumbwheelProfile<'a> {
    /// The actual target app while software wheel profiles are configured.
    /// Every app then owns its thumb-wheel input, even when its value is inherited.
    pub app: Option<&'a str>,
    /// Whether a matching application profile supplied the sensitivity.
    pub overridden: bool,
    /// Effective scroll speed and discrete-action responsiveness for this wheel.
    pub sensitivity: ThumbwheelSensitivity,
}

impl Config {
    /// Resolve the thumb-wheel sensitivity for this app, falling back to the
    /// device's setting, then to the global setting. With no matching profile
    /// the device retains the scroll speed and action threshold of older configs.
    #[must_use]
    pub fn thumbwheel_profile_for<'a>(
        &self,
        device_key: Option<&str>,
        app: Option<&'a str>,
    ) -> ThumbwheelProfile<'a> {
        let profiles = &self.app_settings.per_app_thumbwheel_sensitivity;
        let matched = app.and_then(|id| overlay_for(profiles, &self.apps, id));
        let sensitivity = matched
            .copied()
            .or_else(|| {
                self.devices
                    .get(device_key?)
                    .and_then(|device| device.thumbwheel_sensitivity)
            })
            .unwrap_or(self.app_settings.thumbwheel_sensitivity);
        ThumbwheelProfile {
            app: self.wheel_profile_target(app),
            overridden: matched.is_some(),
            sensitivity,
        }
    }

    /// Resolve software wheel smoothing and OS vertical sensitivity against
    /// their own profiles, then fall back to their global settings. The
    /// hardware-diverted main wheel retains its per-device smoothing switch.
    #[must_use]
    pub fn wheel_scroll_profile_for<'a>(&self, app: Option<&'a str>) -> WheelScrollProfile<'a> {
        let settings = &self.app_settings;
        let smooth =
            app.and_then(|app| overlay_for(&settings.per_app_smooth_scroll, &self.apps, app));
        let sensitivity = app.and_then(|app| {
            overlay_for(
                &settings.per_app_vertical_scroll_sensitivity,
                &self.apps,
                app,
            )
        });
        WheelScrollProfile {
            app: self.wheel_profile_target(app),
            overrides: WheelScrollOverrides {
                smooth_scroll: smooth.is_some(),
                vertical_sensitivity: sensitivity.is_some(),
            },
            smooth_scroll: smooth.copied().unwrap_or(settings.smooth_scroll),
            vertical_sensitivity: sensitivity
                .copied()
                .unwrap_or(settings.vertical_scroll_sensitivity),
        }
    }

    /// Whether any application has a software wheel override, even when
    /// the current mouse target has no application identity.
    #[must_use]
    pub fn has_wheel_scroll_profiles(&self) -> bool {
        self.wheel_scroll_profile_keys().next().is_some()
    }

    /// Without app wheel rules the old global input lifetime spans app switches.
    fn wheel_profile_target<'a>(&self, app: Option<&'a str>) -> Option<&'a str> {
        app.filter(|_| self.has_wheel_scroll_profiles())
    }

    /// Whether eligible wheel input for `app` is smoothed in software.
    #[must_use]
    pub fn smooth_scroll_for(&self, app: Option<&str>) -> bool {
        self.wheel_scroll_profile_for(app).smooth_scroll
    }

    /// Vertical sensitivity of ordinary main-wheel input for `app`.
    #[must_use]
    pub fn vertical_scroll_sensitivity_for(&self, app: Option<&str>) -> VerticalScrollSensitivity {
        self.wheel_scroll_profile_for(app).vertical_sensitivity
    }

    /// Save a smoothing override under this exact key, including an explicit
    /// value equal to the global setting. Returns whether it changed.
    pub fn set_app_smooth_scroll(&mut self, app: &str, enabled: bool) -> bool {
        self.app_settings
            .per_app_smooth_scroll
            .insert(app.to_owned(), enabled)
            != Some(enabled)
    }

    /// Clear this key's smoothing override. Returns whether it existed.
    pub fn remove_app_smooth_scroll(&mut self, app: &str) -> bool {
        self.app_settings
            .per_app_smooth_scroll
            .remove(app)
            .is_some()
    }

    /// Save a vertical sensitivity override under this exact key. An explicit
    /// 1× stays saved across global edits. Returns whether it changed.
    pub fn set_app_vertical_scroll_sensitivity(
        &mut self,
        app: &str,
        sensitivity: VerticalScrollSensitivity,
    ) -> bool {
        self.app_settings
            .per_app_vertical_scroll_sensitivity
            .insert(app.to_owned(), sensitivity)
            != Some(sensitivity)
    }

    /// Clear this key's sensitivity override. Returns whether it existed.
    pub fn remove_app_vertical_scroll_sensitivity(&mut self, app: &str) -> bool {
        self.app_settings
            .per_app_vertical_scroll_sensitivity
            .remove(app)
            .is_some()
    }

    /// Save the thumb-wheel sensitivity for this exact app key, including
    /// an explicit 1×. Returns whether the stored override changed.
    pub fn set_app_thumbwheel_sensitivity(
        &mut self,
        app: &str,
        sensitivity: ThumbwheelSensitivity,
    ) -> bool {
        self.app_settings
            .per_app_thumbwheel_sensitivity
            .insert(app.to_owned(), sensitivity)
            != Some(sensitivity)
    }

    /// Clear the thumb-wheel sensitivity at this exact key, restoring inheritance.
    pub fn remove_app_thumbwheel_sensitivity(&mut self, app: &str) -> bool {
        self.app_settings
            .per_app_thumbwheel_sensitivity
            .remove(app)
            .is_some()
    }

    /// Each key with at least one software wheel setting, once.
    pub fn wheel_scroll_profile_keys(&self) -> impl Iterator<Item = &str> {
        let settings = &self.app_settings;
        let smooth = &settings.per_app_smooth_scroll;
        let vertical = &settings.per_app_vertical_scroll_sensitivity;
        let thumb = &settings.per_app_thumbwheel_sensitivity;
        smooth
            .keys()
            .chain(vertical.keys().filter(|key| !smooth.contains_key(*key)))
            .chain(
                thumb
                    .keys()
                    .filter(|key| !smooth.contains_key(*key) && !vertical.contains_key(*key)),
            )
            .map(String::as_str)
    }

    /// Clear all software wheel settings for this exact profile key.
    pub fn remove_wheel_scroll_profile(&mut self, app: &str) -> bool {
        let smooth = self.remove_app_smooth_scroll(app);
        let vertical = self.remove_app_vertical_scroll_sensitivity(app);
        let thumb = self.remove_app_thumbwheel_sensitivity(app);
        smooth || vertical || thumb
    }

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
        if let Some(bid) = bundle_id
            && let Some(overlay) = overlay_for(&device.per_app_bindings, &self.apps, bid)
        {
            for (k, v) in overlay {
                out.insert(*k, Binding::Single(v.clone()));
            }
        }
        out
    }

    /// Records a per-app override. Creates the device + app entries as
    /// needed; passing an action of `None` removes the override and prunes
    /// the empty app map.
    pub fn set_per_app_binding(
        &mut self,
        device_key: &str,
        bundle_id: &str,
        button: ButtonId,
        action: Option<Action>,
    ) {
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
            .flat_map(|device| device.per_app_bindings.keys().map(String::as_str))
    }

    /// Drop `device_key`'s whole profile for `app`. Nothing happens when there
    /// is none.
    pub fn remove_app_profile(&mut self, device_key: &str, app: &str) {
        if let Some(device) = self.devices.get_mut(device_key) {
            device.per_app_bindings.remove(app);
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

    /// The first per-app profile whose `app:` key names an alias
    /// [`Self::apps`] does not define, as `(scope, key)`; the scope is a
    /// device key or `app_settings` for software scrolling.
    /// Such a profile could never apply, so loading rejects it.
    #[cfg(feature = "fs")]
    pub(super) fn undefined_app_alias(&self) -> Option<(&str, &str)> {
        if let Some(key) = self
            .wheel_scroll_profile_keys()
            .find(|key| self.apps.is_undefined_reference(key))
        {
            return Some(("app_settings", key));
        }
        self.devices.iter().find_map(|(device_key, device)| {
            device
                .per_app_bindings
                .keys()
                .chain(device.action_ring.per_app.keys())
                .find(|key| self.apps.is_undefined_reference(key))
                .map(|key| (device_key.as_str(), key.as_str()))
        })
    }
}
