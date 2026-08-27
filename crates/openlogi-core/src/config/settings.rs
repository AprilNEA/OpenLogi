//! App-wide and per-device *value* settings: [`AppSettings`], [`Appearance`],
//! [`UiScale`], [`AppIcon`], [`Lighting`], [`ScrollResolution`], [`WheelMode`] /
//! [`SmartShift`], and the legacy [`GestureOwner`], plus their serde helpers.

use std::collections::BTreeMap;

use az::SaturatingAs;
use nutype::nutype;
use serde::{Deserialize, Serialize};

use crate::binding::ButtonId;
use crate::color::Rgb;
use crate::hid::{SmartShiftAutoDisengage, SmartShiftThreshold, TunableTorque};

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

/// User-selected scale for text and rem-based interface spacing.
///
/// The core stores a semantic choice rather than GPUI pixels; the desktop maps
/// each variant's percentage onto the window's rem size. Keeping the supported
/// range finite lets every layout be verified at every scale.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiScale {
    /// 90% of the standard interface size.
    Small,
    /// The standard interface size.
    #[default]
    Normal,
    /// 110% of the standard interface size.
    Large,
    /// 125% of the standard interface size.
    ExtraLarge,
}

impl UiScale {
    /// Every supported scale, in the order Settings offers them.
    pub const ALL: [Self; 4] = [Self::Small, Self::Normal, Self::Large, Self::ExtraLarge];

    /// The displayed percentage for this scale.
    #[must_use]
    pub const fn percent(self) -> u16 {
        match self {
            Self::Small => 90,
            Self::Normal => 100,
            Self::Large => 110,
            Self::ExtraLarge => 125,
        }
    }
}

/// Layout used for the Home device gallery.
///
/// This is a presentation preference: the GUI owns how each mode renders, while
/// core keeps the persisted vocabulary platform-free alongside [`Appearance`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceViewMode {
    /// Responsive cards that wrap to keep the finite device set visible.
    #[default]
    Grid,
    /// Compact full-width rows for scanning identity and status.
    List,
    /// A horizontally scrolling row navigated with previous/next controls.
    Carousel,
}

/// Which icon the app wears.
///
/// Variant names are one string doing three jobs, and all three are part of a
/// contract: the value persisted in `config.toml`, the file each alternate
/// ships as inside the macOS bundle, and the name the build compiles its source
/// document under. Renaming one renames all three.
///
/// Platform-free, like [`Appearance`]: honouring it is the frontend's business,
/// and today only macOS can — Windows embeds its icon in the executable at
/// compile time and Linux installs a fixed one from the package.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum AppIcon {
    /// The icon the app is signed with, and the one it wears until a user picks
    /// another.
    #[default]
    Openlogi,
    /// The geometric mark on a faceted, light-refracting fill.
    Prism,
}

impl AppIcon {
    /// Every icon, in the order Settings offers them.
    pub const ALL: [Self; 2] = [Self::Openlogi, Self::Prism];

    /// Whether this is the icon the installed bundle already wears — the one
    /// case a frontend applies by clearing its override rather than by handing
    /// the system a file.
    #[must_use]
    pub fn is_default(self) -> bool {
        matches!(self, Self::Openlogi)
    }
}

/// Preferred source for on-demand device assets.
///
/// `Automatic` races every built-in mirror; the other variants pin a sync to
/// one source. The GUI maps this persisted preference to the shared asset
/// client's source type, keeping endpoint URLs and npm routing out of config.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetSourcePreference {
    /// Use the first healthy built-in mirror.
    #[default]
    Automatic,
    /// Use OpenLogi's official asset endpoint.
    #[serde(rename = "openlogi")]
    OpenLogi,
    /// Use the versioned endpoint on Cloudflare's network.
    Cloudflare,
    /// Use the versioned npm packages through Fastly's network.
    Fastly,
}

/// App-wide preferences not tied to any particular device.
///
/// All fields are `#[serde(default)]` so adding a new one is backward
/// compatible — old config files just keep the default for the new field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent on/off user preferences, not a state machine"
)]
pub struct AppSettings {
    /// Start the background agent at login. **On by default**: the agent is
    /// what keeps remaps working, so a fresh install that silently died on
    /// reboot would be broken-by-default. On macOS this is a *sunk* switch:
    /// the `SMAppService` login item stays registered either way (visible and
    /// revocable under System Settings › Login Items — that consent surface
    /// is what makes the default defensible), and the agent itself reads this
    /// value when launchd starts it — off, and with no client connecting, it
    /// idles out instead of arming. On Linux/Windows the agent reconciles its
    /// autostart unit / Run-key with it. A config written before the flip
    /// keeps the value it saved.
    #[serde(default = "default_true")]
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
    /// (on Linux: no exclusive grabs at all; on macOS the agent also skips
    /// the startup Accessibility prompt). HID++-side features — DPI,
    /// SmartShift, the gesture button, the thumb wheel — are unaffected.
    /// Takes effect on agent restart.
    #[serde(default = "default_true")]
    pub capture_mouse_events: bool,
    /// Whether ordinary mouse-wheel input is replaced with a finite smooth
    /// scroll animation. **Off by default**: while enabled the OS hook
    /// suppresses eligible physical wheel events only after its non-blocking
    /// scroll worker accepts them. Trackpad and other continuous pixel input
    /// remains native. Windows' low-level hook cannot attribute wheel messages
    /// to a device, so the preference applies to every traditional mouse-wheel
    /// message there.
    #[serde(default)]
    pub smooth_scroll: bool,
    /// Distance multiplier for traditional vertical mouse-wheel input.
    /// [`VerticalScrollSensitivity::DEFAULT`] means 1×; trackpad and other
    /// continuous pixel input is never scaled.
    #[serde(default)]
    pub vertical_scroll_sensitivity: VerticalScrollSensitivity,
    /// Which app icon the user picked. Applied at launch, and whenever it
    /// changes, by whichever process owns a surface showing one — on macOS the
    /// GUI hands the choice to the Dock and writes it onto the bundle (so the
    /// icon survives a quit), and the agent restyles the menu-bar item, which
    /// is its own glyph and no one else's to set. Elsewhere it is inert.
    /// Defaults to the icon the app is signed with.
    #[serde(default)]
    pub app_icon: AppIcon,
    /// Whether the GUI automatically downloads device images from
    /// `assets.openlogi.org` when a device appears. `true` (default) keeps
    /// the current behavior; `false` makes no asset network requests at all
    /// (the app falls back to bundled art and the synthetic silhouette). A
    /// manual "Refresh assets" in Settings still fetches on demand regardless.
    /// Whether the GUI automatically downloads device images from the selected
    /// source when a device appears. `true` (default) keeps the current behavior;
    /// `false` makes no asset network requests at all (the app falls back to
    /// bundled art and the synthetic silhouette). A manual "Refresh assets" in
    /// Settings still fetches on demand regardless.
    #[serde(default = "default_true")]
    pub auto_download_assets: bool,
    /// Preferred mirror for automatic and manual device-asset downloads.
    /// Defaults to racing all built-in mirrors; `OPENLOGI_ASSETS` remains a
    /// process-level override for development and diagnostics.
    #[serde(default)]
    pub asset_source: AssetSourcePreference,
    /// UI language as a BCP-47-ish locale code matching the GUI's bundled
    /// locales (e.g. `"en"`, `"de"`, `"pt-BR"`, `"zh-CN"`, `"zh-TW"`; see the
    /// GUI's `i18n::SUPPORTED`). `None` means "follow the system locale", which
    /// the GUI resolves at startup. Stored here so a user's explicit choice
    /// survives restarts regardless of the OS setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Thumb-wheel responsiveness. It scales both the speed of the wheel's
    /// continuous horizontal or remapped vertical scroll and how few rotation
    /// increments a custom wheel action needs to fire.
    /// [`ThumbwheelSensitivity::DEFAULT`] means 1× scroll speed; the wheel is
    /// only diverted from native scrolling once this leaves the default.
    #[serde(default)]
    pub thumbwheel_sensitivity: ThumbwheelSensitivity,
    /// Light/dark appearance preference. Defaults to following the OS.
    #[serde(default)]
    pub appearance: Appearance,
    /// Text and rem-based interface scale. Defaults to 100%.
    #[serde(default)]
    pub ui_scale: UiScale,
    /// Layout used for the Home device gallery. Defaults to the responsive grid.
    #[serde(default)]
    pub device_view_mode: DeviceViewMode,
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

const SENSITIVITY_MIN: u8 = 1;
const SENSITIVITY_MAX: u8 = 100;
const SENSITIVITY_DEFAULT: u8 = 14;

/// Traditional vertical mouse-wheel responsiveness on OpenLogi's `1..=100`
/// scale.
///
/// This is deliberately distinct from [`ThumbwheelSensitivity`]: vertical
/// sensitivity changes only scroll distance and never changes a custom action
/// threshold.
#[nutype(
    const_fn,
    validate(greater_or_equal = SENSITIVITY_MIN, less_or_equal = SENSITIVITY_MAX),
    derive(
        Debug,
        Clone,
        Copy,
        PartialEq,
        Eq,
        PartialOrd,
        Ord,
        TryFrom,
        Into,
        Display,
        Serialize,
        Deserialize
    )
)]
pub struct VerticalScrollSensitivity(u8);

impl VerticalScrollSensitivity {
    /// Lowest selectable sensitivity.
    pub const MIN: Self = match Self::try_new(SENSITIVITY_MIN) {
        Ok(value) => value,
        Err(_) => panic!("valid minimum vertical scroll sensitivity"),
    };
    /// Highest selectable sensitivity.
    pub const MAX: Self = match Self::try_new(SENSITIVITY_MAX) {
        Ok(value) => value,
        Err(_) => panic!("valid maximum vertical scroll sensitivity"),
    };
    /// Out-of-the-box sensitivity. At this value scrolling runs at 1×.
    pub const DEFAULT: Self = match Self::try_new(SENSITIVITY_DEFAULT) {
        Ok(value) => value,
        Err(_) => panic!("valid default vertical scroll sensitivity"),
    };

    /// Round and clamp a floating-point slider value into the valid range.
    #[must_use]
    pub fn from_rounded(value: f32) -> Self {
        let raw = rounded_sensitivity(value);
        let Ok(value) = Self::try_new(raw) else {
            unreachable!("clamped vertical scroll sensitivity is always valid");
        };
        value
    }

    /// Vertical scroll-distance multiplier relative to [`Self::DEFAULT`].
    #[must_use]
    pub fn scroll_multiplier(self) -> f64 {
        f64::from(self.into_inner()) / f64::from(Self::DEFAULT.into_inner())
    }
}

impl Default for VerticalScrollSensitivity {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl From<VerticalScrollSensitivity> for f32 {
    fn from(sensitivity: VerticalScrollSensitivity) -> Self {
        Self::from(sensitivity.into_inner())
    }
}

/// Thumb-wheel responsiveness on OpenLogi's `1..=100` scale.
#[nutype(
    const_fn,
    validate(greater_or_equal = SENSITIVITY_MIN, less_or_equal = SENSITIVITY_MAX),
    derive(
        Debug,
        Clone,
        Copy,
        PartialEq,
        Eq,
        PartialOrd,
        Ord,
        TryFrom,
        Into,
        Display,
        Serialize,
        Deserialize
    )
)]
pub struct ThumbwheelSensitivity(u8);

impl ThumbwheelSensitivity {
    /// Lowest selectable sensitivity.
    pub const MIN: Self = match Self::try_new(SENSITIVITY_MIN) {
        Ok(value) => value,
        Err(_) => panic!("valid minimum thumb-wheel sensitivity"),
    };
    /// Highest selectable sensitivity.
    pub const MAX: Self = match Self::try_new(SENSITIVITY_MAX) {
        Ok(value) => value,
        Err(_) => panic!("valid maximum thumb-wheel sensitivity"),
    };
    /// Out-of-the-box sensitivity. At this value scrolling runs at 1× and
    /// remains native unless a thumb-wheel binding is customized.
    pub const DEFAULT: Self = match Self::try_new(SENSITIVITY_DEFAULT) {
        Ok(value) => value,
        Err(_) => panic!("valid default thumb-wheel sensitivity"),
    };

    /// Round and clamp a floating-point slider value into the valid range.
    #[must_use]
    pub fn from_rounded(value: f32) -> Self {
        let raw = rounded_sensitivity(value);
        let Ok(value) = Self::try_new(raw) else {
            unreachable!("clamped thumb-wheel sensitivity is always valid");
        };
        value
    }

    /// Continuous-scroll speed multiplier relative to [`Self::DEFAULT`].
    #[must_use]
    pub fn scroll_multiplier(self) -> f64 {
        f64::from(self.into_inner()) / f64::from(Self::DEFAULT.into_inner())
    }

    /// Rotation increments required to fire a discrete thumb-wheel action.
    #[must_use]
    pub fn action_threshold(self) -> i32 {
        (2 * i32::from(Self::DEFAULT) - i32::from(self)).max(1)
    }
}

impl Default for ThumbwheelSensitivity {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl From<ThumbwheelSensitivity> for f32 {
    fn from(sensitivity: ThumbwheelSensitivity) -> Self {
        Self::from(sensitivity.into_inner())
    }
}

impl From<ThumbwheelSensitivity> for i32 {
    fn from(sensitivity: ThumbwheelSensitivity) -> Self {
        Self::from(sensitivity.into_inner())
    }
}

fn rounded_sensitivity(value: f32) -> u8 {
    let value = if value.is_nan() {
        f32::from(SENSITIVITY_MIN)
    } else {
        value
    };
    value
        .clamp(f32::from(SENSITIVITY_MIN), f32::from(SENSITIVITY_MAX))
        .round()
        .saturating_as::<u8>()
}

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
            launch_at_login: true,
            check_for_updates: false,
            auto_install_updates: false,
            update_prompt_seen: false,
            show_in_menu_bar: true,
            capture_mouse_events: true,
            smooth_scroll: false,
            vertical_scroll_sensitivity: VerticalScrollSensitivity::DEFAULT,
            auto_download_assets: true,
            asset_source: AssetSourcePreference::Automatic,
            language: None,
            thumbwheel_sensitivity: ThumbwheelSensitivity::DEFAULT,
            appearance: Appearance::System,
            ui_scale: UiScale::Normal,
            device_view_mode: DeviceViewMode::Grid,
            app_icon: AppIcon::Openlogi,
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

/// Per-device RGB lighting: a single static color, brightness, and on/off.
/// Deliberately basic — per-key effects are a later addition.
///
/// Crosses the agent↔GUI IPC (`set_lighting`), so field order is wire format —
/// changes require a `PROTOCOL_VERSION` bump (guarded by
/// `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lighting {
    /// Master on/off for the device's lighting. The color and brightness
    /// persist while disabled, so re-enabling restores the previous look.
    #[serde(default = "default_lighting_enabled")]
    pub enabled: bool,
    /// Static color as 6 hex digits `"RRGGBB"` (no leading `#`). A value
    /// that does not parse is rejected with its TOML location.
    #[serde(
        default = "default_lighting_color",
        deserialize_with = "deserialize_lighting_color"
    )]
    pub color: Rgb,
    /// Brightness percent (`0`–`100`).
    #[serde(
        default = "default_lighting_brightness",
        deserialize_with = "deserialize_brightness"
    )]
    pub brightness: u8,
}

/// Persisted settings for a standalone light such as Logitech Litra.
///
/// Brightness is stored as a normalized percentage so the same config shape
/// works for lumen-based, percentage-based, and stepped light protocols. The
/// selected driver maps it to its native range when applying the setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightSettings {
    /// Whether the light should be on.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Link power to aggregate host-camera activity. This is a policy setting:
    /// brightness, colour temperature, and the persisted manual power choice
    /// remain independent from the transient effective power state.
    #[serde(default, skip_serializing_if = "is_false")]
    pub auto_camera: bool,
    /// Brightness across the device's advertised range.
    #[serde(
        default = "default_light_brightness",
        deserialize_with = "deserialize_brightness"
    )]
    pub brightness_percent: u8,
    /// Desired colour temperature, when the device supports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature_kelvin: Option<u16>,
    /// Optional colour for a driver that exposes RGB controls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Rgb>,
}

const fn default_light_brightness() -> u8 {
    100
}

impl Default for LightSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_camera: false,
            brightness_percent: default_light_brightness(),
            temperature_kelvin: None,
            color: None,
        }
    }
}

impl LightSettings {
    /// Create settings with a normalized brightness percentage.
    #[must_use]
    pub fn new(enabled: bool, brightness_percent: u8, temperature_kelvin: Option<u16>) -> Self {
        Self {
            enabled,
            auto_camera: false,
            brightness_percent: brightness_percent.min(100),
            temperature_kelvin,
            color: None,
        }
    }
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if requires a fn(&T) -> bool signature"
)]
const fn is_false(value: &bool) -> bool {
    !*value
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

/// Reject brightness outside the UI and hardware contract.
fn deserialize_brightness<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = u8::deserialize(deserializer)?;
    if value <= 100 {
        Ok(value)
    } else {
        Err(serde::de::Error::custom(format_args!(
            "brightness must be between 0 and 100, got {value}"
        )))
    }
}

/// Accept the optional `#` prefix supported by older releases, then parse the
/// validated RGB value.
fn deserialize_lighting_color<'de, D>(deserializer: D) -> Result<Rgb, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let color = String::deserialize(deserializer)?;
    color
        .strip_prefix('#')
        .unwrap_or(color.as_str())
        .parse()
        .map_err(serde::de::Error::custom)
}

/// Per-webcam UVC controls, keyed by control name (`brightness`, `focus`,
/// `focus_auto`, …). Each value is the raw device unit (its scale comes from
/// the camera's own min/max); auto toggles store 0/1. Persisted so values
/// survive an unplug or reboot — the GUI re-applies them over USB when the
/// camera is next viewed, since the hardware only retains them until it loses
/// power. Serializes to the same TOML table the earlier fixed-field struct
/// wrote, so existing saved controls load unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CameraControls(pub BTreeMap<String, i32>);

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
/// x2110 / x2111 spec). The sensitivity slider's default.
pub const SMARTSHIFT_AUTO_DISENGAGE_DEFAULT: SmartShiftThreshold =
    match SmartShiftThreshold::try_new(16) {
        Ok(value) => value,
        Err(_) => panic!("valid default SmartShift threshold"),
    };

/// Smallest auto-disengage threshold OpenLogi will store or apply (`8` ≈
/// 2 turn/s). Below this the ratchet releases into free-spin at everyday scroll
/// speeds, leaving the wheel "stuck" spinning (#317); `0` is also the firmware
/// "do not change" sentinel that must never be stored as a real value. A
/// persisted threshold below this floor is rejected on load.
pub const SMARTSHIFT_MIN_AUTO_DISENGAGE: SmartShiftThreshold = match SmartShiftThreshold::try_new(8)
{
    Ok(value) => value,
    Err(_) => panic!("valid minimum SmartShift threshold"),
};

/// Reject a persisted auto-disengage threshold below the supported floor.
fn deserialize_auto_disengage<'de, D>(deserializer: D) -> Result<SmartShiftAutoDisengage, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = SmartShiftAutoDisengage::deserialize(deserializer)?;
    match value {
        SmartShiftAutoDisengage::Threshold(threshold)
            if threshold < SMARTSHIFT_MIN_AUTO_DISENGAGE =>
        {
            Err(serde::de::Error::custom(format_args!(
                "SmartShift auto_disengage must be between {SMARTSHIFT_MIN_AUTO_DISENGAGE} and 255, got {threshold}"
            )))
        }
        _ => Ok(value),
    }
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
#[serde(deny_unknown_fields)]
pub struct SmartShift {
    /// The persisted wheel mode, re-applied to device RAM on reconnect.
    pub mode: WheelMode,
    /// SmartShift auto-disengage threshold (`0x08`–`0xFE`, in 0.25 turn/s
    /// steps), or `0xFF` for a permanently engaged ratchet. A persisted value
    /// below [`SMARTSHIFT_MIN_AUTO_DISENGAGE`] is rejected on load.
    #[serde(deserialize_with = "deserialize_auto_disengage")]
    pub auto_disengage: SmartShiftAutoDisengage,
    /// Firmware tunable-torque level (`1`–`255`), `0` when the device does not
    /// expose tunable torque. HID++ defines the full non-zero byte range.
    #[serde(with = "crate::hid::smartshift::optional_tunable_torque")]
    pub tunable_torque: Option<TunableTorque>,
}

/// The v3-and-older owner-lock choice: which control owned a device's single
/// gesture role. Deserialize-only since v4 — the load migration
/// (`Config::migrate_owner_locked_gestures`) consumes it and rewrites the
/// binding shapes, which are the whole truth from then on. Read as a bare TOML
/// scalar (`"Off"` or a [`ButtonId`] name).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GestureOwner {
    /// Gestures were explicitly turned off for this device.
    Off,
    /// The named button owned the gesture role.
    Button(ButtonId),
}

/// Lenient legacy deserializer for v3-and-older `gesture_owner`. Those releases
/// already treated an unknown value as absent and inferred the owner; preserving
/// that behavior keeps migration compatible. Current schemas reject the field
/// before device deserialization.
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
mod tests {
    use super::*;

    #[test]
    fn smartshift_rejects_values_outside_the_persisted_contract() {
        let parse = |auto_disengage: u8, tunable_torque: u8| {
            let body = format!(
                "mode = \"ratchet\"\nauto_disengage = {auto_disengage}\ntunable_torque = {tunable_torque}\n"
            );
            toml::from_str::<SmartShift>(&body)
        };
        let minimum = u8::from(SMARTSHIFT_MIN_AUTO_DISENGAGE);
        parse(minimum - 1, 50)
            .expect_err("auto_disengage below the persisted minimum must be rejected");
        parse(minimum, 50).expect("the minimum itself is in contract");
        parse(0xff, 0xff).expect("the top of both ranges is in contract");
        assert_eq!(
            parse(minimum, 0)
                .expect("zero torque represents unsupported hardware")
                .tunable_torque,
            None
        );
    }

    #[test]
    fn floating_thumbwheel_sensitivity_rounds_and_saturates_into_the_domain() {
        assert_eq!(u8::from(ThumbwheelSensitivity::from_rounded(49.6)), 50);
        assert_eq!(
            ThumbwheelSensitivity::from_rounded(f32::NAN),
            ThumbwheelSensitivity::MIN
        );
        assert_eq!(
            ThumbwheelSensitivity::from_rounded(f32::NEG_INFINITY),
            ThumbwheelSensitivity::MIN
        );
        assert_eq!(
            ThumbwheelSensitivity::from_rounded(f32::INFINITY),
            ThumbwheelSensitivity::MAX
        );
    }

    #[test]
    fn floating_vertical_scroll_sensitivity_rounds_and_saturates_into_the_domain() {
        assert_eq!(u8::from(VerticalScrollSensitivity::from_rounded(49.6)), 50);
        assert_eq!(
            VerticalScrollSensitivity::from_rounded(f32::NAN),
            VerticalScrollSensitivity::MIN
        );
        assert_eq!(
            VerticalScrollSensitivity::from_rounded(f32::NEG_INFINITY),
            VerticalScrollSensitivity::MIN
        );
        assert_eq!(
            VerticalScrollSensitivity::from_rounded(f32::INFINITY),
            VerticalScrollSensitivity::MAX
        );
    }
}
