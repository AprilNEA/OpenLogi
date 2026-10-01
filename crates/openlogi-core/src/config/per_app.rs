//! Per-application profiles: the button overrides a device stores for one
//! application, and how the application in front is matched to them.
//!
//! An override replaces a whole button with a single action. Editing addresses
//! a profile by its exact key; matching also falls back from a Windows
//! executable path to that executable's `exe:<filename>` key.

use std::{collections::BTreeMap, path::Path};

use super::Config;
use crate::binding::{Action, Binding, ButtonId};

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
        if let Some(bid) = bundle_id
            && let Some(overlay) = app_overlay(&device.per_app_bindings, bid)
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
    /// foreground app `app` (bundle id). Drives the menu-bar popover's "override
    /// active" badge — when the current app has its own bindings for this
    /// device, the global bindings are (partly) overridden.
    #[must_use]
    pub fn has_app_override(&self, device_key: &str, app: &str) -> bool {
        self.devices.get(device_key).is_some_and(|d| {
            app_overlay(&d.per_app_bindings, app).is_some_and(|overlay| !overlay.is_empty())
        })
    }
}

/// Resolve the most specific application overlay for a foreground identifier.
///
/// Exact keys retain precedence. On Windows the foreground identifier is a
/// lower-cased executable path, so `exe:<filename>` provides a stable fallback
/// for Store and self-updating applications whose install directory changes
/// between versions. Recognizing both path separators keeps hand-authored
/// Windows config inspectable on every platform without changing macOS bundle
/// identifiers or Linux application classes.
fn app_overlay<'a, T>(overlays: &'a BTreeMap<String, T>, app: &str) -> Option<&'a T> {
    overlays.get(app).or_else(|| {
        let executable_name = app.rsplit(['\\', '/']).next()?;
        if executable_name.is_empty()
            || !Path::new(executable_name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
        {
            return None;
        }

        overlays.get(&format!("exe:{}", executable_name.to_ascii_lowercase()))
    })
}
