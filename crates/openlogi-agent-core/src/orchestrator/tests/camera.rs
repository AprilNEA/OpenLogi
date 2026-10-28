//! Camera-triggered light automation and the manual override.

use super::*;

#[test]
fn camera_automation_overrides_only_effective_power() {
    let key = "raw:046d:c900:ff43:0202:serial:glow";
    let mut config = Config::default();
    config.set_light(
        key,
        LightSettings {
            enabled: true,
            auto_camera: true,
            brightness_percent: 65,
            temperature_kelvin: Some(4600),
            color: None,
        },
    );
    let mut orch = orchestrator(config);

    orch.set_camera_active(false);
    assert_eq!(
        orch.effective_light_settings(key).map(|light| (
            light.enabled,
            light.brightness_percent,
            light.temperature_kelvin
        )),
        Some((false, 65, Some(4600)))
    );

    orch.set_camera_active(true);
    assert_eq!(
        orch.effective_light_settings(key).map(|light| (
            light.enabled,
            light.brightness_percent,
            light.temperature_kelvin
        )),
        Some((true, 65, Some(4600)))
    );
}

#[test]
fn manual_camera_light_override_is_transient() {
    let key = "raw:046d:c900:ff43:0202:serial:glow";
    let route = DeviceRoute::Bolt {
        receiver_uid: "AA00".to_string(),
        slot: 1,
    };
    let mut config = Config::default();
    config.set_light(
        key,
        LightSettings {
            enabled: true,
            auto_camera: true,
            brightness_percent: 65,
            temperature_kelvin: Some(4600),
            color: None,
        },
    );
    let mut orch = orchestrator(config);
    orch.set_camera_active(true);
    let mut device = dev(key, 1, true);
    device.light_capabilities = Some(LightCapabilities {
        power: true,
        ..LightCapabilities::default()
    });
    orch.devices.push(AgentDevice {
        config_key: key.to_string(),
        route: Some(route.clone()),
        ..device
    });

    assert!(orch.set_manual_light_power(&route, false));
    assert_eq!(
        orch.effective_light_settings(key)
            .map(|light| light.enabled),
        Some(false)
    );

    orch.devices.clear();
    orch.set_camera_active(false);
    orch.set_camera_active(true);
    assert_eq!(
        orch.effective_light_settings(key)
            .map(|light| light.enabled),
        Some(true)
    );
}

#[test]
fn config_reload_keeps_manual_override_for_parameter_edits() {
    let key = "raw:046d:c900:ff43:0202:serial:glow";
    let mut config = Config::default();
    config.set_light(
        key,
        LightSettings {
            enabled: false,
            auto_camera: true,
            brightness_percent: 65,
            temperature_kelvin: Some(4600),
            color: None,
        },
    );
    let mut orch = orchestrator(config.clone());
    orch.set_camera_active(false);
    orch.manual_light_overrides.insert(key.to_string(), true);

    let mut updated = config;
    updated.set_light(
        key,
        LightSettings {
            enabled: false,
            auto_camera: true,
            brightness_percent: 80,
            temperature_kelvin: Some(6500),
            color: None,
        },
    );
    orch.reload_config(updated);

    assert_eq!(
        orch.effective_light_settings(key).map(|light| (
            light.enabled,
            light.brightness_percent,
            light.temperature_kelvin
        )),
        Some((true, 80, Some(6500)))
    );
    assert_eq!(orch.manual_light_overrides.get(key), Some(&true));
}

#[test]
fn config_reload_clears_override_when_camera_mode_changes() {
    let key = "raw:046d:c900:ff43:0202:serial:glow";
    let mut config = Config::default();
    config.set_light(
        key,
        LightSettings {
            enabled: true,
            auto_camera: true,
            brightness_percent: 65,
            temperature_kelvin: Some(4600),
            color: None,
        },
    );
    let mut orch = orchestrator(config.clone());
    orch.manual_light_overrides.insert(key.to_string(), false);

    let mut updated = config;
    updated.set_light(
        key,
        LightSettings {
            enabled: true,
            auto_camera: false,
            brightness_percent: 65,
            temperature_kelvin: Some(4600),
            color: None,
        },
    );
    orch.reload_config(updated);

    assert_eq!(orch.manual_light_overrides.get(key), None);
    assert_eq!(
        orch.effective_light_settings(key)
            .map(|light| light.enabled),
        Some(true)
    );
}
