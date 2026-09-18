//! User configuration, persisted as TOML at the platform-standard config
//! path.
//!
//! Per-device state (button bindings, …) lives under the
//! [`Config::devices`] map, keyed by the device's own identity such as
//! `"unit:6be9d300"` — or, for a device that reports none, by the route it was
//! reached on, such as `"receiver:abc123:slot:2"`. Schema migrations branch on
//! [`Config::schema_version`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

mod device;
#[cfg(feature = "fs")]
mod file;
mod function_key;
mod gestures;
mod identity;
mod key_trigger;
mod migrate;
mod per_app;
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
pub use function_key::FunctionKey;
pub use identity::canonical_device_key;
pub use key_trigger::{KeyModifiers, KeyTrigger, KeyboardConfig, ParseTriggerError};
pub use settings::LightSettings;
pub use settings::{
    AppIcon, AppSettings, Appearance, AssetSourcePreference, CameraControls, DeviceViewMode,
    Lighting, SMARTSHIFT_AUTO_DISENGAGE_DEFAULT, SMARTSHIFT_MIN_AUTO_DISENGAGE, ScrollResolution,
    SmartShift, ThumbwheelSensitivity, UiScale, VerticalScrollSensitivity, WheelMode,
};

#[cfg(all(test, feature = "fs"))]
use crate::binding::GestureDirection;
use crate::binding::{
    Action, ActionRingConfig, ActionRingIcon, ActionRingSlot, Binding, ButtonId, RingAction,
};
use crate::hid::Dpi;
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
