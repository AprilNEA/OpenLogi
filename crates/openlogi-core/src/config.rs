//! User configuration, persisted as TOML at the platform-standard config
//! path.
//!
//! Per-device state (button bindings, …) lives under the
//! [`Config::devices`] map, keyed by a stable physical-device identifier such
//! as `"receiver:abc123:slot:2"`. Schema migrations branch on
//! [`Config::schema_version`].

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod device;
mod key_trigger;
mod settings;

#[cfg(test)]
mod tests;

pub use device::{DeviceConfig, DeviceIdentity};
pub use key_trigger::{KeyModifiers, KeyTrigger, KeyboardConfig, ParseTriggerError};
pub use settings::LightSettings;
pub use settings::{
    AppSettings, Appearance, AssetSourcePreference, CameraControls, DEFAULT_THUMBWHEEL_SENSITIVITY,
    GestureOwner, Lighting, MAX_THUMBWHEEL_SENSITIVITY, MIN_THUMBWHEEL_SENSITIVITY,
    SMARTSHIFT_AUTO_DISENGAGE_DEFAULT, SMARTSHIFT_MIN_AUTO_DISENGAGE, ScrollResolution, SmartShift,
    WheelMode,
};

use crate::binding::{Action, Binding, ButtonId, GestureDirection, default_binding_for};
use crate::paths::{self, PathsError};

/// The schema version the current build produces. Bumped on breaking layout
/// changes; readers branch on the parsed value before consuming the rest of
/// the file.
///
/// v3 changes the device map from model keys to physical-device keys. No v2
/// device entries are migrated because model-scoped settings cannot be assigned
/// safely when two identical devices exist.
///
/// v2 merged the per-device `button_bindings` + `gesture_bindings` maps into a
/// single `bindings: BTreeMap<ButtonId, Binding>`. A v1 file still loads (the
/// `RawDeviceConfig` shim folds the legacy fields) and self-heals to v2 on the
/// next save; [`Config::load_from_path`] rejects only versions *newer* than this
/// so a forward file fails loudly instead of silently losing bindings.
pub const SCHEMA_VERSION: u32 = 3;

/// Top-level config document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Schema version the file was written with. Compared against
    /// [`SCHEMA_VERSION`] on load: older layouts migrate, newer ones are
    /// rejected loudly rather than silently losing settings.
    pub schema_version: u32,
    /// Non-device-scoped preferences (autostart, tray, language, …).
    #[serde(default, skip_serializing_if = "AppSettings::is_default")]
    pub app_settings: AppSettings,
    /// Physical config key of the carousel-selected device, persisted so a
    /// restart restores the last view rather than always landing on the
    /// first paired device. `None` means "fall back to the first device".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_device: Option<String>,
    /// When set (see [`Self::ephemeral`]), [`Self::save_atomic`] is a no-op:
    /// this config never writes the on-disk file. Never true for a loaded or
    /// default-constructed config.
    #[serde(skip)]
    ephemeral: bool,
    /// Per-device state, keyed by the stable physical-device identifier
    /// (e.g. `"receiver:abc123:slot:2"`) so two identical models never share
    /// an entry.
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

/// Failure loading or persisting `config.toml`. The file-scoped variants
/// carry the offending path so callers can surface an actionable message.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The platform config directory could not be resolved (no home
    /// directory for the current user).
    #[error("could not resolve config path")]
    Path(#[from] PathsError),
    /// Reading the config file from disk failed.
    #[error("could not read config at {path}")]
    Read {
        /// The config file the read targeted.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The file was read but is not valid TOML for this schema.
    #[error("could not parse config at {path}")]
    Parse {
        /// The config file that failed to parse.
        path: PathBuf,
        /// The underlying TOML deserialization error.
        #[source]
        source: toml::de::Error,
    },
    /// Writing the updated config back to disk failed.
    #[error("could not write config at {path}")]
    Write {
        /// The config file the write targeted.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The in-memory config could not be serialized to TOML — a bug in the
    /// config types rather than user error, since [`Config`] always
    /// serializes cleanly.
    #[error("could not serialize config")]
    Serialize(#[from] toml::ser::Error),
    /// The file declares a `schema_version` newer than this build
    /// understands; failing loudly avoids silently dropping settings a newer
    /// build wrote.
    #[error("config at {path} has unsupported schema_version {found}")]
    UnsupportedSchemaVersion {
        /// The config file carrying the unsupported version.
        path: PathBuf,
        /// The `schema_version` the file declared.
        found: u32,
    },
}

#[allow(
    clippy::result_large_err,
    reason = "Config I/O keeps rich parse/write context and is not a hot path"
)]
impl Config {
    /// Loads the config from the default user path, returning
    /// [`Config::default`] if the file does not exist yet.
    pub fn load_or_default() -> Result<Self, ConfigError> {
        Self::load_from_path(&paths::config_path()?)
    }

    /// Same as [`Self::load_or_default`] but reads from `path`. Used by tests
    /// to avoid touching the real user config.
    pub fn load_from_path(path: &Path) -> Result<Self, ConfigError> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let mut config: Self =
                    toml::from_str(&text).map_err(|source| ConfigError::Parse {
                        path: path.to_path_buf(),
                        source,
                    })?;
                // Accept any version up to the current one: older files migrate
                // through the per-device [`RawDeviceConfig`] shim and self-heal on
                // the next save. Only a *newer* file is rejected — loudly, so a
                // downgraded binary refuses to load (and silently wipe) a config
                // it can't represent.
                if config.schema_version > SCHEMA_VERSION {
                    return Err(ConfigError::UnsupportedSchemaVersion {
                        path: path.to_path_buf(),
                        found: config.schema_version,
                    });
                }
                // Stamp the in-memory doc to the current version so a re-save
                // writes the migrated v2 shape (the device shim already folded
                // the legacy fields during deserialize).
                config.schema_version = SCHEMA_VERSION;
                Ok(config)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

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

    /// Writes the config atomically to the default user path: serialize to a
    /// sibling temp file, then rename over the target. On Unix the temp file
    /// is created with mode 0600. No-op for an [`Self::ephemeral`] config.
    pub fn save_atomic(&self) -> Result<(), ConfigError> {
        if self.ephemeral {
            return Ok(());
        }
        self.save_to_path(&paths::config_path()?)
    }

    /// Same as [`Self::save_atomic`] but writes to `path`. Used by tests.
    pub fn save_to_path(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: path.to_path_buf(),
                source,
            })?;
        }
        let body = toml::to_string_pretty(self)?;
        write_atomic(path, body.as_bytes()).map_err(|source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        })
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
    pub fn keyboard_bindings(&self) -> &std::collections::HashMap<KeyTrigger, Action> {
        &self.keyboard.bindings
    }

    /// Returns the gesture sub-bindings for `device_key`'s gesture button, or an
    /// empty map if it isn't in gesture mode. Derived from the unified
    /// [`DeviceConfig::bindings`]; kept as a convenience for the agent-side
    /// per-direction adapter.
    #[must_use]
    pub fn gesture_bindings_for(&self, device_key: &str) -> BTreeMap<GestureDirection, Action> {
        match self
            .devices
            .get(device_key)
            .and_then(|d| d.bindings.get(&ButtonId::GestureButton))
        {
            Some(Binding::Gesture(map)) => map.clone(),
            _ => BTreeMap::new(),
        }
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
    /// [`Self::set_gesture_owner`] and [`Self::set_gesture_direction`] so the two
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

    /// The button that owns `device_key`'s single gesture role, or `None` when
    /// gestures are turned off.
    ///
    /// Resolved from the explicit [`DeviceConfig::gesture_owner`] when present;
    /// otherwise inferred (see `Self::infer_gesture_owner`) for configs
    /// predating the field and freshly-migrated pre-v2 files. The dedicated
    /// HID++ gesture button ([`ButtonId::GestureButton`]) owns the role by
    /// default. At most one button gestures per device.
    #[must_use]
    pub fn gesture_owner(&self, device_key: &str) -> Option<ButtonId> {
        let Some(device) = self.devices.get(device_key) else {
            // No config yet → the dedicated HID++ gesture button is the default gesture owner.
            return Some(ButtonId::GestureButton);
        };
        match device.gesture_owner {
            Some(GestureOwner::Off) => None,
            Some(GestureOwner::Button(id)) => Some(id),
            None => Self::infer_gesture_owner(&device.bindings),
        }
    }

    /// Infer the gesture owner for a config predating the explicit
    /// [`DeviceConfig::gesture_owner`] field, from the shape of `bindings` — the
    /// pre-field behavior, so old/migrated configs keep working until the first
    /// explicit owner change stamps the field.
    fn infer_gesture_owner(bindings: &BTreeMap<ButtonId, Binding>) -> Option<ButtonId> {
        // An OS-hook button left in gesture mode took the role over.
        if let Some((id, _)) = bindings
            .iter()
            .find(|(id, b)| **id != ButtonId::GestureButton && b.is_gesture())
        {
            return Some(*id);
        }
        // A dedicated HID++ gesture button explicitly demoted to a single action means gestures off.
        if matches!(
            bindings.get(&ButtonId::GestureButton),
            Some(Binding::Single(_))
        ) {
            return None;
        }
        // Default: the dedicated HID++ gesture button owns the gesture role.
        Some(ButtonId::GestureButton)
    }

    /// Make `button` the device's sole gesture button.
    ///
    /// Records `button` as the explicit [`gesture_owner`](Self::gesture_owner), so
    /// the one-gesture-button-per-device lock is a data-model fact rather than a
    /// destructive demotion of the others — every other gesture-capable button
    /// keeps its own gesture map intact, ready to restore if re-chosen, and is
    /// simply not dispatched while it isn't the owner. `button` is given a full
    /// [`Binding::Gesture`] map: a prior [`Binding::Single`] is kept as the
    /// [`GestureDirection::Click`] action, any existing swipe arms are preserved,
    /// and unbound directions are seeded from
    /// [`default_gesture_binding`](crate::binding::default_gesture_binding) so every
    /// gesture button exposes the same full five-direction set.
    pub fn set_gesture_owner(&mut self, device_key: &str, button: ButtonId) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .gesture_owner = Some(GestureOwner::Button(button));
        self.ensure_gesture_binding(device_key, button)
            .fill_gesture_defaults();
    }

    /// Turn gestures off for `device_key`, recording the explicit "off" choice.
    /// Every button keeps its gesture map intact (nothing is destroyed), so
    /// re-selecting a gesture owner later restores its directions exactly.
    pub fn disable_gestures(&mut self, device_key: &str) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .gesture_owner = Some(GestureOwner::Off);
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
            && let Some(overlay) = device.per_app_bindings.get(bid)
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

    /// HID++ config key of the carousel-selected device, if any.
    #[must_use]
    pub fn selected_device(&self) -> Option<&str> {
        self.selected_device.as_deref()
    }

    /// Update the carousel-selected device. Pass `None` to clear the
    /// selection (e.g. when the previously-selected device disappears).
    pub fn set_selected_device(&mut self, key: Option<String>) {
        self.selected_device = key;
    }

    /// The ordered DPI preset list for `device_key`, or an empty `Vec` if the
    /// device has none configured yet.
    #[must_use]
    pub fn dpi_presets(&self, device_key: &str) -> Vec<u32> {
        self.devices
            .get(device_key)
            .map(|d| d.dpi_presets.clone())
            .unwrap_or_default()
    }

    /// Replace the DPI preset list for `device_key`. Pass an empty `Vec` to
    /// clear (the device block is kept; the field is just omitted on save
    /// thanks to `skip_serializing_if`).
    pub fn set_dpi_presets(&mut self, device_key: &str, presets: Vec<u32>) {
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
            .identity = Some(identity);
    }

    /// Whether `device_key` has a non-empty per-app binding overlay for the
    /// foreground app `app` (bundle id). Drives the menu-bar popover's "override
    /// active" badge — when the current app has its own bindings for this
    /// device, the global bindings are (partly) overridden.
    #[must_use]
    pub fn has_app_override(&self, device_key: &str, app: &str) -> bool {
        self.devices.get(device_key).is_some_and(|d| {
            d.per_app_bindings
                .get(app)
                .is_some_and(|overlay| !overlay.is_empty())
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
    pub fn dpi(&self, device_key: &str) -> Option<u32> {
        self.devices.get(device_key).and_then(|d| d.dpi)
    }

    /// Record the committed sensor DPI for `device_key`, so the agent can
    /// re-apply it when the device reconnects (#189).
    pub fn set_dpi(&mut self, device_key: &str, dpi: u32) {
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
}

/// Write `bytes` to `path` atomically via a randomized temp file + rename,
/// with the directory fsync the old hand-rolled writer lacked.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    #[cfg_attr(
        not(unix),
        expect(unused_mut, reason = "only the unix path mutates the options")
    )]
    let mut options = AtomicWriteFile::options();
    #[cfg(unix)]
    {
        use atomic_write_file::unix::OpenOptionsExt as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        // Force 0600 on every save, matching the previous writer.
        options.preserve_mode(false).mode(0o600);
    }
    let mut file = options.open(path)?;
    io::Write::write_all(&mut file, bytes)?;
    file.commit()
}
