//! Known Logitech keypad identities.
//!
//! The devices speak ordinary HID++ and are handled by the generic device
//! layer. They are listed because some of them report no (or an unmodelled)
//! `DeviceType` on feature `0x0005`, so the protocol cannot name a kind — and
//! kind decides the capture plane. This table is the direct-path sibling of
//! the receiver pairing register: a per-product identity fact claimed here,
//! which `openlogi-device` converts to a `DeviceKind` at its own boundary.
//!
//! An entry also carries its LCD key panel's geometry, the one hardware fact
//! no protocol feature reports: the panel is driven over a vendor report, so
//! only the product identity says it exists and where its keys sit.

use crate::LOGITECH_VENDOR_ID;

/// A known Logitech keypad identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeypadDescriptor {
    /// USB vendor ID.
    pub vendor_id: u16,
    /// USB product ID.
    pub product_id: u16,
    /// Marketed product name.
    pub name: &'static str,
    /// The LCD panel behind the key grid.
    pub key_display: KeyDisplay,
}

impl KeypadDescriptor {
    const fn logitech(product_id: u16, name: &'static str, key_display: KeyDisplay) -> Self {
        Self {
            vendor_id: LOGITECH_VENDOR_ID,
            product_id,
            name,
            key_display,
        }
    }
}

/// A key's cell on a keypad's grid, zero-based from the top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyPosition {
    /// Row, counted down from the top.
    pub row: u16,
    /// Column, counted right from the left.
    pub col: u16,
}

/// A point on a keypad's LCD panel, in panel pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelPoint {
    /// Pixels from the panel's left edge.
    pub x: u16,
    /// Pixels from the panel's top edge.
    pub y: u16,
}

/// How many rows and columns of keys a grid has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyGrid {
    /// Rows of keys.
    pub rows: u16,
    /// Columns of keys.
    pub cols: u16,
}

impl KeyGrid {
    /// Whether `key` falls on this grid.
    #[must_use]
    pub const fn contains(self, key: KeyPosition) -> bool {
        key.row < self.rows && key.col < self.cols
    }

    /// The cell of the key at zero-based `index`, counting row-major — left
    /// to right, then top to bottom — the way a keypad numbers its keys.
    #[must_use]
    pub const fn cell(self, index: u16) -> KeyPosition {
        KeyPosition {
            row: index / self.cols,
            col: index % self.cols,
        }
    }
}

/// The MX Creative Keypad's key grid: three rows of three, which its vendor
/// report numbers row-major from the top-left.
pub const MX_CREATIVE_KEYPAD_GRID: KeyGrid = KeyGrid { rows: 3, cols: 3 };

/// Where a keypad's square LCD keys sit on its panel, in panel pixels.
///
/// Keys are laid out on a regular grid: the key at `(row, col)` is the
/// `key_size`-square tile whose top-left corner is
/// `origin + (col, row) · pitch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyDisplay {
    /// Side length of one key's tile.
    pub key_size: u16,
    /// Top-left corner of the top-left key.
    pub origin: PanelPoint,
    /// Distance between the top-left corners of adjacent keys.
    pub pitch: u16,
    /// The panel's key grid.
    pub grid: KeyGrid,
}

impl KeyDisplay {
    /// The top-left corner of the key at `key`, or `None` off the grid.
    #[must_use]
    pub const fn key_origin(&self, key: KeyPosition) -> Option<PanelPoint> {
        if !self.grid.contains(key) {
            return None;
        }
        Some(PanelPoint {
            x: self.origin.x + key.col * self.pitch,
            y: self.origin.y + key.row * self.pitch,
        })
    }
}

/// The MX Creative Keypad's 480×480 panel: nine 118-pixel keys, 40 pixels
/// apart. Verified on hardware 2026-10 (`openlogi-hid`'s
/// `mx_keypad_lcd_probe`): solid tiles written at these positions filled
/// each key edge to edge, in row-major order.
const MX_CREATIVE_KEYPAD_DISPLAY: KeyDisplay = KeyDisplay {
    key_size: 118,
    origin: PanelPoint { x: 23, y: 6 },
    pitch: 158,
    grid: MX_CREATIVE_KEYPAD_GRID,
};

/// All standalone keypad identities OpenLogi recognizes.
pub const KEYPADS: &[KeypadDescriptor] = &[
    // MX Creative Keypad — wired USB-C only, HID++ 4.5. Its `0x0005` marketing
    // name reads correctly, but its DeviceType is `None`, so this entry is the
    // only kind source for it (see `openlogi-device`'s direct-path resolve).
    KeypadDescriptor::logitech(0xc354, "MX Creative Keypad", MX_CREATIVE_KEYPAD_DISPLAY),
];

/// Finds a keypad descriptor by its USB identity.
#[must_use]
pub fn find_keypad(vendor_id: u16, product_id: u16) -> Option<&'static KeypadDescriptor> {
    KEYPADS
        .iter()
        .find(|device| device.vendor_id == vendor_id && device.product_id == product_id)
}

#[cfg(test)]
mod tests {
    use super::{KEYPADS, KeyDisplay, KeyGrid, KeyPosition, PanelPoint, find_keypad};

    const fn at(row: u16, col: u16) -> KeyPosition {
        KeyPosition { row, col }
    }

    const fn point(x: u16, y: u16) -> PanelPoint {
        PanelPoint { x, y }
    }
    use crate::LOGITECH_VENDOR_ID;

    #[test]
    fn keypad_identities_are_unique() {
        for (index, device) in KEYPADS.iter().enumerate() {
            assert!(
                KEYPADS[..index]
                    .iter()
                    .all(|other| (other.vendor_id, other.product_id)
                        != (device.vendor_id, device.product_id)),
                "duplicate keypad identity {:04x}:{:04x}",
                device.vendor_id,
                device.product_id
            );
        }
    }

    #[test]
    fn complete_identity_selects_mx_creative_keypad_metadata() {
        let keypad = find_keypad(LOGITECH_VENDOR_ID, 0xc354).expect("MX Creative Keypad identity");

        assert_eq!(keypad.name, "MX Creative Keypad");
    }

    #[test]
    fn mx_creative_keypad_keys_sit_on_the_verified_panel_grid() {
        let display = find_keypad(LOGITECH_VENDOR_ID, 0xc354)
            .map(|keypad| keypad.key_display)
            .expect("the MX Creative Keypad's keys are displays");

        assert_eq!(display.key_origin(at(0, 0)), Some(point(23, 6)));
        assert_eq!(display.key_origin(at(0, 2)), Some(point(339, 6)));
        assert_eq!(display.key_origin(at(2, 1)), Some(point(181, 322)));
        // The last key still fits the 480-pixel panel.
        let PanelPoint { x, y } = display.key_origin(at(2, 2)).expect("bottom-right key");
        assert!(x + display.key_size <= 480 && y + display.key_size <= 480);
    }

    #[test]
    fn key_origin_is_none_off_the_grid() {
        let display = KeyDisplay {
            key_size: 10,
            origin: PanelPoint { x: 0, y: 0 },
            pitch: 12,
            grid: KeyGrid { rows: 2, cols: 3 },
        };
        assert_eq!(display.key_origin(at(1, 2)), Some(point(24, 12)));
        assert!(display.key_origin(at(2, 0)).is_none());
        assert!(display.key_origin(at(0, 3)).is_none());
    }

    #[test]
    fn cells_count_row_major_from_the_top_left() {
        let grid = KeyGrid { rows: 2, cols: 3 };
        assert_eq!(grid.cell(0), at(0, 0));
        assert_eq!(grid.cell(2), at(0, 2));
        assert_eq!(grid.cell(3), at(1, 0));
        assert_eq!(grid.cell(5), at(1, 2));
        assert!(!grid.contains(grid.cell(6)));
    }

    #[test]
    fn lookup_requires_the_matching_vendor_and_product() {
        assert!(find_keypad(0xffff, 0xc354).is_none());
        assert!(find_keypad(LOGITECH_VENDOR_ID, 0xc355).is_none());
    }
}
