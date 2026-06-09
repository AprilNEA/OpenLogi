//! User configuration, persisted as TOML at the platform-standard config
//! path.
//!
//! Per-device state (button bindings, …) lives under the
//! [`Config::devices`] map, keyed by the HID++ identifier returned by
//! [`DeviceModelInfo::config_key`](crate::device::DeviceModelInfo::config_key)
//! — e.g. `"2b042"` for an MX Master 4. Schema migrations branch on
//! [`Config::schema_version`].

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::binding::{Action, Binding, ButtonId, GestureDirection, default_binding_for};
use crate::paths::{self, PathsError};

/// The schema version the current build produces. Bumped on breaking layout
/// changes; readers branch on the parsed value before consuming the rest of
/// the file.
///
/// v2 merged the per-device `button_bindings` + `gesture_bindings` maps into a
/// single `bindings: BTreeMap<ButtonId, Binding>`. A v1 file still loads (the
/// [`RawDeviceConfig`] shim folds the legacy fields) and self-heals to v2 on the
/// next save; [`Config::load_from_path`] rejects only versions *newer* than this
/// so a forward file fails loudly instead of silently losing bindings.
pub const SCHEMA_VERSION: u32 = 2;

/// Top-level config document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub schema_version: u32,
    /// Non-device-scoped preferences (autostart, tray, language, …).
    #[serde(default, skip_serializing_if = "AppSettings::is_default")]
    pub app_settings: AppSettings,
    /// HID++ `config_key` of the carousel-selected device, persisted so a
    /// restart restores the last view rather than always landing on the
    /// first paired device. `None` means "fall back to the first device".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_device: Option<String>,
    #[serde(default)]
    pub devices: BTreeMap<String, DeviceConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            app_settings: AppSettings::default(),
            selected_device: None,
            devices: BTreeMap::new(),
        }
    }
}

/// App-wide preferences not tied to any particular device.
///
/// All fields are `#[serde(default)]` so adding a new one is backward
/// compatible — old config files just keep the default for the new field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent on/off user preferences, not a state machine"
)]
pub struct AppSettings {
    /// When true, a macOS `LaunchAgent` plist at
    /// `~/Library/LaunchAgents/org.openlogi.openlogi.plist` is installed
    /// so the app starts on login (P2.2). The plist is reconciled with
    /// this field on every startup; flipping the flag and relaunching is
    /// enough to install / remove it.
    #[serde(default)]
    pub launch_at_login: bool,
    /// Opt-in update check (P2.8). **Off by default** to honour the
    /// README's "no telemetry, no auto-update poller" promise. When true,
    /// the app makes exactly one `HEAD /repos/AprilNEA/OpenLogi/releases/
    /// latest` request per launch and logs whether a newer version is
    /// available — no automatic download.
    #[serde(default)]
    pub check_for_updates: bool,
    /// True once the first-run "check for updates?" prompt has been answered
    /// (either way), so it is never shown again. The prompt is how a
    /// privacy-conscious default of `check_for_updates = false` still lets a
    /// user opt in on first launch.
    #[serde(default)]
    pub update_prompt_seen: bool,
    /// Whether OpenLogi shows a macOS menu-bar (status item) icon. `true`
    /// (default) → it lives in the menu bar, dropping the Dock icon while no
    /// window is open; `false` → it stays an ordinary Dock app with no status
    /// item. macOS-only; ignored on other platforms.
    #[serde(default = "default_true")]
    pub show_in_menu_bar: bool,
    /// UI language as a BCP-47-ish locale code matching the GUI's bundled
    /// locales (`"en"`, `"ja"`, `"ru"`, `"zh-CN"`, `"zh-HK"`, `"zh-TW"`).
    /// `None` means "follow the system locale", which the GUI resolves at
    /// startup. Stored here so a user's explicit choice survives restarts
    /// regardless of the OS setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Thumb-wheel responsiveness, on a [`MIN_THUMBWHEEL_SENSITIVITY`]–
    /// [`MAX_THUMBWHEEL_SENSITIVITY`] scale. It scales both the speed of the
    /// wheel's continuous horizontal scroll and how few rotation increments a
    /// custom wheel action needs to fire. [`DEFAULT_THUMBWHEEL_SENSITIVITY`]
    /// (the out-of-the-box value) means 1× scroll speed; the wheel is only
    /// diverted from native scrolling once this leaves the default.
    #[serde(default = "default_thumbwheel_sensitivity")]
    pub thumbwheel_sensitivity: i32,
}

/// Out-of-the-box [`AppSettings::thumbwheel_sensitivity`]. At this value the
/// wheel's horizontal scroll runs at 1× and the wheel is left to scroll
/// natively (no HID++ diversion) unless a binding diverges from its default.
pub const DEFAULT_THUMBWHEEL_SENSITIVITY: i32 = 14;
/// Lowest selectable [`AppSettings::thumbwheel_sensitivity`].
pub const MIN_THUMBWHEEL_SENSITIVITY: i32 = 1;
/// Highest selectable [`AppSettings::thumbwheel_sensitivity`].
pub const MAX_THUMBWHEEL_SENSITIVITY: i32 = 100;

impl AppSettings {
    /// `skip_serializing_if` helper: true when nothing diverges from the
    /// default, so empty settings don't clutter `config.toml`.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            launch_at_login: false,
            check_for_updates: false,
            update_prompt_seen: false,
            show_in_menu_bar: true,
            language: None,
            thumbwheel_sensitivity: DEFAULT_THUMBWHEEL_SENSITIVITY,
        }
    }
}

/// serde default for [`AppSettings::show_in_menu_bar`]: `true`, so the menu-bar
/// icon is on out of the box and configs predating the field keep that behavior.
fn default_true() -> bool {
    true
}

/// serde default for [`AppSettings::thumbwheel_sensitivity`]: keeps configs
/// predating the field at the 1× default.
const fn default_thumbwheel_sensitivity() -> i32 {
    DEFAULT_THUMBWHEEL_SENSITIVITY
}

/// Per-device RGB lighting: a single static color, brightness, and on/off.
/// Deliberately basic — per-key effects are a later addition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lighting {
    #[serde(default = "default_lighting_enabled")]
    pub enabled: bool,
    /// Static color as 6 hex digits `"RRGGBB"` (no leading `#`).
    #[serde(default = "default_lighting_color")]
    pub color: String,
    /// Brightness percent, clamped to 0–100 on load.
    #[serde(
        default = "default_lighting_brightness",
        deserialize_with = "deserialize_brightness"
    )]
    pub brightness: u8,
}

impl Default for Lighting {
    fn default() -> Self {
        Self {
            enabled: default_lighting_enabled(),
            color: default_lighting_color(),
            brightness: default_lighting_brightness(),
        }
    }
}

fn default_lighting_enabled() -> bool {
    true
}

fn default_lighting_color() -> String {
    "ffffff".to_string()
}

fn default_lighting_brightness() -> u8 {
    100
}

/// Clamp a deserialized brightness into the UI's `0..=100` range, so a
/// hand-edited `config.toml` can't feed out-of-range values into the scaling
/// math (which assumes `brightness <= 100`).
fn deserialize_brightness<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(u8::deserialize(deserializer)?.min(100))
}

/// Which control owns a device's single gesture role.
///
/// Stored explicitly — rather than inferred from which button happens to carry a
/// [`Binding::Gesture`] — so switching the gesture button never has to collapse
/// a button's gesture map to encode the choice: every gesture-capable button
/// keeps its full direction map, and only the owner is dispatched. Serialized as
/// a bare string (`"Off"` or a [`ButtonId`] name) so it stays a TOML scalar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GestureOwner {
    /// Gestures are explicitly turned off for this device.
    Off,
    /// The named button owns the gesture role.
    Button(ButtonId),
}

impl Serialize for GestureOwner {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            // "Off" can't collide with a ButtonId variant name (all CamelCase
            // control names), so the string space is unambiguous.
            GestureOwner::Off => serializer.serialize_str("Off"),
            GestureOwner::Button(id) => id.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for GestureOwner {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::IntoDeserializer;
        let s = String::deserialize(deserializer)?;
        if s == "Off" {
            return Ok(GestureOwner::Off);
        }
        ButtonId::deserialize(s.as_str().into_deserializer()).map(GestureOwner::Button)
    }
}

/// Settings scoped to a single physical device (keyed by HID++ model+ext).
///
/// Deserialization goes through [`RawDeviceConfig`] (`#[serde(from)]`) so
/// pre-v2 files — which split bindings across `button_bindings` +
/// `gesture_bindings` — fold into the unified [`Self::bindings`] map. Only
/// `bindings` is ever serialized, so a migrated file self-heals to the v2 shape
/// on its next save.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(from = "RawDeviceConfig")]
pub struct DeviceConfig {
    /// Which button owns the device's single gesture role, once the user has
    /// chosen explicitly. Absent means "infer" (the thumb pad owns gestures if
    /// present) — see [`Config::gesture_owner`]. Listed first so it serializes
    /// as a scalar ahead of the `bindings` sub-table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gesture_owner: Option<GestureOwner>,
    /// Every rebindable button's binding: a single [`Action`], or — for the
    /// gesture button (and, later, any raw-XY-capable button) — a
    /// [`Binding::Gesture`] per-direction map.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<ButtonId, Binding>,
    /// Per-application binding overlays (P1.4). Keyed by bundle identifier
    /// (e.g. `"com.microsoft.VSCode"` on macOS). When the foreground app's
    /// id matches a key here, those bindings take precedence; anything not
    /// listed falls through to `bindings`. Deliberately `Action`-valued (not
    /// `Binding`): a per-app override replaces the whole button with one
    /// action, never a per-direction gesture overlay.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_app_bindings: BTreeMap<String, BTreeMap<ButtonId, Action>>,
    /// Ordered list of DPI presets cycled through by
    /// [`Action::CycleDpiPresets`] and indexed by
    /// [`Action::SetDpiPreset`]. Empty means "no presets configured" —
    /// the cycle action becomes a no-op until the user adds at least one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dpi_presets: Vec<u32>,
    /// Per-device RGB lighting (static color + brightness + on/off). `None`
    /// until the user changes it, so it stays out of `config.toml` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lighting: Option<Lighting>,
}

/// Deserialize-only shim that folds the pre-v2 `button_bindings` +
/// `gesture_bindings` fields into [`DeviceConfig::bindings`]. Never serialized
/// (only [`DeviceConfig`] is), so reading a legacy file and saving rewrites it
/// in the v2 shape.
#[derive(Deserialize)]
struct RawDeviceConfig {
    /// Explicit gesture owner (v2.1+). Absent on older configs → `None` → the
    /// owner is inferred in [`Config::gesture_owner`].
    #[serde(default)]
    gesture_owner: Option<GestureOwner>,
    /// v2 shape — present on already-migrated files; wins on any key collision.
    #[serde(default)]
    bindings: BTreeMap<ButtonId, Binding>,
    /// Legacy v1 per-button single bindings.
    #[serde(default)]
    button_bindings: BTreeMap<ButtonId, Action>,
    /// Legacy v1 flat gesture map (implicitly the gesture button's directions).
    #[serde(default)]
    gesture_bindings: BTreeMap<GestureDirection, Action>,
    #[serde(default)]
    per_app_bindings: BTreeMap<String, BTreeMap<ButtonId, Action>>,
    #[serde(default)]
    dpi_presets: Vec<u32>,
    #[serde(default)]
    lighting: Option<Lighting>,
}

impl From<RawDeviceConfig> for DeviceConfig {
    fn from(raw: RawDeviceConfig) -> Self {
        let mut bindings = raw.bindings; // the v2 map wins on every key.

        // Re-home the legacy flat gesture map under `GestureButton`. This MUST
        // happen before folding `button_bindings`, so a legacy single
        // `button_bindings[GestureButton]` entry coexisting with a
        // `gesture_bindings` map cannot claim the slot first and silently drop
        // the whole direction map (the pre-v2 rule was "gesture entries win").
        if !raw.gesture_bindings.is_empty() {
            bindings
                .entry(ButtonId::GestureButton)
                .or_insert_with(|| Binding::Gesture(raw.gesture_bindings));
        }
        for (button, action) in raw.button_bindings {
            // A legacy `button_bindings[GestureButton]` is vestigial and must not
            // become a `Binding::Single`: the gesture button never dispatched
            // through the per-button map (it is not an OS-hook button, and its
            // plain press routes through the gesture `Click` slot — see
            // agent-core `bindings_for`). A `Single` here would be unreachable —
            // the GUI hides it and the runtime ignores it — while folding it into
            // `Click` would resurrect a dead binding as a behavior change. Drop
            // it: the gesture map (re-homed above) already owns this button, and
            // an absent entry falls back to the canonical default, exactly as
            // pre-v2.
            if button == ButtonId::GestureButton {
                continue;
            }
            bindings.entry(button).or_insert(Binding::Single(action));
        }

        DeviceConfig {
            gesture_owner: raw.gesture_owner,
            bindings,
            per_app_bindings: raw.per_app_bindings,
            dpi_presets: raw.dpi_presets,
            lighting: raw.lighting,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not resolve config path")]
    Path(#[from] PathsError),
    #[error("could not read config at {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not parse config at {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("could not write config at {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not serialize config")]
    Serialize(#[from] toml::ser::Error),
    #[error("config at {path} has unsupported schema_version {found}")]
    UnsupportedSchemaVersion { path: PathBuf, found: u32 },
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

    /// Writes the config atomically to the default user path: serialize to a
    /// sibling temp file, then rename over the target. On Unix the temp file
    /// is created with mode 0600.
    pub fn save_atomic(&self) -> Result<(), ConfigError> {
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
        let entry = self
            .devices
            .entry(device_key.to_string())
            .or_default()
            .bindings
            .entry(button)
            .or_insert_with(|| default_binding_for(button));
        entry.upgrade_to_gesture();
        if let Binding::Gesture(map) = entry {
            map.insert(direction, action);
        }
    }

    /// The button that owns `device_key`'s single gesture role, or `None` when
    /// gestures are turned off.
    ///
    /// Resolved from the explicit [`DeviceConfig::gesture_owner`] when present;
    /// otherwise inferred (see [`Self::infer_gesture_owner`]) for configs
    /// predating the field and freshly-migrated pre-v2 files. The dedicated thumb
    /// pad ([`ButtonId::GestureButton`]) owns the role by default. At most one
    /// button gestures per device.
    #[must_use]
    pub fn gesture_owner(&self, device_key: &str) -> Option<ButtonId> {
        let Some(device) = self.devices.get(device_key) else {
            // No config yet → the thumb pad is the default gesture owner.
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
        // A thumb pad explicitly demoted to a single action means gestures off.
        if matches!(
            bindings.get(&ButtonId::GestureButton),
            Some(Binding::Single(_))
        ) {
            return None;
        }
        // Default: the thumb pad owns the gesture role.
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
    /// and unbound directions are seeded from [`default_gesture_binding`] so every
    /// gesture button exposes the same full five-direction set.
    pub fn set_gesture_owner(&mut self, device_key: &str, button: ButtonId) {
        let device = self.devices.entry(device_key.to_string()).or_default();
        device.gesture_owner = Some(GestureOwner::Button(button));
        let entry = device
            .bindings
            .entry(button)
            .or_insert_with(|| default_binding_for(button));
        entry.upgrade_to_gesture();
        entry.fill_gesture_defaults();
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
        if let Some(bid) = bundle_id {
            if let Some(overlay) = device.per_app_bindings.get(bid) {
                for (k, v) in overlay {
                    out.insert(*k, Binding::Single(v.clone()));
                }
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
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("toml.tmp");
    {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            io::Write::write_all(&mut f, bytes)?;
            f.sync_all()?;
        }
        #[cfg(not(unix))]
        {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)?;
            io::Write::write_all(&mut f, bytes)?;
            f.sync_all()?;
        }
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
mod tests {
    use super::*;
    use crate::binding::{default_binding, default_gesture_binding};

    fn write_and_read(config: &Config) -> Config {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        config.save_to_path(&path).expect("save");
        Config::load_from_path(&path).expect("load")
    }

    #[test]
    fn missing_file_yields_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nonexistent.toml");
        let cfg = Config::load_from_path(&path).expect("load");
        assert_eq!(cfg.schema_version, SCHEMA_VERSION);
        assert!(cfg.devices.is_empty());
    }

    #[test]
    fn lighting_roundtrips_per_device() {
        let mut cfg = Config::default();
        cfg.set_lighting(
            "g513",
            Lighting {
                enabled: true,
                color: "00aabb".to_string(),
                brightness: 75,
            },
        );
        let restored = write_and_read(&cfg);
        assert_eq!(
            restored.lighting("g513"),
            Some(Lighting {
                enabled: true,
                color: "00aabb".to_string(),
                brightness: 75,
            })
        );
        assert_eq!(restored.lighting("absent"), None);
    }

    #[test]
    fn bindings_roundtrip_per_device() {
        let mut cfg = Config::default();
        cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Copy));
        cfg.set_binding(
            "2b042",
            ButtonId::DpiToggle,
            Binding::Single(Action::CustomShortcut(crate::binding::KeyCombo {
                modifiers: crate::binding::KeyCombo::MOD_CMD,
                key_code: 0x23, // kVK_ANSI_P
                display: "⌘P".into(),
            })),
        );
        cfg.set_binding("4082d", ButtonId::Back, Binding::Single(Action::Paste));

        let parsed = write_and_read(&cfg);

        // Per-device isolation.
        let a = parsed.bindings_for("2b042");
        assert_eq!(a.get(&ButtonId::Back), Some(&Binding::Single(Action::Copy)));
        assert_eq!(
            a.get(&ButtonId::DpiToggle),
            Some(&Binding::Single(Action::CustomShortcut(
                crate::binding::KeyCombo {
                    modifiers: crate::binding::KeyCombo::MOD_CMD,
                    key_code: 0x23,
                    display: "⌘P".into(),
                }
            )))
        );

        let b = parsed.bindings_for("4082d");
        assert_eq!(
            b.get(&ButtonId::Back),
            Some(&Binding::Single(Action::Paste))
        );
        assert_eq!(b.len(), 1, "device b should only see its own bindings");

        // Unknown device returns empty map without panic.
        assert!(parsed.bindings_for("deadbeef").is_empty());
    }

    #[test]
    fn human_readable_toml_layout() {
        let mut cfg = Config::default();
        cfg.set_binding(
            "2b042",
            ButtonId::Back,
            Binding::Single(Action::BrowserBack),
        );
        let body = toml::to_string_pretty(&cfg).expect("serialize");

        // The model id only contains [A-Za-z0-9_], so TOML emits it as a
        // bare-word table key (no surrounding quotes). The test asserts the
        // observable structure rather than locking in a specific quoting.
        assert!(body.contains("schema_version = 2"), "got: {body}");
        assert!(body.contains("[devices.2b042.bindings]"), "got: {body}");
        // A `Single` binding serializes byte-identically to the pre-v2 bare
        // `Action`, so the leaf line is unchanged.
        assert!(body.contains("Back = \"BrowserBack\""), "got: {body}");
    }

    #[test]
    fn dpi_presets_roundtrip_per_device() {
        let mut cfg = Config::default();
        cfg.set_dpi_presets("2b042", vec![800, 1600, 3200]);
        cfg.set_dpi_presets("4082d", vec![400, 1600]);

        let parsed = write_and_read(&cfg);

        assert_eq!(parsed.dpi_presets("2b042"), vec![800, 1600, 3200]);
        assert_eq!(parsed.dpi_presets("4082d"), vec![400, 1600]);
        assert!(parsed.dpi_presets("unknown").is_empty());
    }

    #[test]
    fn empty_dpi_presets_skip_serialization() {
        let mut cfg = Config::default();
        // Add a binding so the device block exists.
        cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Copy));
        cfg.set_dpi_presets("2b042", vec![800]);
        cfg.set_dpi_presets("2b042", vec![]); // clear

        let body = toml::to_string_pretty(&cfg).expect("serialize");
        assert!(
            !body.contains("dpi_presets"),
            "empty dpi_presets should be omitted: {body}"
        );
    }

    #[test]
    fn selected_device_roundtrips() {
        let mut cfg = Config::default();
        assert_eq!(cfg.selected_device(), None);
        cfg.set_selected_device(Some("2b042".into()));
        let parsed = write_and_read(&cfg);
        assert_eq!(parsed.selected_device(), Some("2b042"));
    }

    #[test]
    fn per_app_overlay_takes_precedence() {
        let mut cfg = Config::default();
        cfg.set_binding(
            "2b042",
            ButtonId::Back,
            Binding::Single(Action::BrowserBack),
        );
        cfg.set_binding(
            "2b042",
            ButtonId::Forward,
            Binding::Single(Action::BrowserForward),
        );
        cfg.set_per_app_binding(
            "2b042",
            "com.microsoft.VSCode",
            ButtonId::Back,
            Some(Action::Undo),
        );

        // Global: both buttons are browser nav.
        let global = cfg.effective_bindings("2b042", None);
        assert_eq!(
            global.get(&ButtonId::Back),
            Some(&Binding::Single(Action::BrowserBack))
        );
        assert_eq!(
            global.get(&ButtonId::Forward),
            Some(&Binding::Single(Action::BrowserForward))
        );

        // VSCode: Back overridden (wrapped as Single), Forward inherits.
        let vscode = cfg.effective_bindings("2b042", Some("com.microsoft.VSCode"));
        assert_eq!(
            vscode.get(&ButtonId::Back),
            Some(&Binding::Single(Action::Undo))
        );
        assert_eq!(
            vscode.get(&ButtonId::Forward),
            Some(&Binding::Single(Action::BrowserForward))
        );

        // Unrelated app falls through.
        let other = cfg.effective_bindings("2b042", Some("com.apple.Safari"));
        assert_eq!(
            other.get(&ButtonId::Back),
            Some(&Binding::Single(Action::BrowserBack))
        );
    }

    #[test]
    fn per_app_binding_removal_prunes_empty_app() {
        let mut cfg = Config::default();
        cfg.set_per_app_binding(
            "2b042",
            "com.example.App",
            ButtonId::Back,
            Some(Action::Copy),
        );
        cfg.set_per_app_binding("2b042", "com.example.App", ButtonId::Back, None);
        assert!(
            cfg.devices["2b042"].per_app_bindings.is_empty(),
            "removing last override should prune the app entry"
        );
    }

    #[test]
    fn app_settings_default_omits_block() {
        let cfg = Config::default();
        let body = toml::to_string_pretty(&cfg).expect("serialize");
        assert!(
            !body.contains("app_settings"),
            "default app_settings should be omitted: {body}"
        );
    }

    #[test]
    fn app_settings_launch_at_login_roundtrips() {
        let mut cfg = Config::default();
        cfg.app_settings.launch_at_login = true;
        let parsed = write_and_read(&cfg);
        assert!(parsed.app_settings.launch_at_login);
    }

    #[test]
    fn cleared_selected_device_omits_field() {
        let mut cfg = Config::default();
        cfg.set_selected_device(Some("2b042".into()));
        cfg.set_selected_device(None);
        let body = toml::to_string_pretty(&cfg).expect("serialize");
        assert!(
            !body.contains("selected_device"),
            "cleared selection should not appear: {body}"
        );
    }

    #[test]
    fn empty_device_block_is_skipped_in_output() {
        // Inserting then clearing should not leave a [devices."x"] header
        // with no bindings under it (skip_serializing_if on bindings).
        let mut cfg = Config::default();
        cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Copy));
        cfg.devices
            .get_mut("2b042")
            .expect("entry")
            .bindings
            .clear();
        let body = toml::to_string_pretty(&cfg).expect("serialize");
        assert!(
            !body.contains("Back"),
            "cleared bindings should not appear: {body}"
        );
    }

    #[test]
    fn migrates_v1_button_and_gesture_bindings() {
        // A pre-v2 file: split button_bindings + a flat gesture_bindings map.
        let v1 = "\
schema_version = 1

[devices.2b042.button_bindings]
Back = \"BrowserBack\"

[devices.2b042.gesture_bindings]
Up = \"Copy\"
Click = \"Paste\"
";
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(&path, v1).expect("write");

        // v1 still loads (version <= current) and folds into the merged map.
        let cfg = Config::load_from_path(&path).expect("load v1");
        let bindings = cfg.bindings_for("2b042");
        assert_eq!(
            bindings.get(&ButtonId::Back),
            Some(&Binding::Single(Action::BrowserBack))
        );
        let mut gesture = BTreeMap::new();
        gesture.insert(GestureDirection::Up, Action::Copy);
        gesture.insert(GestureDirection::Click, Action::Paste);
        assert_eq!(
            bindings.get(&ButtonId::GestureButton),
            Some(&Binding::Gesture(gesture))
        );

        // Saving self-heals to the v2 shape: stamped version + merged table,
        // legacy field names gone.
        let body = toml::to_string_pretty(&cfg).expect("serialize");
        assert!(body.contains("schema_version = 2"), "got: {body}");
        assert!(body.contains("[devices.2b042.bindings]"), "got: {body}");
        assert!(!body.contains("button_bindings"), "got: {body}");
        assert!(!body.contains("gesture_bindings"), "got: {body}");
    }

    #[test]
    fn migration_gesture_map_wins_over_legacy_single_gesture_button_entry() {
        // The data-loss guard: when a legacy single button_bindings[GestureButton]
        // entry coexists with a gesture_bindings map (reachable via hand-edited
        // or very old configs), the gesture map must survive — not be shadowed by
        // the single entry. Mirrors the pre-v2 "gesture entries win" rule.
        let v1 = "\
schema_version = 1

[devices.2b042.button_bindings]
GestureButton = \"MissionControl\"

[devices.2b042.gesture_bindings]
Up = \"Copy\"
Down = \"Paste\"
";
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(&path, v1).expect("write");

        let cfg = Config::load_from_path(&path).expect("load v1");
        let mut gesture = BTreeMap::new();
        gesture.insert(GestureDirection::Up, Action::Copy);
        gesture.insert(GestureDirection::Down, Action::Paste);
        assert_eq!(
            cfg.bindings_for("2b042").get(&ButtonId::GestureButton),
            Some(&Binding::Gesture(gesture)),
            "gesture map must win over the legacy single GestureButton entry"
        );
    }

    #[test]
    fn migration_drops_vestigial_lone_gesture_button_single() {
        // A v1 file with only `button_bindings[GestureButton]` and no
        // `gesture_bindings` (the pre-gesture-picker shape). That entry never
        // dispatched in v1 — the gesture button's plain press routes through the
        // gesture `Click` slot, not the per-button map — so migrating it to a
        // `Binding::Single` would leave an unreachable entry the GUI hides and the
        // runtime ignores. It must be dropped, not shadow the gesture path.
        let v1 = "\
schema_version = 1

[devices.2b042.button_bindings]
GestureButton = \"MissionControl\"
Back = \"BrowserBack\"
";
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(&path, v1).expect("write");

        let bindings = Config::load_from_path(&path)
            .expect("load v1")
            .bindings_for("2b042");
        // An ordinary button still migrates to a `Single`...
        assert_eq!(
            bindings.get(&ButtonId::Back),
            Some(&Binding::Single(Action::BrowserBack))
        );
        // ...but the vestigial gesture-button single is gone, leaving the button
        // to fall back to its canonical default rather than an unreachable entry.
        assert_eq!(bindings.get(&ButtonId::GestureButton), None);
    }

    #[test]
    fn rejects_newer_schema_version_but_accepts_v1() {
        // A future version is rejected loudly; the current and older versions
        // load (older ones migrate through the shim).
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(&path, "schema_version = 99\n").expect("write");
        assert!(matches!(
            Config::load_from_path(&path).expect_err("v99 should fail"),
            ConfigError::UnsupportedSchemaVersion { found: 99, .. }
        ));

        fs::write(&path, "schema_version = 1\n").expect("write");
        assert!(
            Config::load_from_path(&path).is_ok(),
            "v1 should still load"
        );
    }

    #[test]
    fn set_gesture_direction_upgrades_single_to_gesture() {
        let mut cfg = Config::default();
        // Start from a Single binding, then bind a swipe direction.
        cfg.set_binding(
            "2b042",
            ButtonId::Back,
            Binding::Single(Action::BrowserBack),
        );
        cfg.set_gesture_direction("2b042", ButtonId::Back, GestureDirection::Up, Action::Copy);

        match cfg.bindings_for("2b042").get(&ButtonId::Back) {
            Some(Binding::Gesture(map)) => {
                // The prior single action is preserved as the Click entry.
                assert_eq!(
                    map.get(&GestureDirection::Click),
                    Some(&Action::BrowserBack)
                );
                assert_eq!(map.get(&GestureDirection::Up), Some(&Action::Copy));
            }
            other => panic!("expected Gesture after upgrade, got {other:?}"),
        }
    }

    #[test]
    fn set_gesture_direction_on_fresh_gesture_button_seeds_click() {
        // Binding one direction on a never-configured gesture button must still
        // persist a `Click`, so the click projection is the canonical default
        // rather than `Action::None` (which reads as a no-op press).
        let mut cfg = Config::default();
        cfg.set_gesture_direction(
            "2b042",
            ButtonId::GestureButton,
            GestureDirection::Up,
            Action::Copy,
        );

        match cfg.bindings_for("2b042").get(&ButtonId::GestureButton) {
            Some(Binding::Gesture(map)) => {
                assert_eq!(map.get(&GestureDirection::Up), Some(&Action::Copy));
                assert_eq!(
                    map.get(&GestureDirection::Click),
                    Some(&crate::binding::default_gesture_binding(
                        GestureDirection::Click
                    )),
                    "a fresh gesture button must seed a Click from its default"
                );
            }
            other => panic!("expected Gesture, got {other:?}"),
        }
    }

    #[test]
    fn gesture_owner_defaults_to_thumb_pad_yields_to_oshook_and_can_be_off() {
        let mut cfg = Config::default();
        // Default: the thumb pad owns the gesture role even with no config.
        assert_eq!(cfg.gesture_owner("2b042"), Some(ButtonId::GestureButton));

        // A thumb-pad gesture binding keeps it the owner.
        cfg.set_gesture_direction(
            "2b042",
            ButtonId::GestureButton,
            GestureDirection::Up,
            Action::MissionControl,
        );
        assert_eq!(cfg.gesture_owner("2b042"), Some(ButtonId::GestureButton));

        // An explicit OS-hook gesture button takes the role over.
        cfg.set_binding(
            "2b042",
            ButtonId::Forward,
            Binding::Gesture(BTreeMap::from([(GestureDirection::Up, Action::Copy)])),
        );
        assert_eq!(cfg.gesture_owner("2b042"), Some(ButtonId::Forward));

        // Turning gestures off explicitly yields `None` (not the thumb-pad default).
        let mut off = Config::default();
        off.disable_gestures("2b042");
        assert_eq!(off.gesture_owner("2b042"), None);
    }

    #[test]
    fn set_gesture_owner_records_owner_without_destroying_other_maps() {
        let mut cfg = Config::default();
        // Customize the thumb pad's Up swipe; it is the (inferred) owner.
        cfg.set_gesture_direction(
            "2b042",
            ButtonId::GestureButton,
            GestureDirection::Up,
            Action::Copy,
        );
        assert_eq!(cfg.gesture_owner("2b042"), Some(ButtonId::GestureButton));

        // Promote Back: the owner becomes Back explicitly; the thumb pad keeps
        // its full gesture map (no destructive demotion).
        cfg.set_binding("2b042", ButtonId::Back, Action::BrowserBack.into());
        cfg.set_gesture_owner("2b042", ButtonId::Back);
        assert_eq!(cfg.gesture_owner("2b042"), Some(ButtonId::Back));

        let bindings = cfg.bindings_for("2b042");
        // Back is a full five-direction gesture button: its prior single action
        // stays as Click, and the swipe arms are seeded from defaults.
        match bindings.get(&ButtonId::Back) {
            Some(Binding::Gesture(map)) => {
                assert_eq!(
                    map.get(&GestureDirection::Click),
                    Some(&Action::BrowserBack)
                );
                assert_eq!(
                    map.get(&GestureDirection::Up),
                    Some(&default_gesture_binding(GestureDirection::Up)),
                    "a promoted button gets full default arms"
                );
            }
            other => panic!("expected Back to be a gesture binding, got {other:?}"),
        }
        // The thumb pad's customized map survived the switch intact.
        match bindings.get(&ButtonId::GestureButton) {
            Some(Binding::Gesture(map)) => {
                assert_eq!(map.get(&GestureDirection::Up), Some(&Action::Copy));
            }
            other => panic!("expected the thumb pad map preserved, got {other:?}"),
        }

        // Switching back restores the user's customization, not defaults
        // (regression guard: owner-switch used to discard the swipe arms).
        cfg.set_gesture_owner("2b042", ButtonId::GestureButton);
        assert_eq!(cfg.gesture_owner("2b042"), Some(ButtonId::GestureButton));
        match cfg.bindings_for("2b042").get(&ButtonId::GestureButton) {
            Some(Binding::Gesture(map)) => {
                assert_eq!(map.get(&GestureDirection::Up), Some(&Action::Copy));
            }
            other => panic!("expected preserved gesture map, got {other:?}"),
        }
    }

    #[test]
    fn set_gesture_owner_seeds_a_fresh_button_with_full_directions() {
        let mut cfg = Config::default();
        // The dedicated thumb pad gets the full default direction map.
        cfg.set_gesture_owner("2b042", ButtonId::GestureButton);
        match cfg.bindings_for("2b042").get(&ButtonId::GestureButton) {
            Some(Binding::Gesture(map)) => {
                for dir in GestureDirection::ALL {
                    assert_eq!(map.get(&dir), Some(&default_gesture_binding(dir)));
                }
            }
            other => panic!("expected full default gesture map, got {other:?}"),
        }

        // A fresh OS-hook button also gets all five directions, not just a Click:
        // its native action stays as Click, and the swipe arms are defaults — so
        // the GUI's shown defaults are exactly what the runtime dispatches.
        cfg.set_gesture_owner("2b042", ButtonId::Forward);
        match cfg.bindings_for("2b042").get(&ButtonId::Forward) {
            Some(Binding::Gesture(map)) => {
                assert_eq!(
                    map.get(&GestureDirection::Click),
                    Some(&default_binding(ButtonId::Forward))
                );
                for dir in [
                    GestureDirection::Up,
                    GestureDirection::Down,
                    GestureDirection::Left,
                    GestureDirection::Right,
                ] {
                    assert_eq!(map.get(&dir), Some(&default_gesture_binding(dir)));
                }
            }
            other => panic!("expected full gesture map for Forward, got {other:?}"),
        }
    }

    #[test]
    fn disable_gestures_turns_off_without_destroying_maps() {
        let mut cfg = Config::default();
        cfg.set_gesture_direction(
            "2b042",
            ButtonId::GestureButton,
            GestureDirection::Up,
            Action::Copy,
        );
        cfg.disable_gestures("2b042");
        // Off, but the thumb pad's customized map is preserved (re-enabling
        // restores it rather than resurrecting a wiped default).
        assert_eq!(cfg.gesture_owner("2b042"), None);
        match cfg.bindings_for("2b042").get(&ButtonId::GestureButton) {
            Some(Binding::Gesture(map)) => {
                assert_eq!(map.get(&GestureDirection::Up), Some(&Action::Copy));
            }
            other => panic!("expected the gesture map preserved while off, got {other:?}"),
        }
    }

    #[test]
    fn gesture_owner_field_roundtrips_as_a_scalar() {
        let mut cfg = Config::default();
        cfg.set_gesture_owner("2b042", ButtonId::Back); // explicit button
        cfg.disable_gestures("4082d"); // explicit off

        let parsed = write_and_read(&cfg);
        assert_eq!(parsed.gesture_owner("2b042"), Some(ButtonId::Back));
        assert_eq!(parsed.gesture_owner("4082d"), None);

        // The custom codec keeps it a bare TOML string (a nested table would risk
        // a value-after-table serialization error, since `bindings` is a table).
        let body = toml::to_string_pretty(&cfg).expect("serialize");
        assert!(body.contains("gesture_owner = \"Back\""), "got: {body}");
        assert!(body.contains("gesture_owner = \"Off\""), "got: {body}");
    }
}
