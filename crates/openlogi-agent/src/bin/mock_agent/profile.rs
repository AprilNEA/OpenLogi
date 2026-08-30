use openlogi_device::fixture::{
    CANONICAL_DEVICE_PROFILE_JSON, DeviceProfile, ProfileDeviceSettings, ProfileSetting,
    ProfileSupport,
};
use openlogi_device::{DeviceRoute, LightCommand, WriteError};

use super::{State, standalone_route};

pub(super) fn built_in_profile() -> Result<DeviceProfile, String> {
    parse_profile(
        CANONICAL_DEVICE_PROFILE_JSON,
        "built-in canonical device profile",
    )
}

pub(super) fn parse_profile(encoded: &str, source: &str) -> Result<DeviceProfile, String> {
    let profile: DeviceProfile = serde_json::from_str(encoded)
        .map_err(|error| format!("could not parse {source}: {error}"))?;
    profile
        .validate()
        .map_err(|error| format!("could not validate {source}: {error}"))?;
    Ok(profile)
}

pub(super) fn unsupported_settings(route: DeviceRoute) -> ProfileDeviceSettings {
    ProfileDeviceSettings {
        route,
        dpi: ProfileSetting::Unsupported,
        smartshift: ProfileSetting::Unsupported,
        wheel: ProfileSetting::Unsupported,
        backlight: ProfileSetting::Unsupported,
        lighting: ProfileSupport::Unsupported,
        light: ProfileSupport::Unsupported,
    }
}

pub(super) fn validate_light_command(
    state: &State,
    route: &DeviceRoute,
    command: LightCommand,
) -> Result<(), WriteError> {
    let settings = state.settings_for(route)?;
    if !settings.light.is_supported() {
        return Err(light_unsupported(command));
    }
    let capabilities = state
        .profile
        .standalone
        .iter()
        .find(|device| standalone_route(device) == *route)
        .and_then(|device| device.light_capabilities)
        .ok_or(WriteError::DeviceNotFound)?;
    match command {
        LightCommand::Power(_) if capabilities.power => Ok(()),
        LightCommand::Power(_) => Err(light_unsupported(command)),
        LightCommand::BrightnessPercent(value) => {
            if capabilities.brightness.is_none() {
                Err(light_unsupported(command))
            } else if value > 100 {
                Err(WriteError::InvalidLightValue {
                    control: "brightness_percent".to_string(),
                    value: u16::from(value),
                })
            } else {
                Ok(())
            }
        }
        LightCommand::TemperatureKelvin(value) => match capabilities.temperature {
            Some(range) if range.contains(value) => Ok(()),
            Some(_) => Err(WriteError::InvalidLightValue {
                control: "temperature_kelvin".to_string(),
                value,
            }),
            None => Err(light_unsupported(command)),
        },
        LightCommand::BrightnessNative(value) => match capabilities.brightness {
            Some(range) if range.contains(value) => Ok(()),
            Some(_) => Err(WriteError::InvalidLightValue {
                control: "brightness_native".to_string(),
                value,
            }),
            None => Err(light_unsupported(command)),
        },
    }
}

fn light_unsupported(command: LightCommand) -> WriteError {
    let control = match command {
        LightCommand::Power(_) => "power",
        LightCommand::BrightnessPercent(_) | LightCommand::BrightnessNative(_) => "brightness",
        LightCommand::TemperatureKelvin(_) => "temperature",
    };
    WriteError::LightUnsupported {
        control: control.to_string(),
    }
}
