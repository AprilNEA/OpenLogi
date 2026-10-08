//! Standard HID controls with a verified native mapping path.

/// Driver that applies device-scoped host HID usage mappings.
pub const NATIVE_REMAP_DRIVER_ID: &str = "org.openlogi.native-hid-remap";

/// A known control and its model-level matching facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeRemapDevice {
    /// Stable catalog identity.
    pub descriptor_id: &'static str,
    /// Stable model identity, not a unit identity.
    pub model_id: &'static str,
    /// Marketed product name.
    pub name: &'static str,
    /// USB vendor ID.
    pub vendor_id: u16,
    /// USB product ID.
    pub product_id: u16,
    /// Top-level HID usage page.
    pub usage_page: u16,
    /// Top-level HID usage.
    pub usage: u16,
    /// Stable short-press control identity.
    pub control_id: &'static str,
    /// English control label.
    pub label: &'static str,
    /// Simplified Chinese control label.
    pub label_zh_cn: &'static str,
    /// Emitted HID usage page.
    pub source_page: u16,
    /// Emitted HID usage.
    pub source_usage: u16,
    /// Suggested key, parsed by the existing shortcut vocabulary.
    pub recommended_key: &'static str,
}

/// Verified DJI Mic 3 USB receiver. The source carries no transmitter identity.
pub const DJI_MIC_3: NativeRemapDevice = NativeRemapDevice {
    descriptor_id: "org.openlogi.dji-mic3-usb",
    model_id: "dji.mic3.rx",
    name: "DJI Mic 3",
    vendor_id: 0x2ca3,
    product_id: 0x4015,
    usage_page: 0x000c,
    usage: 0x0001,
    control_id: "linking",
    label: "Linking button",
    label_zh_cn: "连接键",
    source_page: 0x000c,
    source_usage: 0x00e9,
    recommended_key: "F18",
};

/// Compiled registrations for the native HID mapping driver.
pub const NATIVE_REMAP_DEVICES: &[NativeRemapDevice] = &[DJI_MIC_3];
