//! App-wide and per-device *value* settings: [`AppSettings`], [`Appearance`],
//! [`Lighting`], [`ScrollResolution`], [`WheelMode`] / [`SmartShift`], and
//! [`GestureOwner`], plus
//! their serde `default_*` / `deserialize_*` helpers.

use serde::{Deserialize, Serialize};

use crate::binding::ButtonId;
use crate::color::Rgb;

/// Light/dark appearance preference. `System` follows the OS appearance (the
/// historical behaviour); `Light` / `Dark` force a mode regardless of the OS.
/// Platform-free so the core crate stays GUI-agnostic — the GUI maps this onto
/// gpui-component's `ThemeMode`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    /// Follow the operating system's light/dark setting.
    #[default]
    System,
    /// Always use the light variant of the selected theme.
    Light,
    /// Always use the dark variant of the selected theme.
    Dark,
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
    /// Opt-in automatic install. When true *and* [`Self::check_for_updates`]
    /// surfaces a newer version, the GUI downloads and stages it in the
    /// background; the update is applied on the next restart (never mid-session,
    /// and never auto-relaunched). **Off by default** — it only acts after a
    /// check the user already opted into, and stays inert in unsigned dev builds
    /// where verification fails closed.
    #[serde(default)]
    pub auto_install_updates: bool,
    /// True once the first-run "check for updates?" prompt has been answered
    /// (either way), so it is never shown again. The prompt is how a
    /// privacy-conscious default of `check_for_updates = false` still lets a
    /// user opt in on first launch.
    #[serde(default)]
    pub update_prompt_seen: bool,
    /// Whether OpenLogi shows a macOS menu-bar (status item) icon — and, on
    /// Windows, the notification-area (tray) icon. `true` (default) → the
    /// agent is visible in the menu bar / tray; `false` → it runs with no
    /// visible presence (macOS additionally keeps the ordinary Dock icon
    /// while a window is open). Ignored on Linux.
    #[serde(default = "default_true")]
    pub show_in_menu_bar: bool,
    /// Whether the agent installs the OS-level mouse hook (CGEventTap /
    /// exclusive `evdev` grab / `WH_MOUSE_LL`) that intercepts mouse events
    /// for button remapping. `true` (default) keeps remapping active;
    /// `false` is an escape hatch that leaves every input device untouched
    /// (on Linux: no exclusive grabs at all). HID++-side features — DPI,
    /// SmartShift, the gesture button, the thumb wheel — are unaffected.
    /// Takes effect on agent restart.
    #[serde(default = "default_true")]
    pub capture_mouse_events: bool,
    /// Whether the GUI automatically downloads device images from
    /// `assets.openlogi.org` when a device appears. `true` (default) keeps
    /// the current behavior; `false` makes no asset network requests at all
    /// (the app falls back to bundled art and the synthetic silhouette). A
    /// manual "Refresh assets" in Settings still fetches on demand regardless.
    #[serde(default = "default_true")]
    pub auto_download_assets: bool,
    /// UI language as a BCP-47-ish locale code matching the GUI's bundled
    /// locales (e.g. `"en"`, `"de"`, `"pt-BR"`, `"zh-CN"`, `"zh-TW"`; see the
    /// GUI's `i18n::SUPPORTED`). `None` means "follow the system locale", which
    /// the GUI resolves at startup. Stored here so a user's explicit choice
    /// survives restarts regardless of the OS setting.
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
    /// Light/dark appearance preference. Defaults to following the OS.
    #[serde(default)]
    pub appearance: Appearance,
    /// Name of the theme used in light mode (a [`crate`]-agnostic string
    /// matching a gpui-component theme, e.g. `"OpenLogi Light"`). `None` uses
    /// the OpenLogi brand light theme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_light: Option<String>,
    /// Name of the theme used in dark mode. `None` uses the OpenLogi brand dark
    /// theme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_dark: Option<String>,
    /// Corner-radius override for the UI, in pixels (the Appearance page offers
    /// `0` / `6` / `12`). `None` keeps each theme's own radius.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_radius: Option<u8>,
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
            auto_install_updates: false,
            update_prompt_seen: false,
            show_in_menu_bar: true,
            capture_mouse_events: true,
            auto_download_assets: true,
            language: None,
            thumbwheel_sensitivity: DEFAULT_THUMBWHEEL_SENSITIVITY,
            appearance: Appearance::System,
            theme_light: None,
            theme_dark: None,
            ui_radius: None,
        }
    }
}

/// serde default for the on-by-default [`AppSettings`] toggles
/// ([`AppSettings::show_in_menu_bar`], [`AppSettings::capture_mouse_events`],
/// [`AppSettings::auto_download_assets`]), so configs predating a field keep the
/// out-of-the-box behavior.
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
///
/// Crosses the agent↔GUI IPC (`set_lighting`), so field order is wire format —
/// changes require a `PROTOCOL_VERSION` bump (guarded by
/// `openlogi-agent-core/tests/wire_format.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lighting {
    /// Master on/off for the device's lighting. The color and brightness
    /// persist while disabled, so re-enabling restores the previous look.
    #[serde(default = "default_lighting_enabled")]
    pub enabled: bool,
    /// Static color as 6 hex digits `"RRGGBB"` (no leading `#`). A value
    /// that does not parse falls back to white on load — the same per-field
    /// tolerance as `brightness`, because failing the whole load would
    /// discard the user's entire config (see the `load_or_default` callers).
    #[serde(
        default = "default_lighting_color",
        deserialize_with = "deserialize_lighting_color"
    )]
    pub color: Rgb,
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

fn default_lighting_color() -> Rgb {
    Rgb::WHITE
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

/// Accept the optional `#` prefix supported by older releases, then fall back
/// to white when the configured color does not parse, mirroring the `brightness`
/// clamp above instead of failing the whole config load.
fn deserialize_lighting_color<'de, D>(deserializer: D) -> Result<Rgb, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let color = String::deserialize(deserializer)?;
    Ok(color
        .strip_prefix('#')
        .unwrap_or(color.as_str())
        .parse()
        .unwrap_or(Rgb::WHITE))
}

/// Vertical wheel reporting resolution for HID++ `0x2121 HiResWheel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollResolution {
    /// One scroll report per physical ratchet step.
    Low,
    /// Finer-grained reports between physical ratchet steps.
    High,
}

/// Scroll-wheel mode for [`SmartShift`]: free-spin or ratchet (clicky).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WheelMode {
    /// Free-spin — the wheel rotates without détentes.
    Free,
    /// Ratchet (clicky) scrolling. With SmartShift enabled the firmware
    /// auto-releases into free-spin past the configured
    /// [`auto_disengage`](SmartShift::auto_disengage) speed.
    Ratchet,
}

/// SmartShift auto-disengage out-of-box default (`16` ≈ 4 turn/s, per the
/// x2110 / x2111 spec). The sensitivity slider's default and the heal target
/// for a corrupt persisted threshold.
pub const SMARTSHIFT_AUTO_DISENGAGE_DEFAULT: u8 = 16;

/// Smallest auto-disengage threshold OpenLogi will store or apply (`8` ≈
/// 2 turn/s). Below this the ratchet releases into free-spin at everyday scroll
/// speeds, leaving the wheel "stuck" spinning (#317); `0` is also the firmware
/// "do not change" sentinel that must never be stored as a real value. A
/// persisted threshold below this floor is a corrupt artifact and is healed to
/// [`SMARTSHIFT_AUTO_DISENGAGE_DEFAULT`] on load.
pub const SMARTSHIFT_MIN_AUTO_DISENGAGE: u8 = 8;

/// Heal a persisted auto-disengage threshold on load: anything below
/// [`SMARTSHIFT_MIN_AUTO_DISENGAGE`] (including the `0` sentinel) becomes the
/// default. `0xFF` (permanent ratchet) and every real threshold at or above the
/// floor pass through unchanged.
fn deserialize_auto_disengage<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = u8::deserialize(deserializer)?;
    Ok(if value < SMARTSHIFT_MIN_AUTO_DISENGAGE {
        tracing::warn!(
            value,
            min = SMARTSHIFT_MIN_AUTO_DISENGAGE,
            default = SMARTSHIFT_AUTO_DISENGAGE_DEFAULT,
            "healed persisted SmartShift auto-disengage threshold below supported floor"
        );
        SMARTSHIFT_AUTO_DISENGAGE_DEFAULT
    } else {
        value
    })
}

/// Per-device SmartShift wheel configuration, persisted so the agent can
/// re-apply it when the device reconnects: the values are written to device
/// RAM and do not survive a power cycle (#189), despite earlier assumptions
/// that the device kept them in NVM.
///
/// Config-file only — never crosses the IPC (the agent reads it from
/// `config.toml` on reload), so it is free to evolve without a
/// `PROTOCOL_VERSION` bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartShift {
    /// The persisted wheel mode, re-applied to device RAM on reconnect.
    pub mode: WheelMode,
    /// SmartShift auto-disengage threshold (`0x08`–`0xFE`, in 0.25 turn/s
    /// steps), or `0xFF` for a permanently engaged ratchet. A persisted value
    /// below [`SMARTSHIFT_MIN_AUTO_DISENGAGE`] is healed to the default on load.
    #[serde(deserialize_with = "deserialize_auto_disengage")]
    pub auto_disengage: u8,
    /// Tunable-torque force percentage (`1`–`100`), `0` when the device
    /// doesn't support tunable torque.
    pub tunable_torque: u8,
}

/// Which control owns a device's single gesture role.
///
/// Stored explicitly — rather than inferred from which button happens to carry a
/// [`Binding::Gesture`](crate::binding::Binding::Gesture) — so switching the
/// gesture button never has to collapse a button's gesture map to encode the
/// choice: every gesture-capable button keeps its full direction map, and only
/// the owner is dispatched. Serialized as a bare string (`"Off"` or a
/// [`ButtonId`] name) so it stays a TOML scalar.
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

/// Lenient field deserializer for `RawDeviceConfig::gesture_owner`
/// (`crate::config::device`). An unrecognized or miscased value (`"back"`, a
/// typo, a future-version button name) is treated as absent — i.e. "infer the
/// owner" — rather than failing the whole-document parse and reverting *every*
/// device's settings to defaults. Mirrors [`deserialize_brightness`], which
/// clamps a bad value instead of erroring; a hand-editable config should
/// degrade one field, not the document.
pub(super) fn deserialize_gesture_owner<'de, D>(
    deserializer: D,
) -> Result<Option<GestureOwner>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    if s == "Off" {
        return Ok(Some(GestureOwner::Off));
    }
    // Parse the button name with a throwaway error type so an unknown token maps
    // to `None` (infer) rather than propagating an error.
    let button = ButtonId::deserialize(
        serde::de::value::StrDeserializer::<serde::de::value::Error>::new(&s),
    )
    .ok();
    Ok(button.map(GestureOwner::Button))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
mod tests {
    use super::*;

    #[test]
    fn low_auto_disengage_heals_to_default_on_load() {
        // A pre-#317 config could persist a runaway-low threshold (or the `0`
        // sentinel); loading it must heal to the default so reapply doesn't
        // re-program free-spin-on-any-scroll into the device — while a real
        // threshold and the `0xFF` permanent-ratchet value pass through.
        let heal = |v: u8| {
            let body = format!("mode = \"ratchet\"\nauto_disengage = {v}\ntunable_torque = 50\n");
            toml::from_str::<SmartShift>(&body)
                .expect("parse")
                .auto_disengage
        };
        assert_eq!(heal(0), SMARTSHIFT_AUTO_DISENGAGE_DEFAULT);
        assert_eq!(heal(1), SMARTSHIFT_AUTO_DISENGAGE_DEFAULT);
        assert_eq!(
            heal(SMARTSHIFT_MIN_AUTO_DISENGAGE - 1),
            SMARTSHIFT_AUTO_DISENGAGE_DEFAULT
        );
        assert_eq!(
            heal(SMARTSHIFT_MIN_AUTO_DISENGAGE),
            SMARTSHIFT_MIN_AUTO_DISENGAGE
        );
        assert_eq!(heal(16), 16);
        assert_eq!(heal(0xff), 0xff);
    }
}
