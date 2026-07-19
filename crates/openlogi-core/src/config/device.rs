//! Per-device config: [`DeviceIdentity`], [`DeviceConfig`], and the
//! [`RawDeviceConfig`] migration shim that folds pre-v2 files into the
//! unified `bindings` map.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::settings::{
    GestureOwner, Lighting, ScrollResolution, SmartShift, deserialize_gesture_owner,
};
use crate::binding::{Action, Binding, ButtonId, GestureDirection};
use crate::device::{Capabilities, DeviceKind, DeviceModelInfo};

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
}

/// Settings scoped to a single physical device.
///
/// Deserialization goes through `RawDeviceConfig` (`#[serde(from)]`) so
/// pre-v2 files — which split bindings across `button_bindings` +
/// `gesture_bindings` — fold into the unified [`Self::bindings`] map. Only
/// `bindings` is ever serialized, so a migrated file self-heals to the v2 shape
/// on its next save.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(from = "RawDeviceConfig")]
pub struct DeviceConfig {
    /// Which button owns the device's single gesture role, once the user has
    /// chosen explicitly. Absent means "infer" (the dedicated HID++ gesture
    /// button owns gestures if present) — see
    /// [`Config::gesture_owner`](crate::config::Config::gesture_owner). Listed
    /// first so it serializes as a scalar ahead of the `bindings` sub-table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gesture_owner: Option<GestureOwner>,
    /// Last-known identity (name / kind / capabilities), captured while the
    /// device was online. Lets the UI render this device — with the right
    /// config panels — on a cold start before any probe, or while it sleeps.
    /// `None` for configs written before this field existed or by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<DeviceIdentity>,
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
    /// The sensor DPI the user committed for this device. Persisted because
    /// the value lives in device RAM and resets on a power cycle (#189); the
    /// agent re-applies it when the device reconnects. `None` until the user
    /// first changes DPI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi: Option<u32>,
    /// Per-device RGB lighting (static color + brightness + on/off). `None`
    /// until the user changes it, so it stays out of `config.toml` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lighting: Option<Lighting>,
    /// Per-device SmartShift wheel configuration, re-applied on reconnect for
    /// the same reason as [`Self::dpi`]. `None` until the user changes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smartshift: Option<SmartShift>,
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
}

/// `skip_serializing_if` helper for plain `bool` fields whose default is
/// `false`: keeps an unset toggle out of `config.toml` entirely.
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if requires a fn(&T) -> bool signature"
)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// Deserialize-only shim that folds the pre-v2 `button_bindings` +
/// `gesture_bindings` fields into [`DeviceConfig::bindings`]. Never serialized
/// (only [`DeviceConfig`] is), so reading a legacy file and saving rewrites it
/// in the v2 shape.
#[derive(Deserialize)]
struct RawDeviceConfig {
    /// Explicit gesture owner (v2.1+). Absent on older configs → `None` → the
    /// owner is inferred in
    /// [`Config::gesture_owner`](crate::config::Config::gesture_owner). A
    /// present-but-invalid value is tolerated as `None` (infer), not a parse
    /// error — see [`deserialize_gesture_owner`].
    #[serde(default, deserialize_with = "deserialize_gesture_owner")]
    gesture_owner: Option<GestureOwner>,
    #[serde(default)]
    identity: Option<DeviceIdentity>,
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
    dpi: Option<u32>,
    #[serde(default)]
    lighting: Option<Lighting>,
    #[serde(default)]
    smartshift: Option<SmartShift>,
    #[serde(default)]
    invert_scroll: bool,
    #[serde(default)]
    scroll_resolution: Option<ScrollResolution>,
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
            identity: raw.identity,
            bindings,
            per_app_bindings: raw.per_app_bindings,
            dpi_presets: raw.dpi_presets,
            dpi: raw.dpi,
            lighting: raw.lighting,
            smartshift: raw.smartshift,
            invert_scroll: raw.invert_scroll,
            scroll_resolution: raw.scroll_resolution,
        }
    }
}
