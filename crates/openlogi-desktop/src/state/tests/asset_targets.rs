//! Which devices the asset sync targets.

use super::*;

#[test]
fn known_offline_device_is_an_asset_sync_target() {
    let model = DeviceModelInfo {
        entity_count: 0,
        serial_number: None,
        unit_id: [0; 4],
        transports: DeviceTransports::default(),
        model_ids: [0xb034, 0, 0],
        extended_model_id: 2,
    };
    let mut config = Config::ephemeral();
    config.set_device_identity(
        "2b034",
        DeviceIdentity {
            display_name: "MX Anywhere 3S".to_string(),
            kind: DeviceKind::Mouse,
            capabilities: Capabilities::presumed_from_kind(DeviceKind::Mouse),
            light_capabilities: None,
            model_info: Some(model.clone()),
            codename: Some("MX Anywhere 3S".to_string()),
            driver_id: None,
            registry_model_id: None,
        },
    );
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources::in_memory(config, &AssetResolver::new(), commands));

    assert_eq!(
        state.asset_models(),
        vec![crate::services::assets::sync::AssetTarget::Hidpp {
            model,
            codename: Some("MX Anywhere 3S".to_string()),
        }]
    );
}

#[test]
fn identical_standalone_units_share_one_model_asset_target() {
    let first = superseded_litra_light();
    let mut second = first.clone();
    second.address.identity = "serial:glow-second".into();
    second.serial_number = Some("glow-second".into());
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        standalone: &[first, second],
        ..Sources::in_memory(Config::default(), &AssetResolver::new(), commands)
    });

    assert_eq!(
        state.asset_models(),
        vec![crate::services::assets::sync::AssetTarget::Standalone {
            registry_model_id: "8c900".into(),
        }]
    );
}
