//! Per-device config: [`DeviceIdentity`], [`DeviceConfig`], and the
//! [`RawDeviceConfig`] migration shim that folds pre-v2 files into the
//! unified `bindings` map.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::settings::{
    CameraControls, GestureOwner, LightSettings, Lighting, ScrollResolution, SmartShift,
    ThumbwheelSensitivity, deserialize_gesture_owner,
};
use crate::binding::{Action, ActionRingConfig, Binding, ButtonId, GestureDirection};
use crate::device::{Capabilities, DeviceKind, DeviceModelInfo, LightCapabilities};
use crate::hid::Dpi;

/// Last-known identity of a device, captured while it was online so the UI can
/// render its card and the *correct* config panels before any live HID++ probe
/// completes — or while the device is asleep and can't be probed at all.
///
/// Every field is a **static property of the model**, not of the current
/// connection: an MX Master 3S has adjustable DPI whether or not it is awake.
/// That is what makes this safe to persist — it never goes stale. It is also
/// free of any per-unit identifier (no serial number, no unit id), so caching
/// it adds no privacy surface beyond the `config_key` already used as the map
/// key. Persisting identity is what stops a sleeping/just-booted mouse from
/// vanishing from the device list (and losing its Pointer/Buttons panels)
/// until a cold probe happens to win its race — see issue #159.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceIdentity {
    /// The name shown in the carousel, as resolved from the asset registry the
    /// last time the device was online.
    pub display_name: String,
    /// HID++ model identity from feature 0x0003, when available. Persisted so
    /// the GUI can resolve the same curated asset while the device is asleep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_info: Option<DeviceModelInfo>,
    /// Firmware codename, when available. Used as an asset-resolution hint and
    /// as a readable fallback for devices without curated model metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codename: Option<String>,
    /// The device's resolved [`DeviceKind`] (asset registry preferred, HID++
    /// classification as fallback).
    pub kind: DeviceKind,
    /// Configuration capabilities measured from the device's HID++ feature
    /// table. This is the field that keeps a sleeping mouse's panels visible.
    pub capabilities: Capabilities,
    /// Standalone-light controls measured by its protocol driver, if this is
    /// a non-HID++ light. Old configs omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light_capabilities: Option<LightCapabilities>,
    /// Standalone driver family that produced this identity, when applicable.
    /// Old configs and HID++ devices omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_id: Option<String>,
    /// Optional model-level identity in the OpenLogi asset registry. This is
    /// not a physical-device key and never contains a serial or OS node id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_model_id: Option<String>,
}

impl DeviceIdentity {
    /// Remove per-unit identifiers before this model snapshot is persisted.
    #[must_use]
    pub fn without_unit_identifiers(mut self) -> Self {
        if let Some(model) = &mut self.model_info {
            model.serial_number = None;
            model.unit_id = [0; 4];
        }
        self
    }
}

/// One route a device has been seen on, plus what differs on it.
///
/// The key of the containing map is [`DeviceStableId::route_key`]. The set of
/// keys doubles as the index that identifies a device while it is asleep and
/// only its route is known, which is why it is persisted rather than
/// recomputed.
///
/// [`DeviceStableId::route_key`]: crate::device_order::DeviceStableId::route_key
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LinkConfig {
    /// Capabilities measured on this link. A device may genuinely expose
    /// different features per transport — a G502 LIGHTSPEED publishes
    /// `0x2121 HiResWheel` over its receiver and not over USB — so this is
    /// recorded per link rather than per device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Capabilities>,
    /// Settings the user deliberately made different on this link. Empty for
    /// the overwhelmingly common case where a device behaves the same either
    /// way.
    #[serde(default, skip_serializing_if = "LinkOverrides::is_empty")]
    pub overrides: LinkOverrides,
}

/// Per-link settings overrides. Each `Some` shadows the device-level value
/// for as long as the device is reached by that route.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LinkOverrides {
    /// Pointer resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi: Option<Dpi>,
    /// Native wheel inversion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invert_scroll: Option<bool>,
    /// Wheel resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_resolution: Option<ScrollResolution>,
    /// Lighting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lighting: Option<Lighting>,
    /// SmartShift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smartshift: Option<SmartShift>,
}

impl LinkOverrides {
    /// Whether nothing is overridden on this link.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Settings scoped to a single physical device.
///
/// Deserialization goes through `RawDeviceConfig` (`#[serde(from)]`) so
/// pre-v2 files — which split bindings across `button_bindings` +
/// `gesture_bindings` — fold into the unified [`Self::bindings`] map. Only
/// `bindings` is ever serialized, so a migrated file is rewritten to the v2
/// shape on its next save.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "RawDeviceConfig")]
pub struct DeviceConfig {
    /// Whether OpenLogi manages this device at all. `false` leaves the device
    /// fully native: no capture session (no HID++ diversion of any control)
    /// and no volatile-settings re-apply on reconnect. Defaults to `true` and
    /// is only serialized when disabled.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// User-assigned name for this physical device. The persisted
    /// [`DeviceIdentity::display_name`] remains the hardware model name so an
    /// inventory refresh can never overwrite this alias or mistake it for
    /// model metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_name: Option<String>,
    /// Legacy owner-lock carrier, deserialize-only: the v3-and-older
    /// `gesture_owner` field, held here just long enough for the version-gated
    /// load migration (`Config::migrate_owner_locked_gestures`) to consume it.
    /// Never serialized — since v4 the binding shape is the whole truth
    /// (gesture mode is per-button; see
    /// [`Config::set_gesture_mode`](crate::config::Config::set_gesture_mode)).
    #[serde(skip_serializing)]
    // Consumed only by the `fs` half's load migration. The field stays in
    // every build: it is part of the shape serde *deserializes*, and dropping
    // it would turn an old config's key into an unknown field.
    #[cfg_attr(
        not(feature = "fs"),
        expect(clippy::allow_attributes, reason = "see above"),
        allow(dead_code, reason = "only the `fs` half's load migration reads it")
    )]
    pub(super) gesture_owner: Option<GestureOwner>,
    /// Last-known identity (name / kind / capabilities), captured while the
    /// device was online. Lets the UI render this device — with the right
    /// config panels — on a cold start before any probe, or while it sleeps.
    /// `None` for configs written before this field existed or by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<DeviceIdentity>,
    /// Routes this device has been seen on. Keys are
    /// [`DeviceStableId::route_key`]; see [`LinkConfig`].
    ///
    /// [`DeviceStableId::route_key`]: crate::device_order::DeviceStableId::route_key
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub links: BTreeMap<String, LinkConfig>,
    /// Every rebindable button's binding: a single [`Action`], an independent
    /// short/long action pair, or — for a button in gesture mode — a
    /// [`Binding::Gesture`] per-direction map.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<ButtonId, Binding>,
    /// Direction maps of buttons whose gesture mode is currently OFF, keyed by
    /// button — pure UX memory so re-enabling restores the user's customized
    /// arms exactly
    /// (see [`Config::set_gesture_mode`](crate::config::Config::set_gesture_mode)).
    /// Never dispatched: the runtime reads only `bindings`, where a demoted
    /// button is a [`Binding::Single`] of its former `Click`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub disabled_gestures: BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    /// Per-application binding overlays (P1.4). Keyed by bundle identifier
    /// (e.g. `"com.microsoft.VSCode"` on macOS). When the foreground app's
    /// id matches a key here, those bindings take precedence; anything not
    /// listed falls through to `bindings`. Deliberately `Action`-valued (not
    /// `Binding`): a per-app override replaces the whole button with one
    /// action, never a per-direction gesture overlay.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_app_bindings: BTreeMap<String, BTreeMap<ButtonId, Action>>,
    /// Host-rendered Actions Ring settings and complete per-application layouts.
    #[serde(default, skip_serializing_if = "ActionRingConfig::is_default")]
    pub action_ring: ActionRingConfig,
    /// Ordered list of DPI presets cycled through by
    /// [`Action::CycleDpiPresets`] and indexed by
    /// [`Action::SetDpiPreset`]. Empty means "no presets configured" —
    /// the cycle action becomes a no-op until the user adds at least one.
    #[serde(
        default,
        deserialize_with = "deserialize_dpi_presets",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub dpi_presets: Vec<Dpi>,
    /// The sensor DPI the user committed for this device. Persisted because
    /// the value lives in device RAM and resets on a power cycle (#189); the
    /// agent re-applies it when the device reconnects. `None` until the user
    /// first changes DPI.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_dpi",
        skip_serializing_if = "Option::is_none"
    )]
    pub dpi: Option<Dpi>,
    /// Per-device RGB lighting (static color + brightness + on/off). `None`
    /// until the user changes it, so it stays out of `config.toml` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lighting: Option<Lighting>,
    /// Per-device standalone-light settings. Separate from [`Self::lighting`],
    /// which is the existing HID++ keyboard RGB configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<LightSettings>,
    /// Per-device SmartShift wheel configuration, re-applied on reconnect for
    /// the same reason as [`Self::dpi`]. `None` until the user changes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smartshift: Option<SmartShift>,
    /// Per-webcam UVC image controls (brightness/contrast/…). `None` until the
    /// user adjusts one, so it stays out of `config.toml` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_controls: Option<CameraControls>,
    /// User-saved camera profiles (name → control snapshot). Built-in profiles
    /// (Default / Streaming / Video call) live in the GUI, not here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub camera_profiles: BTreeMap<String, CameraControls>,
    /// The camera profile last applied from the GUI, highlighted on reopen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_profile: Option<String>,
    /// Per-device thumb-wheel sensitivity override. `None` falls back to the
    /// app-wide
    /// [`AppSettings::thumbwheel_sensitivity`](crate::config::AppSettings::thumbwheel_sensitivity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbwheel_sensitivity: Option<ThumbwheelSensitivity>,
    /// Invert this device's scroll-wheel direction relative to the OS setting
    /// (issue #126): on, a wheel tick scrolls the opposite way, so a user who
    /// keeps macOS "natural scrolling" for the trackpad can have a traditional
    /// "reverse" wheel on the mouse. Vertical only; the agent applies it through
    /// the device's HID++ native wheel-inversion mode when supported. `false`
    /// (default) is the native direction, and is omitted from `config.toml`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub invert_scroll: bool,
    /// Persisted HID++ `0x2121` wheel resolution. `None` leaves the device's
    /// current resolution unmanaged and omits the field from `config.toml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_resolution: Option<ScrollResolution>,
    /// Physical config keys of pointing devices that follow this keyboard's
    /// host switch channel. The relationship is keyboard-initiated: pressing
    /// one of this device's host keys switches every listed target first, then
    /// lets the keyboard leave the current host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_switch_targets: Vec<String>,
    /// Keyboard Fn-lock state (HID++ fn inversion, `0x40a2`/`0x40a3`): `true`
    /// means the F-row sends F1–F12 without holding Fn. The state lives in
    /// device RAM per host, so the agent re-applies it on reconnect like
    /// [`Self::dpi`]. `None` means "never set — leave the keyboard alone".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fn_lock: Option<bool>,
}

impl DeviceConfig {
    /// Pointer resolution on `route_key`: the link's override when the user
    /// set one there, else the device-level value.
    #[must_use]
    pub fn effective_dpi(&self, route_key: &str) -> Option<Dpi> {
        self.link_overrides(route_key)
            .and_then(|overrides| overrides.dpi)
            .or(self.dpi)
    }

    /// Native wheel inversion on `route_key`: the link's override when the
    /// user set one there, else the device-level value.
    #[must_use]
    pub fn effective_invert_scroll(&self, route_key: &str) -> bool {
        self.link_overrides(route_key)
            .and_then(|overrides| overrides.invert_scroll)
            .unwrap_or(self.invert_scroll)
    }

    /// Wheel resolution on `route_key`: the link's override when the user set
    /// one there, else the device-level value.
    #[must_use]
    pub fn effective_scroll_resolution(&self, route_key: &str) -> Option<ScrollResolution> {
        self.link_overrides(route_key)
            .and_then(|overrides| overrides.scroll_resolution)
            .or(self.scroll_resolution)
    }

    /// Lighting on `route_key`: the link's override when the user set one
    /// there, else the device-level value.
    #[must_use]
    pub fn effective_lighting(&self, route_key: &str) -> Option<&Lighting> {
        self.link_overrides(route_key)
            .and_then(|overrides| overrides.lighting.as_ref())
            .or(self.lighting.as_ref())
    }

    /// SmartShift on `route_key`: the link's override when the user set one
    /// there, else the device-level value.
    #[must_use]
    pub fn effective_smartshift(&self, route_key: &str) -> Option<SmartShift> {
        self.link_overrides(route_key)
            .and_then(|overrides| overrides.smartshift)
            .or(self.smartshift)
    }

    fn link_overrides(&self, route_key: &str) -> Option<&LinkOverrides> {
        self.links.get(route_key).map(|link| &link.overrides)
    }

    /// Whether this entry carries anything the *user* configured, as opposed
    /// to nothing but the metadata OpenLogi writes on its own: the probed
    /// [`Self::identity`] and the [`Self::links`] route index.
    ///
    /// This is what lets [`Config::resolve_device_key`] tell a bare entry
    /// created by an identity probe from a pre-upgrade entry holding real
    /// bindings and DPI. Comparing a stripped clone against [`Default`],
    /// rather than testing fields one by one, keeps the answer honest as the
    /// struct grows: a setting added tomorrow counts from the day it exists,
    /// with nothing here to remember to update.
    ///
    /// [`Config::resolve_device_key`]: crate::config::Config::resolve_device_key
    #[must_use]
    pub fn holds_settings(&self) -> bool {
        let mut stripped = self.clone();
        stripped.identity = None;
        stripped.links = BTreeMap::new();
        stripped != Self::default()
    }
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            // A fresh entry (e.g. created by a first DPI write) must stay
            // managed — `enabled: false` is an explicit user choice only.
            enabled: true,
            custom_name: None,
            gesture_owner: None,
            identity: None,
            links: BTreeMap::new(),
            bindings: BTreeMap::new(),
            disabled_gestures: BTreeMap::new(),
            per_app_bindings: BTreeMap::new(),
            action_ring: ActionRingConfig::default(),
            dpi_presets: Vec::new(),
            dpi: None,
            lighting: None,
            light: None,
            smartshift: None,
            camera_controls: None,
            camera_profiles: BTreeMap::new(),
            camera_profile: None,
            thumbwheel_sensitivity: None,
            invert_scroll: false,
            scroll_resolution: None,
            host_switch_targets: Vec::new(),
            fn_lock: None,
        }
    }
}

/// `serde(default)` helper for `bool` fields that default to `true`.
fn default_true() -> bool {
    true
}

/// `skip_serializing_if` helper for `bool` fields whose default is `true`.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if requires a fn(&T) -> bool signature"
)]
fn is_true(b: &bool) -> bool {
    *b
}

/// `skip_serializing_if` helper for plain `bool` fields whose default is
/// `false`: keeps an unset toggle out of `config.toml` entirely.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if requires a fn(&T) -> bool signature"
)]
fn is_false(b: &bool) -> bool {
    !*b
}

fn deserialize_dpi_presets<'de, D>(deserializer: D) -> Result<Vec<Dpi>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Vec::<u32>::deserialize(deserializer)?
        .into_iter()
        .map(|value| {
            Dpi::try_from(value).map_err(|_| {
                serde::de::Error::custom(format_args!(
                    "DPI must fit the HID++ 16-bit range, got {value}"
                ))
            })
        })
        .collect()
}

fn deserialize_optional_dpi<'de, D>(deserializer: D) -> Result<Option<Dpi>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<u32>::deserialize(deserializer)?;
    value
        .map(|value| {
            Dpi::try_from(value).map_err(|_| {
                serde::de::Error::custom(format_args!(
                    "DPI must fit the HID++ 16-bit range, got {value}"
                ))
            })
        })
        .transpose()
}

/// Deserialize-only shim that folds the pre-v2 `button_bindings` +
/// `gesture_bindings` fields into [`DeviceConfig::bindings`]. Never serialized
/// (only [`DeviceConfig`] is), so reading a legacy file and saving rewrites it
/// in the v2 shape.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDeviceConfig {
    /// Explicit gesture owner (v2.1+). Absent on older configs → `None` → the
    /// owner is inferred during the version-gated migration. A
    /// present-but-invalid legacy value is tolerated as `None` for compatibility
    /// with v3-and-older behavior; current schemas reject the field first.
    #[serde(default, deserialize_with = "deserialize_gesture_owner")]
    gesture_owner: Option<GestureOwner>,
    #[serde(default)]
    identity: Option<DeviceIdentity>,
    /// See [`DeviceConfig::links`].
    #[serde(default)]
    links: BTreeMap<String, LinkConfig>,
    /// v2 shape — present on already-migrated files; wins on any key collision.
    #[serde(default)]
    bindings: BTreeMap<ButtonId, Binding>,
    /// v4 stash of turned-off gesture maps (see [`DeviceConfig::disabled_gestures`]).
    #[serde(default)]
    disabled_gestures: BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    /// Legacy v1 per-button single bindings.
    #[serde(default)]
    button_bindings: BTreeMap<ButtonId, Action>,
    /// Legacy v1 flat gesture map (implicitly the gesture button's directions).
    #[serde(default)]
    gesture_bindings: BTreeMap<GestureDirection, Action>,
    #[serde(default)]
    per_app_bindings: BTreeMap<String, BTreeMap<ButtonId, Action>>,
    #[serde(default)]
    action_ring: ActionRingConfig,
    #[serde(default, deserialize_with = "deserialize_dpi_presets")]
    dpi_presets: Vec<Dpi>,
    #[serde(default, deserialize_with = "deserialize_optional_dpi")]
    dpi: Option<Dpi>,
    #[serde(default)]
    lighting: Option<Lighting>,
    #[serde(default)]
    light: Option<LightSettings>,
    #[serde(default)]
    smartshift: Option<SmartShift>,
    #[serde(default)]
    camera_controls: Option<CameraControls>,
    #[serde(default)]
    camera_profiles: BTreeMap<String, CameraControls>,
    #[serde(default)]
    camera_profile: Option<String>,
    #[serde(default)]
    thumbwheel_sensitivity: Option<ThumbwheelSensitivity>,
    #[serde(default)]
    invert_scroll: bool,
    #[serde(default)]
    scroll_resolution: Option<ScrollResolution>,
    #[serde(default)]
    host_switch_targets: Vec<String>,
    #[serde(default)]
    fn_lock: Option<bool>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    custom_name: Option<String>,
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
            enabled: raw.enabled,
            custom_name: raw.custom_name,
            gesture_owner: raw.gesture_owner,
            identity: raw.identity.map(DeviceIdentity::without_unit_identifiers),
            links: raw.links,
            bindings,
            disabled_gestures: raw.disabled_gestures,
            per_app_bindings: raw.per_app_bindings,
            action_ring: raw.action_ring,
            dpi_presets: raw.dpi_presets,
            dpi: raw.dpi,
            lighting: raw.lighting,
            light: raw.light,
            smartshift: raw.smartshift,
            camera_controls: raw.camera_controls,
            camera_profiles: raw.camera_profiles,
            camera_profile: raw.camera_profile,
            thumbwheel_sensitivity: raw.thumbwheel_sensitivity,
            invert_scroll: raw.invert_scroll,
            scroll_resolution: raw.scroll_resolution,
            host_switch_targets: raw.host_switch_targets,
            fn_lock: raw.fn_lock,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DeviceConfig;

    #[test]
    fn host_switch_targets_round_trip_as_physical_keys() -> Result<(), Box<dyn std::error::Error>> {
        let config: DeviceConfig = toml::from_str(
            r#"host_switch_targets = [
  "receiver:keyboard:slot:1",
  "receiver:mouse:slot:2",
]"#,
        )?;

        assert_eq!(
            config.host_switch_targets,
            ["receiver:keyboard:slot:1", "receiver:mouse:slot:2"]
        );
        let serialized = toml::to_string(&config)?;
        assert!(serialized.contains("host_switch_targets"));
        Ok(())
    }
}
