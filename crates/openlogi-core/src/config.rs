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

use crate::binding::{Action, ButtonId, GestureDirection};
use crate::paths::{self, PathsError};

/// The schema version the current build produces. Bumped on breaking layout
/// changes; readers branch on the parsed value before consuming the rest of
/// the file.
pub const SCHEMA_VERSION: u32 = 1;

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

/// Settings scoped to a single physical device (keyed by HID++ model+ext).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceConfig {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub button_bindings: BTreeMap<ButtonId, Action>,
    /// Per-application binding overlays (P1.4). Keyed by bundle identifier
    /// (e.g. `"com.microsoft.VSCode"` on macOS). When the foreground app's
    /// id matches a key here, those bindings take precedence; anything not
    /// listed falls through to `button_bindings`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_app_bindings: BTreeMap<String, BTreeMap<ButtonId, Action>>,
    /// Sub-bindings for the gesture button: hold + swipe direction or a
    /// plain click. Edited via the gesture picker; the legacy single
    /// `button_bindings[GestureButton]` entry is ignored on devices that
    /// have entries here. Hardware dispatch is a P1.5 follow-up.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub gesture_bindings: BTreeMap<GestureDirection, Action>,
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
                let config: Self = toml::from_str(&text).map_err(|source| ConfigError::Parse {
                    path: path.to_path_buf(),
                    source,
                })?;
                if config.schema_version != SCHEMA_VERSION {
                    return Err(ConfigError::UnsupportedSchemaVersion {
                        path: path.to_path_buf(),
                        found: config.schema_version,
                    });
                }
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
    pub fn bindings_for(&self, device_key: &str) -> BTreeMap<ButtonId, Action> {
        self.devices
            .get(device_key)
            .map(|d| d.button_bindings.clone())
            .unwrap_or_default()
    }

    /// Records `action` as the binding for `button` on `device_key`,
    /// creating the device entry if needed.
    pub fn set_binding(&mut self, device_key: &str, button: ButtonId, action: Action) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .button_bindings
            .insert(button, action);
    }

    /// Returns the gesture sub-bindings stored for `device_key`, or an empty
    /// map if none are set yet.
    #[must_use]
    pub fn gesture_bindings_for(&self, device_key: &str) -> BTreeMap<GestureDirection, Action> {
        self.devices
            .get(device_key)
            .map(|d| d.gesture_bindings.clone())
            .unwrap_or_default()
    }

    /// Records `action` for `direction` of `device_key`'s gesture button.
    pub fn set_gesture_binding(
        &mut self,
        device_key: &str,
        direction: GestureDirection,
        action: Action,
    ) {
        self.devices
            .entry(device_key.to_string())
            .or_default()
            .gesture_bindings
            .insert(direction, action);
    }

    /// Resolve the effective binding map for `device_key`, overlaying the
    /// per-app entry for `bundle_id` (if any) on top of the global per-device
    /// `button_bindings`. Per-app values win; everything else falls through.
    ///
    /// Returns an empty map when the device has no recorded bindings yet.
    /// Callers (the GUI / hook) layer their own defaults on top.
    #[must_use]
    pub fn effective_bindings(
        &self,
        device_key: &str,
        bundle_id: Option<&str>,
    ) -> BTreeMap<ButtonId, Action> {
        let Some(device) = self.devices.get(device_key) else {
            return BTreeMap::new();
        };
        let mut out = device.button_bindings.clone();
        if let Some(bid) = bundle_id {
            if let Some(overlay) = device.per_app_bindings.get(bid) {
                for (k, v) in overlay {
                    out.insert(*k, v.clone());
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
        cfg.set_binding("2b042", ButtonId::Back, Action::Copy);
        cfg.set_binding(
            "2b042",
            ButtonId::DpiToggle,
            Action::CustomShortcut(crate::binding::KeyCombo {
                modifiers: crate::binding::KeyCombo::MOD_CMD,
                key_code: 0x23, // kVK_ANSI_P
                display: "⌘P".into(),
            }),
        );
        cfg.set_binding("4082d", ButtonId::Back, Action::Paste);

        let parsed = write_and_read(&cfg);

        // Per-device isolation.
        let a = parsed.bindings_for("2b042");
        assert_eq!(a.get(&ButtonId::Back), Some(&Action::Copy));
        assert_eq!(
            a.get(&ButtonId::DpiToggle),
            Some(&Action::CustomShortcut(crate::binding::KeyCombo {
                modifiers: crate::binding::KeyCombo::MOD_CMD,
                key_code: 0x23,
                display: "⌘P".into(),
            }))
        );

        let b = parsed.bindings_for("4082d");
        assert_eq!(b.get(&ButtonId::Back), Some(&Action::Paste));
        assert_eq!(b.len(), 1, "device b should only see its own bindings");

        // Unknown device returns empty map without panic.
        assert!(parsed.bindings_for("deadbeef").is_empty());
    }

    #[test]
    fn human_readable_toml_layout() {
        let mut cfg = Config::default();
        cfg.set_binding("2b042", ButtonId::Back, Action::BrowserBack);
        let body = toml::to_string_pretty(&cfg).expect("serialize");

        // The model id only contains [A-Za-z0-9_], so TOML emits it as a
        // bare-word table key (no surrounding quotes). The test asserts the
        // observable structure rather than locking in a specific quoting.
        assert!(body.contains("schema_version = 1"), "got: {body}");
        assert!(
            body.contains("[devices.2b042.button_bindings]"),
            "got: {body}"
        );
        assert!(body.contains("Back = \"BrowserBack\""), "got: {body}");
    }

    #[test]
    fn rejects_unknown_schema_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(&path, "schema_version = 99\n").expect("write");
        let err = Config::load_from_path(&path).expect_err("should fail");
        assert!(matches!(
            err,
            ConfigError::UnsupportedSchemaVersion { found: 99, .. }
        ));
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
        cfg.set_binding("2b042", ButtonId::Back, Action::Copy);
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
        cfg.set_binding("2b042", ButtonId::Back, Action::BrowserBack);
        cfg.set_binding("2b042", ButtonId::Forward, Action::BrowserForward);
        cfg.set_per_app_binding(
            "2b042",
            "com.microsoft.VSCode",
            ButtonId::Back,
            Some(Action::Undo),
        );

        // Global: both buttons are browser nav.
        let global = cfg.effective_bindings("2b042", None);
        assert_eq!(global.get(&ButtonId::Back), Some(&Action::BrowserBack));
        assert_eq!(
            global.get(&ButtonId::Forward),
            Some(&Action::BrowserForward)
        );

        // VSCode: Back overridden, Forward inherits.
        let vscode = cfg.effective_bindings("2b042", Some("com.microsoft.VSCode"));
        assert_eq!(vscode.get(&ButtonId::Back), Some(&Action::Undo));
        assert_eq!(
            vscode.get(&ButtonId::Forward),
            Some(&Action::BrowserForward)
        );

        // Unrelated app falls through.
        let other = cfg.effective_bindings("2b042", Some("com.apple.Safari"));
        assert_eq!(other.get(&ButtonId::Back), Some(&Action::BrowserBack));
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
        // with no bindings under it (skip_serializing_if on button_bindings).
        let mut cfg = Config::default();
        cfg.set_binding("2b042", ButtonId::Back, Action::Copy);
        cfg.devices
            .get_mut("2b042")
            .expect("entry")
            .button_bindings
            .clear();
        let body = toml::to_string_pretty(&cfg).expect("serialize");
        assert!(
            !body.contains("Back"),
            "cleared bindings should not appear: {body}"
        );
    }
}
