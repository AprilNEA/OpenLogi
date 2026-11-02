//! Implements the `LedControl` feature (ID `0x1300`): the LEDs a device lets
//! software drive, such as the DPI LED of the MX Ergo.
//!
//! The firmware drives the LEDs until software takes control of them with
//! `SetSWControl`. Logi Options+ takes control before it lights the DPI LED
//! in precision mode, and never hands it back
//! (`devio::Feature1300LEDControl`, `logi::devices::feature_1300_led_control`).

use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// What an LED shows, from `GetLEDInfo`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LedKind {
    /// The battery level.
    Battery,
    /// The DPI or pointer-speed state.
    Dpi,
    /// The onboard profile.
    Profile,
    /// A logo light.
    Logo,
    /// A decorative light.
    Cosmetic,
    /// A type this crate does not model.
    Other(u8),
}

impl LedKind {
    const fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::Battery,
            2 => Self::Dpi,
            3 => Self::Profile,
            4 => Self::Logo,
            5 => Self::Cosmetic,
            other => Self::Other(other),
        }
    }
}

/// A state software can set on an LED with `SetLEDState`.
///
/// The spec lists more states (blink, breathing and others) as bits of the
/// same field. OpenLogi writes only these two.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LedState {
    /// The LED is dark.
    Off,
    /// The LED is lit.
    On,
}

impl LedState {
    /// The state's bit in `SetLEDState` and in [`LedInfo::capabilities`].
    #[must_use]
    pub const fn raw(self) -> u16 {
        match self {
            Self::Off => 0x0001,
            Self::On => 0x0002,
        }
    }
}

/// One LED, from `GetLEDInfo`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LedInfo {
    /// The LED's index, as the other functions take it.
    pub index: u8,
    /// What the LED shows.
    pub kind: LedKind,
    /// How many physical LEDs this LED drives.
    pub physical_leds: u8,
    /// The states software can set, one bit per state ([`LedState::raw`]).
    pub capabilities: u16,
    /// The states the LED can keep in non-volatile memory.
    pub nv_capabilities: u8,
}

impl LedInfo {
    /// Whether software can set `state` on this LED.
    #[must_use]
    pub const fn supports(&self, state: LedState) -> bool {
        self.capabilities & state.raw() != 0
    }

    fn from_payload(payload: &[u8; 16]) -> Self {
        Self {
            index: payload[0],
            kind: LedKind::from_raw(payload[1]),
            physical_leds: payload[2],
            capabilities: u16::from_be_bytes([payload[3], payload[4]]),
            nv_capabilities: payload[5],
        }
    }
}

/// Implements the `LedControl` / `0x1300` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x1300, version = 0)]
pub struct LedControlFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl LedControlFeature {
    /// Retrieves the number of LEDs (`GetLEDCount`).
    pub async fn get_led_count(&self) -> Result<u8, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(payload[0])
    }

    /// Retrieves the LED at `index` (`GetLEDInfo`).
    pub async fn get_led_info(&self, index: u8) -> Result<LedInfo, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(1, [index, 0x00, 0x00])
            .await?
            .extend_payload();
        Ok(LedInfo::from_payload(&payload))
    }

    /// Whether software drives the LEDs now, not the firmware
    /// (`GetSWControl`).
    pub async fn get_sw_control(&self) -> Result<bool, Hidpp20Error> {
        let payload = self.endpoint.call(2, [0; 3]).await?.extend_payload();
        Ok(payload[0] != 0)
    }

    /// Gives the LEDs to software (`true`) or back to the firmware
    /// (`SetSWControl`).
    pub async fn set_sw_control(&self, software: bool) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call(3, [u8::from(software), 0x00, 0x00])
            .await?;
        Ok(())
    }

    /// Sets the LED at `index` to `state` (`SetLEDState`). The LED shows it
    /// only while software drives the LEDs ([`Self::set_sw_control`]).
    pub async fn set_led_state(&self, index: u8, state: LedState) -> Result<(), Hidpp20Error> {
        let mut args = [0; 16];
        args[0] = index;
        args[1..3].copy_from_slice(&state.raw().to_be_bytes());
        self.endpoint.call_long(5, args).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
