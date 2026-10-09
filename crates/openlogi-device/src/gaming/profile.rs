//! Format-3/5 profile inspection. Offsets are reverse-engineered by libratbag,
//! not an official Logitech specification. Unknown bindings are kept verbatim.

use super::GamingError;
use serde::{Deserialize, Serialize};

/// Edits only the named fields; other bytes, including lighting and macros, survive.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilePatch {
    /// Optional UTF-16 profile name (at most 23 code units).
    pub name: Option<String>,
    /// Optional report rate, restricted to 125/250/500/1000 Hz.
    pub report_rate_hz: Option<u16>,
    /// Optional five DPI stages, with zero for a disabled stage.
    pub dpi: Option<[u16; 5]>,
    /// Optional zero-based default DPI stage.
    pub default_dpi_slot: Option<u8>,
    /// Optional zero-based DPI-shift stage.
    pub shift_dpi_slot: Option<u8>,
    /// Zero-based firmware button indices, not G HUB marketing labels.
    #[serde(default)]
    pub buttons: Vec<ButtonPatch>,
}

/// One assignment in one layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ButtonPatch {
    /// Zero-based firmware index.
    pub index: u8,
    /// Assignment layer.
    pub layer: Layer,
    /// Validated replacement action.
    pub action: ButtonAction,
}

/// Onboard binding layer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// Ordinary assignments.
    Normal,
    /// Assignments while a G-Shift button is held.
    Shifted,
}

/// Known firmware actions. HID usages are deliberately distinct from OS key codes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ButtonAction {
    /// Mouse button 1 through 16.
    Mouse {
        /// One-based mouse button number.
        button: u8,
    },
    /// Keyboard-page HID usage plus the USB modifier bitmask.
    Key {
        /// Keyboard HID usage, 4 through 231.
        usage: u8,
        /// Modifier bits (left Ctrl = 1, Shift = 2, Alt = 4, GUI = 8).
        modifiers: u8,
    },
    /// Consumer-page HID usage.
    Consumer {
        /// Consumer usage identifier.
        usage: u16,
    },
    /// Firmware action named by its documented reverse-engineered code.
    Special {
        /// 1/2 tilt; 3..7 DPI; 8..10 profiles; 11 G-Shift; 12 battery; 16/17 scroll.
        code: u8,
    },
    /// Remove an assignment.
    Disabled,
}

impl ButtonAction {
    fn encode(&self) -> Result<[u8; 4], GamingError> {
        Ok(match *self {
            Self::Mouse {
                button: button @ 1..=16,
            } => {
                let [hi, lo] = (1u16 << (button - 1)).to_be_bytes();
                [0x80, 1, hi, lo]
            }
            Self::Key {
                usage: usage @ 4..=231,
                modifiers,
            } => [0x80, 2, modifiers, usage],
            Self::Consumer {
                usage: usage @ 1..=1023,
            } => {
                let [hi, lo] = usage.to_be_bytes();
                [0x80, 3, hi, lo]
            }
            Self::Special {
                code: code @ (1..=12 | 16 | 17),
            } => [0x90, code, 0, 0],
            Self::Disabled => [0xff; 4],
            _ => {
                return Err(GamingError::Invalid(
                    "unsupported button action value".into(),
                ));
            }
        })
    }
}

/// Produce a validated sector image in memory; this does not perform I/O.
pub fn patch_profile(
    original: &[u8],
    count: u8,
    patch: &ProfilePatch,
) -> Result<Vec<u8>, GamingError> {
    ProfileSummary::decode(1, true, original, count)?;
    if !checksum_valid(original) {
        return Err(GamingError::Invalid(
            "refusing to edit a profile with an invalid CRC".into(),
        ));
    }
    let mut out = original.to_vec();
    if let Some(name) = &patch.name {
        let name: Vec<_> = name.encode_utf16().collect();
        if name.len() > 23 || name.contains(&0) {
            return Err(GamingError::Invalid(
                "profile name must fit 23 UTF-16 code units".into(),
            ));
        }
        out[160..208].fill(0);
        for (i, unit) in name.iter().enumerate() {
            out[160 + i * 2..162 + i * 2].copy_from_slice(&unit.to_le_bytes());
        }
    }
    if let Some(rate) = patch.report_rate_hz {
        out[0] = match rate {
            125 => 8,
            250 => 4,
            500 => 2,
            1000 => 1,
            _ => {
                return Err(GamingError::Invalid(
                    "report rate must be 125/250/500/1000 Hz".into(),
                ));
            }
        };
    }
    if let Some(dpi) = patch.dpi {
        if dpi
            .iter()
            .any(|v| *v != 0 && (!(100..=25600).contains(v) || v % 50 != 0))
        {
            return Err(GamingError::Invalid(
                "DPI must be zero or 100..25600 in steps of 50".into(),
            ));
        }
        for (i, value) in dpi.iter().enumerate() {
            out[3 + i * 2..5 + i * 2].copy_from_slice(&value.to_le_bytes());
        }
    }
    if let Some(slot) = patch.default_dpi_slot {
        out[1] = slot;
    }
    if let Some(slot) = patch.shift_dpi_slot {
        out[2] = slot;
    }
    for slot in [out[1], out[2]] {
        if slot >= 5 || out[3 + usize::from(slot) * 2..5 + usize::from(slot) * 2] == [0, 0] {
            return Err(GamingError::Invalid(
                "default and shift DPI must reference enabled stages".into(),
            ));
        }
    }
    for (i, binding) in patch.buttons.iter().enumerate() {
        if binding.index >= count
            || patch.buttons[..i]
                .iter()
                .any(|p| p.index == binding.index && p.layer == binding.layer)
        {
            return Err(GamingError::Invalid(
                "duplicate or out-of-range button assignment".into(),
            ));
        }
        let start = match binding.layer {
            Layer::Normal => 32,
            Layer::Shifted => 96,
        } + usize::from(binding.index) * 4;
        out[start..start + 4].copy_from_slice(&binding.action.encode()?);
    }
    if !out[32..32 + usize::from(count) * 4]
        .as_chunks::<4>()
        .0
        .contains(&[0x80, 1, 0, 1])
    {
        return Err(GamingError::Invalid(
            "at least one normal-layer left-click must remain".into(),
        ));
    }
    let end = out.len() - 2;
    let crc = crc16(&out[..end]);
    out[end..].copy_from_slice(&crc.to_be_bytes());
    Ok(out)
}

/// One four-byte binding, preserved without a lossy enum conversion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Binding {
    /// Onboard representation (not a Windows virtual-key code).
    pub raw: [u8; 4],
    /// Human-readable diagnostic description.
    pub description: String,
}

impl Binding {
    fn decode(raw: [u8; 4]) -> Self {
        let description = match raw {
            [0x80, 1, hi, lo] => format!("mouse mask 0x{:04x}", u16::from_be_bytes([hi, lo])),
            [0x80, 2, modifiers, key] => {
                format!("keyboard HID 0x{key:02x}, modifiers 0x{modifiers:02x}")
            }
            [0x80, 3, hi, lo] => format!("consumer HID 0x{:04x}", u16::from_be_bytes([hi, lo])),
            [0x90, special, _, _] => match special {
                1 => "tilt left",
                2 => "tilt right",
                3 => "DPI next",
                4 => "DPI previous",
                5 => "DPI cycle",
                6 => "DPI default",
                7 => "DPI shift",
                8 => "profile next",
                9 => "profile previous",
                10 => "profile cycle",
                11 => "G-Shift",
                12 => "battery",
                16 => "scroll down",
                17 => "scroll up",
                _ => "unknown special",
            }
            .into(),
            [0, page, _, offset] => format!("macro at sector {page}, offset {offset}"),
            [0xff, ..] => "disabled".into(),
            _ => "unknown (preserved)".into(),
        };
        Self { raw, description }
    }
}

/// Decoded settings alongside a checksum result; invalid data stays visible.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSummary {
    /// Physical one-based user-memory sector.
    pub sector: u16,
    /// Whether the directory enables this profile.
    pub enabled: bool,
    /// UTF-16LE profile name (empty for erased/default names).
    pub name: String,
    /// Whether the sector checksum matches.
    pub checksum_valid: bool,
    /// Raw report interval. Zero and unknown values are not normalized.
    pub report_interval_ms: u8,
    /// Default zero-based DPI slot.
    pub default_dpi_slot: u8,
    /// DPI-shift zero-based slot.
    pub shift_dpi_slot: u8,
    /// The five stored DPI values; zero means disabled.
    pub dpi: [u16; 5],
    /// Physical button assignments in firmware order.
    pub buttons: Vec<Binding>,
    /// Assignments while G-Shift is held, in the same order.
    pub shifted_buttons: Vec<Binding>,
}

impl ProfileSummary {
    pub(super) fn decode(
        sector: u16,
        enabled: bool,
        raw: &[u8],
        count: u8,
    ) -> Result<Self, GamingError> {
        if ![255, 256].contains(&raw.len()) || count > 16 {
            return Err(GamingError::Invalid("invalid profile dimensions".into()));
        }
        let words: Vec<_> = raw[160..208]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|v| *v != 0 && *v != 0xffff)
            .collect();
        let bindings = |start: usize| {
            raw[start..start + usize::from(count) * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| Binding::decode([b[0], b[1], b[2], b[3]]))
                .collect()
        };
        Ok(Self {
            sector,
            enabled,
            name: String::from_utf16_lossy(&words),
            checksum_valid: checksum_valid(raw),
            report_interval_ms: raw[0],
            default_dpi_slot: raw[1],
            shift_dpi_slot: raw[2],
            dpi: std::array::from_fn(|i| u16::from_le_bytes([raw[3 + i * 2], raw[4 + i * 2]])),
            buttons: bindings(32),
            shifted_buttons: bindings(96),
        })
    }
}

/// Validate a sector checksum using the actual firmware-advertised length.
#[must_use]
pub fn checksum_valid(bytes: &[u8]) -> bool {
    let Some(end) = bytes.len().checked_sub(2) else {
        return false;
    };
    crc16(&bytes[..end]) == u16::from_be_bytes([bytes[end], bytes[end + 1]])
}

/// CRC-16/CCITT-FALSE used by the onboard sector format.
#[must_use]
pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0xffff_u16;
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 == 0 {
                crc << 1
            } else {
                (crc << 1) ^ 0x1021
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_crc_vector() {
        assert_eq!(crc16(b"123456789"), 0x29b1);
    }

    #[test]
    fn decodes_independent_profile_bytes_and_both_layers() {
        let mut raw = [0xff; 256];
        raw[..13].copy_from_slice(&[1, 1, 0, 0x20, 3, 0x40, 6, 0x80, 0x0c, 0, 0, 0, 0]);
        raw[32..36].copy_from_slice(&[0x80, 1, 0, 1]);
        raw[96..100].copy_from_slice(&[0x90, 11, 0, 0]);
        raw[160..164].copy_from_slice(&[b'A', 0, 0, 0]);
        let decoded = ProfileSummary::decode(1, true, &raw, 1).unwrap();
        assert_eq!(decoded.dpi, [800, 1600, 3200, 0, 0]);
        assert_eq!(decoded.name, "A");
        assert_eq!(decoded.buttons[0].raw, [0x80, 1, 0, 1]);
        assert_eq!(decoded.shifted_buttons[0].description, "G-Shift");
        assert!(!decoded.checksum_valid);
    }
}
