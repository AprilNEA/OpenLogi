use super::*;
use crate::{binding::Action, device::DeviceKind, peripheral::*};
use std::collections::BTreeMap;

fn record() -> PeripheralRecord {
    PeripheralRecord {
        model: ModelId::try_new("example.button").unwrap(),
        physical: Some(PhysicalDeviceId {
            key: "unit:1234".into(),
            evidence: IdentityEvidence::Protocol,
        }),
        session: SessionId {
            endpoint: EndpointId("test:1".into()),
            generation: 1,
        },
        endpoints: Vec::new(),
        name: "Button".into(),
        kind: DeviceKind::Unknown,
        driver: DriverSelection {
            descriptor: DescriptorId::try_new("example.button.usb").unwrap(),
            driver: DriverId::try_new("example.mapper").unwrap(),
            source: DriverSource::Builtin,
            digest: None,
        },
        driver_error: None,
        connection: ConnectionStatus::Online,
        scopes: vec![ScopeKind::Model, ScopeKind::Physical, ScopeKind::Session],
        operations: Vec::new(),
        capabilities: vec![CapabilityRecord {
            id: CapabilityId::try_new("input-remap/main").unwrap(),
            version: 1,
            capability: Capability::InputRemap(InputRemapCapability {
                controls: vec![InputControl {
                    id: ControlId::try_new("linking").unwrap(),
                    labels: BTreeMap::from([("en".into(), "Linking button".into())]),
                    source: InputSource::HidUsage(HidUsage {
                        page: 12,
                        usage: 233,
                    }),
                    trigger: Trigger::ShortPress,
                    recommended_key: Some("F18".parse().unwrap()),
                }],
                targets: TargetKind::KeyboardKey,
                per_app: false,
            }),
            scopes: vec![ScopeKind::Model, ScopeKind::Physical, ScopeKind::Session],
            unavailable: None,
            evidence: CapabilityEvidence::Declared,
            values: BTreeMap::new(),
        }],
    }
}

#[test]
fn preferred_scope_limits_new_settings_to_the_current_attachment() {
    let mut config = Config::default();
    let mut device = record();
    device.scopes = vec![ScopeKind::Session, ScopeKind::Model];
    config.set_peripheral_enabled(&device, true).unwrap();
    assert!(config.peripheral_rule(&device).unwrap().unwrap().enabled);
    device.session.generation += 1;
    assert_eq!(config.peripheral_rule(&device).unwrap(), None);

    device.scopes = vec![ScopeKind::Physical, ScopeKind::Model];
    device.physical = None;
    let accepted = config.peripherals.clone();
    assert!(matches!(
        config.set_peripheral_enabled(&device, true),
        Err(PeripheralError::InvalidSettings(_))
    ));
    assert_eq!(config.peripherals, accepted);
}

#[test]
fn disabled_overrides_win_and_stale_sessions_do_not_capture_a_new_attachment() {
    let mut config = Config::default();
    let mut device = record();
    config.set_peripheral_enabled(&device, true).unwrap();
    let model = config.peripherals[0].clone();
    let mut physical = model.clone();
    physical.id = "unit".into();
    physical.scope = ConfigScope::Physical("unit:1234".into());
    physical.enabled = false;
    config.peripherals.push(physical);
    let mut session = model;
    session.id = "session".into();
    session.scope = ConfigScope::Session(device.session.clone());
    config.peripherals.push(session);
    assert_eq!(
        config.peripheral_rule(&device).unwrap().unwrap().id,
        "session"
    );
    device.session.generation += 1;
    let selected = config.peripheral_rule(&device).unwrap().unwrap();
    assert_eq!(selected.id, "unit");
    assert!(
        !selected.enabled,
        "a disabled physical rule must not enable the model default"
    );
    let mut duplicate = selected.clone();
    duplicate.id = "duplicate".into();
    config.peripherals.push(duplicate);
    assert!(matches!(
        config.peripheral_rule(&device),
        Err(PeripheralError::DriverConflict(_))
    ));
}

#[test]
fn binding_edits_validate_atomically_repair_invalid_values_and_preserve_future_contracts() {
    let mut config = Config::default();
    let mut device = record();
    let capability = device.capabilities[0].id.clone();
    let control = ControlId::try_new("linking").unwrap();
    let action = Action::CustomShortcut("F18".parse().unwrap());
    config
        .set_peripheral_binding(&device, &capability, control.clone(), Some(action.clone()))
        .unwrap();
    assert!(
        !config.peripherals[0].enabled,
        "choosing a target does not enable writes"
    );
    let accepted = config.peripherals.clone();
    for (control, action) in [
        (
            control.clone(),
            Action::CustomShortcut("Cmd+F18".parse().unwrap()),
        ),
        (ControlId::try_new("unknown").unwrap(), action.clone()),
    ] {
        assert!(matches!(
            config.set_peripheral_binding(&device, &capability, control, Some(action)),
            Err(PeripheralError::InvalidSettings(_))
        ));
        assert_eq!(config.peripherals, accepted);
    }
    config.peripherals[0]
        .capabilities
        .get_mut(&capability)
        .unwrap()
        .bindings
        .insert(control.clone(), Action::Copy);
    config
        .set_peripheral_binding(&device, &capability, control.clone(), Some(action.clone()))
        .unwrap();
    assert_eq!(
        config.peripherals, accepted,
        "an invalid current target must remain repairable"
    );
    config.peripherals[0]
        .capabilities
        .get_mut(&capability)
        .unwrap()
        .version = 2;
    device.capabilities[0].version = 2;
    assert!(matches!(
        config.set_peripheral_binding(&device, &capability, control, Some(action)),
        Err(PeripheralError::Unsupported(_))
    ));
    config.set_peripheral_enabled(&device, false).unwrap();
    let stored = toml::to_string(&config).unwrap();
    assert_eq!(
        toml::from_str::<Config>(&stored).unwrap().peripherals,
        config.peripherals
    );
    assert_eq!(config.peripherals[0].capabilities[&capability].version, 2);
}
