//! Which standalone lights a config reload writes: only those whose effective
//! settings now send other commands, so an unrelated save never undoes a
//! change made on the light itself.

use super::*;
use crate::orchestrator::LightWrite;
use openlogi_core::device::{LightValueRange, LightValueUnit};

const KEY: &str = "raw:046d:c900:ff43:0202:serial:glow";

fn range(min: u16, max: u16, unit: LightValueUnit) -> LightValueRange {
    let Ok(range) = LightValueRange::new(min, max, 1, unit) else {
        panic!("valid light range fixture");
    };
    range
}

fn full_light() -> LightCapabilities {
    LightCapabilities {
        power: true,
        brightness: Some(range(0, 100, LightValueUnit::Percent)),
        temperature: Some(range(2700, 6500, LightValueUnit::Kelvin)),
        ..LightCapabilities::default()
    }
}

fn settings(enabled: bool, brightness_percent: u8, temperature_kelvin: u16) -> LightSettings {
    LightSettings {
        enabled,
        auto_camera: false,
        brightness_percent,
        temperature_kelvin: Some(temperature_kelvin),
        color: None,
    }
}

fn config_with(light: Option<LightSettings>) -> Config {
    let mut config = Config::default();
    if let Some(light) = light {
        config.set_light(KEY, light);
    }
    config
}

/// An orchestrator holding `config` with one light online under [`KEY`].
fn with_light(config: Config, capabilities: LightCapabilities) -> Orchestrator {
    let mut orch = orchestrator(config);
    let mut light = dev(KEY, 1, true);
    light.light_capabilities = Some(capabilities);
    orch.devices.push(light);
    orch
}

fn route() -> DeviceRoute {
    let Some(route) = dev(KEY, 1, true).route else {
        panic!("fixture device has a route");
    };
    route
}

#[test]
fn an_unrelated_save_writes_no_light() {
    let light = settings(true, 65, 4600);
    let mut orch = with_light(config_with(Some(light)), full_light());

    let mut updated = config_with(Some(light));
    updated.app_settings.smooth_scroll = !updated.app_settings.smooth_scroll;

    assert_eq!(orch.replace_config(updated), Vec::new());
}

#[test]
fn a_changed_light_setting_is_written() {
    let mut orch = with_light(config_with(Some(settings(true, 65, 4600))), full_light());

    let changed = settings(true, 80, 4600);
    assert_eq!(
        orch.replace_config(config_with(Some(changed))),
        vec![LightWrite {
            route: route(),
            light: changed,
            capabilities: full_light(),
        }]
    );
}

#[test]
fn a_change_the_light_cannot_take_is_not_written() {
    let brightness_only = LightCapabilities {
        power: true,
        brightness: Some(range(0, 100, LightValueUnit::Percent)),
        ..LightCapabilities::default()
    };
    let mut orch = with_light(config_with(Some(settings(true, 65, 4600))), brightness_only);

    assert_eq!(
        orch.replace_config(config_with(Some(settings(true, 65, 6500)))),
        Vec::new()
    );
}

#[test]
fn a_flipped_camera_policy_writes_the_effective_power() {
    let manual = settings(true, 65, 4600);
    let mut orch = with_light(config_with(Some(manual)), full_light());
    orch.set_camera_active(false);

    let linked = LightSettings {
        auto_camera: true,
        ..manual
    };
    assert_eq!(
        orch.replace_config(config_with(Some(linked))),
        vec![LightWrite {
            route: route(),
            light: LightSettings {
                enabled: false,
                ..linked
            },
            capabilities: full_light(),
        }]
    );
}

#[test]
fn a_dropped_manual_override_writes_the_saved_power() {
    let linked = LightSettings {
        auto_camera: true,
        ..settings(false, 65, 4600)
    };
    let mut orch = with_light(config_with(Some(linked)), full_light());
    orch.set_camera_active(false);
    assert!(orch.set_manual_light_power(&route(), true));

    let manual = settings(false, 65, 4600);
    assert_eq!(
        orch.replace_config(config_with(Some(manual))),
        vec![LightWrite {
            route: route(),
            light: manual,
            capabilities: full_light(),
        }]
    );
}

#[test]
fn a_first_saved_light_setting_is_written() {
    let mut orch = with_light(config_with(None), full_light());

    let first = settings(false, 40, 3000);
    assert_eq!(
        orch.replace_config(config_with(Some(first))),
        vec![LightWrite {
            route: route(),
            light: first,
            capabilities: full_light(),
        }]
    );
}

#[test]
fn only_an_online_light_is_written() {
    let mut orch = orchestrator(config_with(Some(settings(true, 65, 4600))));
    let mut offline = dev(KEY, 1, false);
    offline.light_capabilities = Some(full_light());
    orch.devices.push(offline);
    orch.devices.push(dev("bolt:mouse", 2, true));

    assert_eq!(
        orch.replace_config(config_with(Some(settings(true, 80, 4600)))),
        Vec::new()
    );
}
