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
mod per_device;
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

use crate::binding::Action;
#[cfg(all(test, feature = "fs"))]
use crate::binding::{Binding, ButtonId, GestureDirection};
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
}
