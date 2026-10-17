//! Changes made on a light itself: the agent reports them, and the desktop
//! keeps them as the light's settings, as Options+ does.

use openlogi_core::device::{ChangedOnLight, LightState};
use openlogi_ipc::{ChosenLight, ObservedLight};

use super::fn_lock::drain;
use super::*;
use crate::services::ipc::{Command, ReloadConfig};

fn lumens() -> LightValueRange {
    LightValueRange::new(20, 250, 1, LightValueUnit::Lumens).expect("valid range")
}

fn capabilities() -> LightCapabilities {
    LightCapabilities {
        power: true,
        brightness: Some(lumens()),
        temperature: Some(
            LightValueRange::new(2700, 6500, 100, LightValueUnit::Kelvin).expect("valid range"),
        ),
        ..LightCapabilities::default()
    }
}

/// A Litra Glow reached under `identity`; a serial number makes it one
/// whose settings are saved.
fn glow(identity: &str, serial: Option<&str>, online: bool) -> StandaloneDevice {
    StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: identity.into(),
        },
        display_name: "Litra Glow".into(),
        manufacturer: Some("Logi".into()),
        serial_number: serial.map(str::to_string),
        unit_id: [0; 4],
        kind: DeviceKind::Light,
        online,
        capabilities: None,
        light_capabilities: Some(capabilities()),
        driver_id: "litra".into(),
        registry_model_id: Some("8c900".into()),
    }
}

fn saved_glow() -> StandaloneDevice {
    glow("serial:glow-1", Some("glow-1"), true)
}

fn state_with(
    light: &StandaloneDevice,
    config: Config,
) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        standalone: std::slice::from_ref(light),
        ..Sources::in_memory(config, &resolver, commands)
    });
    drain(&mut receiver);
    (state, receiver)
}

/// `light` reading back `on`, `brightness` lumens (135 lm is 50 %) and
/// `kelvin`, with `changed` made on the light itself.
fn observed(
    light: &StandaloneDevice,
    (on, brightness, kelvin): (bool, u16, u16),
    changed: ChangedOnLight,
) -> ObservedLight {
    ObservedLight {
        route: DeviceRoute::from(&light.address),
        state: LightState {
            capabilities: capabilities(),
            on,
            brightness: Some(brightness),
            temperature_kelvin: Some(kelvin),
            brightness_limit: None,
        },
        changed_on_light: changed,
    }
}

fn brightness_changed_to(percent: u8) -> ChangedOnLight {
    ChangedOnLight {
        brightness_percent: Some(percent),
        ..ChangedOnLight::default()
    }
}

/// The adoption saves and reloads once, with no chosen controls: the saved
/// values are copies of what the lights reported, not choices of the user.
fn saved_and_reloaded(commands: &[Command]) -> bool {
    matches!(
        commands,
        [Command::ReloadConfig(ReloadConfig { chosen, .. })] if chosen.is_empty()
    )
}

#[test]
fn a_change_on_a_saved_light_replaces_only_that_value_and_reloads_once() {
    let light = saved_glow();
    let (mut state, mut receiver) = state_with(&light, Config::ephemeral());
    let key = state.current_record().expect("light record").device_key();
    state
        .config
        .edit(|config| config.set_light(key.as_str(), LightSettings::new(true, 80, Some(4000))));
    let lights = [observed(
        &light,
        (true, 135, 4000),
        brightness_changed_to(50),
    )];

    assert_eq!(
        state.adopt_changes_on_lights(&lights),
        [lighting_changed(&key)]
    );
    assert_eq!(
        state.config.light(key.as_str()),
        Some(LightSettings::new(true, 50, Some(4000)))
    );
    assert_eq!(state.light(), LightSettings::new(true, 50, Some(4000)));
    assert!(saved_and_reloaded(&drain(&mut receiver)));

    // The agent reports the change until its reload; keeping it again saves
    // nothing.
    assert_eq!(state.adopt_changes_on_lights(&lights), []);
    assert!(drain(&mut receiver).is_empty());
}

#[test]
fn a_light_never_saved_keeps_what_it_reads_back_for_the_other_controls() {
    let light = saved_glow();
    let (mut state, mut receiver) = state_with(&light, Config::ephemeral());
    let key = state.current_record().expect("light record").device_key();
    assert_eq!(state.config.light(key.as_str()), None);

    let lights = [observed(
        &light,
        (false, 135, 5000),
        brightness_changed_to(50),
    )];
    assert_eq!(
        state.adopt_changes_on_lights(&lights),
        [lighting_changed(&key)]
    );
    assert_eq!(
        state.config.light(key.as_str()),
        Some(LightSettings::new(false, 50, Some(5000)))
    );
    assert!(saved_and_reloaded(&drain(&mut receiver)));
}

#[test]
fn a_light_with_a_gui_write_in_flight_keeps_its_change_until_the_write_settles() {
    let light = saved_glow();
    let (mut state, mut receiver) = state_with(&light, Config::ephemeral());
    let key = state.current_record().expect("light record").device_key();
    let requested = LightSettings::new(true, 80, None);
    let _ = state.commit_light(requested);
    let sent: Vec<_> = drain(&mut receiver)
        .into_iter()
        .filter_map(|command| match command {
            Command::SetLight(SetLight {
                command,
                request_id,
                ..
            }) => Some((command, request_id)),
            _ => None,
        })
        .collect();
    assert!(!sent.is_empty(), "the GUI write reaches the agent");

    let lights = [observed(
        &light,
        (true, 60, 4000),
        brightness_changed_to(17),
    )];
    assert_eq!(state.adopt_changes_on_lights(&lights), []);
    assert_eq!(state.light(), requested);
    assert!(drain(&mut receiver).is_empty());

    for (command, request_id) in sent {
        let _ = state.apply_light_command_result(key.clone(), request_id, command, Ok(()));
    }
    assert_eq!(state.config.light(key.as_str()), Some(requested));
    drain(&mut receiver);
    assert_eq!(
        state.adopt_changes_on_lights(&lights),
        [lighting_changed(&key)]
    );
    assert_eq!(
        state.config.light(key.as_str()),
        Some(LightSettings::new(true, 17, None))
    );
    assert!(saved_and_reloaded(&drain(&mut receiver)));
}

#[test]
fn a_light_saved_after_keeping_a_session_value_shows_the_change_it_saves() {
    let light = glow("id:saved-later", None, true);
    let (mut state, mut receiver) = state_with(&light, Config::ephemeral());
    let key = state.current_record().expect("light record").device_key();
    let session = [observed(
        &light,
        (false, 60, 3000),
        brightness_changed_to(17),
    )];
    assert_eq!(
        state.adopt_changes_on_lights(&session),
        [lighting_changed(&key)]
    );
    assert_eq!(state.light(), LightSettings::new(false, 17, Some(3000)));
    assert!(drain(&mut receiver).is_empty());

    // The light's settings can be saved from here on, under the same key.
    for record in &mut state.devices.records {
        record.persistent = true;
    }
    let config_key = state
        .current_record()
        .and_then(|record| record.persistent_config_key())
        .expect("a saved light")
        .to_string();
    let lights = [observed(
        &light,
        (false, 135, 3000),
        brightness_changed_to(50),
    )];
    assert_eq!(
        state.adopt_changes_on_lights(&lights),
        [lighting_changed(&key)]
    );
    assert_eq!(
        state.config.light(&config_key),
        Some(LightSettings::new(false, 50, Some(3000)))
    );
    assert_eq!(
        state.light(),
        LightSettings::new(false, 50, Some(3000)),
        "the saved settings are shown, not the value kept for the session"
    );
    assert!(saved_and_reloaded(&drain(&mut receiver)));
}

#[test]
fn a_session_only_light_keeps_the_change_in_memory() {
    let light = glow("id:session-node", None, true);
    let (mut state, mut receiver) = state_with(&light, Config::ephemeral());
    let key = state.current_record().expect("light record").device_key();
    let changed = ChangedOnLight {
        enabled: Some(false),
        ..ChangedOnLight::default()
    };
    let lights = [observed(&light, (false, 135, 4000), changed)];

    assert_eq!(
        state.adopt_changes_on_lights(&lights),
        [lighting_changed(&key)]
    );
    assert_eq!(state.light(), LightSettings::new(false, 50, Some(4000)));
    assert_eq!(state.config.light(key.as_str()), None);
    assert!(
        drain(&mut receiver).is_empty(),
        "nothing is saved or reloaded"
    );
    assert_eq!(state.adopt_changes_on_lights(&lights), []);
}

#[test]
fn only_an_online_light_on_the_reported_route_is_changed() {
    let offline = glow("serial:glow-1", Some("glow-1"), false);
    let (mut state, mut receiver) = state_with(&offline, Config::ephemeral());
    let key = state.current_record().expect("light record").device_key();
    let change = brightness_changed_to(50);
    assert_eq!(
        state.adopt_changes_on_lights(&[observed(&offline, (true, 135, 4000), change)]),
        []
    );

    let (mut state, _receiver) = state_with(&saved_glow(), Config::ephemeral());
    let other = glow("serial:glow-2", Some("glow-2"), true);
    assert_eq!(
        state.adopt_changes_on_lights(&[observed(&other, (true, 135, 4000), change)]),
        []
    );
    assert_eq!(state.config.light(key.as_str()), None);
    assert!(drain(&mut receiver).is_empty());
}

#[test]
fn a_light_with_nothing_changed_on_it_is_left_alone() {
    let light = saved_glow();
    let (mut state, mut receiver) = state_with(&light, Config::ephemeral());
    let key = state.current_record().expect("light record").device_key();
    let lights = [observed(
        &light,
        (false, 135, 5000),
        ChangedOnLight::default(),
    )];
    assert_eq!(state.adopt_changes_on_lights(&lights), []);
    assert_eq!(state.config.light(key.as_str()), None);
    assert!(drain(&mut receiver).is_empty());
}

#[test]
fn an_agent_snapshot_keeps_the_changes_it_reports() {
    let light = saved_glow();
    let (mut state, mut receiver) = state_with(&light, Config::ephemeral());
    let key = state.current_record().expect("light record").device_key();
    let snapshot = AgentSnapshot {
        status: AgentStatus {
            accessibility_granted: true,
            hook_installed: true,
            launch_at_login: false,
            inventory: InventoryHealth::Ready,
            protocol_version: PROTOCOL_VERSION,
            agent_version: "light-adoption-test".to_string(),
            input_monitoring_granted: true,
            hid_open_failures: false,
        },
        inventory: Vec::new(),
        standalone: vec![light.clone()],
        camera_active: false,
        pairing: None,
        foreground: ForegroundApps::default(),
        lights: vec![observed(
            &light,
            (true, 135, 5000),
            brightness_changed_to(50),
        )],
    };

    let changes = state.apply_agent_snapshot(&snapshot, &AssetResolver::new(), &[]);
    assert!(
        changes.events.contains(&lighting_changed(&key)),
        "the snapshot announces the light's new settings"
    );
    assert_eq!(
        state.config.light(key.as_str()),
        Some(LightSettings::new(true, 50, Some(5000)))
    );
    assert!(
        drain(&mut receiver)
            .iter()
            .any(|command| matches!(command, Command::ReloadConfig(_)))
    );
}

/// The controls a save chose, if the commands are exactly one reload.
fn chosen_by_reload(commands: &[Command]) -> Option<Vec<ChosenLight>> {
    match commands {
        [Command::ReloadConfig(ReloadConfig { chosen, .. })] => Some(chosen.clone()),
        _ => None,
    }
}

/// The offline Glow, saved at 80 % and 4000 K.
fn offline_saved_glow() -> (
    AppState,
    tokio::sync::mpsc::UnboundedReceiver<Command>,
    String,
) {
    let light = glow("serial:glow-1", Some("glow-1"), false);
    let (mut state, receiver) = state_with(&light, Config::ephemeral());
    let key = state.current_record().expect("light record").device_key();
    state.config.edit(|config| {
        config.set_light(key.as_str(), LightSettings::new(true, 80, Some(4000)));
    });
    (state, receiver, key.as_str().to_owned())
}

#[test]
fn a_save_without_a_write_chooses_the_controls_it_changed() {
    // An offline light is saved without a write, so no write takes the
    // change made on the light: the reload names the controls the user set.
    let (mut state, mut receiver, key) = offline_saved_glow();
    let _ = state.commit_light(LightSettings::new(true, 60, Some(3000)));
    assert_eq!(
        chosen_by_reload(&drain(&mut receiver)),
        Some(vec![ChosenLight {
            device: key,
            power: false,
            brightness: true,
            temperature: true,
        }])
    );
}

#[test]
fn a_save_without_a_write_that_keeps_the_light_values_chooses_nothing() {
    let (mut state, mut receiver, key) = offline_saved_glow();
    let _ = state.commit_light(LightSettings::new(false, 80, Some(4000)));
    assert_eq!(
        chosen_by_reload(&drain(&mut receiver)),
        Some(vec![ChosenLight {
            device: key.clone(),
            power: true,
            ..ChosenLight::default()
        }])
    );
    let _ = state.commit_light(LightSettings::new(false, 80, Some(4000)));
    assert_eq!(chosen_by_reload(&drain(&mut receiver)), Some(Vec::new()));
}

#[test]
fn a_manual_power_saved_without_a_write_chooses_the_power() {
    // With `auto_camera` on (macOS only), the power is a manual override
    // with a save of its own; without it, the power is a plain light edit.
    for auto_camera in [false, true] {
        let (mut state, mut receiver, key) = offline_saved_glow();
        state.config.edit(|config| {
            let mut light = LightSettings::new(true, 80, Some(4000));
            light.auto_camera = auto_camera;
            config.set_light(key.as_str(), light);
        });
        let _ = state.commit_manual_light_power(false);
        assert_eq!(
            chosen_by_reload(&drain(&mut receiver)),
            Some(vec![ChosenLight {
                device: key,
                power: true,
                ..ChosenLight::default()
            }]),
            "auto_camera = {auto_camera}"
        );
    }
}

#[test]
fn a_save_after_a_write_chooses_nothing() {
    // The write took the change made on the light before it started.
    let light = saved_glow();
    let (mut state, mut receiver) = state_with(&light, Config::ephemeral());
    let key = state.current_record().expect("light record").device_key();
    let _ = state.commit_light(LightSettings::new(true, 60, Some(4000)));
    let writes: Vec<_> = drain(&mut receiver)
        .into_iter()
        .map(|command| {
            let Command::SetLight(SetLight {
                command,
                request_id,
                ..
            }) = command
            else {
                panic!("expected only light writes");
            };
            (command, request_id)
        })
        .collect();
    assert!(!writes.is_empty());
    for (command, request_id) in writes {
        let _ = state.apply_light_command_result(key.clone(), request_id, command, Ok(()));
    }
    assert!(saved_and_reloaded(&drain(&mut receiver)));
}
