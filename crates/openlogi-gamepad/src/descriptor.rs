//! Xbox-layout HID report descriptor for browser `mapping: "standard"`.
//!
//! Usages follow Generic Desktop Game Pad + Button page so Chromium assigns
//! the standard indices. Report layout matches [`crate::state::GamepadState::to_input_report`].

/// 8-byte input + 2-byte output (dual rumble) report descriptor.
pub const STANDARD_GAMEPAD_REPORT_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x05, // Usage (Game Pad)
    0xa1, 0x01, // Collection (Application)
    // Sticks + triggers (6 bytes)
    0x09, 0x30, //   Usage (X)
    0x09, 0x31, //   Usage (Y)
    0x09, 0x32, //   Usage (Z) — right stick X
    0x09, 0x35, //   Usage (Rz) — right stick Y
    0x09, 0x33, //   Usage (Rx) — left trigger
    0x09, 0x34, //   Usage (Ry) — right trigger
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xff, 0x00, // Logical Maximum (255)
    0x75, 0x08, //   Report Size (8)
    0x95, 0x06, //   Report Count (6)
    0x81, 0x02, //   Input (Data,Var,Abs)
    // Hat switch (4 bits) + padding into button low nibble
    0x05, 0x01, //   Usage Page (Generic Desktop)
    0x09, 0x39, //   Usage (Hat switch)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x07, //   Logical Maximum (7)
    0x35, 0x00, //   Physical Minimum (0)
    0x46, 0x3b, 0x01, // Physical Maximum (315)
    0x65, 0x14, //   Unit (Degrees)
    0x75, 0x04, //   Report Size (4)
    0x95, 0x01, //   Report Count (1)
    0x81, 0x42, //   Input (Data,Var,Abs,Null)
    // 12 buttons in the remaining bits of byte 6 + byte 7
    0x05, 0x09, //   Usage Page (Button)
    0x19, 0x01, //   Usage Minimum (1)
    0x29, 0x0c, //   Usage Maximum (12)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x01, //   Logical Maximum (1)
    0x75, 0x01, //   Report Size (1)
    0x95, 0x0c, //   Report Count (12)
    0x81, 0x02, //   Input (Data,Var,Abs)
    // Output: dual rumble (strong, weak)
    0x05, 0x0f, //   Usage Page (Physical Interface)
    0x09, 0x97, //   Usage (Vendor — dual motor magnitudes)
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xff, 0x00, // Logical Maximum (255)
    0x75, 0x08, //   Report Size (8)
    0x95, 0x02, //   Report Count (2)
    0x91, 0x02, //   Output (Data,Var,Abs)
    0xc0, // End Collection
];

/// OpenLogi virtual-pad USB IDs (pid.codes open-source block style). Not a
/// Microsoft Xbox VID — the report descriptor's usages drive standard mapping.
pub const OPENLOGI_GAMEPAD_VID: u32 = 0x1209;
pub const OPENLOGI_GAMEPAD_PID: u32 = 0x0C06;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_is_nonempty() {
        assert!(STANDARD_GAMEPAD_REPORT_DESCRIPTOR.len() > 40);
    }
}
