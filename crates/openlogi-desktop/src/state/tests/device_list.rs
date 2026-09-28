//! From agent snapshots to the device list: the canonical profile's projection, folding, battery-only changes, and forgetting a device.

use super::*;

/// A mouse paired to a Bolt receiver, reachable by receiver UID + slot.
/// Shares its receiver UID (`82839805`) and unit id (`6be9d300`) with
/// `identity::tests::settings_still_under_the_pre_upgrade_key_are_read_from_it`,
/// so a config entry pre-seeded at `"receiver:82839805:slot:1"` is exactly
/// the legacy, route-keyed entry `adopt_routes` folds into `"unit:6be9d300"`.
fn receiver_inventory() -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "Bolt Receiver".to_string(),
            vendor_id: 0x046d,
            product_id: 0xc548,
            unique_id: Some("82839805".to_string()),
        },
        paired: vec![PairedDevice {
            slot: 1,
            codename: Some("MX Master 3S".to_string()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: Some(DeviceModelInfo {
                entity_count: 1,
                serial_number: None,
                unit_id: [0x6b, 0xe9, 0xd3, 0x00],
                transports: DeviceTransports::default(),
                model_ids: [0xb034, 0, 0],
                extended_model_id: 2,
            }),
            capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Mouse)),
        }],
    }
}

fn canonical_device_profile() -> DeviceProfile {
    let profile: DeviceProfile =
        serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).expect("canonical profile parses");
    profile.validate().expect("canonical profile validates");
    profile
}

fn snapshot_candidate(profile: &DeviceProfile) -> AgentSnapshot {
    let editor = app("org.openlogi.synthetic-editor", "Synthetic Editor");
    AgentSnapshot {
        status: AgentStatus {
            accessibility_granted: true,
            hook_installed: true,
            launch_at_login: false,
            inventory: InventoryHealth::Ready,
            protocol_version: PROTOCOL_VERSION,
            agent_version: "synthetic-profile-test".to_string(),
            input_monitoring_granted: true,
            hid_open_failures: false,
        },
        inventory: profile.inventories.clone(),
        standalone: profile.standalone.clone(),
        camera_active: true,
        pairing: None,
        foreground: ForegroundApps {
            current: Some(editor.clone()),
            recent: vec![editor],
        },
    }
}

fn canonical_profile_state(
    commands: tokio::sync::mpsc::UnboundedSender<crate::services::ipc::Command>,
) -> AppState {
    let resolver = AssetResolver::new();
    AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands))
}

fn profile_device(state: &AppState, unit_id: [u8; 4]) -> &DeviceRecord {
    state
        .devices()
        .iter()
        .find(|record| record.unit_id == unit_id)
        .expect("canonical profile device is projected")
}

#[test]
fn canonical_profile_projects_identity_routes_capabilities_and_battery() {
    let profile = canonical_device_profile();
    let snapshot = snapshot_candidate(&profile);
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = canonical_profile_state(commands);

    let changes = state.apply_agent_snapshot(&snapshot, &resolver, &[]);

    assert!(changes.inventory_ready);
    assert_eq!(
        changes.events,
        [
            super::StateEvent::InventoryChanged,
            super::StateEvent::AgentChanged,
            super::StateEvent::CameraChanged,
            super::StateEvent::ForegroundChanged,
        ]
    );
    assert_eq!(state.last_inventory(), snapshot.inventory);
    assert_eq!(state.agent_status(), Some(&snapshot.status));
    assert_eq!(state.devices().len(), 5);

    let receiver_mouse = profile_device(&state, [79, 76, 68, 1]);
    assert_eq!(receiver_mouse.config_key, "unit:4f4c4401");
    assert_eq!(
        receiver_mouse.route,
        Some(DeviceRoute::Bolt {
            receiver_uid: "OL-BOLT-UID-0001".to_string(),
            slot: 1,
        })
    );
    assert_eq!(
        receiver_mouse.capabilities,
        profile.inventories[0].paired[0].capabilities
    );
    assert_eq!(
        receiver_mouse
            .battery
            .as_ref()
            .map(|battery| battery.percentage),
        Some(80)
    );

    let offline_mouse = state
        .devices()
        .iter()
        .find(|record| record.slot == 2)
        .expect("offline receiver slot is projected");
    assert!(!offline_mouse.online);
    assert_eq!(
        offline_mouse.route,
        Some(DeviceRoute::Bolt {
            receiver_uid: "OL-BOLT-UID-0001".to_string(),
            slot: 2,
        })
    );
    assert_eq!(offline_mouse.capabilities, None);
    assert_eq!(offline_mouse.battery, None);

    let keyboard = profile_device(&state, [79, 76, 68, 2]);
    assert_eq!(keyboard.config_key, "unit:4f4c4402");
    assert_eq!(
        keyboard.capabilities,
        profile.inventories[0].paired[2].capabilities
    );
    assert_eq!(
        keyboard.battery.as_ref().map(|battery| battery.percentage),
        Some(100)
    );

    let direct = profile_device(&state, [79, 76, 68, 3]);
    assert_eq!(direct.config_key, "unit:4f4c4403");
    assert_eq!(
        direct.route,
        Some(DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb020,
        })
    );
    assert_eq!(
        direct.capabilities,
        profile.inventories[1].paired[0].capabilities
    );
    assert_eq!(
        direct.battery.as_ref().map(|battery| battery.percentage),
        Some(55)
    );

    let light = profile_device(&state, [79, 76, 68, 4]);
    assert_eq!(light.config_key, "unit:4f4c4404");
    assert_eq!(
        light.route,
        Some(DeviceRoute::RawHid {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "OPENLOGI-FIXTURE-RAWHID-001".to_string(),
        })
    );
    assert_eq!(
        light.light_capabilities,
        profile.standalone[0].light_capabilities
    );
}

#[test]
fn canonical_profile_snapshots_replace_online_state_and_remove_absent_receivers() {
    let profile = canonical_device_profile();
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = canonical_profile_state(commands);
    state.apply_agent_snapshot(&snapshot_candidate(&profile), &resolver, &[]);
    assert!(
        state
            .devices()
            .iter()
            .any(|record| record.slot == 2 && !record.online)
    );

    let mut replaced = snapshot_candidate(&profile);
    replaced.inventory[0].paired[0].online = false;
    replaced.inventory[0].paired[0].battery = None;
    replaced.inventory[0].paired[0].capabilities = None;
    replaced.inventory[1].paired[0].online = false;
    replaced.inventory[1].paired[0].battery = None;
    replaced.inventory[1].paired[0].capabilities = None;
    state.apply_agent_snapshot(&replaced, &resolver, &[]);
    let receiver_mouse = profile_device(&state, [79, 76, 68, 1]);
    assert!(!receiver_mouse.online);
    assert_eq!(receiver_mouse.battery, None);
    assert_eq!(receiver_mouse.capabilities, None);
    let direct_mouse = profile_device(&state, [79, 76, 68, 3]);
    assert!(!direct_mouse.online);
    assert_eq!(direct_mouse.battery, None);
    assert_eq!(direct_mouse.capabilities, None);

    let mut without_receiver = replaced;
    without_receiver.inventory.remove(0);
    for _ in 0..=INVENTORY_MISS_GRACE {
        state.apply_agent_snapshot(&without_receiver, &resolver, &[]);
    }

    assert!(
        state
            .devices()
            .iter()
            .all(|record| !matches!(record.route, Some(DeviceRoute::Bolt { .. }))),
        "all absent receiver records are removed after the probe-miss grace"
    );
    assert!(state.devices().iter().any(|record| {
        matches!(record.route, Some(DeviceRoute::Direct { .. })) && !record.online
    }));
    assert!(state.devices().iter().any(|record| {
        matches!(record.route, Some(DeviceRoute::RawHid { .. })) && record.online
    }));
}

#[test]
fn canonical_profile_light_setting_errors_reach_desktop_state() {
    let profile = canonical_device_profile();
    let raw_settings = profile
        .settings
        .iter()
        .find(|settings| matches!(settings.route, DeviceRoute::RawHid { .. }))
        .expect("canonical raw-HID settings");
    assert_eq!(raw_settings.light, ProfileSupport::Supported);

    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = canonical_profile_state(commands);
    state.apply_agent_snapshot(&snapshot_candidate(&profile), &resolver, &[]);
    let light_index = state
        .devices()
        .iter()
        .position(|record| record.unit_id == [79, 76, 68, 4])
        .expect("canonical light is projected");
    let _ = state.select_device(light_index);
    let mut reloads = 0;
    loop {
        match receiver.try_recv() {
            Ok(crate::services::ipc::Command::ReloadConfig(_)) => reloads += 1,
            Ok(_) => panic!("unexpected command before the light write"),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                panic!("light command channel disconnected")
            }
        }
    }
    assert!(reloads > 0);
    let light = state.current_record().expect("canonical light is selected");
    assert!(light.online);
    assert_eq!(light.route.as_ref(), Some(&raw_settings.route));
    assert_eq!(
        openlogi_core::hid::commands_for_light_settings(
            LightSettings::new(false, 50, Some(3000)),
            light
                .light_capabilities
                .expect("canonical light capabilities are projected"),
        )
        .len(),
        3
    );
    let key = light.device_key();

    let _ = state.commit_light(LightSettings::new(false, 50, Some(3000)));
    let mut pending = Vec::new();
    loop {
        match receiver.try_recv() {
            Ok(crate::services::ipc::Command::SetLight(SetLight {
                route,
                command,
                key: command_key,
                request_id,
            })) => {
                assert_eq!(&route, &raw_settings.route);
                assert_eq!(command_key, key);
                pending.push((command, request_id));
            }
            Ok(_) => panic!("unexpected command while collecting light writes"),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                panic!("light command channel disconnected")
            }
        }
    }
    assert_eq!(pending.len(), 3);
    assert!(pending.windows(2).all(|pair| pair[0].1 == pair[1].1));

    let setting_error = WriteError::DeviceUnreachable {
        index: openlogi_core::hid::DIRECT_DEVICE_INDEX,
    };
    let expected_error = setting_error.to_string();
    for (command, request_id) in pending {
        let result = if matches!(
            command,
            openlogi_core::hid::LightCommand::TemperatureKelvin(_)
        ) {
            Err(setting_error.clone())
        } else {
            Ok(())
        };
        assert_eq!(
            state.apply_light_command_result(key.clone(), request_id, command, result),
            [lighting_changed(&key)]
        );
    }
    assert!(matches!(
        state.light_command_status(),
        Some(LightCommandStatus::Failed(error)) if error == expected_error
    ));
}

#[test]
fn failed_fold_persist_does_not_orphan_the_device_list() {
    // Reproduces the bug traced in the pre-PR review: `refresh_inventories`
    // folds a legacy route-keyed config entry into the device's canonical
    // identity key, then tries to persist. When the write fails (here,
    // `ConfigPersistence::ReadOnly`), `persist_config` rolls `self.config`
    // back to its pre-fold, legacy-keyed state — but without the fix,
    // `refresh_inventories` still assigned `self.device_list` from the
    // now-stale, folded `merged_list`. From then on `device_list` names a
    // `config_key` that does not exist in `config`, so every
    // `config.devices.get(record.config_key)` lookup silently misses.
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut config = Config::ephemeral();
    config.set_dpi("receiver:82839805:slot:1", Dpi::new(3200));
    let mut state = AppState::new(Sources {
        persistence: ConfigPersistence::ReadOnly("simulated unwritable config.toml".to_string()),
        ..Sources::in_memory(config, &resolver, commands)
    });
    assert!(state.devices().is_empty(), "no inventory seen yet");

    let changed = state.refresh_inventories(&[receiver_inventory()], &[], &resolver, &[]);

    assert!(
        changed.is_empty(),
        "a failed fold-persist must not report a change — a caller \
         acting on it would treat the now-discarded `merged_list` as live"
    );
    assert!(
        state.devices().is_empty(),
        "device_list must stay at its pre-refresh value — built from the \
         folded config that failed to persist and was rolled back, the new \
         list would no longer agree with `state.config`"
    );
    assert!(
        state
            .config
            .devices
            .contains_key("receiver:82839805:slot:1"),
        "the rollback must restore the legacy entry still holding the \
         user's settings"
    );
    assert!(
        !state.config.devices.contains_key("unit:6be9d300"),
        "the folded canonical entry must not survive a rolled-back persist"
    );
    for record in state.devices() {
        let Some(config_key) = record.persistent_config_key() else {
            continue;
        };
        assert!(
            state.config.devices.contains_key(config_key),
            "device_list record names {config_key}, which must exist in \
             `config` — device_list and config must never disagree"
        );
    }
}

/// A battery reading that changed on an otherwise identical device must reach
/// the device list. The old guard compared nine hand-picked fields and
/// `battery` was not among them, so the rebuilt list — carrying the fresh
/// percentage — was discarded and every battery readout in the UI (gallery
/// card, detail page, native Device menu) stayed frozen until some *other*
/// compared field happened to move.
#[test]
fn a_battery_only_change_reaches_the_device_list() {
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let unit_id = [1, 2, 3, 4];
    let mut state = AppState::new(Sources {
        inventories: &[inventory_with_battery(unit_id, 50)],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    assert_eq!(
        state.devices()[0].battery.as_ref().map(|b| b.percentage),
        Some(50)
    );

    let changed =
        state.refresh_inventories(&[inventory_with_battery(unit_id, 40)], &[], &resolver, &[]);

    assert_eq!(
        changed,
        [StateEvent::InventoryChanged],
        "a battery change is a change"
    );
    assert_eq!(
        state.devices()[0].battery.as_ref().map(|b| b.percentage),
        Some(40),
        "the fresh reading must replace the stale one"
    );
}

/// The guard still exists: an identical snapshot is a no-op, so quiet cycles
/// cost no window refresh. Without this the previous test could be satisfied
/// by simply always returning `true`.
#[test]
fn an_identical_snapshot_is_still_a_no_op() {
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let unit_id = [1, 2, 3, 4];
    let mut state = AppState::new(Sources {
        inventories: &[inventory_with_battery(unit_id, 50)],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });

    assert!(
        state
            .refresh_inventories(&[inventory_with_battery(unit_id, 50)], &[], &resolver, &[])
            .is_empty()
    );
}

fn inventory_with_battery(unit_id: [u8; 4], percentage: u8) -> DeviceInventory {
    let mut inventory = direct_inventory(unit_id);
    inventory.paired[0].battery = Some(BatteryInfo {
        percentage,
        level: BatteryLevel::Good,
        status: BatteryStatus::Discharging,
    });
    inventory
}

/// One offline placeholder seeded from a persisted identity — the shape a
/// sleeping Bluetooth mouse leaves behind after a restart.
fn state_with_an_offline_identity(persistence: ConfigPersistence) -> AppState {
    let mut config = Config::ephemeral();
    config.set_device_identity(
        "2b034",
        DeviceIdentity {
            display_name: "MX Anywhere 3S".to_string(),
            kind: DeviceKind::Mouse,
            capabilities: Capabilities::presumed_from_kind(DeviceKind::Mouse),
            light_capabilities: None,
            model_info: Some(DeviceModelInfo {
                entity_count: 0,
                serial_number: None,
                unit_id: [0; 4],
                transports: DeviceTransports::default(),
                model_ids: [0xb034, 0, 0],
                extended_model_id: 2,
            }),
            codename: Some("MX Anywhere 3S".to_string()),
            driver_id: None,
            registry_model_id: None,
        },
    );
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    AppState::new(Sources {
        persistence,
        ..Sources::in_memory(config, &AssetResolver::new(), commands)
    })
}

/// Forgetting an offline device removes both its placeholder card and its
/// persisted entry, so no later inventory refresh can reseed it.
#[test]
fn forgetting_an_offline_device_drops_its_card_and_config_entry() {
    let mut state = state_with_an_offline_identity(ConfigPersistence::MemoryOnly);
    assert_eq!(state.devices().len(), 1);
    let record_key = state.devices()[0].record_key();

    assert_eq!(
        state.forget_device(&record_key),
        [StateEvent::InventoryChanged]
    );

    assert!(state.devices().is_empty());
    assert!(
        state
            .config
            .edit(|config| config.device_identity("2b034").is_none()),
        "the persisted entry must go with the card"
    );
}

/// A live device refuses deletion — the next snapshot would simply
/// re-register it.
#[test]
fn a_live_device_refuses_to_be_forgotten() {
    let mut state = state_with_a_known_mouse();
    let record_key = state.devices()[0].record_key();

    assert!(state.forget_device(&record_key).is_empty());
    assert_eq!(state.devices().len(), 1);
}

/// A save that cannot land keeps the card: the config store restores the
/// persisted revision and `forget_device` reports the failure, instead of the
/// card vanishing until the next refresh resurrects it.
#[test]
fn a_failed_save_keeps_the_forgotten_device() {
    let mut state = state_with_an_offline_identity(ConfigPersistence::ReadOnly("read-only".into()));
    let record_key = state.devices()[0].record_key();

    assert!(state.forget_device(&record_key).is_empty());

    assert_eq!(state.devices().len(), 1, "the card must stay");
    assert!(
        state
            .config
            .edit(|config| config.device_identity("2b034").is_some()),
        "the persisted entry must survive the failed save"
    );
}
