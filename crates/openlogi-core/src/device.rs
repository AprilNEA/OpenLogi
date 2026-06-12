//! Serializable device-model types.
//!
//! These mirror the HID++ types from the `hidpp` crate but live here so the
//! CLI and any future GUI can depend on them without dragging in the protocol
//! crate or its async transport.

use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// What a device can be *configured* to do, derived from the HID++ feature
/// table it reports (feature `0x0001`). This is the source of truth for which
/// configuration panels the UI offers — a panel shows iff the device exposes
/// the feature that drives it. Gating on capability rather than on
/// [`DeviceKind`] is what keeps a misclassified device from losing its panels
/// (issue #127): kind is an identity guess, capability is what the firmware
/// actually announced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Capabilities {
    /// Reprogrammable buttons — HID++ `0x1b00`–`0x1b04` (ReprogControls).
    pub buttons: bool,
    /// Adjustable pointer resolution — HID++ `0x2201` / `0x2202` (AdjustableDpi).
    pub pointer: bool,
    /// Per-key RGB the lighting panel can actually drive — HID++ `PerKeyLighting`
    /// (`0x8080`), the feature `set_keyboard_color` writes. Legacy/zone/backlight
    /// lighting families aren't driven by the panel, so they don't flip this and
    /// don't earn an inert Lighting tab.
    pub lighting: bool,
}

impl Capabilities {
    /// Derive capabilities from the set of HID++ feature IDs a device reports.
    /// Membership of a driving feature ID flips the corresponding flag.
    #[must_use]
    pub fn from_feature_ids(ids: &[u16]) -> Self {
        const BUTTONS: [u16; 5] = [0x1b00, 0x1b01, 0x1b02, 0x1b03, 0x1b04];
        const POINTER: [u16; 2] = [0x2201, 0x2202];
        // Only PerKeyLighting (0x8080) — the feature the lighting panel drives via
        // `set_keyboard_color`. Advertising a non-per-key family (legacy 0x8070,
        // backlight 0x198x) would otherwise earn a tab the panel can't drive.
        const LIGHTING: [u16; 1] = [0x8080];
        let has = |family: &[u16]| ids.iter().any(|id| family.contains(id));
        Self {
            buttons: has(&BUTTONS),
            pointer: has(&POINTER),
            lighting: has(&LIGHTING),
        }
    }

    /// Best-effort capabilities for a device we could not probe (offline /
    /// never reached), guessed from its [`DeviceKind`]. Used only as a fallback
    /// when no measured [`Capabilities`] exist — a sleeping mouse should still
    /// show its button/pointer panels so its bindings (host-side) stay
    /// configurable.
    #[must_use]
    pub fn presumed_from_kind(kind: DeviceKind) -> Self {
        match kind {
            DeviceKind::Mouse | DeviceKind::Trackball => Self {
                buttons: true,
                pointer: true,
                lighting: false,
            },
            DeviceKind::Keyboard => Self {
                lighting: true,
                ..Self::default()
            },
            _ => Self::default(),
        }
    }
}

/// Coarse battery bucket reported by the device firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatteryStatus {
    Discharging,
    Charging,
    ChargingSlow,
    Full,
    Error,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatteryInfo {
    pub percentage: u8,
    pub level: BatteryLevel,
    pub status: BatteryStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct DeviceTransports {
    pub usb: bool,
    pub equad: bool,
    pub btle: bool,
    pub bluetooth: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Configuration capabilities derived from the device's HID++ feature
    /// table. `None` for devices we couldn't probe (offline / unreachable);
    /// the GUI then falls back to [`Capabilities::presumed_from_kind`].
    pub capabilities: Option<Capabilities>,
}

/// One receiver and its paired devices — the unit the agent's inventory
/// snapshot is made of.
///
/// Crosses the agent↔GUI IPC (everything it embeds too: [`ReceiverInfo`],
/// [`PairedDevice`], battery/model-info/capability types). bincode encodes
/// field and variant *order*, so reordering, retyping, or wrapping any field
/// in this tree is a wire-format change and requires a `PROTOCOL_VERSION`
/// bump (guarded by `openlogi-agent-core/tests/wire_format.rs`).
#[derive(Debug, Clone, Serialize, Deserialize)]
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

    #[test]
    fn capabilities_track_the_driving_feature_ids() {
        use super::Capabilities;
        // A typical MX mouse: ReprogControls (0x1b04) + ExtendedAdjustableDpi
        // (0x2202), no lighting.
        let mouse = Capabilities::from_feature_ids(&[0x0003, 0x1b04, 0x2202, 0x2110]);
        assert_eq!(
            mouse,
            Capabilities {
                buttons: true,
                pointer: true,
                lighting: false,
            }
        );
        // A wired G-series keyboard: PerKeyLighting (0x8080), no DPI/buttons.
        let keyboard = Capabilities::from_feature_ids(&[0x0001, 0x8080]);
        assert_eq!(
            keyboard,
            Capabilities {
                buttons: false,
                pointer: false,
                lighting: true,
            }
        );
        // No driving features → nothing offered.
        assert_eq!(
            Capabilities::from_feature_ids(&[0x0000, 0x0003]),
            Capabilities::default()
        );
    }

    #[test]
    fn presumed_capabilities_keep_an_unprobed_mouse_configurable() {
        use super::Capabilities;
        let mouse = Capabilities::presumed_from_kind(DeviceKind::Mouse);
        assert!(mouse.buttons && mouse.pointer && !mouse.lighting);
        assert!(Capabilities::presumed_from_kind(DeviceKind::Keyboard).lighting);
        // An unidentified device presumes nothing — it must be measured.
        assert_eq!(
            Capabilities::presumed_from_kind(DeviceKind::Unknown),
            Capabilities::default()
        );
    }
}
