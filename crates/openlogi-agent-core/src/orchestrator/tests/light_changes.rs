//! What the online standalone lights report: a value changed on the light
//! itself stands in for the saved one until the GUI saves it, and only lights
//! online on their own route are published and watched.
use super::*;
use crate::orchestrator::LightWrite;
use crate::watchers::light::WatchedLight;
use openlogi_core::device::{ChangedOnLight, LightState, LightValueRange, LightValueUnit};
use openlogi_hid::{LITRA_GLOW_PRODUCT_ID, LightChange, LightCommand, LightControls, LitraModel};
use openlogi_ipc::ObservedLight;

const KEY: &str = "raw:046d:c900:ff43:0202:serial:glow";

fn range(min: u16, max: u16, step: u16, unit: LightValueUnit) -> LightValueRange {
    let Ok(range) = LightValueRange::new(min, max, step, unit) else {
        panic!("valid light range fixture");
    };
    range
}

fn capabilities() -> LightCapabilities {
    LightCapabilities {
        power: true,
        brightness: Some(range(20, 250, 1, LightValueUnit::Lumens)),
        temperature: Some(range(2700, 6500, 100, LightValueUnit::Kelvin)),
        ..LightCapabilities::default()
    }
}

fn glow_route(identity: &str) -> DeviceRoute {
    DeviceRoute::RawHid {
        vendor_id: 0x046d,
        product_id: LITRA_GLOW_PRODUCT_ID,
        usage_page: 0xff43,
        usage_id: 0x0202,
        identity: identity.to_string(),
    }
}

fn route() -> DeviceRoute {
    glow_route("serial:glow")
}

fn glow(key: &str, route: DeviceRoute, online: bool) -> AgentDevice {
    let mut light = dev(key, 1, online);
    light.route = Some(route);
    light.kind = openlogi_core::device::DeviceKind::Light;
    light.light_capabilities = Some(capabilities());
    light
}

/// An orchestrator holding `config` with one Litra Glow online under [`KEY`].
fn with_glow(config: Config) -> Orchestrator {
    let mut orch = orchestrator(config);
    orch.devices.push(glow(KEY, route(), true));
    orch
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

fn config_with(light: LightSettings) -> Config {
    let mut config = Config::default();
    config.set_light(KEY, light);
    config
}

/// The light's state: `brightness` in lumens, 135 lm being 50 % of its range.
fn state(on: bool, brightness: u16, temperature_kelvin: u16) -> LightState {
    LightState {
        capabilities: capabilities(),
        on,
        brightness: Some(brightness),
        temperature_kelvin: Some(temperature_kelvin),
        brightness_limit: None,
    }
}

fn change(state: LightState, power: bool, brightness: bool, temperature: bool) -> LightChange {
    LightChange {
        state,
        on_light: LightControls {
            power,
            brightness,
            temperature,
        },
    }
}

/// The light pressed off, to 135 lm (50 %) and to 5000 K on its own buttons.
fn pressed_everything() -> LightChange {
    change(state(false, 135, 5000), true, true, true)
}

fn published(orch: &Orchestrator) -> Vec<ObservedLight> {
    orch.observable.snapshot().lights
}

fn changed_on_light(orch: &Orchestrator) -> ChangedOnLight {
    let lights = published(orch);
    let [light] = lights.as_slice() else {
        panic!("one light published, got {lights:?}");
    };
    light.changed_on_light
}

#[test]
fn a_change_on_the_light_stands_in_for_the_saved_setting() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    orch.light_changed(&route(), pressed_everything());
    assert_eq!(
        orch.effective_light_settings(KEY),
        Some(settings(false, 50, 5000))
    );
    assert_eq!(
        published(&orch),
        vec![ObservedLight {
            route: route(),
            state: state(false, 135, 5000),
            changed_on_light: ChangedOnLight {
                enabled: Some(false),
                brightness_percent: Some(50),
                temperature_kelvin: Some(5000),
            },
        }]
    );
}

#[test]
fn only_the_controls_changed_on_the_light_are_kept() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    // The light reports all three values; only its brightness was pressed.
    orch.light_changed(
        &route(),
        change(state(false, 135, 5000), false, true, false),
    );
    assert_eq!(
        changed_on_light(&orch),
        ChangedOnLight {
            brightness_percent: Some(50),
            ..ChangedOnLight::default()
        }
    );
    assert_eq!(
        orch.effective_light_settings(KEY),
        Some(settings(true, 50, 4000))
    );
}

#[test]
fn a_brightness_outside_the_range_is_not_kept() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    orch.light_changed(&route(), change(state(true, 300, 4000), false, true, false));
    assert!(orch.lights_changed_on_light.is_empty());
    assert_eq!(changed_on_light(&orch), ChangedOnLight::default());
}

#[test]
fn an_unrelated_save_keeps_the_change_and_writes_no_light() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    orch.light_changed(&route(), pressed_everything());
    let mut unrelated = config_with(settings(true, 80, 4000));
    unrelated.app_settings.smooth_scroll = !unrelated.app_settings.smooth_scroll;
    assert_eq!(orch.replace_config(unrelated), Vec::new());
    assert_eq!(
        orch.effective_light_settings(KEY),
        Some(settings(false, 50, 5000))
    );
}

#[test]
fn saving_the_changed_values_adopts_them_without_a_write() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    orch.light_changed(&route(), pressed_everything());
    assert_eq!(
        orch.replace_config(config_with(settings(false, 50, 5000))),
        Vec::new()
    );
    assert!(orch.lights_changed_on_light.is_empty());
    assert_eq!(changed_on_light(&orch), ChangedOnLight::default());
}

#[test]
fn a_newer_saved_value_replaces_the_change_and_is_written() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    orch.light_changed(&route(), pressed_everything());
    // Brightness is saved anew; power and temperature stay as pressed.
    let updated = config_with(settings(true, 90, 4000));
    assert_eq!(
        orch.replace_config(updated),
        vec![LightWrite {
            route: route(),
            light: settings(false, 90, 5000),
            capabilities: capabilities(),
        }]
    );
    assert_eq!(
        changed_on_light(&orch),
        ChangedOnLight {
            enabled: Some(false),
            brightness_percent: None,
            temperature_kelvin: Some(5000),
        }
    );
}

#[test]
fn a_camera_linked_light_keeps_its_power_as_a_manual_choice() {
    let mut orch = with_glow(config_with(LightSettings {
        auto_camera: true,
        ..settings(true, 80, 4000)
    }));
    orch.light_changed(
        &route(),
        change(state(false, 135, 4000), true, false, false),
    );
    assert_eq!(orch.manual_light_overrides.get(KEY), Some(&false));
    assert!(orch.lights_changed_on_light.is_empty());
}

#[test]
fn a_gui_write_forgets_only_the_control_it_set() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    orch.light_changed(&route(), pressed_everything());
    orch.light_written(&route(), LightCommand::BrightnessNative(200));
    assert_eq!(
        changed_on_light(&orch),
        ChangedOnLight {
            enabled: Some(false),
            brightness_percent: None,
            temperature_kelvin: Some(5000),
        }
    );
    orch.light_written(&route(), LightCommand::Power(true));
    assert_eq!(
        changed_on_light(&orch),
        ChangedOnLight {
            temperature_kelvin: Some(5000),
            ..ChangedOnLight::default()
        }
    );
    orch.light_written(&route(), LightCommand::TemperatureKelvin(4000));
    assert!(orch.lights_changed_on_light.is_empty());

    orch.light_changed(&route(), change(state(true, 135, 4000), false, true, false));
    orch.light_written(&route(), LightCommand::BrightnessPercent(70));
    assert!(orch.lights_changed_on_light.is_empty());
}

#[test]
fn a_light_is_published_only_while_online_on_the_route_it_was_read_on() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    orch.light_read(&glow_route("serial:other"), state(true, 135, 4000));
    assert_eq!(published(&orch), Vec::new());

    orch.light_read(&route(), state(true, 135, 4000));
    assert_eq!(published(&orch).len(), 1);

    orch.devices[0].online = false;
    orch.publish_lights();
    assert_eq!(published(&orch), Vec::new());

    orch.devices[0] = glow(KEY, glow_route("serial:moved"), true);
    orch.publish_lights();
    assert_eq!(published(&orch), Vec::new());
}

#[test]
fn only_online_litra_lights_are_watched() {
    let mut orch = with_glow(Config::default());
    orch.devices
        .push(glow("raw:offline", glow_route("serial:offline"), false));
    let mut not_a_litra = dev("bolt:light", 2, true);
    not_a_litra.light_capabilities = Some(capabilities());
    orch.devices.push(not_a_litra);
    orch.devices.push(dev("bolt:mouse", 3, true));
    assert_eq!(
        orch.watched_lights(),
        vec![WatchedLight {
            route: route(),
            model: LitraModel::Glow,
        }]
    );
}

#[test]
fn a_change_back_to_the_saved_value_is_forgotten_at_the_next_save() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    orch.light_changed(&route(), change(state(true, 135, 4000), false, true, false));
    // 204 lm is 80 % of the light's range: pressed back to the saved value.
    orch.light_changed(&route(), change(state(true, 204, 4000), false, true, false));
    let mut unrelated = config_with(settings(true, 80, 4000));
    unrelated.app_settings.smooth_scroll = !unrelated.app_settings.smooth_scroll;
    assert_eq!(orch.replace_config(unrelated), Vec::new());
    assert!(orch.lights_changed_on_light.is_empty());
    assert_eq!(changed_on_light(&orch), ChangedOnLight::default());
}

#[test]
fn the_watcher_is_given_the_online_lights() {
    let mut orch = with_glow(Config::default());
    let watched = orch.shared().watched_lights;
    orch.publish_device_runtime();
    assert_eq!(
        watched.borrow().as_slice(),
        [WatchedLight {
            route: route(),
            model: LitraModel::Glow,
        }]
    );
    orch.devices[0].online = false;
    orch.publish_device_runtime();
    assert_eq!(watched.borrow().as_slice(), []);
}

#[test]
fn a_change_on_a_route_no_online_light_has_is_ignored() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    orch.light_changed(&glow_route("serial:other"), pressed_everything());
    assert!(orch.lights_changed_on_light.is_empty());
    assert_eq!(
        orch.effective_light_settings(KEY),
        Some(settings(true, 80, 4000))
    );
}
