//! What the online standalone lights report: a value changed on the light
//! itself stands in for the saved one until the GUI saves it, and only lights
//! online on their own route are published and watched.
use super::*;
use crate::orchestrator::{LightWrite, TakenLightChange};
use crate::watchers::light::WatchedLight;
use openlogi_core::device::{ChangedOnLight, LightState, LightValueRange, LightValueUnit};
use openlogi_core::hid::WriteError;
use openlogi_hid::{LITRA_GLOW_PRODUCT_ID, LightChange, LightCommand, LightControls, LitraModel};
use openlogi_ipc::{AcknowledgedLightChange, ObservedLight};
use tokio::sync::Mutex;

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
fn a_state_an_openlogi_write_read_back_is_published_and_keeps_the_change() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    // The brightness was pressed, then OpenLogi wrote the temperature.
    orch.light_changed(&route(), change(state(true, 135, 4000), false, true, false));
    orch.light_changed(
        &route(),
        change(state(true, 135, 5000), false, false, false),
    );

    assert_eq!(
        published(&orch),
        vec![ObservedLight {
            route: route(),
            state: state(true, 135, 5000),
            changed_on_light: ChangedOnLight {
                brightness_percent: Some(50),
                ..ChangedOnLight::default()
            },
        }]
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

fn taken(changed: ChangedOnLight) -> TakenLightChange {
    TakenLightChange {
        key: KEY.to_string(),
        changed,
    }
}

#[test]
fn a_gui_write_of_a_control_not_changed_on_the_light_takes_nothing() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    orch.light_changed(&route(), change(state(true, 135, 5000), false, false, true));
    let changed = changed_on_light(&orch);
    assert_eq!(changed.temperature_kelvin, Some(5000));
    assert_eq!(
        orch.light_writing(&route(), LightCommand::BrightnessPercent(70)),
        None
    );
    assert_eq!(changed_on_light(&orch), changed);
}

#[test]
fn a_gui_write_takes_only_the_control_it_sets() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    orch.light_changed(&route(), pressed_everything());
    let pressed = changed_on_light(&orch);
    assert_eq!(
        orch.light_writing(&glow_route("serial:other"), LightCommand::Power(true)),
        None,
        "a write to another light takes nothing from this one"
    );
    assert_eq!(changed_on_light(&orch), pressed);
    assert_eq!(
        orch.light_writing(&route(), LightCommand::BrightnessNative(200)),
        Some(taken(ChangedOnLight {
            brightness_percent: Some(50),
            ..ChangedOnLight::default()
        }))
    );
    assert_eq!(
        changed_on_light(&orch),
        ChangedOnLight {
            brightness_percent: None,
            ..pressed
        }
    );
    assert_eq!(
        orch.light_writing(&route(), LightCommand::Power(true)),
        Some(taken(ChangedOnLight {
            enabled: Some(false),
            ..ChangedOnLight::default()
        }))
    );
    assert_eq!(
        changed_on_light(&orch),
        ChangedOnLight {
            temperature_kelvin: Some(5000),
            ..ChangedOnLight::default()
        }
    );
    assert_eq!(
        orch.light_writing(&route(), LightCommand::TemperatureKelvin(4000)),
        Some(taken(ChangedOnLight {
            temperature_kelvin: Some(5000),
            ..ChangedOnLight::default()
        }))
    );
    assert!(orch.lights_changed_on_light.is_empty());
    assert_eq!(changed_on_light(&orch), ChangedOnLight::default());
    orch.light_changed(&route(), change(state(true, 135, 4000), false, true, false));
    assert!(
        orch.light_writing(&route(), LightCommand::BrightnessPercent(70))
            .is_some()
    );
    assert!(orch.lights_changed_on_light.is_empty());
    assert_eq!(
        orch.light_writing(&route(), LightCommand::Power(true)),
        None,
        "nothing changed on the light, nothing taken"
    );
}

#[test]
fn a_failed_gui_write_puts_back_only_what_no_newer_change_replaced() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    orch.light_changed(&route(), pressed_everything());
    let pressed = changed_on_light(&orch);
    let Some(brightness) = orch.light_writing(&route(), LightCommand::BrightnessNative(200)) else {
        panic!("the pressed brightness is taken");
    };
    orch.light_write_failed(brightness);
    assert_eq!(changed_on_light(&orch), pressed);

    let Some(temperature) = orch.light_writing(&route(), LightCommand::TemperatureKelvin(4000))
    else {
        panic!("the pressed temperature is taken");
    };
    orch.light_changed(
        &route(),
        change(state(false, 135, 6000), false, false, true),
    );
    orch.light_write_failed(temperature);
    assert_eq!(
        changed_on_light(&orch),
        ChangedOnLight {
            temperature_kelvin: Some(6000),
            ..pressed
        },
        "the change announced during the write is newer than the one taken"
    );

    let Some(brightness) = orch.light_writing(&route(), LightCommand::BrightnessPercent(80)) else {
        panic!("the pressed brightness is taken");
    };
    orch.light_changed(&route(), change(state(false, 60, 6000), false, true, false));
    orch.light_write_failed(brightness);
    let newer = ChangedOnLight {
        brightness_percent: Some(17),
        temperature_kelvin: Some(6000),
        ..pressed
    };
    assert_eq!(
        changed_on_light(&orch),
        newer,
        "a brightness announced during the write is newer than the one taken"
    );

    let taken: Vec<_> = [
        LightCommand::Power(true),
        LightCommand::BrightnessPercent(80),
        LightCommand::TemperatureKelvin(4000),
    ]
    .into_iter()
    .filter_map(|command| orch.light_writing(&route(), command))
    .collect();
    assert_eq!(taken.len(), 3);
    assert!(orch.lights_changed_on_light.is_empty());
    for taken in taken {
        orch.light_write_failed(taken);
    }
    assert_eq!(changed_on_light(&orch), newer);

    orch.light_write_failed(TakenLightChange {
        key: KEY.to_string(),
        changed: ChangedOnLight::default(),
    });
    assert_eq!(changed_on_light(&orch), newer);
}

#[test]
fn a_failed_gui_write_puts_the_change_back_after_the_light_went_offline() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    orch.light_changed(&route(), pressed_everything());
    let pressed = changed_on_light(&orch);
    let Some(brightness) = orch.light_writing(&route(), LightCommand::BrightnessNative(200)) else {
        panic!("the pressed brightness is taken");
    };
    orch.devices[0].online = false;
    orch.light_write_failed(brightness);
    assert_eq!(
        orch.lights_changed_on_light.get(KEY),
        Some(&pressed),
        "the light may still hold the change, so its reconnect writes it back"
    );
    assert_eq!(
        orch.effective_light_settings(KEY)
            .map(|light| light.brightness_percent),
        Some(50)
    );
}

#[tokio::test]
async fn a_successful_gui_write_keeps_the_old_change_out() {
    let orch = Mutex::new(with_glow(config_with(settings(true, 80, 4000))));
    orch.lock()
        .await
        .light_changed(&route(), pressed_everything());
    let pressed = changed_on_light(&*orch.lock().await);
    let written = crate::orchestrator::write_light(
        &orch,
        &route(),
        LightCommand::BrightnessNative(200),
        async { Ok(()) },
    )
    .await;
    assert_eq!(written, Ok(()));
    assert_eq!(
        changed_on_light(&*orch.lock().await),
        ChangedOnLight {
            brightness_percent: None,
            ..pressed
        },
        "the light now holds the user's choice"
    );
}

#[tokio::test]
async fn a_snapshot_published_during_a_gui_write_no_longer_offers_the_old_change() {
    let orch = Mutex::new(with_glow(config_with(settings(true, 80, 4000))));
    orch.lock()
        .await
        .light_changed(&route(), pressed_everything());
    let written = crate::orchestrator::write_light(
        &orch,
        &route(),
        LightCommand::BrightnessNative(200),
        async {
            let mut orch = orch.lock().await;
            assert_eq!(changed_on_light(&orch).brightness_percent, None);
            // The light announces a press while the write is under way.
            orch.light_changed(&route(), change(state(false, 60, 5000), false, true, false));
            Ok(())
        },
    )
    .await;
    assert_eq!(written, Ok(()));
    assert_eq!(
        changed_on_light(&*orch.lock().await),
        ChangedOnLight {
            enabled: Some(false),
            brightness_percent: Some(17),
            temperature_kelvin: Some(5000),
        },
        "a change the light announced during the write stands"
    );
}

#[tokio::test]
async fn a_failed_gui_write_offers_the_old_change_again() {
    let orch = Mutex::new(with_glow(config_with(settings(true, 80, 4000))));
    orch.lock()
        .await
        .light_changed(&route(), pressed_everything());
    let pressed = changed_on_light(&*orch.lock().await);
    let written =
        crate::orchestrator::write_light(&orch, &route(), LightCommand::Power(true), async {
            assert_eq!(changed_on_light(&*orch.lock().await).enabled, None);
            Err(WriteError::DeviceNotFound)
        })
        .await;
    assert_eq!(written, Err(WriteError::DeviceNotFound));
    assert_eq!(changed_on_light(&*orch.lock().await), pressed);
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

/// The light's own buttons set it to 135 lm (50 %) and then to 158 lm (60 %).
fn pressed_to_50_then_60(orch: &mut Orchestrator) {
    orch.light_changed(&route(), change(state(true, 135, 4000), false, true, false));
    orch.light_changed(&route(), change(state(true, 158, 4000), false, true, false));
}

fn brightness_changed_on_light(orch: &Orchestrator) -> Option<u8> {
    orch.lights_changed_on_light
        .get(KEY)
        .and_then(|changed| changed.brightness_percent)
}

fn writes_brightness(percent: u8) -> Vec<LightWrite> {
    vec![LightWrite {
        route: route(),
        light: settings(true, percent, 4000),
        capabilities: capabilities(),
    }]
}

/// The GUI says it saved `changed` for the Glow.
fn acknowledge(orch: &mut Orchestrator, changed: ChangedOnLight) {
    orch.acknowledge_light_changes(&[AcknowledgedLightChange {
        route: route(),
        changed,
    }]);
}

fn brightness(percent: u8) -> ChangedOnLight {
    ChangedOnLight {
        brightness_percent: Some(percent),
        ..ChangedOnLight::default()
    }
}

#[test]
fn an_acknowledged_earlier_report_keeps_the_newer_change_on_the_light() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    pressed_to_50_then_60(&mut orch);
    // The GUI saves the 50 % it was told before the press to 60 % reached it.
    acknowledge(&mut orch, brightness(50));
    assert_eq!(
        orch.replace_config(config_with(settings(true, 50, 4000))),
        Vec::new()
    );
    assert_eq!(brightness_changed_on_light(&orch), Some(60));
    assert_eq!(
        orch.effective_light_settings(KEY),
        Some(settings(true, 60, 4000))
    );
    // Its save of 60 % then adopts the newer press.
    acknowledge(&mut orch, brightness(60));
    assert_eq!(
        orch.replace_config(config_with(settings(true, 60, 4000))),
        Vec::new()
    );
    assert!(orch.lights_changed_on_light.is_empty());
}

#[test]
fn a_save_of_an_earlier_report_the_gui_did_not_acknowledge_replaces_the_change() {
    // A hand edit of config.toml that happens to match an earlier press.
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    pressed_to_50_then_60(&mut orch);
    assert_eq!(
        orch.replace_config(config_with(settings(true, 50, 4000))),
        writes_brightness(50)
    );
    assert_eq!(brightness_changed_on_light(&orch), None);
}

#[test]
fn an_acknowledgement_lasts_for_one_reload() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    pressed_to_50_then_60(&mut orch);
    acknowledge(&mut orch, brightness(50));
    // A reload that saved nothing for the light still uses it up.
    assert_eq!(
        orch.replace_config(config_with(settings(true, 80, 4000))),
        Vec::new()
    );
    assert!(orch.light_acknowledgements.is_empty());
    assert_eq!(brightness_changed_on_light(&orch), Some(60));
    assert_eq!(
        orch.replace_config(config_with(settings(true, 50, 4000))),
        writes_brightness(50)
    );
    assert_eq!(brightness_changed_on_light(&orch), None);
}

#[test]
fn an_acknowledgement_does_not_keep_a_change_over_another_value() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    pressed_to_50_then_60(&mut orch);
    acknowledge(&mut orch, brightness(50));
    assert_eq!(
        orch.replace_config(config_with(settings(true, 70, 4000))),
        writes_brightness(70)
    );
    assert_eq!(brightness_changed_on_light(&orch), None);
}

#[test]
fn the_acknowledgements_of_a_light_add_up_by_control() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    acknowledge(&mut orch, brightness(50));
    acknowledge(
        &mut orch,
        ChangedOnLight {
            enabled: Some(false),
            temperature_kelvin: Some(3000),
            ..ChangedOnLight::default()
        },
    );
    acknowledge(&mut orch, brightness(60));
    assert_eq!(
        orch.light_acknowledgements.get(KEY).copied(),
        Some(ChangedOnLight {
            enabled: Some(false),
            brightness_percent: Some(60),
            temperature_kelvin: Some(3000),
        })
    );
}

#[test]
fn an_acknowledgement_on_a_route_no_online_light_has_is_ignored() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    orch.acknowledge_light_changes(&[AcknowledgedLightChange {
        route: glow_route("serial:other"),
        changed: brightness(50),
    }]);
    assert!(orch.light_acknowledgements.is_empty());
}

#[test]
fn an_acknowledged_earlier_power_and_temperature_keep_the_newer_change() {
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    orch.light_changed(&route(), change(state(false, 135, 3000), true, false, true));
    orch.light_changed(&route(), change(state(true, 135, 3500), true, false, true));
    acknowledge(
        &mut orch,
        ChangedOnLight {
            enabled: Some(false),
            temperature_kelvin: Some(3000),
            ..ChangedOnLight::default()
        },
    );
    assert_eq!(
        orch.replace_config(config_with(settings(false, 80, 3000))),
        Vec::new()
    );
    assert_eq!(
        orch.lights_changed_on_light.get(KEY).copied(),
        Some(ChangedOnLight {
            enabled: Some(true),
            brightness_percent: None,
            temperature_kelvin: Some(3500),
        })
    );
}

#[test]
fn a_save_that_removes_a_control_drops_its_change_on_the_light() {
    // An empty value is not an acknowledgement: no acknowledgement and no
    // saved value must not count as the GUI saving an earlier report.
    let mut orch = with_glow(config_with(settings(true, 80, 4000)));
    orch.light_changed(&route(), change(state(true, 135, 3500), false, false, true));
    orch.replace_config(config_with(LightSettings {
        temperature_kelvin: None,
        ..settings(true, 80, 4000)
    }));
    assert_eq!(orch.lights_changed_on_light.get(KEY), None);
}
