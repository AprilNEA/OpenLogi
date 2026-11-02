//! Camera settings: the legacy-key migration, the preview's stream mode, and
//! camera-triggered automation.

use super::*;

fn camera_controls(brightness: i32) -> openlogi_core::config::CameraControls {
    openlogi_core::config::CameraControls(std::collections::BTreeMap::from([(
        "brightness".into(),
        brightness,
    )]))
}

fn camera_state(config: Config) -> AppState {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    AppState::new(Sources::in_memory(config, &AssetResolver::new(), tx))
}

#[test]
fn migrate_lifts_legacy_port_bound_camera_key() {
    let mut config = Config::ephemeral();
    let model = "camera:046d:0893";
    let legacy = "camera-0x1123000046d0893";
    config.set_camera_controls(legacy, camera_controls(42));
    let mut state = camera_state(config);

    state.migrate_legacy_camera_key(model, "0x1123000046d0893");

    assert_eq!(
        state
            .config
            .camera_controls(model)
            .map(|c| c.0["brightness"]),
        Some(42)
    );
    assert!(state.config.camera_controls(legacy).is_none());
}

#[test]
fn migrate_does_not_overwrite_existing_model_settings() {
    let mut config = Config::ephemeral();
    let model = "camera:046d:0893";
    let legacy = "camera-0x1123000046d0893";
    config.set_camera_controls(model, camera_controls(1));
    config.set_camera_controls(legacy, camera_controls(99));
    let mut state = camera_state(config);

    state.migrate_legacy_camera_key(model, "0x1123000046d0893");

    assert_eq!(
        state
            .config
            .camera_controls(model)
            .map(|c| c.0["brightness"]),
        Some(1)
    );
    assert_eq!(
        state
            .config
            .camera_controls(legacy)
            .map(|c| c.0["brightness"]),
        Some(99)
    );
}

const STREAM_KEY: &str = "camera:046d:0893";

fn stream_mode(width: u32, height: u32, fps: u32) -> openlogi_camera::StreamMode {
    openlogi_camera::StreamMode { width, height, fps }
}

#[test]
fn no_stream_mode_is_saved_until_one_is_chosen() {
    let state = camera_state(Config::ephemeral());
    assert_eq!(state.camera_stream_mode(STREAM_KEY), None);
}

#[test]
fn a_chosen_stream_mode_is_saved_and_announced() {
    let mut state = camera_state(Config::ephemeral());
    let mode = stream_mode(1920, 1080, 60);

    let events = state.commit_camera_stream_mode(STREAM_KEY, mode);

    assert_eq!(events, [StateEvent::CameraChanged]);
    assert_eq!(state.camera_stream_mode(STREAM_KEY), Some(mode));
    // Another camera's choice is its own.
    assert_eq!(state.camera_stream_mode("camera:046d:085e"), None);
}

#[test]
fn choosing_the_saved_stream_mode_again_announces_nothing() {
    let mut state = camera_state(Config::ephemeral());
    let mode = stream_mode(1280, 720, 30);
    let _ = state.commit_camera_stream_mode(STREAM_KEY, mode);

    assert!(
        state.commit_camera_stream_mode(STREAM_KEY, mode).is_empty(),
        "a repeat pick must not restart the stream"
    );
    assert_eq!(state.camera_stream_mode(STREAM_KEY), Some(mode));
}

#[test]
fn a_changed_stream_mode_replaces_the_saved_one() {
    let mut state = camera_state(Config::ephemeral());
    let _ = state.commit_camera_stream_mode(STREAM_KEY, stream_mode(1920, 1080, 60));

    let events = state.commit_camera_stream_mode(STREAM_KEY, stream_mode(640, 480, 15));

    assert_eq!(events, [StateEvent::CameraChanged]);
    assert_eq!(
        state.camera_stream_mode(STREAM_KEY),
        Some(stream_mode(640, 480, 15))
    );
}

#[test]
fn a_stream_mode_is_not_saved_among_the_uvc_controls() {
    // The controls map is what the panel writes to the camera and what a
    // profile snapshots; the preview's mode is neither.
    let mut state = camera_state(Config::ephemeral());

    let _ = state.commit_camera_stream_mode(STREAM_KEY, stream_mode(1280, 720, 30));

    assert!(state.config.camera_controls(STREAM_KEY).is_none());
    assert!(state.config.camera_stream_mode(STREAM_KEY).is_some());
}

#[test]
fn saving_a_stream_mode_keeps_the_uvc_controls_and_they_keep_it() {
    let mut config = Config::ephemeral();
    config.set_camera_controls(STREAM_KEY, camera_controls(42));
    let mut state = camera_state(config);
    let mode = stream_mode(1920, 1080, 30);

    let _ = state.commit_camera_stream_mode(STREAM_KEY, mode);
    assert_eq!(
        state.camera_control(STREAM_KEY, openlogi_camera::CameraControl::Brightness),
        Some(42),
        "choosing a mode must not lose a saved control"
    );

    let _ = state.commit_camera_control(STREAM_KEY, openlogi_camera::CameraControl::Contrast, 7);
    let _ = state.commit_camera_auto(STREAM_KEY, openlogi_camera::AutoToggle::Focus, true);
    assert_eq!(
        state.camera_stream_mode(STREAM_KEY),
        Some(mode),
        "adjusting a control must not lose the chosen mode"
    );
}

#[test]
fn a_stream_mode_with_a_zero_is_not_saved() {
    let mut state = camera_state(Config::ephemeral());

    let events = state.commit_camera_stream_mode(STREAM_KEY, stream_mode(1920, 1080, 0));

    assert!(events.is_empty());
    assert_eq!(state.camera_stream_mode(STREAM_KEY), None);
    assert!(state.config.camera_stream_mode(STREAM_KEY).is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn camera_automation_preserves_manual_power_and_clears_transient_override() {
    let light = StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "serial:glow-camera".into(),
        },
        display_name: "Litra Glow".into(),
        manufacturer: Some("Logi".into()),
        serial_number: Some("glow-camera".into()),
        unit_id: [0; 4],
        kind: DeviceKind::Light,
        online: true,
        capabilities: None,
        light_capabilities: Some(LightCapabilities {
            power: true,
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
    state.config.edit(|config| {
        config.set_light(
            key.as_str(),
            LightSettings {
                enabled: false,
                auto_camera: true,
                brightness_percent: 70,
                temperature_kelvin: None,
                color: None,
            },
        );
    });

    assert!(!state.light_enabled());
    assert_eq!(state.set_camera_active(true), [StateEvent::CameraChanged]);
    assert!(state.light_enabled());
    assert!(!state.light().enabled);

    let _ = state.commit_manual_light_power(false);
    assert!(!state.light_enabled());
    assert!(matches!(
        receiver.try_recv(),
        Ok(crate::services::ipc::Command::SetLightManualPower(
            SetLightManualPower { enabled: false, .. }
        ))
    ));

    assert_eq!(state.set_camera_active(false), [StateEvent::CameraChanged]);
    assert_eq!(state.set_camera_active(true), [StateEvent::CameraChanged]);
    assert!(state.light_enabled());
    assert!(!state.light().enabled);
}

#[cfg(target_os = "macos")]
#[test]
fn enabling_camera_automation_queues_effective_camera_power() {
    let light = StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "serial:glow-effective".into(),
        },
        display_name: "Litra Glow".into(),
        manufacturer: Some("Logi".into()),
        serial_number: Some("glow-effective".into()),
        unit_id: [0; 4],
        kind: DeviceKind::Light,
        online: true,
        capabilities: None,
        light_capabilities: Some(LightCapabilities {
            power: true,
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
    let _ = state.set_camera_active(true);
    let mut settings = state.light();
    settings.enabled = false;
    settings.auto_camera = true;

    let _ = state.commit_light(settings);

    assert!(matches!(
        receiver.try_recv(),
        Ok(crate::services::ipc::Command::SetLight(SetLight {
            command: openlogi_core::hid::LightCommand::Power(true),
            ..
        }))
    ));
    assert!(!state.light().enabled);
    assert!(state.light_enabled());
}
