//! The button bindings of a mouse whose onboard flash keeps a set per
//! profile (the G700).
//!
//! Such a mouse stores what its buttons do in each of its onboard profiles,
//! and OpenLogi keeps one configuration per profile to match: switching the
//! profile, on the mouse or from the settings app, plays that profile's own
//! buttons and never carries another profile's configuration into it.
//!
//! A profile's configuration is keyed by the flash **page** the profile is
//! stored on, not by its directory slot. The page is the profile's identity
//! for as long as it exists: disabling a profile shifts every later slot up
//! (`Directory::without`), enabling one appends it, so a slot number would
//! hand one profile's buttons to another. A disabled profile keeps its page
//! and so its configuration, which applies again once it is enabled; a copy
//! is a new page, which [`Config::copy_flash_profile`] gives the source's
//! configuration.
//!
//! The device entry's own [`DeviceConfig::bindings`] and
//! [`DeviceConfig::per_app_bindings`] hold the profile the settings app last
//! edited, named by [`DeviceConfig::flash_page`], so the binding editor and
//! every accessor keyed by device work on it unchanged; the other profiles
//! wait in [`DeviceConfig::flash_profiles`]. Which maps belong to which page
//! is this module's decision alone: readers ask
//! [`Config::flash_profile_config`], the editor moves the edited profile with
//! [`Config::select_flash_profile`].

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::button_map::{deserialize_button_map, deserialize_button_maps_by_app};
use super::{Config, DeviceConfig};
use crate::binding::{Action, Binding, ButtonId};

/// The bindings of one onboard profile that is not the one the device entry
/// holds (see the module documentation).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashProfileBindings {
    /// The flash page the profile is stored on.
    pub page: u8,
    /// Its buttons' bindings, as [`DeviceConfig::bindings`].
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "deserialize_button_map"
    )]
    pub bindings: BTreeMap<ButtonId, Binding>,
    /// Its per-app overrides, as [`DeviceConfig::per_app_bindings`].
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "deserialize_button_maps_by_app"
    )]
    pub per_app_bindings: BTreeMap<String, BTreeMap<ButtonId, Action>>,
}

impl FlashProfileBindings {
    fn is_empty(&self) -> bool {
        self.bindings.is_empty() && self.per_app_bindings.is_empty()
    }
}

impl DeviceConfig {
    /// Move the device entry's own maps out, as the bindings of `page`.
    fn take_profile(&mut self, page: u8) -> FlashProfileBindings {
        FlashProfileBindings {
            page,
            bindings: std::mem::take(&mut self.bindings),
            per_app_bindings: std::mem::take(&mut self.per_app_bindings),
        }
    }

    /// The parked profile on `page`, removed.
    fn unpark(&mut self, page: u8) -> Option<FlashProfileBindings> {
        let position = self.flash_profiles.iter().position(|p| p.page == page)?;
        Some(self.flash_profiles.remove(position))
    }

    /// Park `profile`, replacing what `page` held; an empty one is dropped.
    fn park(&mut self, profile: FlashProfileBindings) {
        self.flash_profiles.retain(|p| p.page != profile.page);
        if !profile.is_empty() {
            self.flash_profiles.push(profile);
            self.flash_profiles.sort_by_key(|p| p.page);
        }
    }
}

impl Config {
    /// The pages of the flash mouse `key`'s onboard profiles that OpenLogi
    /// holds bindings for.
    #[must_use]
    pub fn flash_profile_pages(&self, key: &str) -> BTreeSet<u8> {
        let Some(device) = self.devices.get(key) else {
            return BTreeSet::new();
        };
        let own = device
            .flash_page
            .filter(|_| !(device.bindings.is_empty() && device.per_app_bindings.is_empty()));
        device
            .flash_profiles
            .iter()
            .map(|profile| profile.page)
            .chain(own)
            .collect()
    }

    /// This configuration as the flash mouse `key` reads it while it plays
    /// the profile on `page`: its entry's bindings and per-app overrides are
    /// that profile's, so every reader keyed by device — the flash intents,
    /// the hook's bindings — sees one profile and no other. Maps the entry
    /// holds for no page (no [`DeviceConfig::flash_page`]) belong to none.
    #[must_use]
    pub fn flash_profile_config(&self, key: &str, page: u8) -> Self {
        let mut config = self.clone();
        if let Some(device) = config.devices.get_mut(key)
            && device.flash_page != Some(page)
        {
            let profile = device.unpark(page).unwrap_or_default();
            device.bindings = profile.bindings;
            device.per_app_bindings = profile.per_app_bindings;
        }
        config
    }

    /// Make the profile on `page` the one `key`'s entry holds, so the
    /// binding editor edits it: the profile held so far is parked under its
    /// page and `page`'s comes out. Maps held for no page yet become
    /// `page`'s unless it has bindings of its own. Returns whether anything
    /// moved.
    pub fn select_flash_profile(&mut self, key: &str, page: u8) -> bool {
        let device = self.devices.entry(key.to_owned()).or_default();
        if device.flash_page == Some(page) {
            return false;
        }
        if let Some(previous) = device.flash_page {
            let held = device.take_profile(previous);
            device.park(held);
            let next = device.unpark(page).unwrap_or_default();
            device.bindings = next.bindings;
            device.per_app_bindings = next.per_app_bindings;
        } else if let Some(next) = device.unpark(page) {
            // Maps held for no page are the selected profile's only when it
            // has none of its own: they were never applied to any profile,
            // its own were.
            device.bindings = next.bindings;
            device.per_app_bindings = next.per_app_bindings;
        }
        device.flash_page = Some(page);
        true
    }

    /// Give the profile on page `to` — a copy of the one on `from` — a copy
    /// of `from`'s bindings, replacing whatever `to` held. Returns whether
    /// anything changed.
    pub fn copy_flash_profile(&mut self, key: &str, from: u8, to: u8) -> bool {
        let Some(device) = self.devices.get_mut(key) else {
            return false;
        };
        if from == to {
            return false;
        }
        let source = if device.flash_page == Some(from) {
            FlashProfileBindings {
                page: to,
                bindings: device.bindings.clone(),
                per_app_bindings: device.per_app_bindings.clone(),
            }
        } else {
            let mut parked = device
                .flash_profiles
                .iter()
                .find(|p| p.page == from)
                .cloned()
                .unwrap_or_default();
            parked.page = to;
            parked
        };
        if device.flash_page == Some(to) {
            let changed = device.bindings != source.bindings
                || device.per_app_bindings != source.per_app_bindings;
            device.bindings = source.bindings;
            device.per_app_bindings = source.per_app_bindings;
            return changed;
        }
        let before = device.flash_profiles.clone();
        device.park(source);
        device.flash_profiles != before
    }
}

#[cfg(test)]
mod tests;
