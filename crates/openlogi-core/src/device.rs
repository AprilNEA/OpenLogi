//! Serializable device-model types.
//!
//! These mirror the HID++ types from the `hidpp` crate but live here so the
//! CLI and any future GUI can depend on them without dragging in the protocol
//! crate or its async transport.

use serde::Serialize;

/// What a paired peripheral is. Mirrors `hidpp::receiver::bolt::BoltDeviceKind`
/// but is owned by us so consumers don't depend on `hidpp`.
///
/// Several upstream "device type" vocabularies feed this one enum, and they do
/// **not** agree on numbers: the Bolt pairing register uses `Unknown=0,
/// Keyboard=1, Mouse=2, …`, while the HID++ `0x0005` feature uses
/// `Keyboard=0, …, Mouse=3, …` (no `Unknown` at all). The asset registry adds a
/// third, free-form *string* type (`"mouse"`, case-inconsistently `"MOUSE"`).
/// They are converted to this enum at their respective boundaries — never by
/// reinterpreting one source's raw byte with another's table — so the numeric
/// mismatch can't leak past those mappers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    Mouse,
    Keyboard,
    Numpad,
    Presenter,
    Remote,
    Trackball,
    Touchpad,
    Tablet,
    Gamepad,
    Joystick,
    Headset,
    Unknown,
}

impl DeviceKind {
    /// Parse the OpenLogi asset registry's `type` string into a [`DeviceKind`].
    ///
    /// The registry field is free-form and case-inconsistent (both `"mouse"`
    /// and `"MOUSE"` ship), so we case-fold before matching. Values we don't
    /// model map to [`DeviceKind::Unknown`], which callers treat as "no asset
    /// opinion" and fall back to the HID++ classification.
    #[must_use]
    pub fn from_registry_type(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "mouse" => Self::Mouse,
            "keyboard" => Self::Keyboard,
            "numpad" => Self::Numpad,
            "presenter" => Self::Presenter,
            "remote" | "remotecontrol" => Self::Remote,
            "trackball" => Self::Trackball,
            "touchpad" | "trackpad" => Self::Touchpad,
            "tablet" => Self::Tablet,
            "gamepad" => Self::Gamepad,
            "joystick" => Self::Joystick,
            "headset" => Self::Headset,
            _ => Self::Unknown,
        }
    }
}

/// Coarse battery bucket reported by the device firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BatteryLevel {
    Critical,
    Low,
    Good,
    Full,
    Unknown,
}

/// Charging state. Mirrors `hidpp 0.2`'s `BatteryStatus` plus `Unknown` for
/// values added in future protocol versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BatteryStatus {
    Discharging,
    Charging,
    ChargingSlow,
    Full,
    Error,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatteryInfo {
    pub percentage: u8,
    pub level: BatteryLevel,
    pub status: BatteryStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReceiverInfo {
    pub name: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub unique_id: Option<String>,
}

/// HID++ `DeviceInformation` (feature 0x0003) snapshot used to identify a
/// device against external registries (e.g. the OpenLogi asset index).
///
/// `model_ids` is the per-transport PID array reported by the firmware,
/// ordered to match the transports flagged in [`Self::transports`] (USB,
/// eQuad, BTLE, Bluetooth) — slots that aren't enabled stay `0`. The Logi
/// Options+ asset registry's `modelId` (e.g. `"6b023"`) is the concatenation
/// of an extended-model byte and one of these PIDs, so callers usually want
/// to format `extended_model_id` + `model_ids[N]` to match.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceModelInfo {
    pub entity_count: u8,
    /// HID++ DeviceInformation serial number, when the device supports the
    /// optional serial-number function.
    pub serial_number: Option<String>,
    pub unit_id: [u8; 4],
    pub transports: DeviceTransports,
    pub model_ids: [u16; 3],
    pub extended_model_id: u8,
}

impl DeviceModelInfo {
    /// Stable identifier used to key per-device configuration (button
    /// bindings, etc.) and to look up assets in the OpenLogi asset registry.
    ///
    /// Format: `{extended_model_id:x}{model_ids[0]:04x}` — the same string
    /// the depot `manifest.json` uses for its `modelId` field. Example: an
    /// MX Master 4 with `extended_model_id = 0x02` and `model_ids[0] = 0xb042`
    /// resolves to `"2b042"`.
    #[must_use]
    pub fn config_key(&self) -> String {
        format!("{:x}{:04x}", self.extended_model_id, self.model_ids[0])
    }
}

/// Mirror of hidpp's `DeviceTransport` bitfield — one bool per protocol the
/// device firmware exposes. The shape is dictated by HID++ feature 0x0003;
/// a state machine doesn't fit since a single device can announce multiple
/// transports simultaneously.
#[allow(
    clippy::struct_excessive_bools,
    reason = "bitfield mirroring HID++ DeviceInformation; transports are independent flags"
)]
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct DeviceTransports {
    pub usb: bool,
    pub equad: bool,
    pub btle: bool,
    pub bluetooth: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PairedDevice {
    /// Receiver-assigned slot (1..=6 for Bolt).
    pub slot: u8,
    pub codename: Option<String>,
    /// Wireless product ID. `None` for offline / unreachable devices on hidpp 0.2.
    pub wpid: Option<u16>,
    pub kind: DeviceKind,
    pub online: bool,
    pub battery: Option<BatteryInfo>,
    /// Output of HID++ feature 0x0003 — populated for online devices that
    /// expose the feature. Drives asset-registry lookups in the GUI.
    pub model_info: Option<DeviceModelInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInventory {
    pub receiver: ReceiverInfo,
    pub paired: Vec<PairedDevice>,
}

#[cfg(test)]
mod tests {
    use super::DeviceKind;

    #[test]
    fn registry_type_is_case_folded() {
        // The registry ships both `"mouse"` and `"MOUSE"`; both must resolve so
        // the asset cross-check can't silently miss a depot.
        assert_eq!(DeviceKind::from_registry_type("mouse"), DeviceKind::Mouse);
        assert_eq!(DeviceKind::from_registry_type("MOUSE"), DeviceKind::Mouse);
        assert_eq!(
            DeviceKind::from_registry_type("  Keyboard "),
            DeviceKind::Keyboard
        );
    }

    #[test]
    fn unknown_registry_type_defers_to_the_caller() {
        // Unmodelled / empty → Unknown, i.e. "no asset opinion".
        assert_eq!(
            DeviceKind::from_registry_type("webcam"),
            DeviceKind::Unknown
        );
        assert_eq!(DeviceKind::from_registry_type(""), DeviceKind::Unknown);
    }
}
