//! User configuration, persisted as TOML at the platform-standard config
//! path.
//!
//! Per-device state (button bindings, …) lives under the
//! [`Config::devices`] map, keyed by a stable physical-device identifier such
//! as `"receiver:abc123:slot:2"`. Schema migrations branch on
//! [`Config::schema_version`].

use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};

mod device;
#[cfg(feature = "fs")]
mod file;
mod identity;
mod key_trigger;
mod settings;

// Stacked, not `all(test, …)`: clippy reads the combined form as a test
// outside a test module and withdraws the `unwrap`/`expect` exemption.
#[cfg(test)]
#[cfg(feature = "fs")]
mod tests;

pub use device::{DeviceConfig, DeviceIdentity, LinkConfig, LinkOverrides};
#[cfg(feature = "fs")]
pub use file::{ConfigError, ConfigFile};
#[cfg(all(test, feature = "fs"))]
use file::{backup_existing_config, config_backup_path};
pub use identity::canonical_device_key;
pub use key_trigger::{KeyModifiers, KeyTrigger, KeyboardConfig, ParseTriggerError};
pub use settings::LightSettings;
pub use settings::{
    AppIcon, AppSettings, Appearance, AssetSourcePreference, CameraControls, DeviceViewMode,
    Lighting, SMARTSHIFT_AUTO_DISENGAGE_DEFAULT, SMARTSHIFT_MIN_AUTO_DISENGAGE, ScrollResolution,
    SmartShift, ThumbwheelSensitivity, UiScale, VerticalScrollSensitivity, WheelMode,
};

use crate::binding::{
    Action, ActionRingConfig, ActionRingIcon, ActionRingSlot, Binding, ButtonId, GestureDirection,
    RingAction, default_binding, default_binding_for, default_gesture_binding,
};
use crate::device_order::PhysicalDeviceKey;
use crate::hid::Dpi;
#[cfg(feature = "fs")]
use settings::GestureOwner;
/// The schema version the current build produces. Bumped whenever the
/// persisted shape or enum vocabulary changes; readers inspect this value
/// before consuming the rest of the file.
///
/// v7 aligns the thumb-wheel scroll defaults with its normalised physical
/// direction. Pre-v7 explicit default pairs are migrated in device and
/// per-application profiles so they remain native rather than becoming a
/// reversal (see `Config::migrate_thumbwheel_native_direction`).
///
/// v6 adds threshold-based `{ short = ..., long = ... }` button bindings.
///
/// v5 also drops the transport prefix from `direct:` keys: `direct:046d:c08d:unit:6be9d300`
/// names the mouse *and the cable it was plugged into*, so a device moved to a
/// different route was silently orphaned from its settings.
/// [`Config::migrate_transport_scoped_keys`] rewrites such a key to its bare
/// identity fragment (`unit:6be9d300`) — including `selected_device` and every
/// `host_switch_targets` entry — and keeps the dropped route as a
/// [`DeviceConfig::links`] entry. `receiver:` keys are left alone: nothing on
/// disk says which device occupies a pairing slot, so those are folded at
/// runtime instead, on the next online sighting (see `adopt_route`).
///
/// v5 adds the app-wide `ui_scale` preference. Older files default to the
/// standard 100% scale.
///
/// Per-device custom names and the Home gallery view preference are optional
/// and did not require a version bump: absent fields use the model name and
/// responsive grid respectively.
///
/// v4 removes the one-gesture-button-per-device owner lock: gesture mode is a
/// per-button fact read from the binding shape, so `gesture_owner` no longer
/// serializes. Loading a v3-or-older file resolves the old owner and rewrites
/// the shapes to dispatch identically
/// (see `Config::migrate_owner_locked_gestures`); the version gate is what
/// keeps that pass off v4 files, where several gesture-shaped buttons are a
/// deliberate state, not a dormant leftover.
///
/// v3 changes the device map from model keys to physical-device keys. No v2
/// device entries are migrated because model-scoped settings cannot be assigned
/// safely when two identical devices exist.
///
/// v2 merged the per-device `button_bindings` + `gesture_bindings` maps into a
/// single `bindings: BTreeMap<ButtonId, Binding>`. A v1 file still loads (the
/// `RawDeviceConfig` shim folds the legacy fields) and self-heals to v2 on the
/// next save; [`Config::load_from_path`] accepts supported versions `1` through
/// [`SCHEMA_VERSION`] so an invalid or forward file fails loudly instead of
/// silently losing bindings.
pub const SCHEMA_VERSION: u32 = 7;

/// Top-level config document.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Schema version the file was written with. Compared against
    /// [`SCHEMA_VERSION`] on load: supported older layouts migrate, while zero
    /// and newer layouts are rejected rather than silently losing settings.
    pub schema_version: u32,
    /// Non-device-scoped preferences (autostart, tray, language, …).
    #[serde(default, skip_serializing_if = "AppSettings::is_default")]
    pub app_settings: AppSettings,
    /// Physical config key of the active device, persisted so a
    /// restart restores the last view rather than always landing on the
    /// first paired device. `None` means "fall back to the first device".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_device: Option<String>,
    /// When set (see [`Self::ephemeral`]), [`Self::save_atomic`] is a no-op:
    /// this config never writes the on-disk file. Never true for a loaded or
    /// default-constructed config.
    #[serde(skip)]
    // Read only by the `fs` half, which is where saving happens. The field
    // stays in every build: `Config::ephemeral()` is public API, and a field
    // that exists conditionally is a struct whose shape depends on a feature.
    #[cfg_attr(
        not(feature = "fs"),
        expect(clippy::allow_attributes, reason = "see above"),
        allow(dead_code, reason = "only the `fs` half suppresses a save")
    )]
    ephemeral: bool,
    /// Per-device state, normally keyed by the stable physical-device
    /// identifier (e.g. `"receiver:abc123:slot:2"`). A serial-less camera's
    /// custom name instead uses its OS capture id so same-model cameras remain
    /// distinguishable.
    #[serde(default)]
    pub devices: BTreeMap<String, DeviceConfig>,
    /// Keyboard remappings, independent of device. The function-key remapper
    /// (M1) reads this; `#[serde(default)]` keeps older configs without a
    /// `[keyboard]` section loading unchanged.
    #[serde(default)]
    pub keyboard: KeyboardConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            app_settings: AppSettings::default(),
            selected_device: None,
            devices: BTreeMap::new(),
            ephemeral: false,
            keyboard: KeyboardConfig::default(),
        }
    }
}

impl Config {
    /// A config that never touches the on-disk file: [`Self::save_atomic`] is
    /// a no-op. For tests that drive the state layer's persistence paths —
    /// with a default config those would overwrite the developer's real
    /// `config.toml` with test fixtures.
    #[must_use]
    pub fn ephemeral() -> Self {
        Self {
            ephemeral: true,
            ..Self::default()
        }
    }

    /// Returns the bindings stored for `device_key`, or an empty map if the
    /// device has no committed bindings yet.
    #[must_use]
    pub fn bindings_for(&self, device_key: &str) -> BTreeMap<ButtonId, Binding> {
        self.devices
            .get(device_key)
            .map(|d| d.bindings.clone())
            .unwrap_or_default()
    }

    /// Records `binding` for `button` on `device_key`, creating the device
    /// entry if needed. Replaces the whole binding (use
    /// [`Self::set_gesture_direction`] to edit one direction of a gesture
    /// binding in place).
    pub fn set_binding(&mut self, device_key: &str, button: ButtonId, binding: Binding) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .bindings
            .insert(button, binding);
    }

    /// Records (or, with `action = None`, clears) the F-key `trigger` binding
    /// in the global `[keyboard]` map. Keyboard bindings are device-agnostic —
    /// one map applies across all keyboards — so this mirrors [`Self::set_binding`]
    /// minus the device key.
    pub fn set_keyboard_binding(&mut self, trigger: KeyTrigger, action: Option<Action>) {
        match action {
            Some(a) => {
                self.keyboard.bindings.insert(trigger, a);
            }
            None => {
                self.keyboard.bindings.remove(&trigger);
            }
        }
    }

    /// The global keyboard F-key bindings (read accessor).
    #[must_use]
    pub fn keyboard_bindings(&self) -> &BTreeMap<KeyTrigger, Action> {
        &self.keyboard.bindings
    }

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

    /// The single button the pre-v4 owner-locked runtime would have dispatched
    /// gestures from, inferred from the binding shapes — the owner-lock-era
    /// resolution rule, retained solely for
    /// [`Self::migrate_owner_locked_gestures`]. `None` means gestures were off.
    #[cfg(feature = "fs")]
    fn infer_gesture_owner(bindings: &BTreeMap<ButtonId, Binding>) -> Option<ButtonId> {
        // An OS-hook button left in gesture mode took the role over.
        if let Some((id, _)) = bindings
            .iter()
            .find(|(id, b)| **id != ButtonId::GestureButton && b.is_gesture())
        {
            return Some(*id);
        }
        // A dedicated HID++ gesture button explicitly assigned non-gesture
        // behavior means gestures were off.
        if matches!(
            bindings.get(&ButtonId::GestureButton),
            Some(Binding::Single(_) | Binding::LongPress(_))
        ) {
            return None;
        }
        // Default: the dedicated HID++ gesture button owns the gesture role.
        Some(ButtonId::GestureButton)
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
    /// pinned off with an explicit `Single` at its canonical default, which
    /// the capture layer leaves native.
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

    /// One-time load migration for owner-locked files (`schema_version <= 3`).
    ///
    /// Under the owner lock at most one button dispatched gestures; every other
    /// gesture-capable button could keep a dormant direction map awaiting
    /// re-selection, with [`DeviceConfig::gesture_owner`] recording the choice
    /// (absent = infer). The shape-driven model has no dormant state — a stored
    /// [`Binding::Gesture`] IS gesture mode — so this resolves the old owner
    /// and rewrites the shapes to dispatch exactly what the old config did:
    ///
    /// - the owner keeps its gesture map. A HID++ owner whose stored binding
    ///   is absent or `Single`-shaped gets the seeded default direction map
    ///   materialized: the v3 runtime seeded at projection time and dispatched
    ///   that map regardless of the stored shape, so leaving the shape
    ///   non-gesture would silently lose gestures in the rewritten file. (An
    ///   OS-hook owner is different — the v3 hook only dispatched a stored
    ///   gesture map, so a `Single` owner stays single.)
    /// - every other gesture-shaped binding is stashed into
    ///   [`DeviceConfig::disabled_gestures`] — keeping the owner-lock model's
    ///   restore-on-reselection promise — and demotes to a [`Binding::Single`]
    ///   of its `Click`, the only part of a dormant map the old runtime
    ///   dispatched;
    /// - a non-owner dedicated gesture button with no stored binding is pinned
    ///   with an explicit `Single` at its canonical default (absence would
    ///   re-enter gesture mode under the gesture-shaped default), which the
    ///   capture layer leaves native;
    /// - the consumed `gesture_owner` never serializes again — the shape is
    ///   the whole truth from here on.
    #[cfg(feature = "fs")]
    fn migrate_owner_locked_gestures(&mut self) {
        for device in self.devices.values_mut() {
            let owner = match device.gesture_owner.take() {
                Some(GestureOwner::Off) => None,
                Some(GestureOwner::Button(id)) => Some(id),
                None => Self::infer_gesture_owner(&device.bindings),
            };
            for (id, binding) in &mut device.bindings {
                if Some(*id) != owner {
                    if let Binding::Gesture(map) = binding {
                        device.disabled_gestures.insert(*id, map.clone());
                    }
                    binding.demote_to_single(default_binding(*id));
                }
            }
            if let Some(owner) = owner
                && owner.is_hidpp_gesture_source()
            {
                let seeded = || {
                    Binding::Gesture(
                        GestureDirection::ALL
                            .iter()
                            .copied()
                            .map(|d| (d, default_gesture_binding(d)))
                            .collect(),
                    )
                };
                match device.bindings.get_mut(&owner) {
                    // A stored non-gesture shape is replaced by the map v3
                    // actually dispatched.
                    Some(binding) if !binding.is_gesture() => *binding = seeded(),
                    Some(_) => {}
                    // An absent owner only needs materializing when its
                    // canonical default is not gesture-shaped (the haptic
                    // panel); an absent dedicated button already means
                    // default gesture mode.
                    None => {
                        if !default_binding_for(owner).is_gesture() {
                            device.bindings.insert(owner, seeded());
                        }
                    }
                }
            }
            if owner != Some(ButtonId::GestureButton) {
                device
                    .bindings
                    .entry(ButtonId::GestureButton)
                    .or_insert_with(|| Binding::Single(default_binding(ButtonId::GestureButton)));
            }
        }
    }

    /// Rewrite v4 transport-scoped direct keys to identity keys.
    ///
    /// `direct:046d:c08d:unit:6be9d300` names one mouse *and the cable it was
    /// plugged into*; `unit:6be9d300` names the mouse. The route it came from
    /// is kept as a link so the index survives the rename. Receiver keys are
    /// left alone — nothing on disk says which device is in a pairing slot, so
    /// they are folded at runtime instead (see `adopt_route`).
    ///
    /// A `direct:` key can appear three ways: as a device's own map key, as
    /// `selected_device`, or inside another device's `host_switch_targets` —
    /// and the last of those can name a device with no `[devices.…]` table of
    /// its own (nothing but the reference survives). The rename is computed
    /// once over every occurrence so all three are rewritten consistently,
    /// not just the ones that also own a device entry.
    ///
    /// Two entries can rename onto the same key — one mouse reached over both
    /// USB and Bluetooth-direct has a v4 entry per route — so the second one
    /// is folded in rather than inserted over the first. That is the one case
    /// where this pass would otherwise not be lossless.
    pub fn migrate_transport_scoped_keys(&mut self) {
        // A v4 direct key is `direct:<vid>:<pid>:<identity-kind>:<identity>`.
        // Splitting off the two leading id fields recovers the route to keep
        // and the identity fragment that becomes the new key.
        let parse_rename = |key: &str| -> Option<(String, String)> {
            let rest = key.strip_prefix("direct:")?;
            let mut parts = rest.splitn(3, ':');
            let vendor = parts.next()?;
            let product = parts.next()?;
            let identity = parts.next()?;
            PhysicalDeviceKey::parse(identity)?;
            Some((identity.to_string(), format!("direct:{vendor}:{product}")))
        };

        let renames: BTreeMap<String, (String, String)> = self
            .devices
            .keys()
            .cloned()
            .chain(self.selected_device.iter().cloned())
            .chain(
                self.devices
                    .values()
                    .flat_map(|device| device.host_switch_targets.iter().cloned()),
            )
            .filter_map(|key| {
                let renamed = parse_rename(&key)?;
                Some((key, renamed))
            })
            .collect();

        for (old, (new, route)) in &renames {
            let Some(mut device) = self.devices.remove(old) else {
                continue;
            };
            device.links.entry(route.clone()).or_default();
            // One device reached on two direct routes — an MX Master 3S over
            // USB and over Bluetooth-direct — has two v4 entries that rename
            // to the same identity key. Inserting would drop whichever lost
            // the `BTreeMap` ordering, bindings and all; folding is what
            // makes this phase lossless, and it is the same merge adoption
            // performs at runtime, so the second entry's disagreements land
            // as overrides on the route they were set for.
            match self.devices.get_mut(new) {
                Some(existing) => identity::fold(existing, device, route),
                None => {
                    self.devices.insert(new.clone(), device);
                }
            }
        }
        if let Some(new) = self
            .selected_device
            .as_deref()
            .and_then(|old| renames.get(old))
            .map(|(new, _)| new.clone())
        {
            self.selected_device = Some(new);
        }
        for device in self.devices.values_mut() {
            for target in &mut device.host_switch_targets {
                if let Some((new, _)) = renames.get(target) {
                    *target = new.clone();
                }
            }
        }
    }

    /// Preserve the native behavior of explicit pre-v7 thumb-wheel defaults.
    ///
    /// Before v7 the built-in pair was forward/up → right and backward/down →
    /// left. Because that pair equaled the defaults, an explicit copy in either
    /// a device or per-app profile kept the wheel native. v7 swaps the defaults
    /// after normalising device polarity; leaving an old pair in place would
    /// now request a real reversal. Rewrite only profiles whose effective pair
    /// was exactly the old default. Mixed/custom pairs already had literal
    /// diverted semantics and must remain untouched.
    #[cfg(feature = "fs")]
    fn migrate_thumbwheel_native_direction(&mut self) {
        let old_up = Action::HorizontalScrollRight;
        let old_down = Action::HorizontalScrollLeft;
        let new_up = Action::HorizontalScrollLeft;
        let new_down = Action::HorizontalScrollRight;
        let is_old_default = |binding: Option<&Binding>, old: &Action| match binding {
            None => true,
            Some(Binding::Single(action)) => action == old,
            Some(Binding::Gesture(_) | Binding::LongPress(_)) => false,
        };

        for device in self.devices.values_mut() {
            let global_up_is_old =
                is_old_default(device.bindings.get(&ButtonId::ThumbwheelScrollUp), &old_up);
            let global_down_is_old = is_old_default(
                device.bindings.get(&ButtonId::ThumbwheelScrollDown),
                &old_down,
            );

            for overlay in device.per_app_bindings.values_mut() {
                let overrides_direction = overlay.contains_key(&ButtonId::ThumbwheelScrollUp)
                    || overlay.contains_key(&ButtonId::ThumbwheelScrollDown);
                let effective_up_is_old = overlay
                    .get(&ButtonId::ThumbwheelScrollUp)
                    .map_or(global_up_is_old, |action| action == &old_up);
                let effective_down_is_old = overlay
                    .get(&ButtonId::ThumbwheelScrollDown)
                    .map_or(global_down_is_old, |action| action == &old_down);
                if overrides_direction && effective_up_is_old && effective_down_is_old {
                    // Write the complete pair. This also handles a profile that
                    // reached the old defaults by overriding one half of a
                    // custom global pair and inheriting the other half.
                    overlay.insert(ButtonId::ThumbwheelScrollUp, new_up.clone());
                    overlay.insert(ButtonId::ThumbwheelScrollDown, new_down.clone());
                }
            }

            if global_up_is_old && global_down_is_old {
                if let Some(Binding::Single(action)) =
                    device.bindings.get_mut(&ButtonId::ThumbwheelScrollUp)
                {
                    *action = new_up.clone();
                }
                if let Some(Binding::Single(action)) =
                    device.bindings.get_mut(&ButtonId::ThumbwheelScrollDown)
                {
                    *action = new_down.clone();
                }
            }
        }
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

    /// Actions Ring settings for `device_key`, falling back to defaults when
    /// the device has no saved ring configuration.
    #[must_use]
    pub fn action_ring(&self, device_key: &str) -> ActionRingConfig {
        self.devices
            .get(device_key)
            .map(|device| device.action_ring.clone())
            .unwrap_or_default()
    }

    /// Enable or disable `device_key`'s Actions Ring.
    pub fn set_action_ring_enabled(&mut self, device_key: &str, enabled: bool) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .action_ring
            .enabled = enabled;
    }

    /// Enable or disable ring hover and activation haptics.
    pub fn set_action_ring_haptics(&mut self, device_key: &str, enabled: bool) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .action_ring
            .haptics = enabled;
    }

    /// Replace or clear one slot in the default Actions Ring layout.
    pub fn set_action_ring_slot(
        &mut self,
        device_key: &str,
        slot: ActionRingSlot,
        action: Option<RingAction>,
    ) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .action_ring
            .default
            .set_action(slot, action);
    }

    /// Set or restore the action-derived icon for one default ring slot.
    pub fn set_action_ring_icon(
        &mut self,
        device_key: &str,
        slot: ActionRingSlot,
        icon: Option<ActionRingIcon>,
    ) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .action_ring
            .default
            .set_icon(slot, icon);
    }

    /// HID++ config key of the active device, if any.
    #[must_use]
    pub fn selected_device(&self) -> Option<&str> {
        self.selected_device.as_deref()
    }

    /// Update the active device. Pass `None` to clear the
    /// selection (e.g. when the previously-selected device disappears).
    pub fn set_selected_device(&mut self, key: Option<String>) {
        self.selected_device = key;
    }

    /// The ordered DPI preset list for `device_key`, or an empty `Vec` if the
    /// device has none configured yet.
    #[must_use]
    pub fn dpi_presets(&self, device_key: &str) -> Vec<Dpi> {
        self.devices
            .get(device_key)
            .map(|d| d.dpi_presets.clone())
            .unwrap_or_default()
    }

    /// Replace the DPI preset list for `device_key`. Pass an empty `Vec` to
    /// clear (the device block is kept; the field is just omitted on save
    /// thanks to `skip_serializing_if`).
    pub fn set_dpi_presets(&mut self, device_key: &str, presets: Vec<Dpi>) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .dpi_presets = presets;
    }

    /// The last-known [`DeviceIdentity`] for `device_key`, or `None` if the
    /// device has never been seen online (or was configured before identities
    /// were recorded).
    #[must_use]
    pub fn device_identity(&self, device_key: &str) -> Option<&DeviceIdentity> {
        self.devices
            .get(device_key)
            .and_then(|d| d.identity.as_ref())
    }

    /// Record (or refresh) the identity captured for `device_key` while it was
    /// online, creating the device entry if needed.
    pub fn set_device_identity(&mut self, device_key: &str, identity: DeviceIdentity) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .identity = Some(identity.without_unit_identifiers());
    }

    /// Drop everything recorded for `device_key` — identity, custom name, and
    /// per-device settings. Returns whether an entry existed.
    pub fn remove_device(&mut self, device_key: &str) -> bool {
        self.devices.remove(device_key).is_some()
    }

    /// The user-assigned name for `device_key`, if one is configured.
    #[must_use]
    pub fn device_custom_name(&self, device_key: &str) -> Option<&str> {
        self.devices
            .get(device_key)
            .and_then(|device| device.custom_name.as_deref())
    }

    /// Set the user-assigned name for `device_key`, or clear it to use the
    /// hardware model name again.
    pub fn set_device_custom_name(&mut self, device_key: &str, custom_name: Option<String>) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .custom_name = custom_name;
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

    /// Iterate every device we've recorded an identity for, as
    /// `(config_key, identity)`. Used to seed offline placeholder cards so a
    /// known device stays visible (with its panels) before any live probe.
    pub fn known_identities(&self) -> impl Iterator<Item = (&str, &DeviceIdentity)> {
        self.devices
            .iter()
            .filter_map(|(k, d)| d.identity.as_ref().map(|i| (k.as_str(), i)))
    }

    /// The lighting config for `device_key`, or `None` if unset.
    #[must_use]
    pub fn lighting(&self, device_key: &str) -> Option<Lighting> {
        self.devices
            .get(device_key)
            .and_then(|d| d.lighting.clone())
    }

    /// Replace the lighting config for `device_key`.
    pub fn set_lighting(&mut self, device_key: &str, lighting: Lighting) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .lighting = Some(lighting);
    }

    /// The saved UVC image controls for `device_key`, or `None` if never set.
    #[must_use]
    pub fn camera_controls(&self, device_key: &str) -> Option<CameraControls> {
        self.devices
            .get(device_key)
            .and_then(|d| d.camera_controls.clone())
    }

    /// Replace the saved UVC image controls for `device_key`.
    pub fn set_camera_controls(&mut self, device_key: &str, controls: CameraControls) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .camera_controls = Some(controls);
    }

    /// The saved custom camera profiles for `device_key` (name → snapshot).
    #[must_use]
    pub fn camera_profiles(&self, device_key: &str) -> BTreeMap<String, CameraControls> {
        self.devices
            .get(device_key)
            .map(|d| d.camera_profiles.clone())
            .unwrap_or_default()
    }

    /// Save (or overwrite) a custom camera profile for `device_key`.
    pub fn save_camera_profile(&mut self, device_key: &str, name: &str, snap: CameraControls) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .camera_profiles
            .insert(name.to_string(), snap);
    }

    /// Delete a custom camera profile, clearing the active selection if it
    /// named it. Unknown names are a no-op.
    pub fn delete_camera_profile(&mut self, device_key: &str, name: &str) {
        if let Some(device) = self.devices.get_mut(device_key) {
            device.camera_profiles.remove(name);
            if device.camera_profile.as_deref() == Some(name) {
                device.camera_profile = None;
            }
        }
    }

    /// The last-applied camera profile name for `device_key`, if any.
    #[must_use]
    pub fn camera_active_profile(&self, device_key: &str) -> Option<String> {
        self.devices
            .get(device_key)
            .and_then(|d| d.camera_profile.clone())
    }

    /// Record which camera profile `device_key` last applied.
    pub fn set_camera_active_profile(&mut self, device_key: &str, name: Option<String>) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .camera_profile = name;
    }

    /// The standalone-light config for `device_key`, or `None` if unset.
    #[must_use]
    pub fn light(&self, device_key: &str) -> Option<LightSettings> {
        self.devices.get(device_key).and_then(|d| d.light)
    }

    /// Replace the standalone-light config for `device_key`.
    pub fn set_light(&mut self, device_key: &str, light: LightSettings) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .light = Some(light);
    }

    /// The committed sensor DPI for `device_key`, or `None` if never set.
    #[must_use]
    pub fn dpi(&self, device_key: &str) -> Option<Dpi> {
        self.devices.get(device_key).and_then(|d| d.dpi)
    }

    /// Record the committed sensor DPI for `device_key`, so the agent can
    /// re-apply it when the device reconnects (#189).
    pub fn set_dpi(&mut self, device_key: &str, dpi: Dpi) {
        self.devices.entry(device_key.to_string()).or_default().dpi = Some(dpi);
    }

    /// The SmartShift wheel config for `device_key`, or `None` if never set.
    #[must_use]
    pub fn smartshift(&self, device_key: &str) -> Option<SmartShift> {
        self.devices.get(device_key).and_then(|d| d.smartshift)
    }

    /// The persisted keyboard Fn-lock state for `device_key`, or `None` when
    /// the user never set one (the keyboard keeps its own state).
    #[must_use]
    pub fn fn_lock(&self, device_key: &str) -> Option<bool> {
        self.devices.get(device_key).and_then(|d| d.fn_lock)
    }

    /// Record the SmartShift wheel config for `device_key`, so the agent can
    /// re-apply it when the device reconnects (#189).
    pub fn set_smartshift(&mut self, device_key: &str, smartshift: SmartShift) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .smartshift = Some(smartshift);
    }

    /// Whether `device_key`'s scroll wheel is inverted (issue #126). `false`
    /// (the native direction) for an unconfigured or absent device.
    #[must_use]
    pub fn invert_scroll(&self, device_key: &str) -> bool {
        self.devices
            .get(device_key)
            .is_some_and(|d| d.invert_scroll)
    }

    /// Set whether `device_key`'s scroll wheel is inverted. The agent reads this
    /// on the next `ReloadConfig` and applies it in the OS hook.
    pub fn set_invert_scroll(&mut self, device_key: &str, invert: bool) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .invert_scroll = invert;
    }

    /// The configured wheel resolution for `device_key`, or `None` when
    /// OpenLogi should leave the device's current resolution unchanged.
    #[must_use]
    pub fn scroll_resolution(&self, device_key: &str) -> Option<ScrollResolution> {
        self.devices
            .get(device_key)
            .and_then(|device| device.scroll_resolution)
    }

    /// Set the wheel resolution OpenLogi should restore for `device_key`.
    /// Passing `None` returns the device to its unmanaged default state.
    pub fn set_scroll_resolution(
        &mut self,
        device_key: &str,
        resolution: Option<ScrollResolution>,
    ) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .scroll_resolution = resolution;
    }

    /// Whether OpenLogi manages `device_key` at all (capture + volatile
    /// re-apply). Unconfigured devices are managed.
    #[must_use]
    pub fn device_enabled(&self, device_key: &str) -> bool {
        self.devices.get(device_key).is_none_or(|d| d.enabled)
    }

    /// Enable or disable OpenLogi's management of `device_key`.
    pub fn set_device_enabled(&mut self, device_key: &str, enabled: bool) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .enabled = enabled;
    }

    /// The effective thumb-wheel sensitivity for `device_key`: the device's
    /// override when set, else the app-wide default.
    #[must_use]
    pub fn thumbwheel_sensitivity(&self, device_key: &str) -> ThumbwheelSensitivity {
        self.devices
            .get(device_key)
            .and_then(|d| d.thumbwheel_sensitivity)
            .unwrap_or(self.app_settings.thumbwheel_sensitivity)
    }

    /// Set (or clear, with `None`) `device_key`'s thumb-wheel sensitivity
    /// override.
    pub fn set_device_thumbwheel_sensitivity(
        &mut self,
        device_key: &str,
        sensitivity: Option<ThumbwheelSensitivity>,
    ) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .thumbwheel_sensitivity = sensitivity;
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
