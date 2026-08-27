//! Onboard-profile snapshot types that cross the agent↔GUI IPC.
//!
//! HID++ `0x8100` G402-family profiles store DPI tables and LED effects in
//! flash. The GUI shows those values; the decode lives in `openlogi-device`.

use serde::{Deserialize, Serialize};

use crate::binding::{Action, ButtonId};
use crate::color::Rgb;
use crate::hid::Dpi;

use std::collections::BTreeMap;

/// Active onboard profile contents the GUI can display without a second HID
/// read. Field order is wire format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardProfileSnapshot {
    /// Decoded G-series button table.
    pub bindings: BTreeMap<ButtonId, Action>,
    /// Enabled DPI slots (`0` firmware slots are omitted).
    pub dpi_presets: Vec<Dpi>,
    /// Logo then side LED, when the profile format carries them.
    pub leds: Vec<OnboardLed>,
}

/// One onboard LED zone (logo or side).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardLed {
    /// Firmware effect for this zone.
    pub mode: OnboardLedMode,
    /// Primary colour (fixed / breathing / starlight sky). Black when the
    /// mode has no colour of its own (off, colour cycle).
    pub color: Rgb,
    /// Intensity percent (`1`–`100`). Firmware `0` means 100.
    pub brightness: u8,
}

/// Onboard LED effect. Variant order is wire format — append only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnboardLedMode {
    /// LEDs off.
    Off,
    /// Solid colour.
    On,
    /// Spectrum colour cycle.
    Cycle,
    /// Travelling colour wave.
    ColorWave,
    /// Starlight twinkle.
    Starlight,
    /// Breathing / pulse.
    Breathing,
    /// Ripple.
    Ripple,
    /// Custom animation slot or an unrecognised firmware mode.
    Custom,
}

impl OnboardLedMode {
    /// Map a G402-family onboard LED mode byte.
    ///
    /// Reverse-engineered: libratbag `hidpp20_led_mode`.
    #[must_use]
    pub const fn from_firmware(mode: u8) -> Self {
        match mode {
            0x00 => Self::Off,
            0x01 => Self::On,
            0x03 => Self::Cycle,
            0x04 => Self::ColorWave,
            0x05 => Self::Starlight,
            0x0a => Self::Breathing,
            0x0b => Self::Ripple,
            // 0x0c is the custom-animation slot; unknown bytes share that arm.
            _ => Self::Custom,
        }
    }

    /// English catalog key for this effect (the GUI `tr!` source string).
    #[must_use]
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::On => "Solid color",
            Self::Cycle => "Color cycle",
            Self::ColorWave => "Color wave",
            Self::Starlight => "Starlight",
            Self::Breathing => "Breathing",
            Self::Ripple => "Ripple",
            Self::Custom => "Custom animation",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_led_modes_map_to_named_effects() {
        assert_eq!(OnboardLedMode::from_firmware(0x00), OnboardLedMode::Off);
        assert_eq!(OnboardLedMode::from_firmware(0x01), OnboardLedMode::On);
        assert_eq!(
            OnboardLedMode::from_firmware(0x0a),
            OnboardLedMode::Breathing
        );
        assert_eq!(OnboardLedMode::from_firmware(0xff), OnboardLedMode::Custom);
        assert_eq!(OnboardLedMode::Breathing.label_key(), "Breathing");
    }
}
