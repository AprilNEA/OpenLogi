//! Serializable device-model types.
//!
//! These mirror the HID++ types from the `hidpp` crate but live here so the
//! CLI and any future GUI can depend on them without dragging in the protocol
//! crate or its async transport.

use serde::{Deserialize, Serialize};

mod light;

pub use light::{LightCapabilities, LightValueRange, LightValueRangeError, LightValueUnit};

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
    /// Mice — the family OpenLogi's binding/DPI panels primarily target.
    Mouse,
    /// Keyboards, including lighting-capable ones.
    Keyboard,
    /// Standalone numeric keypads.
    Numpad,
    /// Presentation remotes (slide clickers).
    Presenter,
    /// Remote controls; the registry's `"remotecontrol"` string also folds here.
    Remote,
    /// Trackballs — treated like mice for presumed capabilities.
    Trackball,
    /// External touchpads; the registry's `"trackpad"` string also folds here.
    Touchpad,
    /// Pen/graphics tablets.
    Tablet,
    /// Game controllers, mirrored from the Bolt pairing vocabulary.
    Gamepad,
    /// Joysticks, mirrored from the Bolt pairing vocabulary.
    Joystick,
    /// Audio headsets paired through a receiver.
    Headset,
    /// Logitech webcam (UVC), configured through `openlogi-camera`.
    Camera,
    /// Not classified by any source.
    Unknown,
    /// Standalone light or other illumination device controlled outside HID++.
    ///
    /// This is an identity hint only. UI controls are gated by the dedicated
    /// light capability descriptor, never by this variant alone.
    Light,
}

impl DeviceKind {
    /// Parse the OpenLogi asset registry's `type` string into a [`DeviceKind`].
    ///
    /// The registry field is free-form and case-inconsistent (both `"mouse"`
    /// and `"MOUSE"` ship), so we case-fold before matching. `None` for values
    /// we don't model — "no asset opinion", typed as the absence of one rather
    /// than overloading [`DeviceKind::Unknown`], which stays a real wire value
    /// a live device can carry.
    #[must_use]
    pub fn from_registry_type(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "mouse" => Some(Self::Mouse),
            "keyboard" => Some(Self::Keyboard),
            "numpad" => Some(Self::Numpad),
            "presenter" => Some(Self::Presenter),
            "remote" | "remotecontrol" => Some(Self::Remote),
            "trackball" => Some(Self::Trackball),
            "touchpad" | "trackpad" => Some(Self::Touchpad),
            "tablet" => Some(Self::Tablet),
            "gamepad" => Some(Self::Gamepad),
            "joystick" => Some(Self::Joystick),
            "headset" => Some(Self::Headset),
            "camera" => Some(Self::Camera),
            "light" | "lighting" | "illumination_light" => Some(Self::Light),
            _ => None,
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
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "capabilities is a serialized feature-bit DTO; independent booleans keep the IPC/config shape explicit"
)]
pub struct Capabilities {
    /// Reprogrammable buttons — HID++ `0x1b00`–`0x1b04` (ReprogControls).
    pub buttons: bool,
    /// Adjustable pointer resolution — HID++ `0x2201` / `0x2202` (AdjustableDpi).
    pub pointer: bool,
    /// Solid-colour RGB the lighting panel can actually drive — HID++
    /// `ColorLedEffects` (`0x8070`) or `PerKeyLighting` (`0x8080`), the features
    /// `set_keyboard_color` writes. Backlight-only families aren't driven by the
    /// panel, so they don't flip this and don't earn an inert Lighting tab.
    pub lighting: bool,
    /// Native vertical wheel inversion — HID++ `0x2121 HiResWheel` with the
    /// firmware-reported `has_invert` capability.
    pub scroll_inversion: bool,
    /// HID++ `0x2121 HiResWheel` is present, so the wheel reporting resolution
    /// can be read and changed independently of inversion support.
    #[serde(default)]
    pub hires_wheel: bool,
    /// A horizontal thumb wheel is available: either the dedicated HID++
    /// `0x2150 Thumbwheel` feature or a legacy `0x6501 Gestures2` descriptor
    /// (gesture id 46, used by MX Master 2S).
    #[serde(default)]
    pub thumbwheel: bool,
    /// Programmable haptic feedback — reverse-engineered HID++ `0x19b0`.
    #[serde(default)]
    pub haptic_feedback: bool,
    /// A divertable Haptic Sense Panel control (`0x01a0`) was found in the
    /// device's `0x1b04` control table.
    #[serde(default)]
    pub haptic_panel: bool,
}

impl Capabilities {
    /// Derive capabilities from the set of HID++ feature IDs a device reports.
    /// Membership of a driving feature ID flips the corresponding flag.
    #[must_use]
    pub fn from_feature_ids(ids: &[u16]) -> Self {
        const BUTTONS: [u16; 5] = [0x1b00, 0x1b01, 0x1b02, 0x1b03, 0x1b04];
        const POINTER: [u16; 2] = [0x2201, 0x2202];
        // ColorLedEffects (0x8070), PerKeyLighting2 (0x8081) and PerKeyLighting
        // (0x8080) — all three driven by `set_keyboard_color`, which prefers
        // 0x8070's fixed effect to override a running onboard profile and falls
        // back through 0x8081 to 0x8080. Other families (backlight 0x198x) stay
        // out so they don't earn a tab the panel can't drive.
        const LIGHTING: [u16; 3] = [0x8080, 0x8070, 0x8081];
        let has = |family: &[u16]| ids.iter().any(|id| family.contains(id));
        Self {
            buttons: has(&BUTTONS),
            pointer: has(&POINTER),
            lighting: has(&LIGHTING),
            scroll_inversion: false,
            hires_wheel: ids.contains(&0x2121),
            thumbwheel: ids.contains(&0x2150),
            haptic_feedback: ids.contains(&0x19b0),
            haptic_panel: false,
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
                scroll_inversion: false,
                hires_wheel: false,
                thumbwheel: false,
                haptic_feedback: false,
                haptic_panel: false,
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
    /// Almost depleted — the firmware's most urgent bucket.
    Critical,
    /// Running low; worth surfacing a charge hint.
    Low,
    /// Comfortable middle range, no user action needed.
    Good,
    /// At or near full charge.
    Full,
    /// The firmware did not report a level, or reported one we don't model.
    Unknown,
}

/// Charging state. Mirrors `hidpp 0.2`'s `BatteryStatus` plus `Unknown` for
/// values added in future protocol versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatteryStatus {
    /// Running on battery.
    Discharging,
    /// Charging at the normal rate.
    Charging,
    /// Charging at reduced current (e.g. from a weak power source).
    ChargingSlow,
    /// Charge complete while still connected to power.
    Full,
    /// The device reported a charging fault.
    Error,
    /// A status value this build doesn't model (future protocol additions).
    Unknown,
}

/// Battery snapshot for one paired device, as last polled over HID++.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatteryInfo {
    /// Reported charge percentage (`0..=100`).
    pub percentage: u8,
    /// Coarse bucket for UI that doesn't want the raw percentage.
    pub level: BatteryLevel,
    /// Charging state at poll time.
    pub status: BatteryStatus,
}

/// Identity of an enumerated receiver — no paired-device state (that lives
/// in [`DeviceInventory::paired`]). For a direct (Bluetooth/wired) device,
/// a synthetic entry mirroring the device's own HID identity fills this role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiverInfo {
    /// Product string from the HID enumeration (e.g. `"Logi Bolt Receiver"`).
    pub name: String,
    /// USB vendor ID (`0x046d` for Logitech).
    pub vendor_id: u16,
    /// USB product ID distinguishing the receiver model.
    pub product_id: u16,
    /// Platform-reported serial, when one is exposed. Deliberately excluded
    /// from diagnostics (see [`crate::diagnostics::ReceiverDiag`]).
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceModelInfo {
    /// Number of firmware entities (main firmware, bootloader, …) the
    /// device reports.
    pub entity_count: u8,
    /// HID++ DeviceInformation serial number, when the device supports the
    /// optional serial-number function.
    pub serial_number: Option<String>,
    /// Per-unit ID bytes — unique to the physical unit, unlike the
    /// model-level fields around it.
    pub unit_id: [u8; 4],
    /// Which transports the firmware supports; defines the slot order of
    /// [`Self::model_ids`].
    pub transports: DeviceTransports,
    /// Per-transport PIDs for the transports enabled in [`Self::transports`],
    /// packed from the front in ascending HID++ transport-bit order:
    /// Bluetooth (bit 0), BTLE (bit 1), eQuad (bit 2), USB (bit 3). Only
    /// enabled transports take a slot — a disabled transport is skipped, not
    /// zero-filled — so unused trailing slots stay `0` and the array holds at
    /// most three PIDs.
    ///
    /// Note this is the reverse of the field order of [`DeviceTransports`],
    /// which lists `usb` first. A device with `usb + equad` reports
    /// `[eQuad PID, USB PID, 0]`; one with `equad + btle` reports
    /// `[BTLE PID, eQuad PID, 0]`.
    pub model_ids: [u16; 3],
    /// Extra model byte prefixed to a PID to form the asset registry's
    /// `modelId` — see [`Self::config_key`].
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "bitfield mirroring HID++ DeviceInformation; transports are independent flags"
)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceTransports {
    /// Wired USB.
    pub usb: bool,
    /// Logitech eQuad — the Unifying/Bolt receiver RF protocol.
    pub equad: bool,
    /// Bluetooth Low Energy.
    pub btle: bool,
    /// Classic Bluetooth.
    pub bluetooth: bool,
}

/// One device in the agent's inventory snapshot: a receiver pairing slot,
/// or a direct (Bluetooth/wired) attachment under its synthetic
/// [`ReceiverInfo`]. Embedded in [`DeviceInventory`], so its field order is
/// IPC wire format — see that type's contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedDevice {
    /// Receiver-assigned slot (1..=6 for Bolt).
    pub slot: u8,
    /// Firmware codename (e.g. `"MX Master 3S"`), when reported.
    pub codename: Option<String>,
    /// Wireless product ID. `None` for offline / unreachable devices on hidpp 0.2.
    pub wpid: Option<u16>,
    /// Best-guess classification. Identity only — panel gating uses
    /// [`Self::capabilities`] instead, so a misread kind can't hide panels
    /// (issue #127).
    pub kind: DeviceKind,
    /// Whether the device was reachable at enumeration time; offline devices
    /// keep their slot with reduced detail.
    pub online: bool,
    /// Last battery reading, `None` when offline or the device doesn't
    /// report battery.
    pub battery: Option<BatteryInfo>,
    /// Output of HID++ feature 0x0003 — populated for online devices that
    /// expose the feature. Drives asset-registry lookups in the GUI.
    pub model_info: Option<DeviceModelInfo>,
    /// Configuration capabilities derived from the device's HID++ feature
    /// table. `None` for devices we couldn't probe (offline / unreachable);
    /// the GUI then falls back to [`Capabilities::presumed_from_kind`].
    pub capabilities: Option<Capabilities>,
}

/// Address of a standalone raw-HID interface.
///
/// The identity is an opaque transport-generated string. It is deliberately
/// kept separate from the HID++ receiver/slot address so a raw device cannot
/// accidentally enter the HID++ `Direct` path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RawDeviceAddress {
    /// HID vendor ID.
    pub vendor_id: u16,
    /// HID product ID.
    pub product_id: u16,
    /// HID usage page.
    pub usage_page: u16,
    /// HID usage ID.
    pub usage_id: u16,
    /// Identity chosen by the transport: a serial when available, otherwise
    /// an explicitly transient OS-node identity. It is never an enumeration
    /// index and a transient value is not persisted as a physical key.
    pub identity: String,
}

/// A standalone device that is not a HID++ receiver pairing slot.
///
/// This is the inventory bridge for Litra and future non-HID++ categories.
/// Receiver-backed devices continue to use [`PairedDevice`] inside
/// [`DeviceInventory`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StandaloneDevice {
    /// Raw HID address used to re-find the interface.
    pub address: RawDeviceAddress,
    /// Human-readable name supplied by the OS/HID descriptor.
    pub display_name: String,
    /// Human-readable manufacturer, when available.
    pub manufacturer: Option<String>,
    /// Device serial, when the HID backend exposes one.
    pub serial_number: Option<String>,
    /// Stable four-byte identity when the protocol/driver provides one.
    /// Raw HID drivers may use zeroes when no such field exists.
    pub unit_id: [u8; 4],
    /// Identity classification. Capability fields gate controls.
    pub kind: DeviceKind,
    /// Whether this interface was present in the latest completed scan.
    pub online: bool,
    /// HID++ capabilities are absent for a non-HID++ device.
    pub capabilities: Option<Capabilities>,
    /// Standalone capability descriptor, if the selected driver recognizes it.
    pub light_capabilities: Option<LightCapabilities>,
    /// Stable identifier of the driver family that owns this raw interface.
    /// This is deliberately separate from the product ID so a future family
    /// can share a protocol driver across several product variants.
    pub driver_id: String,
    /// Optional model-level identity in the OpenLogi asset registry.
    ///
    /// This is deliberately appended: `StandaloneDevice` crosses the
    /// append-only GUI↔agent bincode wire format.
    #[serde(default)]
    pub registry_model_id: Option<String>,
}

/// One receiver and its paired devices — the unit the agent's inventory
/// snapshot is made of.
///
/// Crosses the agent↔GUI IPC (everything it embeds too: [`ReceiverInfo`],
/// [`PairedDevice`], battery/model-info/capability types). bincode encodes
/// field and variant *order*, so reordering, retyping, or wrapping any field
/// in this tree is a wire-format change and requires a `PROTOCOL_VERSION`
/// bump (guarded by `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInventory {
    /// The receiver's identity — synthetic (mirroring the device itself)
    /// for a direct Bluetooth/wired attachment.
    pub receiver: ReceiverInfo,
    /// The devices reached through this receiver; a direct attachment
    /// carries exactly one entry.
    pub paired: Vec<PairedDevice>,
}

#[cfg(test)]
mod tests {
    use super::{
        BatteryInfo, BatteryLevel, BatteryStatus, Capabilities, DeviceInventory, DeviceKind,
        DeviceModelInfo, DeviceTransports, LightValueRange, LightValueUnit, PairedDevice,
        ReceiverInfo,
    };

    fn inventory(slot: u8, wpid: Option<u16>, battery_percentage: u8) -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "Logi Bolt Receiver".to_string(),
                vendor_id: 0x046d,
                product_id: 0xc548,
                unique_id: Some("receiver-1".to_string()),
            },
            paired: vec![PairedDevice {
                slot,
                codename: Some("MX Test".to_string()),
                wpid,
                kind: DeviceKind::Mouse,
                online: true,
                battery: Some(BatteryInfo {
                    percentage: battery_percentage,
                    level: BatteryLevel::Good,
                    status: BatteryStatus::Discharging,
                }),
                model_info: Some(DeviceModelInfo {
                    entity_count: 1,
                    serial_number: Some("serial-1".to_string()),
                    unit_id: [1, 2, 3, 4],
                    transports: DeviceTransports {
                        usb: true,
                        equad: true,
                        btle: false,
                        bluetooth: false,
                    },
                    model_ids: [0xb023, 0, 0],
                    extended_model_id: 0x02,
                }),
                capabilities: Some(Capabilities {
                    buttons: true,
                    pointer: true,
                    lighting: false,
                    scroll_inversion: false,
                    hires_wheel: false,
                    thumbwheel: false,
                    haptic_feedback: false,
                    haptic_panel: false,
                }),
            }],
        }
    }

    #[test]
    fn device_inventory_equality_includes_nested_device_fields() {
        let base = inventory(1, Some(0xb023), 86);
        assert_eq!(base, base.clone());

        assert_ne!(
            base,
            inventory(2, Some(0xb023), 86),
            "slot changes must affect inventory equality"
        );
        assert_ne!(
            base,
            inventory(1, Some(0xb024), 86),
            "wireless product id changes must affect inventory equality"
        );
        assert_ne!(
            base,
            inventory(1, Some(0xb023), 87),
            "nested battery changes must affect inventory equality"
        );
    }

    #[test]
    fn registry_type_is_case_folded() {
        // The registry ships both `"mouse"` and `"MOUSE"`; both must resolve so
        // the asset cross-check can't silently miss a depot.
        assert_eq!(
            DeviceKind::from_registry_type("mouse"),
            Some(DeviceKind::Mouse)
        );
        assert_eq!(
            DeviceKind::from_registry_type("MOUSE"),
            Some(DeviceKind::Mouse)
        );
        assert_eq!(
            DeviceKind::from_registry_type("  Keyboard "),
            Some(DeviceKind::Keyboard)
        );
    }

    #[test]
    fn unknown_registry_type_defers_to_the_caller() {
        // Unmodelled / empty → no asset opinion.
        assert_eq!(DeviceKind::from_registry_type("webcam"), None);
        assert_eq!(DeviceKind::from_registry_type(""), None);
    }

    #[test]
    fn capabilities_track_the_driving_feature_ids() {
        use super::Capabilities;
        // A typical MX mouse: ReprogControls (0x1b04) + ExtendedAdjustableDpi
        // (0x2202), no lighting.
        let mouse =
            Capabilities::from_feature_ids(&[0x0003, 0x1b04, 0x2121, 0x2150, 0x2202, 0x2110]);
        assert_eq!(
            mouse,
            Capabilities {
                buttons: true,
                pointer: true,
                lighting: false,
                scroll_inversion: false,
                hires_wheel: true,
                thumbwheel: true,
                haptic_feedback: false,
                haptic_panel: false,
            }
        );
        assert!(!Capabilities::from_feature_ids(&[0x0003, 0x1b04]).thumbwheel);
        // A wired G-series keyboard: PerKeyLighting (0x8080), no DPI/buttons.
        let keyboard = Capabilities::from_feature_ids(&[0x0001, 0x8080]);
        assert_eq!(
            keyboard,
            Capabilities {
                buttons: false,
                pointer: false,
                lighting: true,
                scroll_inversion: false,
                hires_wheel: false,
                thumbwheel: false,
                haptic_feedback: false,
                haptic_panel: false,
            }
        );
        // No driving features → nothing offered.
        assert_eq!(
            Capabilities::from_feature_ids(&[0x0000, 0x0003]),
            Capabilities::default()
        );
    }

    #[test]
    fn every_drivable_lighting_family_earns_the_tab() {
        // `set_keyboard_color` walks 0x8070 → 0x8081 → 0x8080, so a keyboard
        // exposing any one of them can be coloured and must get the tab.
        // 0x8081 was missing here, which left such a keyboard with no lighting
        // UI at all.
        for id in [0x8070, 0x8080, 0x8081] {
            assert!(
                Capabilities::from_feature_ids(&[0x0001, id]).lighting,
                "0x{id:04x} must offer the lighting tab"
            );
        }
        // Backlight (0x198x) stays out — the panel cannot drive it.
        assert!(!Capabilities::from_feature_ids(&[0x0001, 0x1982]).lighting);
    }

    #[test]
    fn persisted_capabilities_without_appended_wheel_fields_load_as_unsupported()
    -> Result<(), toml::de::Error> {
        use super::Capabilities;

        let capabilities: Capabilities = toml::from_str(
            r"
                buttons = true
                pointer = true
                lighting = false
                scroll_inversion = true
            ",
        )?;

        assert!(!capabilities.hires_wheel);
        assert!(!capabilities.thumbwheel);
        assert!(capabilities.scroll_inversion);
        Ok(())
    }

    #[test]
    fn presumed_capabilities_keep_an_unprobed_mouse_configurable() {
        use super::Capabilities;
        let mouse = Capabilities::presumed_from_kind(DeviceKind::Mouse);
        assert!(mouse.buttons && mouse.pointer && !mouse.lighting);
        assert!(!mouse.thumbwheel);
        assert!(Capabilities::presumed_from_kind(DeviceKind::Keyboard).lighting);
        // An unidentified device presumes nothing — it must be measured.
        assert_eq!(
            Capabilities::presumed_from_kind(DeviceKind::Unknown),
            Capabilities::default()
        );
    }

    #[test]
    fn light_ranges_reject_invalid_grids_and_units() {
        LightValueRange::new(10, 1, 1, LightValueUnit::Lumens)
            .expect_err("a minimum above the maximum must be rejected");
        LightValueRange::new(0, 10, 0, LightValueUnit::Lumens)
            .expect_err("a zero step must be rejected");
        LightValueRange::new(0, 10, 3, LightValueUnit::Lumens)
            .expect_err("a step that does not divide the span must be rejected");
        LightValueRange::new(0, 101, 1, LightValueUnit::Percent)
            .expect_err("a percent range above 100 must be rejected");
    }

    #[test]
    fn light_ranges_quantize_without_leaving_the_advertised_grid() {
        let range = LightValueRange::new(20, 250, 10, LightValueUnit::Lumens).expect("valid range");
        assert_eq!(range.native_for_percent(0), Some(20));
        assert_eq!(range.native_for_percent(50), Some(140));
        assert_eq!(range.native_for_percent(100), Some(250));
        assert_eq!(range.quantize(249), 250);
        assert!(range.contains(range.native_for_percent(65).expect("mapped value")));
    }

    #[test]
    fn invalid_light_ranges_fail_toml_deserialization() {
        let result = toml::from_str::<LightValueRange>(
            "min = 2700\nmax = 6500\nstep = 0\nunit = 'kelvin'\n",
        );
        result.expect_err("a zero step must not survive deserialization");
    }
}
