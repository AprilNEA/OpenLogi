use openlogi_core::config::SMARTSHIFT_AUTO_DISENGAGE_DEFAULT;
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, Capabilities, DeviceInventory, DeviceKind,
    DeviceModelInfo, DeviceTransports, LightCapabilities, LightValueRange, LightValueUnit,
    PairedDevice, RawDeviceAddress, ReceiverInfo, StandaloneDevice,
};
use openlogi_core::hid::LOGITECH_VENDOR_ID;
use openlogi_device::fixture::{
    DeviceProfile, FIXTURE_SCHEMA_VERSION, ProfileDeviceSettings, ProfileSetting, ProfileSupport,
};
use openlogi_device::{
    DIRECT_DEVICE_INDEX, DeviceRoute, Dpi, DpiCapabilities, DpiInfo, LITRA_GLOW_PRODUCT_ID,
    LightCommand, ScrollReportingTarget, ScrollResolution, ScrollWheelMode,
    SmartShiftAutoDisengage, SmartShiftMode, SmartShiftStatus, WriteError,
};

use super::{
    DIRECT_PID, KEYBOARD_SLOT, MOCK_TORQUE, MOUSE_SLOT, OFFLINE_SLOT, RECEIVER_UID, State,
};

pub(super) fn built_in_profile() -> Result<DeviceProfile, String> {
    let mouse_route = DeviceRoute::Bolt {
        receiver_uid: RECEIVER_UID.to_string(),
        slot: MOUSE_SLOT,
    };
    let offline_route = DeviceRoute::Bolt {
        receiver_uid: RECEIVER_UID.to_string(),
        slot: OFFLINE_SLOT,
    };
    let keyboard_route = DeviceRoute::Bolt {
        receiver_uid: RECEIVER_UID.to_string(),
        slot: KEYBOARD_SLOT,
    };
    let direct_route = DeviceRoute::Direct {
        vendor_id: LOGITECH_VENDOR_ID,
        product_id: DIRECT_PID,
    };
    let standalone = standalone_light()?;
    let light_route = standalone_route(&standalone);

    Ok(DeviceProfile {
        schema_version: FIXTURE_SCHEMA_VERSION,
        id: "openlogi-mock-demo".to_string(),
        name: "OpenLogi animated mock devices".to_string(),
        inventories: vec![bolt_inventory(), direct_inventory()],
        standalone: vec![standalone],
        settings: vec![
            ProfileDeviceSettings {
                route: mouse_route,
                dpi: supported_dpi(1600, (200u16..=8000).step_by(50).collect())?,
                smartshift: ProfileSetting::Supported(SmartShiftStatus {
                    mode: SmartShiftMode::Ratchet,
                    auto_disengage: SmartShiftAutoDisengage::Threshold(
                        SMARTSHIFT_AUTO_DISENGAGE_DEFAULT,
                    ),
                    tunable_torque: Some(MOCK_TORQUE),
                }),
                wheel: ProfileSetting::Supported(ScrollWheelMode {
                    resolution: ScrollResolution::High,
                    inverted: false,
                    target: ScrollReportingTarget::Native,
                }),
                backlight: ProfileSetting::Unsupported,
                lighting: ProfileSupport::Unsupported,
                light: ProfileSupport::Unsupported,
            },
            unsupported_settings(offline_route),
            ProfileDeviceSettings {
                route: keyboard_route,
                dpi: ProfileSetting::Unsupported,
                smartshift: ProfileSetting::Unsupported,
                wheel: ProfileSetting::Unsupported,
                backlight: ProfileSetting::Unsupported,
                lighting: ProfileSupport::Supported,
                light: ProfileSupport::Unsupported,
            },
            ProfileDeviceSettings {
                route: direct_route,
                dpi: supported_dpi(1000, (400u16..=4000).step_by(100).collect())?,
                smartshift: ProfileSetting::Unsupported,
                wheel: ProfileSetting::Unsupported,
                backlight: ProfileSetting::Unsupported,
                lighting: ProfileSupport::Unsupported,
                light: ProfileSupport::Unsupported,
            },
            ProfileDeviceSettings {
                route: light_route,
                dpi: ProfileSetting::Unsupported,
                smartshift: ProfileSetting::Unsupported,
                wheel: ProfileSetting::Unsupported,
                backlight: ProfileSetting::Unsupported,
                lighting: ProfileSupport::Unsupported,
                light: ProfileSupport::Supported,
            },
        ],
    })
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

fn supported_dpi(current: u16, values: Vec<u16>) -> Result<ProfileSetting<DpiInfo>, String> {
    let capabilities =
        DpiCapabilities::new(values).map_err(|error| format!("invalid built-in DPI: {error}"))?;
    Ok(ProfileSetting::Supported(DpiInfo {
        current: Dpi::new(current),
        capabilities,
    }))
}

fn standalone_light() -> Result<StandaloneDevice, String> {
    let brightness = LightValueRange::new(0, 100, 1, LightValueUnit::Percent)
        .map_err(|error| error.to_string())?;
    let temperature = LightValueRange::new(2700, 6500, 100, LightValueUnit::Kelvin)
        .map_err(|error| error.to_string())?;
    Ok(StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: LOGITECH_VENDOR_ID,
            product_id: LITRA_GLOW_PRODUCT_ID,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "MOCK-LITRA-01".to_string(),
        },
        display_name: "Litra Glow".to_string(),
        manufacturer: Some("Logitech".to_string()),
        serial_number: Some("MOCKLITRA1".to_string()),
        unit_id: [0x0d, 0x0e, 0x0f, 0x10],
        kind: DeviceKind::Unknown,
        online: true,
        capabilities: None,
        light_capabilities: Some(LightCapabilities {
            power: true,
            brightness: Some(brightness),
            temperature: Some(temperature),
            color: false,
            zones: false,
        }),
        driver_id: "litra".to_string(),
        registry_model_id: Some("8c900".to_string()),
    })
}

fn standalone_route(device: &StandaloneDevice) -> DeviceRoute {
    DeviceRoute::RawHid {
        vendor_id: device.address.vendor_id,
        product_id: device.address.product_id,
        usage_page: device.address.usage_page,
        usage_id: device.address.usage_id,
        identity: device.address.identity.clone(),
    }
}

fn bolt_inventory() -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "Logi Bolt Receiver".to_string(),
            vendor_id: LOGITECH_VENDOR_ID,
            product_id: 0xc548,
            unique_id: Some(RECEIVER_UID.to_string()),
        },
        paired: vec![
            PairedDevice {
                slot: MOUSE_SLOT,
                codename: Some("MX Master 3S".to_string()),
                wpid: Some(0xb034),
                kind: DeviceKind::Mouse,
                online: true,
                battery: Some(BatteryInfo {
                    percentage: 80,
                    level: BatteryLevel::Good,
                    status: BatteryStatus::Discharging,
                }),
                model_info: Some(DeviceModelInfo {
                    entity_count: 3,
                    serial_number: Some("2140LZ00MOCK".to_string()),
                    unit_id: [0x01, 0x02, 0x03, 0x04],
                    transports: DeviceTransports {
                        usb: false,
                        equad: true,
                        btle: true,
                        bluetooth: false,
                    },
                    model_ids: [0xb034, 0x4082, 0],
                    extended_model_id: 0x0b,
                }),
                capabilities: Some(Capabilities {
                    buttons: true,
                    pointer: true,
                    lighting: false,
                    scroll_inversion: true,
                    hires_wheel: true,
                    thumbwheel: true,
                    haptic_feedback: true,
                    haptic_panel: true,
                }),
            },
            PairedDevice {
                slot: OFFLINE_SLOT,
                codename: Some("MX Anywhere 3".to_string()),
                wpid: Some(0x4090),
                kind: DeviceKind::Mouse,
                online: false,
                battery: None,
                model_info: None,
                capabilities: None,
            },
            PairedDevice {
                slot: KEYBOARD_SLOT,
                codename: Some("MX Keys".to_string()),
                wpid: Some(0x408a),
                kind: DeviceKind::Keyboard,
                online: true,
                battery: Some(BatteryInfo {
                    percentage: 100,
                    level: BatteryLevel::Full,
                    status: BatteryStatus::Full,
                }),
                model_info: Some(DeviceModelInfo {
                    entity_count: 2,
                    serial_number: None,
                    unit_id: [0x05, 0x06, 0x07, 0x08],
                    transports: DeviceTransports {
                        usb: false,
                        equad: true,
                        btle: true,
                        bluetooth: false,
                    },
                    model_ids: [0xb35b, 0x408a, 0],
                    extended_model_id: 0,
                }),
                capabilities: Some(Capabilities {
                    buttons: false,
                    pointer: false,
                    lighting: true,
                    scroll_inversion: false,
                    hires_wheel: false,
                    thumbwheel: false,
                    haptic_feedback: false,
                    haptic_panel: false,
                }),
            },
        ],
    }
}

fn direct_inventory() -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "MX Vertical".to_string(),
            vendor_id: LOGITECH_VENDOR_ID,
            product_id: DIRECT_PID,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: DIRECT_DEVICE_INDEX,
            codename: Some("MX Vertical".to_string()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: Some(BatteryInfo {
                percentage: 55,
                level: BatteryLevel::Good,
                status: BatteryStatus::Discharging,
            }),
            model_info: Some(DeviceModelInfo {
                entity_count: 2,
                serial_number: None,
                unit_id: [0x09, 0x0a, 0x0b, 0x0c],
                transports: DeviceTransports {
                    usb: true,
                    equad: false,
                    btle: true,
                    bluetooth: false,
                },
                model_ids: [DIRECT_PID, 0, 0],
                extended_model_id: 0,
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
