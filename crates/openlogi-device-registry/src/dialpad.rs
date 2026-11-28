//! Known Logitech dialpad identities.
//!
//! A dialpad speaks ordinary HID++ — its rotary controls through `0x4610`
//! MultiRoller, its keys through `0x1b04` — but its `0x0005` `DeviceType`
//! names no kind OpenLogi models, and nothing in the protocol says which
//! roller is the large dial or where each key sits. Those are product facts,
//! claimed here once and converted by `openlogi-device` at its own boundary.
//!
//! Facts are from hardware (`docs/hardware/mx-creative-console.md`).

use crate::LOGITECH_VENDOR_ID;

/// Where one of a dialpad's keys sits around its dial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DialpadKeyPosition {
    /// Above the dial, on the left.
    TopLeft,
    /// Above the dial, on the right.
    TopRight,
    /// Below the dial, on the left.
    BottomLeft,
    /// Below the dial, on the right.
    BottomRight,
}

/// One dialpad key: its `0x1b04` control ID and where it sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialpadKey {
    /// The key's `0x1b04` control ID.
    pub cid: u16,
    /// The key's place on the device.
    pub position: DialpadKeyPosition,
}

/// A known Logitech dialpad identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialpadDescriptor {
    /// USB vendor ID.
    pub vendor_id: u16,
    /// Product ID (the Bluetooth PID for a Bluetooth-only device).
    pub product_id: u16,
    /// Marketed product name.
    pub name: &'static str,
    /// The `0x4610` roller id of the large dial.
    pub dial_roller: u8,
    /// The `0x4610` roller id of the small roller.
    pub roller_roller: u8,
    /// The keys around the dial, top row first, left to right.
    pub keys: [DialpadKey; 4],
}

/// The MX Creative Dialpad, Bluetooth LE. Roller ids and key positions were
/// mapped on hardware 2026-11-03: turning the dial reported roller `1`, the
/// roller reported `0`, and pressing the keys top-left, top-right,
/// bottom-left, bottom-right reported `0x0053`, `0x0056`, `0x0059`, `0x005a`.
const MX_CREATIVE_DIALPAD: DialpadDescriptor = DialpadDescriptor {
    vendor_id: LOGITECH_VENDOR_ID,
    product_id: 0xbc00,
    name: "MX Creative Dialpad",
    dial_roller: 1,
    roller_roller: 0,
    keys: [
        DialpadKey {
            cid: 0x0053,
            position: DialpadKeyPosition::TopLeft,
        },
        DialpadKey {
            cid: 0x0056,
            position: DialpadKeyPosition::TopRight,
        },
        DialpadKey {
            cid: 0x0059,
            position: DialpadKeyPosition::BottomLeft,
        },
        DialpadKey {
            cid: 0x005a,
            position: DialpadKeyPosition::BottomRight,
        },
    ],
};

/// All dialpad identities OpenLogi recognizes.
pub const DIALPADS: &[DialpadDescriptor] = &[MX_CREATIVE_DIALPAD];

/// Finds a dialpad descriptor by its USB / Bluetooth identity.
#[must_use]
pub fn find_dialpad(vendor_id: u16, product_id: u16) -> Option<&'static DialpadDescriptor> {
    DIALPADS
        .iter()
        .find(|d| d.vendor_id == vendor_id && d.product_id == product_id)
}

impl DialpadDescriptor {
    /// The key with control ID `cid`, if it is one of this dialpad's.
    #[must_use]
    pub fn key(&self, cid: u16) -> Option<&DialpadKey> {
        self.keys.iter().find(|k| k.cid == cid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_mx_creative_dialpad_by_its_bluetooth_pid() {
        let dialpad = find_dialpad(LOGITECH_VENDOR_ID, 0xbc00).expect("registered");
        assert_eq!(dialpad.name, "MX Creative Dialpad");
        assert_ne!(dialpad.dial_roller, dialpad.roller_roller);
        assert_eq!(
            dialpad.key(0x005a).map(|k| k.position),
            Some(DialpadKeyPosition::BottomRight)
        );
        assert!(find_dialpad(LOGITECH_VENDOR_ID, 0xc354).is_none());
    }
}
