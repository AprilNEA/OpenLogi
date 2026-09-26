//! Device identity as it is persisted: what is kept, what is stripped, and custom names.

use super::*;

#[test]
fn device_identity_roundtrips_and_is_iterable() {
    use crate::device::{Capabilities, DeviceKind};

    let mut cfg = Config::default();
    let mouse = DeviceIdentity {
        display_name: "MX Master 3S".to_string(),
        model_info: None,
        codename: None,
        kind: DeviceKind::Mouse,
        capabilities: Capabilities {
            buttons: true,
            pointer: true,
            lighting: false,
            scroll_inversion: false,
            hires_wheel: true,
            thumbwheel: false,
            haptic_feedback: false,
            haptic_panel: false,
            dpi_gestures: true,
        },
        light_capabilities: None,
        driver_id: None,
        registry_model_id: None,
    };
    cfg.set_device_identity("2b034", mouse.clone());
    // Recording an identity must not disturb unrelated per-device state.
    cfg.set_binding(
        "2b034",
        ButtonId::Back,
        Binding::Single(Action::BrowserBack),
    );

    let parsed = write_and_read(&cfg);
    assert_eq!(parsed.device_identity("2b034"), Some(&mouse));
    assert_eq!(parsed.device_identity("absent"), None);
    assert_eq!(
        parsed.stored_bindings("2b034").get(&ButtonId::Back),
        Some(&Binding::Single(Action::BrowserBack)),
        "identity must coexist with bindings on the same device block"
    );
    assert_eq!(
        parsed.known_identities().collect::<Vec<_>>(),
        vec![("2b034", &mouse)]
    );
}

#[test]
fn custom_device_name_roundtrips_without_changing_model_identity() {
    use crate::device::{Capabilities, DeviceKind};

    let mut config = Config::default();
    config.set_device_identity(
        "receiver:test:slot:1",
        DeviceIdentity {
            display_name: "MX Master 4".into(),
            model_info: None,
            codename: None,
            kind: DeviceKind::Mouse,
            capabilities: Capabilities::default(),
            light_capabilities: None,
            driver_id: None,
            registry_model_id: None,
        },
    );
    config.set_device_custom_name("receiver:test:slot:1", Some("Office".into()));

    let parsed = write_and_read(&config);

    assert_eq!(
        parsed.device_custom_name("receiver:test:slot:1"),
        Some("Office")
    );
    assert_eq!(
        parsed
            .device_identity("receiver:test:slot:1")
            .map(|identity| identity.display_name.as_str()),
        Some("MX Master 4")
    );
}

#[test]
fn persisted_identity_strips_per_unit_identifiers() {
    use crate::device::{Capabilities, DeviceKind, DeviceModelInfo, DeviceTransports};

    let mut config = Config::default();
    config.set_device_identity(
        "receiver:test:slot:1",
        DeviceIdentity {
            display_name: "Mouse".into(),
            model_info: Some(DeviceModelInfo {
                entity_count: 1,
                serial_number: Some("private-serial".into()),
                unit_id: [1, 2, 3, 4],
                transports: DeviceTransports::default(),
                model_ids: [0xb034, 0, 0],
                extended_model_id: 2,
            }),
            codename: None,
            kind: DeviceKind::Mouse,
            capabilities: Capabilities::default(),
            light_capabilities: None,
            driver_id: None,
            registry_model_id: None,
        },
    );
    let model = config
        .device_identity("receiver:test:slot:1")
        .and_then(|identity| identity.model_info.as_ref())
        .expect("model info");
    assert_eq!(model.serial_number, None);
    assert_eq!(model.unit_id, [0; 4]);
}
