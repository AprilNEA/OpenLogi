use super::*;

#[test]
fn no_argument_mode_uses_the_canonical_profile_with_demo_time() {
    let state = state_from_args(std::iter::empty()).expect("no-argument mock state");
    let canonical: DeviceProfile =
        serde_json::from_str(openlogi_device::fixture::CANONICAL_DEVICE_PROFILE_JSON)
            .expect("canonical profile parses");

    assert_eq!(state.profile, canonical);
    assert!(state.clock.is_demo());
}

fn demo_state() -> State {
    State::new(
        built_in_profile().expect("built-in profile should construct"),
        MockClock::Demo(Instant::now()),
    )
    .expect("built-in profile should validate")
}

fn test_state() -> State {
    State::new(
        built_in_profile().expect("built-in profile should construct"),
        MockClock::Test(Duration::ZERO),
    )
    .expect("built-in profile should validate")
}

fn test_state_with_backlight(backlight: ProfileSetting<BacklightState>) -> State {
    let mut profile = built_in_profile().expect("built-in profile should construct");
    profile.settings[0].backlight = backlight;
    State::new(profile, MockClock::Test(Duration::ZERO)).expect("backlight profile should validate")
}

fn test_state_with_unavailable_reads() -> State {
    let mut profile = built_in_profile().expect("built-in profile should construct");
    let settings = &mut profile.settings[0];
    settings.dpi = ProfileSetting::Unavailable;
    settings.smartshift = ProfileSetting::Unavailable;
    settings.wheel = ProfileSetting::Unavailable;
    settings.backlight = ProfileSetting::Unavailable;
    State::new(profile, MockClock::Test(Duration::ZERO))
        .expect("unavailable settings should preserve capability consistency")
}

fn mouse_route() -> DeviceRoute {
    DeviceRoute::Bolt {
        receiver_uid: RECEIVER_UID.to_string(),
        slot: MOUSE_SLOT,
    }
}

fn state_with_discovery() -> State {
    let mut state = demo_state();
    let id = state
        .begin_pairing()
        .expect("idle mock should admit pairing");
    state
        .pairing_session(id)
        .expect("new pairing session should be active")
        .discovered = Some(FoundDevice {
        address: CANDIDATE_ADDRESS,
        name: "ERGO K860".to_string(),
    });
    state.set_phase(PairingPhase::Found(vec![FoundDevice {
        address: CANDIDATE_ADDRESS,
        name: "ERGO K860".to_string(),
    }]));
    state
}

#[test]
fn fixture_argument_and_file_boundary_load_a_validated_profile() {
    let directory = tempfile::tempdir().expect("temporary fixture directory");
    let path = directory.path().join("profile.json");
    let profile = built_in_profile().expect("built-in profile should construct");
    fs::write(
        &path,
        openlogi_device::fixture::CANONICAL_DEVICE_PROFILE_JSON,
    )
    .expect("fixture file should be writable");

    let parsed =
        parse_fixture_arg([OsString::from("--fixture"), path.clone().into_os_string()].into_iter())
            .expect("fixture argument should parse");
    assert_eq!(parsed.as_deref(), Some(path.as_path()));
    assert_eq!(load_fixture_profile(&path), Ok(profile));

    let mut invalid: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("fixture file should remain readable"))
            .expect("fixture JSON should parse");
    invalid["schema_version"] = serde_json::Value::from(99);
    fs::write(
        &path,
        serde_json::to_vec_pretty(&invalid).expect("invalid profile should serialize"),
    )
    .expect("invalid fixture should be writable");
    let error = load_fixture_profile(&path).expect_err("invalid profile must be rejected");
    assert!(error.contains("unsupported device profile schema version 99"));
}

#[test]
fn fixture_clock_is_frozen_until_logical_time_advances() {
    let mut state = test_state();
    let initial = snapshot_of(&state);

    assert_eq!(snapshot_of(&state), initial);
    state.advance_test_time(FOREGROUND_SWITCH_PERIOD);
    let foreground_changed = snapshot_of(&state);
    assert_ne!(foreground_changed.foreground, initial.foreground);
    assert_eq!(foreground_changed.inventory, initial.inventory);
    assert_eq!(foreground_changed.camera_active, initial.camera_active);

    state.advance_test_time(
        CAMERA_TOGGLE_PERIOD
            .checked_sub(FOREGROUND_SWITCH_PERIOD)
            .expect("camera period should exceed foreground period"),
    );
    let camera_changed = snapshot_of(&state);
    assert_ne!(camera_changed.camera_active, initial.camera_active);
    assert_eq!(camera_changed.inventory, initial.inventory);
}

#[tokio::test]
async fn profile_backed_setting_rpcs_round_trip() {
    let agent = MockAgent::new(test_state());
    let context = tarpc::context::current();
    let route = mouse_route();
    let initial = agent
        .clone()
        .read_dpi(context, route.clone())
        .await
        .expect("mouse should expose DPI");
    assert_eq!(initial.current, Dpi::new(1600));

    agent
        .clone()
        .set_dpi(tarpc::context::current(), route.clone(), Dpi::new(1631))
        .await
        .expect("DPI write should succeed");
    let written = agent
        .clone()
        .read_dpi(tarpc::context::current(), route.clone())
        .await
        .expect("DPI should read back");
    assert_eq!(written.current, Dpi::new(1650));

    let mut smartshift = agent
        .clone()
        .read_smartshift(tarpc::context::current(), route.clone())
        .await
        .expect("mouse should expose SmartShift");
    smartshift.mode = smartshift.mode.flipped();
    agent
        .clone()
        .set_smartshift(tarpc::context::current(), route, smartshift)
        .await
        .expect("SmartShift write should succeed");
    assert_eq!(
        agent
            .clone()
            .read_smartshift(tarpc::context::current(), mouse_route())
            .await
            .expect("SmartShift should read back"),
        smartshift
    );
}

#[tokio::test]
async fn profile_backed_wheel_and_backlight_reads_return_exact_values() {
    let agent = MockAgent::new(test_state());
    let wheel = agent
        .read_wheel(tarpc::context::current(), mouse_route())
        .await
        .expect("mouse should expose its profile wheel state");
    assert_eq!(
        wheel,
        ScrollWheelMode {
            resolution: openlogi_core::config::ScrollResolution::High,
            inverted: false,
            target: openlogi_hid::ScrollReportingTarget::Native,
        }
    );

    let expected = BacklightState {
        enabled: true,
        mode: openlogi_hid::BacklightMode::PermanentManual,
        status: openlogi_hid::BacklightStatus::PermanentManual,
        current_level: 3,
        nb_levels: 8,
    };
    let agent = MockAgent::new(test_state_with_backlight(ProfileSetting::Supported(
        expected,
    )));
    assert_eq!(
        agent
            .read_backlight(tarpc::context::current(), mouse_route())
            .await
            .expect("backlight read should return exact profile state"),
        expected
    );
}

#[tokio::test]
async fn wheel_and_backlight_reads_preserve_route_and_support_errors() {
    let agent = MockAgent::new(test_state());
    let keyboard = DeviceRoute::Bolt {
        receiver_uid: RECEIVER_UID.to_string(),
        slot: KEYBOARD_SLOT,
    };
    assert!(matches!(
        agent
            .clone()
            .read_wheel(tarpc::context::current(), keyboard.clone())
            .await,
        Err(WriteError::FeatureUnsupported {
            feature_hex: 0x2121
        })
    ));
    assert!(matches!(
        agent
            .clone()
            .read_backlight(tarpc::context::current(), keyboard)
            .await,
        Err(WriteError::FeatureUnsupported {
            feature_hex: 0x1982
        })
    ));

    let offline = DeviceRoute::Bolt {
        receiver_uid: RECEIVER_UID.to_string(),
        slot: OFFLINE_SLOT,
    };
    assert!(matches!(
        agent
            .clone()
            .read_wheel(tarpc::context::current(), offline.clone())
            .await,
        Err(WriteError::DeviceUnreachable {
            index: OFFLINE_SLOT
        })
    ));
    assert!(matches!(
        agent
            .clone()
            .read_backlight(tarpc::context::current(), offline)
            .await,
        Err(WriteError::DeviceUnreachable {
            index: OFFLINE_SLOT
        })
    ));

    let unknown = DeviceRoute::Direct {
        vendor_id: 0xffff,
        product_id: 0xffff,
    };
    assert!(matches!(
        agent
            .clone()
            .read_wheel(tarpc::context::current(), unknown.clone())
            .await,
        Err(WriteError::DeviceNotFound)
    ));
    assert!(matches!(
        agent
            .read_backlight(tarpc::context::current(), unknown)
            .await,
        Err(WriteError::DeviceNotFound)
    ));
}

#[tokio::test]
async fn unavailable_setting_reads_and_writes_remain_transient() {
    let smartshift = test_state().profile.settings[0]
        .smartshift
        .value()
        .copied()
        .expect("canonical mouse has SmartShift state");
    let agent = MockAgent::new(test_state_with_unavailable_reads());
    let route = mouse_route();

    assert_unreachable(
        agent
            .clone()
            .read_dpi(tarpc::context::current(), route.clone())
            .await,
        MOUSE_SLOT,
    );
    assert_unreachable(
        agent
            .clone()
            .set_dpi(tarpc::context::current(), route.clone(), Dpi::new(1600))
            .await,
        MOUSE_SLOT,
    );
    assert_unreachable(
        agent
            .clone()
            .read_smartshift(tarpc::context::current(), route.clone())
            .await,
        MOUSE_SLOT,
    );
    assert_unreachable(
        agent
            .clone()
            .set_smartshift(tarpc::context::current(), route.clone(), smartshift)
            .await,
        MOUSE_SLOT,
    );
    assert_unreachable(
        agent
            .clone()
            .read_wheel(tarpc::context::current(), route.clone())
            .await,
        MOUSE_SLOT,
    );
    assert_unreachable(
        agent.read_backlight(tarpc::context::current(), route).await,
        MOUSE_SLOT,
    );
}

fn assert_unreachable<T>(result: Result<T, WriteError>, index: u8) {
    let Err(error) = result else {
        panic!("setting should be unreachable");
    };
    assert!(matches!(
        error,
        WriteError::DeviceUnreachable { index: actual } if actual == index
    ));
}

#[tokio::test]
async fn profile_backed_support_and_route_errors_remain_typed() {
    let agent = MockAgent::new(test_state());
    agent
        .clone()
        .set_lighting(
            tarpc::context::current(),
            DeviceRoute::Bolt {
                receiver_uid: RECEIVER_UID.to_string(),
                slot: KEYBOARD_SLOT,
            },
            Lighting::default(),
        )
        .await
        .expect("keyboard should accept profile-backed lighting writes");
    let unsupported_lighting = agent
        .clone()
        .set_lighting(
            tarpc::context::current(),
            mouse_route(),
            Lighting::default(),
        )
        .await;
    assert!(matches!(
        unsupported_lighting,
        Err(WriteError::FeatureUnsupported {
            feature_hex: 0x8070
        })
    ));

    let light_route = {
        let state = agent.state.lock().await;
        standalone_route(&state.profile.standalone[0])
    };
    agent
        .clone()
        .set_light(
            tarpc::context::current(),
            light_route.clone(),
            LightCommand::TemperatureKelvin(3000),
        )
        .await
        .expect("standalone light should accept an in-range command");
    let invalid_light = agent
        .clone()
        .set_light(
            tarpc::context::current(),
            light_route,
            LightCommand::TemperatureKelvin(3001),
        )
        .await;
    assert!(matches!(
        invalid_light,
        Err(WriteError::InvalidLightValue {
            control,
            value: 3001
        }) if control == "temperature_kelvin"
    ));

    let unsupported = agent
        .clone()
        .read_dpi(
            tarpc::context::current(),
            DeviceRoute::Bolt {
                receiver_uid: RECEIVER_UID.to_string(),
                slot: KEYBOARD_SLOT,
            },
        )
        .await;
    assert!(matches!(
        unsupported,
        Err(WriteError::FeatureUnsupported {
            feature_hex: 0x2201
        })
    ));

    let offline = agent
        .clone()
        .read_dpi(
            tarpc::context::current(),
            DeviceRoute::Bolt {
                receiver_uid: RECEIVER_UID.to_string(),
                slot: OFFLINE_SLOT,
            },
        )
        .await;
    assert!(matches!(
        offline,
        Err(WriteError::DeviceUnreachable {
            index: OFFLINE_SLOT
        })
    ));

    let unknown = agent
        .read_dpi(
            tarpc::context::current(),
            DeviceRoute::Direct {
                vendor_id: 0xffff,
                product_id: 0xffff,
            },
        )
        .await;
    assert!(matches!(unknown, Err(WriteError::DeviceNotFound)));
}

#[tokio::test]
async fn generation_changes_only_when_the_snapshot_changes() {
    let agent = MockAgent::new(test_state());
    let initial = agent.current().await;

    agent
        .clone()
        .set_dpi(tarpc::context::current(), mouse_route(), Dpi::new(1700))
        .await
        .expect("DPI write should succeed");
    assert_eq!(agent.current().await.generation, initial.generation);

    agent
        .state
        .lock()
        .await
        .advance_test_time(FOREGROUND_SWITCH_PERIOD);
    let changed = agent.current().await;
    assert_eq!(changed.generation, initial.generation + 1);
    assert_ne!(changed.snapshot.foreground, initial.snapshot.foreground);
    assert_eq!(agent.current().await.generation, changed.generation);
}

#[tokio::test]
async fn fixture_mode_does_not_start_wall_clock_pairing_tasks() {
    let result = MockAgent::new(test_state())
        .start_pairing(tarpc::context::current(), ReceiverSelector::First)
        .await;
    assert!(matches!(
        result,
        Err(PairingCommandError::WatcherUnavailable)
    ));
}

#[test]
fn cancel_and_device_selection_are_atomic_in_either_order() {
    let mut cancel_first = state_with_discovery();
    cancel_first.cancel_pairing();

    assert!(matches!(
        cancel_first.select_pairing_device(CANDIDATE_ADDRESS),
        Err(PairingCommandError::NoActiveSession)
    ));
    assert!(cancel_first.pairing.is_none());
    assert_eq!(cancel_first.phase, None);
    assert!(matches!(
        cancel_first.next_pairing_update(),
        Some(PairingUpdate::Searching)
    ));
    assert!(matches!(
        cancel_first.next_pairing_update(),
        Some(PairingUpdate::Failed(PairingFailure::Cancelled))
    ));

    let mut select_first = state_with_discovery();
    select_first
        .select_pairing_device(CANDIDATE_ADDRESS)
        .expect("discovered device should be selectable");
    assert_eq!(select_first.phase, Some(PairingPhase::Pairing));

    select_first.cancel_pairing();

    assert!(select_first.pairing.is_none());
    assert_eq!(select_first.phase, None);
    assert!(matches!(
        select_first.next_pairing_update(),
        Some(PairingUpdate::Searching)
    ));
    assert!(matches!(
        select_first.next_pairing_update(),
        Some(PairingUpdate::Failed(PairingFailure::Cancelled))
    ));
}
