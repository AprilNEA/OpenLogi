//! Standalone light writes: failures, superseded writes and transient state.

use super::*;

fn next_light_command(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<crate::services::ipc::Command>,
) -> (openlogi_core::hid::LightCommand, u64) {
    let Ok(crate::services::ipc::Command::SetLight(SetLight {
        command,
        request_id,
        ..
    })) = receiver.try_recv()
    else {
        panic!("expected a light command");
    };
    (command, request_id)
}

#[test]
fn light_write_failure_reaches_the_gui_state() {
    let light = StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "serial:glow-1".into(),
        },
        display_name: "Litra Glow".into(),
        manufacturer: Some("Logi".into()),
        serial_number: Some("glow-1".into()),
        unit_id: [0; 4],
        kind: DeviceKind::Light,
        online: true,
        capabilities: None,
        light_capabilities: Some(LightCapabilities {
            power: true,
            brightness: Some(
                LightValueRange::new(20, 250, 1, LightValueUnit::Lumens).expect("valid range"),
            ),
            ..LightCapabilities::default()
        }),
        driver_id: "litra".into(),
        registry_model_id: Some("8c900".into()),
    };
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        standalone: &[light],
        ..Sources::in_memory(Config::default(), &AssetResolver::new(), commands)
    });
    let key = state.current_record().expect("light record").device_key();
    let requested = LightSettings::new(false, 50, None);
    let _ = state.commit_light(requested);
    let Ok(crate::services::ipc::Command::SetLight(SetLight {
        command: openlogi_core::hid::LightCommand::Power(false),
        request_id,
        ..
    })) = receiver.try_recv()
    else {
        panic!("expected the power command");
    };
    let Ok(crate::services::ipc::Command::SetLight(SetLight {
        command: openlogi_core::hid::LightCommand::BrightnessPercent(50),
        request_id: brightness_request_id,
        ..
    })) = receiver.try_recv()
    else {
        panic!("expected the brightness command");
    };
    assert_eq!(brightness_request_id, request_id);
    assert_eq!(state.light(), requested);
    assert_eq!(state.config.light(key.as_str()), None);
    assert!(matches!(
        state.light_command_status(),
        Some(LightCommandStatus::Pending)
    ));
    assert_eq!(
        state.apply_light_command_result(
            key.clone(),
            request_id,
            openlogi_core::hid::LightCommand::Power(false),
            Ok(()),
        ),
        [lighting_changed(&key)]
    );
    assert_eq!(state.light(), requested);
    assert_eq!(
        state.apply_light_command_result(
            key.clone(),
            request_id,
            openlogi_core::hid::LightCommand::BrightnessPercent(50),
            Err(WriteError::AmbiguousRawDevice),
        ),
        [lighting_changed(&key)]
    );
    assert!(matches!(
        state.light_command_status(),
        Some(LightCommandStatus::Failed(message)) if message.contains("multiple raw HID")
    ));
    assert_eq!(
        state.light(),
        LightSettings::new(false, LightSettings::default().brightness_percent, None)
    );
    assert_eq!(state.config.light(key.as_str()), Some(state.light()));
}

#[test]
fn superseded_light_write_keeps_prior_successes_for_reconciliation() {
    let light = superseded_litra_light();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        standalone: &[light],
        ..Sources::in_memory(Config::default(), &AssetResolver::new(), commands)
    });
    let key = state.current_record().expect("light record").device_key();

    let _ = state.commit_light(LightSettings::new(false, 40, None));
    let (first_power, first_request_id) = next_light_command(&mut receiver);
    let (first_brightness, first_brightness_request_id) = next_light_command(&mut receiver);
    assert_eq!(first_power, openlogi_core::hid::LightCommand::Power(false));
    assert_eq!(
        first_brightness,
        openlogi_core::hid::LightCommand::BrightnessPercent(40)
    );
    assert_eq!(first_brightness_request_id, first_request_id);

    let _ = state.commit_light(LightSettings::new(true, 60, None));
    let (second_power, second_request_id) = next_light_command(&mut receiver);
    let (second_brightness, second_brightness_request_id) = next_light_command(&mut receiver);
    assert_eq!(second_power, openlogi_core::hid::LightCommand::Power(true));
    assert_eq!(
        second_brightness,
        openlogi_core::hid::LightCommand::BrightnessPercent(60)
    );
    assert_ne!(second_request_id, first_request_id);
    assert_eq!(second_brightness_request_id, second_request_id);

    assert_eq!(
        state.apply_light_command_result(
            key.clone(),
            second_request_id,
            openlogi_core::hid::LightCommand::Power(true),
            Ok(()),
        ),
        [lighting_changed(&key)]
    );
    assert_eq!(
        state.apply_light_command_result(
            key.clone(),
            second_request_id,
            openlogi_core::hid::LightCommand::BrightnessPercent(60),
            Err(WriteError::AmbiguousRawDevice),
        ),
        [lighting_changed(&key)]
    );
    assert_eq!(state.light(), LightSettings::new(true, 60, None));
    assert!(matches!(
        state.light_command_status(),
        Some(LightCommandStatus::Pending)
    ));

    assert_eq!(
        state.apply_light_command_result(
            key.clone(),
            first_request_id,
            openlogi_core::hid::LightCommand::Power(false),
            Ok(()),
        ),
        [lighting_changed(&key)]
    );
    assert_eq!(
        state.apply_light_command_result(
            key.clone(),
            first_request_id,
            openlogi_core::hid::LightCommand::BrightnessPercent(40),
            Ok(()),
        ),
        [lighting_changed(&key)]
    );

    assert!(matches!(
        state.light_command_status(),
        Some(LightCommandStatus::Failed(message)) if message.contains("multiple raw HID")
    ));
    assert_eq!(state.light(), LightSettings::new(true, 40, None));
    assert_eq!(state.config.light(key.as_str()), Some(state.light()));
}

#[test]
fn transient_light_state_is_kept_in_memory_and_only_supported_commands_are_sent() {
    let light = StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "id:session-node".into(),
        },
        display_name: "Brightness-only light".into(),
        manufacturer: Some("Test".into()),
        serial_number: None,
        unit_id: [0; 4],
        kind: DeviceKind::Light,
        online: true,
        capabilities: None,
        light_capabilities: Some(LightCapabilities {
            power: false,
            brightness: Some(
                LightValueRange::new(0, 100, 1, LightValueUnit::Percent).expect("valid range"),
            ),
            ..LightCapabilities::default()
        }),
        driver_id: "test-light".into(),
        registry_model_id: None,
    };
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        standalone: &[light],
        ..Sources::in_memory(Config::default(), &AssetResolver::new(), commands)
    });
    let settings = LightSettings::new(false, 37, None);

    let _ = state.commit_light(settings);

    assert_eq!(state.light(), settings);
    assert!(!state.light_enabled());
    assert!(matches!(
        receiver.try_recv(),
        Ok(crate::services::ipc::Command::SetLight(SetLight {
            command: openlogi_core::hid::LightCommand::BrightnessPercent(37),
            ..
        }))
    ));
    assert!(receiver.try_recv().is_err());
}
