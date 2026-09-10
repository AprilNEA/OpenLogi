//! Semantic standalone-light commands — pure data, no I/O.
//!
//! The driver that encodes [`LightCommand`] into a device-specific raw HID
//! report (e.g. Litra) lives in `openlogi_hid::write::litra`.

use serde::{Deserialize, Serialize};

use crate::config::LightSettings;
use crate::device::LightCapabilities;

/// A semantic command accepted by the standalone-light layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LightCommand {
    /// Turn the light on or off.
    Power(bool),
    /// Set normalized brightness from 0 to 100 percent.
    BrightnessPercent(u8),
    /// Set colour temperature in Kelvin.
    TemperatureKelvin(u16),
    /// Set brightness in the native unit advertised by the selected model.
    /// This is primarily a diagnostic/CLI convenience; persisted settings
    /// remain normalized percentages.
    BrightnessNative(u16),
}

/// Expand protocol-neutral saved settings into only the controls advertised
/// by a standalone light. Unsupported controls are omitted rather than sent
/// speculatively, which keeps power-only and brightness-only drivers usable.
///
/// The order is part of the contract: every value write is bracketed by the
/// power write on the side that leaves the device lit while it receives them —
/// power on first when switching on, power off last when switching off. A
/// device that is told to go dark must not then be handed a value.
#[must_use]
pub fn commands_for_light_settings(
    settings: LightSettings,
    capabilities: LightCapabilities,
) -> Vec<LightCommand> {
    let mut commands = Vec::new();
    // A Litra flashes when a brightness report reaches it a few hundred
    // milliseconds after its power-off report — exactly the gap a re-apply
    // leaves between the two writes.
    if capabilities.power && settings.enabled {
        commands.push(LightCommand::Power(true));
    }
    if capabilities.brightness.is_some() {
        commands.push(LightCommand::BrightnessPercent(settings.brightness_percent));
    }
    if capabilities.temperature.is_some()
        && let Some(kelvin) = settings.temperature_kelvin
    {
        commands.push(LightCommand::TemperatureKelvin(kelvin));
    }
    if capabilities.power && !settings.enabled {
        commands.push(LightCommand::Power(false));
    }
    commands
}

#[cfg(test)]
mod tests {
    use super::{LightCommand, commands_for_light_settings};
    use crate::config::LightSettings;
    use crate::device::{LightCapabilities, LightValueRange, LightValueUnit};

    #[test]
    fn light_settings_expand_only_to_advertised_controls() {
        let Ok(brightness) = LightValueRange::new(0, 100, 1, LightValueUnit::Percent) else {
            panic!("valid brightness fixture");
        };
        let settings = LightSettings::new(false, 37, Some(4600));
        let commands = commands_for_light_settings(
            settings,
            LightCapabilities {
                brightness: Some(brightness),
                ..LightCapabilities::default()
            },
        );

        assert_eq!(commands, vec![LightCommand::BrightnessPercent(37)]);
    }

    fn full_capabilities() -> LightCapabilities {
        let Ok(brightness) = LightValueRange::new(0, 100, 1, LightValueUnit::Percent) else {
            panic!("valid brightness fixture");
        };
        let Ok(temperature) = LightValueRange::new(2700, 6500, 100, LightValueUnit::Kelvin) else {
            panic!("valid temperature fixture");
        };
        LightCapabilities {
            power: true,
            brightness: Some(brightness),
            temperature: Some(temperature),
            ..LightCapabilities::default()
        }
    }

    #[test]
    fn switching_on_powers_up_before_writing_values() {
        let commands = commands_for_light_settings(
            LightSettings::new(true, 60, Some(4600)),
            full_capabilities(),
        );

        assert_eq!(
            commands,
            vec![
                LightCommand::Power(true),
                LightCommand::BrightnessPercent(60),
                LightCommand::TemperatureKelvin(4600),
            ]
        );
    }

    #[test]
    fn switching_off_writes_values_before_powering_down() {
        // The device must not receive a value report after being told to go
        // dark: a Litra flashes when brightness arrives just after power-off.
        let commands = commands_for_light_settings(
            LightSettings::new(false, 60, Some(4600)),
            full_capabilities(),
        );

        assert_eq!(
            commands,
            vec![
                LightCommand::BrightnessPercent(60),
                LightCommand::TemperatureKelvin(4600),
                LightCommand::Power(false),
            ]
        );
    }
}
