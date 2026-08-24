//! AppState unit tests.

use std::collections::BTreeMap;

use openlogi_core::binding::{Action, Binding, ButtonId};
use openlogi_core::config::{
    Config, DeviceIdentity, LightSettings, Lighting, ScrollResolution, ThumbwheelSensitivity,
};
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, Capabilities, DeviceInventory, DeviceKind,
    DeviceModelInfo, DeviceTransports, LightCapabilities, LightValueRange, LightValueUnit,
    PairedDevice, RawDeviceAddress, ReceiverInfo, StandaloneDevice,
};
use openlogi_core::hid::{
    Dpi, SmartShiftAutoDisengage, SmartShiftMode, SmartShiftStatus, SmartShiftThreshold, WriteError,
};

use openlogi_core::app::ForegroundApp;
use openlogi_ipc::ForegroundApps;

use crate::features::mouse::thumbwheel::ThumbwheelPreset;
use crate::services::assets::AssetResolver;

use super::bindings::apply_thumbwheel_pair;
use super::devices::build_device_list;
use super::scroll::set_scroll_resolution_if_supported;
use super::smartshift::{smartshift_read_is_current, smartshift_write_outcome};
use super::{AppState, ConfigPersistence, LightCommandStatus, Load, SmartShiftWriteStatus};

#[test]
fn read_only_config_rolls_back_mutations_and_does_not_reload_agent() {
    let cache = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::with_runtime(
        Config::ephemeral(),
        &[],
        &[],
        &cache,
        &[],
        ConfigPersistence::ReadOnly("invalid config".into()),
        commands,
    );

    state.set_thumbwheel_sensitivity(ThumbwheelSensitivity::from_rounded(50.0));

    assert_eq!(
        state.app_settings().thumbwheel_sensitivity,
        ThumbwheelSensitivity::DEFAULT
    );
    assert_eq!(state.config_issue(), Some("invalid config"));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn agent_reload_error_stays_visible_until_a_successful_confirmation() {
    let cache = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::with_runtime(
        Config::ephemeral(),
        &[],
        &[],
        &cache,
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );
    assert!(
        state.apply_config_reload_result(Err(openlogi_ipc::ConfigReloadError {
            message: "agent rejected config".into(),
        }))
    );
    assert_eq!(state.config_issue(), Some("agent rejected config"));
    assert!(state.apply_config_reload_result(Ok(())));
    assert_eq!(state.config_issue(), None);
}

/// Config key of the mouse [`direct_inventory`] builds with a real unit id.
const KNOWN_MOUSE_KEY: &str = "direct:046d:b023:unit:a393cae0";

fn direct_inventory(unit_id: [u8; 4]) -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "MX Master 3S".to_string(),
            vendor_id: 0x046d,
            product_id: 0xb023,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: openlogi_core::hid::DIRECT_DEVICE_INDEX,
            codename: Some("MX Master 3S".to_string()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: Some(DeviceModelInfo {
                entity_count: 1,
                serial_number: None,
                unit_id,
                transports: DeviceTransports::default(),
                model_ids: [0xb034, 0, 0],
                extended_model_id: 2,
            }),
            capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Mouse)),
        }],
    }
}

/// A second, unmistakably different mouse, so a test can move the carousel.
fn second_mouse_inventory() -> DeviceInventory {
    let mut inventory = direct_inventory([0x11, 0x22, 0x33, 0x44]);
    inventory.receiver.name = "MX Anywhere 3S".to_string();
    inventory.receiver.product_id = 0xb037;
    inventory
}

fn superseded_litra_light() -> StandaloneDevice {
    StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "serial:glow-superseded".into(),
        },
        display_name: "Litra Glow".into(),
        manufacturer: Some("Logi".into()),
        serial_number: Some("glow-superseded".into()),
        unit_id: [0; 4],
        kind: DeviceKind::Light,
        online: true,
        capabilities: None,
        light_capabilities: Some(LightCapabilities {
            power: true,
            brightness: Some(
                LightValueRange::new(20, 250, 1, LightValueUnit::Lumens).expect("valid range"),
            ),
            ..LightCapabilities::default()
        }),
        driver_id: "litra".into(),
        registry_model_id: Some("8c900".into()),
    }
}

fn next_light_command(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<crate::services::ipc::Command>,
) -> (openlogi_core::hid::LightCommand, u64) {
    let Ok(crate::services::ipc::Command::SetLight(_, command, _, request_id)) =
        receiver.try_recv()
    else {
        panic!("expected a light command");
    };
    (command, request_id)
}

#[test]
fn thumbwheel_pair_updates_both_memory_and_config_entries() {
    let mut bindings = std::collections::BTreeMap::new();
    let mut config = Config::ephemeral();
    let key = "2b034";

    assert!(apply_thumbwheel_pair(
        &mut bindings,
        &mut config,
        Some(key),
        None,
        ThumbwheelPreset::Volume.pair(),
    ));
    assert_eq!(
        bindings.get(&ButtonId::ThumbwheelScrollDown),
        Some(&Action::VolumeDown)
    );
    assert_eq!(
        bindings.get(&ButtonId::ThumbwheelScrollUp),
        Some(&Action::VolumeUp)
    );
    let persisted = config.bindings_for(key);
    assert_eq!(
        persisted.get(&ButtonId::ThumbwheelScrollDown),
        Some(&Binding::Single(Action::VolumeDown))
    );
    assert_eq!(
        persisted.get(&ButtonId::ThumbwheelScrollUp),
        Some(&Binding::Single(Action::VolumeUp))
    );
}

#[test]
fn transient_thumbwheel_pair_stays_in_memory_without_persistence() {
    let mut bindings = std::collections::BTreeMap::new();
    let mut config = Config::ephemeral();

    assert!(!apply_thumbwheel_pair(
        &mut bindings,
        &mut config,
        None,
        None,
        ThumbwheelPreset::CycleDpi.pair(),
    ));
    assert_eq!(bindings.len(), 2);
    assert!(config.bindings_for("missing").is_empty());
}

/// A state holding the one persistent mouse, so per-device config has a key.
fn state_with_a_known_mouse() -> AppState {
    let cache = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    AppState::with_runtime(
        Config::ephemeral(),
        &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
        &[],
        &cache,
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    )
}

fn app(id: &str, display_name: &str) -> ForegroundApp {
    ForegroundApp {
        id: id.to_string(),
        display_name: display_name.to_string(),
    }
}

/// A known mouse with `app`'s profile open for editing.
fn state_editing(app: &str) -> AppState {
    let mut state = state_with_a_known_mouse();
    state.set_editing_app(Some(app.to_string()));
    assert_eq!(state.editing_app(), Some(app), "scope did not take");
    state
}

#[test]
fn a_binding_committed_in_a_per_app_profile_leaves_the_global_one_alone() {
    let mut state = state_editing("com.apple.Safari");
    state.commit_binding(ButtonId::Back, Action::Undo);

    assert_eq!(
        state
            .config
            .per_app_overrides(KNOWN_MOUSE_KEY, "com.apple.Safari"),
        Some(&BTreeMap::from([(ButtonId::Back, Action::Undo)]))
    );
    assert!(
        state.config.bindings_for(KNOWN_MOUSE_KEY).is_empty(),
        "the device's global bindings must be untouched"
    );
}

#[test]
fn clearing_an_override_falls_back_to_the_global_binding() {
    let mut state = state_with_a_known_mouse();
    state.commit_binding(ButtonId::Back, Action::Copy);
    state.set_editing_app(Some("com.apple.Safari".into()));
    state.commit_binding(ButtonId::Back, Action::Undo);
    assert_eq!(
        state.button_bindings.get(&ButtonId::Back),
        Some(&Action::Undo)
    );

    state.clear_app_binding(ButtonId::Back);

    assert_eq!(
        state.button_bindings.get(&ButtonId::Back),
        Some(&Action::Copy),
        "the panel falls back to what the default profile binds"
    );
    assert!(
        state
            .config
            .per_app_overrides(KNOWN_MOUSE_KEY, "com.apple.Safari")
            .is_none(),
        "an emptied profile is pruned, not left behind"
    );
}

#[test]
fn clearing_a_thumbwheel_override_drops_both_directions() {
    let mut state = state_with_a_known_mouse();
    state.commit_thumbwheel_preset(ThumbwheelPreset::Volume);
    state.set_editing_app(Some("com.apple.Safari".into()));
    state.commit_thumbwheel_preset(ThumbwheelPreset::CycleDpi);

    state.clear_app_thumbwheel();

    assert_eq!(
        state.button_bindings.get(&ButtonId::ThumbwheelScrollDown),
        Some(&Action::VolumeDown)
    );
    assert_eq!(
        state.button_bindings.get(&ButtonId::ThumbwheelScrollUp),
        Some(&Action::VolumeUp)
    );
    assert!(
        state
            .config
            .per_app_overrides(KNOWN_MOUSE_KEY, "com.apple.Safari")
            .is_none(),
        "both halves must be cleared so the empty profile is pruned"
    );
}

#[test]
fn gesture_mode_is_not_editable_from_inside_a_per_app_profile() {
    // The trap this guards: `set_gesture_mode` writes the device's global
    // bindings, so honouring it here would change every application from a
    // panel labelled with one. A per-app entry is `Action`-valued and has no
    // per-direction shape to promote into.
    let mut state = state_editing("com.apple.Safari");

    state.commit_gesture_mode(ButtonId::MiddleClick, true);

    assert!(
        !state
            .config
            .is_gesture_mode(KNOWN_MOUSE_KEY, ButtonId::MiddleClick),
        "a per-app profile must not promote a button globally"
    );
    assert!(
        state.current_gesture_maps().is_empty(),
        "and no gesture menu is offered in that scope"
    );
}

#[test]
fn a_gesture_button_stays_one_when_the_scope_returns_to_the_default_profile() {
    let mut state = state_with_a_known_mouse();
    state.commit_gesture_mode(ButtonId::MiddleClick, true);
    let global = state.current_gesture_maps();
    assert!(global.contains_key(&ButtonId::MiddleClick));

    state.set_editing_app(Some("com.apple.Safari".into()));
    assert!(state.current_gesture_maps().is_empty());
    assert_eq!(
        state.gesture_bindings, global,
        "the inspector cache keeps inherited gestures while per-app editing hides their controls"
    );
    // The device still has its gestures — only the open profile cannot show
    // them, which is what the device card must keep reporting.
    assert_eq!(
        state.device_gesture_binding_count(),
        global.values().map(BTreeMap::len).sum::<usize>()
    );

    state.set_editing_app(None);
    assert_eq!(state.current_gesture_maps(), global);
}

#[test]
fn a_profile_belongs_to_the_device_it_was_opened_on() {
    // Overlays are per-device, so a scope must not follow the carousel onto
    // another mouse and silently edit a profile the user never opened.
    let cache = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::with_runtime(
        Config::ephemeral(),
        &[
            direct_inventory([0xa3, 0x93, 0xca, 0xe0]),
            second_mouse_inventory(),
        ],
        &[],
        &cache,
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );
    let other = state
        .device_list
        .iter()
        .position(|record| record.config_key != KNOWN_MOUSE_KEY)
        .expect("the fixture pairs a second device");
    let known = state
        .device_list
        .iter()
        .position(|record| record.config_key == KNOWN_MOUSE_KEY)
        .expect("the fixture pairs the known mouse");

    state.set_current_device(known);
    state.set_editing_app(Some("com.apple.Safari".into()));

    state.set_current_device(other);
    assert_eq!(
        state.editing_app(),
        None,
        "another device falls back to its own global profile"
    );

    state.set_current_device(known);
    assert_eq!(
        state.editing_app(),
        Some("com.apple.Safari"),
        "and returning restores the profile that was open here"
    );
}

#[test]
fn the_active_profile_is_the_default_until_the_app_in_front_is_overridden() {
    let mut state = state_with_a_known_mouse();
    let safari = app("com.apple.Safari", "Safari");
    state.set_foreground(ForegroundApps {
        current: Some(safari.clone()),
        recent: vec![safari],
    });

    assert_eq!(
        state.active_profile_name(),
        None,
        "an app with no overrides runs the device's global bindings"
    );

    state.config.set_per_app_binding(
        KNOWN_MOUSE_KEY,
        "com.apple.Safari",
        ButtonId::Back,
        Some(Action::Undo),
    );
    assert_eq!(state.active_profile_name(), Some("Safari"));
}

#[test]
fn the_profile_shown_is_the_apps_even_while_this_window_has_focus() {
    // The frontmost application is OpenLogi whenever the user is looking at
    // this panel, so keying off `current` would report "Default profile" for
    // exactly the moment the row is on screen (issue: the row had no content
    // at all before). The recent list excludes our own windows, so its head is
    // the app the user came from.
    let mut state = state_with_a_known_mouse();
    state.config.set_per_app_binding(
        KNOWN_MOUSE_KEY,
        "com.apple.Safari",
        ButtonId::Back,
        Some(Action::Undo),
    );
    state.set_foreground(ForegroundApps {
        current: Some(app(openlogi_core::brand::APP_ID, "OpenLogi")),
        recent: vec![app("com.apple.Safari", "Safari")],
    });

    assert_eq!(state.active_profile_name(), Some("Safari"));
}

#[test]
fn a_host_with_no_readable_foreground_app_reports_the_default_profile() {
    let mut state = state_with_a_known_mouse();
    state.config.set_per_app_binding(
        KNOWN_MOUSE_KEY,
        "com.apple.Safari",
        ButtonId::Back,
        Some(Action::Undo),
    );
    // A pure-Wayland session with no usable backend, or a watcher that could
    // not start: the agent reports nothing and no profile can be in effect.
    assert!(!state.set_foreground(ForegroundApps::default()));
    assert_eq!(state.active_profile_name(), None);
}

#[test]
fn transient_identity_is_not_persisted_or_retained_after_resolution() {
    let cache = AssetResolver::new();
    let transient_inventory = direct_inventory([0; 4]);
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::with_runtime(
        Config::ephemeral(),
        &[transient_inventory],
        &[],
        &cache,
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );
    let transient_key = "direct:046d:b023:unit:00000000";

    assert_eq!(state.device_list.len(), 1);
    assert!(state.config.device_identity(transient_key).is_none());
    state.commit_dpi(Dpi::new(2400));
    assert!(state.config.dpi(transient_key).is_none());

    let stable_list = build_device_list(
        &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
        &[],
        &cache,
        &state.config,
        &[],
    );
    let merged = state.merge_inventory_snapshot(stable_list);

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].config_key, "direct:046d:b023:unit:a393cae0");
    assert!(merged[0].is_persistent());
}

#[test]
fn transient_probe_folds_into_its_known_card() {
    // #482: a half-read probe (all-zero unit id) of the only known device
    // with that vid/pid must not evict the known card or appear beside it —
    // the card keeps its identity and takes the live volatile state.
    let cache = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::with_runtime(
        Config::ephemeral(),
        &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
        &[],
        &cache,
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );
    let stable_key = "direct:046d:b023:unit:a393cae0";
    assert_eq!(state.device_list[0].config_key, stable_key);

    let transient_list =
        build_device_list(&[direct_inventory([0; 4])], &[], &cache, &state.config, &[]);
    let merged = state.merge_inventory_snapshot(transient_list);

    assert_eq!(merged.len(), 1, "no second card for the half-read probe");
    assert_eq!(merged[0].config_key, stable_key);
    assert!(merged[0].is_persistent());
    assert!(merged[0].online, "the live probe supplies volatile state");
    assert!(merged[0].route.is_some(), "the live route is kept usable");
}

#[test]
fn transient_record_beside_its_live_device_is_dropped() {
    // Both a full and a half-read probe of the same wire product in one
    // snapshot: the transient record is probe noise, not a second device.
    let cache = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::with_runtime(
        Config::ephemeral(),
        &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
        &[],
        &cache,
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );

    let both = build_device_list(
        &[
            direct_inventory([0xa3, 0x93, 0xca, 0xe0]),
            direct_inventory([0; 4]),
        ],
        &[],
        &cache,
        &state.config,
        &[],
    );
    assert_eq!(both.len(), 2);
    let merged = state.merge_inventory_snapshot(both);

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].config_key, "direct:046d:b023:unit:a393cae0");
    assert!(merged[0].online);
}

#[test]
fn transient_probe_adopts_the_absent_sibling_of_a_live_twin() {
    // Two same-model devices; one probes complete, the other half-reads.
    // The live twin must not get the transient discarded as its own noise:
    // the half-read probe can only be the sibling, which keeps its card
    // online and routed.
    let cache = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::with_runtime(
        Config::ephemeral(),
        &[
            direct_inventory([1, 1, 1, 1]),
            direct_inventory([2, 2, 2, 2]),
        ],
        &[],
        &cache,
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );

    let snapshot = build_device_list(
        &[direct_inventory([1, 1, 1, 1]), direct_inventory([0; 4])],
        &[],
        &cache,
        &state.config,
        &[],
    );
    let merged = state.merge_inventory_snapshot(snapshot);

    assert_eq!(merged.len(), 2, "no third card for the half-read probe");
    let Some(sibling) = merged
        .iter()
        .find(|r| r.config_key == "direct:046d:b023:unit:02020202")
    else {
        panic!("the sibling card must survive under its physical key");
    };
    assert!(
        sibling.online,
        "the half-read probe keeps the sibling online"
    );
    assert!(sibling.route.is_some(), "the live route stays usable");
}

#[test]
fn ambiguous_transient_probe_is_not_adopted() {
    // Two same-model devices are known; a half-read probe could be either,
    // so neither card may steal it.
    let cache = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::with_runtime(
        Config::ephemeral(),
        &[
            direct_inventory([1, 1, 1, 1]),
            direct_inventory([2, 2, 2, 2]),
        ],
        &[],
        &cache,
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );
    assert_eq!(state.device_list.len(), 2);

    let transient_list =
        build_device_list(&[direct_inventory([0; 4])], &[], &cache, &state.config, &[]);
    let merged = state.merge_inventory_snapshot(transient_list);

    assert_eq!(merged.len(), 3, "both known cards survive on grace");
    assert_eq!(
        merged.iter().filter(|r| !r.is_persistent()).count(),
        1,
        "the transient card stays its own record"
    );
}

#[test]
fn historical_transient_lighting_is_not_exposed_without_a_live_record() {
    let transient_key = "direct:046d:b023:unit:00000000";
    let mut config = Config::ephemeral();
    config.set_lighting(transient_key, Lighting::default());
    assert!(config.lighting(transient_key).is_some());
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::with_runtime(
        config,
        &[],
        &[],
        &AssetResolver::new(),
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );

    assert!(state.device_list.is_empty());
    assert!(state.lighting_for(transient_key).is_none());
}

#[test]
fn smartshift_write_feedback_requires_the_written_value() {
    let expected = SmartShiftStatus {
        mode: SmartShiftMode::Ratchet,
        auto_disengage: SmartShiftAutoDisengage::Threshold(SmartShiftThreshold::from_rounded(12.0)),
        tunable_torque: None,
    };
    assert_eq!(smartshift_write_outcome(expected, None), None);
    assert_eq!(
        smartshift_write_outcome(expected, Some(&Load::Ready(expected))),
        Some(SmartShiftWriteStatus::Confirmed)
    );
    assert_eq!(
        smartshift_write_outcome(
            expected,
            Some(&Load::Ready(SmartShiftStatus {
                auto_disengage: SmartShiftAutoDisengage::Threshold(
                    SmartShiftThreshold::from_rounded(13.0),
                ),
                ..expected
            })),
        ),
        Some(SmartShiftWriteStatus::Failed)
    );
    assert_eq!(
        smartshift_write_outcome(
            expected,
            Some(&Load::<SmartShiftStatus>::Failed("timeout".to_string(),))
        ),
        Some(SmartShiftWriteStatus::Failed)
    );
}

#[test]
fn stale_smartshift_reads_do_not_resolve_newer_writes() {
    let expected = SmartShiftStatus {
        mode: SmartShiftMode::Ratchet,
        auto_disengage: SmartShiftAutoDisengage::Threshold(SmartShiftThreshold::from_rounded(12.0)),
        tunable_torque: None,
    };
    let applying = SmartShiftWriteStatus::Applying {
        expected,
        write_id: 2,
    };

    assert!(smartshift_read_is_current(Some(2), Some(&applying)));
    assert!(!smartshift_read_is_current(Some(1), Some(&applying)));
    assert!(!smartshift_read_is_current(None, Some(&applying)));
    assert!(!smartshift_read_is_current(
        Some(2),
        Some(&SmartShiftWriteStatus::Confirmed)
    ));
    assert!(smartshift_read_is_current(None, None));
}

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
    let state = AppState::with_runtime(
        config,
        &[],
        &[],
        &AssetResolver::new(),
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );

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
    let state = AppState::with_runtime(
        Config::default(),
        &[],
        &[first, second],
        &AssetResolver::new(),
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );

    assert_eq!(
        state.asset_models(),
        vec![crate::services::assets::sync::AssetTarget::Standalone {
            registry_model_id: "8c900".into(),
        }]
    );
}

#[test]
fn gui_state_saves_and_clears_supported_wheel_resolution() {
    let mut config = Config::ephemeral();
    assert!(set_scroll_resolution_if_supported(
        &mut config,
        "mouse",
        true,
        Some(ScrollResolution::Low),
    ));
    assert_eq!(
        config.scroll_resolution("mouse"),
        Some(ScrollResolution::Low)
    );

    assert!(set_scroll_resolution_if_supported(
        &mut config,
        "mouse",
        true,
        None,
    ));
    assert_eq!(config.scroll_resolution("mouse"), None);
}

#[test]
fn gui_state_ignores_unsupported_wheel_resolution() {
    let mut config = Config::ephemeral();
    assert!(!set_scroll_resolution_if_supported(
        &mut config,
        "mouse",
        false,
        Some(ScrollResolution::High),
    ));
    assert_eq!(config.scroll_resolution("mouse"), None);
}

fn camera_controls(brightness: i32) -> openlogi_core::config::CameraControls {
    openlogi_core::config::CameraControls(std::collections::BTreeMap::from([(
        "brightness".into(),
        brightness,
    )]))
}

fn camera_state(config: Config) -> AppState {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    AppState::with_runtime(
        config,
        &[],
        &[],
        &AssetResolver::new(),
        &[],
        ConfigPersistence::MemoryOnly,
        tx,
    )
}

#[test]
fn migrate_lifts_legacy_port_bound_camera_key() {
    let mut config = Config::ephemeral();
    let model = "camera:046d:0893";
    let legacy = "camera-0x1123000046d0893";
    config.set_camera_controls(legacy, camera_controls(42));
    let mut state = camera_state(config);

    state.migrate_legacy_camera_key(model, "0x1123000046d0893");

    assert_eq!(
        state
            .config
            .camera_controls(model)
            .map(|c| c.0["brightness"]),
        Some(42)
    );
    assert!(state.config.camera_controls(legacy).is_none());
}

#[test]
fn migrate_does_not_overwrite_existing_model_settings() {
    let mut config = Config::ephemeral();
    let model = "camera:046d:0893";
    let legacy = "camera-0x1123000046d0893";
    config.set_camera_controls(model, camera_controls(1));
    config.set_camera_controls(legacy, camera_controls(99));
    let mut state = camera_state(config);

    state.migrate_legacy_camera_key(model, "0x1123000046d0893");

    assert_eq!(
        state
            .config
            .camera_controls(model)
            .map(|c| c.0["brightness"]),
        Some(1)
    );
    assert_eq!(
        state
            .config
            .camera_controls(legacy)
            .map(|c| c.0["brightness"]),
        Some(99)
    );
}

#[test]
fn light_write_failure_reaches_the_gui_state() {
    let light = StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "serial:glow-1".into(),
        },
        display_name: "Litra Glow".into(),
        manufacturer: Some("Logi".into()),
        serial_number: Some("glow-1".into()),
        unit_id: [0; 4],
        kind: DeviceKind::Light,
        online: true,
        capabilities: None,
        light_capabilities: Some(LightCapabilities {
            power: true,
            brightness: Some(
                LightValueRange::new(20, 250, 1, LightValueUnit::Lumens).expect("valid range"),
            ),
            ..LightCapabilities::default()
        }),
        driver_id: "litra".into(),
        registry_model_id: Some("8c900".into()),
    };
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::with_runtime(
        Config::default(),
        &[],
        &[light],
        &AssetResolver::new(),
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );
    let key = state
        .current_record()
        .expect("light record")
        .config_key
        .clone();
    let requested = LightSettings::new(false, 50, None);
    state.commit_light(requested);
    let Ok(crate::services::ipc::Command::SetLight(
        _,
        openlogi_core::hid::LightCommand::Power(false),
        _,
        request_id,
    )) = receiver.try_recv()
    else {
        panic!("expected the power command");
    };
    let Ok(crate::services::ipc::Command::SetLight(
        _,
        openlogi_core::hid::LightCommand::BrightnessPercent(50),
        _,
        brightness_request_id,
    )) = receiver.try_recv()
    else {
        panic!("expected the brightness command");
    };
    assert_eq!(brightness_request_id, request_id);
    assert_eq!(state.light(), requested);
    assert_eq!(state.config.light(&key), None);
    assert!(matches!(
        state.light_command_status(),
        Some(LightCommandStatus::Pending)
    ));
    assert!(state.apply_light_command_result(
        key.clone(),
        request_id,
        openlogi_core::hid::LightCommand::Power(false),
        Ok(()),
    ));
    assert_eq!(state.light(), requested);
    assert!(state.apply_light_command_result(
        key.clone(),
        request_id,
        openlogi_core::hid::LightCommand::BrightnessPercent(50),
        Err(WriteError::AmbiguousRawDevice),
    ));
    assert!(matches!(
        state.light_command_status(),
        Some(LightCommandStatus::Failed(message)) if message.contains("multiple raw HID")
    ));
    assert_eq!(
        state.light(),
        LightSettings::new(false, LightSettings::default().brightness_percent, None)
    );
    assert_eq!(state.config.light(&key), Some(state.light()));
}

#[test]
fn superseded_light_write_keeps_prior_successes_for_reconciliation() {
    let light = superseded_litra_light();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::with_runtime(
        Config::default(),
        &[],
        &[light],
        &AssetResolver::new(),
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );
    let key = state
        .current_record()
        .expect("light record")
        .config_key
        .clone();

    state.commit_light(LightSettings::new(false, 40, None));
    let (first_power, first_request_id) = next_light_command(&mut receiver);
    let (first_brightness, first_brightness_request_id) = next_light_command(&mut receiver);
    assert_eq!(first_power, openlogi_core::hid::LightCommand::Power(false));
    assert_eq!(
        first_brightness,
        openlogi_core::hid::LightCommand::BrightnessPercent(40)
    );
    assert_eq!(first_brightness_request_id, first_request_id);

    state.commit_light(LightSettings::new(true, 60, None));
    let (second_power, second_request_id) = next_light_command(&mut receiver);
    let (second_brightness, second_brightness_request_id) = next_light_command(&mut receiver);
    assert_eq!(second_power, openlogi_core::hid::LightCommand::Power(true));
    assert_eq!(
        second_brightness,
        openlogi_core::hid::LightCommand::BrightnessPercent(60)
    );
    assert_ne!(second_request_id, first_request_id);
    assert_eq!(second_brightness_request_id, second_request_id);

    assert!(state.apply_light_command_result(
        key.clone(),
        second_request_id,
        openlogi_core::hid::LightCommand::Power(true),
        Ok(()),
    ));
    assert!(state.apply_light_command_result(
        key.clone(),
        second_request_id,
        openlogi_core::hid::LightCommand::BrightnessPercent(60),
        Err(WriteError::AmbiguousRawDevice),
    ));
    assert_eq!(state.light(), LightSettings::new(true, 60, None));
    assert!(matches!(
        state.light_command_status(),
        Some(LightCommandStatus::Pending)
    ));

    assert!(state.apply_light_command_result(
        key.clone(),
        first_request_id,
        openlogi_core::hid::LightCommand::Power(false),
        Ok(()),
    ));
    assert!(state.apply_light_command_result(
        key.clone(),
        first_request_id,
        openlogi_core::hid::LightCommand::BrightnessPercent(40),
        Ok(()),
    ));

    assert!(matches!(
        state.light_command_status(),
        Some(LightCommandStatus::Failed(message)) if message.contains("multiple raw HID")
    ));
    assert_eq!(state.light(), LightSettings::new(true, 40, None));
    assert_eq!(state.config.light(&key), Some(state.light()));
}

#[test]
fn transient_light_state_is_kept_in_memory_and_only_supported_commands_are_sent() {
    let light = StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "id:session-node".into(),
        },
        display_name: "Brightness-only light".into(),
        manufacturer: Some("Test".into()),
        serial_number: None,
        unit_id: [0; 4],
        kind: DeviceKind::Light,
        online: true,
        capabilities: None,
        light_capabilities: Some(LightCapabilities {
            power: false,
            brightness: Some(
                LightValueRange::new(0, 100, 1, LightValueUnit::Percent).expect("valid range"),
            ),
            ..LightCapabilities::default()
        }),
        driver_id: "test-light".into(),
        registry_model_id: None,
    };
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::with_runtime(
        Config::default(),
        &[],
        &[light],
        &AssetResolver::new(),
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );
    let settings = LightSettings::new(false, 37, None);

    state.commit_light(settings);

    assert_eq!(state.light(), settings);
    assert!(!state.light_enabled());
    assert!(matches!(
        receiver.try_recv(),
        Ok(crate::services::ipc::Command::SetLight(
            _,
            openlogi_core::hid::LightCommand::BrightnessPercent(37),
            _,
            _
        ))
    ));
    assert!(receiver.try_recv().is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn camera_automation_preserves_manual_power_and_clears_transient_override() {
    let light = StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "serial:glow-camera".into(),
        },
        display_name: "Litra Glow".into(),
        manufacturer: Some("Logi".into()),
        serial_number: Some("glow-camera".into()),
        unit_id: [0; 4],
        kind: DeviceKind::Light,
        online: true,
        capabilities: None,
        light_capabilities: Some(LightCapabilities {
            power: true,
            ..LightCapabilities::default()
        }),
        driver_id: "litra".into(),
        registry_model_id: Some("8c900".into()),
    };
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::with_runtime(
        Config::default(),
        &[],
        &[light],
        &AssetResolver::new(),
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );
    let key = state
        .current_record()
        .expect("light record")
        .config_key
        .clone();
    state.config.set_light(
        &key,
        LightSettings {
            enabled: false,
            auto_camera: true,
            brightness_percent: 70,
            temperature_kelvin: None,
            color: None,
        },
    );

    assert!(!state.light_enabled());
    assert!(state.set_camera_active(true));
    assert!(state.light_enabled());
    assert!(!state.light().enabled);

    state.commit_manual_light_power(false);
    assert!(!state.light_enabled());
    assert!(matches!(
        receiver.try_recv(),
        Ok(crate::services::ipc::Command::SetLightManualPower(
            _,
            false,
            _,
            _
        ))
    ));

    assert!(state.set_camera_active(false));
    assert!(state.set_camera_active(true));
    assert!(state.light_enabled());
    assert!(!state.light().enabled);
}

#[cfg(target_os = "macos")]
#[test]
fn enabling_camera_automation_queues_effective_camera_power() {
    let light = StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "serial:glow-effective".into(),
        },
        display_name: "Litra Glow".into(),
        manufacturer: Some("Logi".into()),
        serial_number: Some("glow-effective".into()),
        unit_id: [0; 4],
        kind: DeviceKind::Light,
        online: true,
        capabilities: None,
        light_capabilities: Some(LightCapabilities {
            power: true,
            ..LightCapabilities::default()
        }),
        driver_id: "litra".into(),
        registry_model_id: Some("8c900".into()),
    };
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::with_runtime(
        Config::default(),
        &[],
        &[light],
        &AssetResolver::new(),
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );
    state.set_camera_active(true);
    let mut settings = state.light();
    settings.enabled = false;
    settings.auto_camera = true;

    state.commit_light(settings);

    assert!(matches!(
        receiver.try_recv(),
        Ok(crate::services::ipc::Command::SetLight(
            _,
            openlogi_core::hid::LightCommand::Power(true),
            _,
            _
        ))
    ));
    assert!(!state.light().enabled);
    assert!(state.light_enabled());
}

#[test]
fn gesture_maps_cover_every_gesture_mode_button() {
    // With per-button gesture mode, the GUI's display maps carry one entry
    // per gesture-mode button: the dedicated button's seeded default map
    // plus a promoted OS-hook button's stored map — simultaneously.
    use openlogi_core::binding::{ButtonId, GestureDirection};

    let mut config = Config::default();
    config.set_device_identity(
        "2b042",
        DeviceIdentity {
            display_name: "MX Master 4".to_string(),
            kind: DeviceKind::Mouse,
            capabilities: Capabilities::presumed_from_kind(DeviceKind::Mouse),
            light_capabilities: None,
            model_info: None,
            codename: None,
            driver_id: None,
            registry_model_id: None,
        },
    );
    config.set_gesture_mode("2b042", ButtonId::Back, true);
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::with_runtime(
        config,
        &[],
        &[],
        &AssetResolver::new(),
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );

    let maps = state.current_gesture_maps();
    let dedicated = maps
        .get(&ButtonId::GestureButton)
        .expect("the dedicated button's default gesture mode must be shown");
    assert!(
        dedicated.contains_key(&GestureDirection::Up),
        "HID++ maps are shown seeded, matching watcher dispatch"
    );
    assert!(
        maps.contains_key(&ButtonId::Back),
        "a promoted OS-hook button gets its own menu simultaneously"
    );
}

/// A battery reading that changed on an otherwise identical device must reach
/// the device list. The old guard compared nine hand-picked fields and
/// `battery` was not among them, so the rebuilt list — carrying the fresh
/// percentage — was discarded and every battery readout in the UI (gallery
/// card, detail page, native Device menu) stayed frozen until some *other*
/// compared field happened to move.
#[test]
fn a_battery_only_change_reaches_the_device_list() {
    let cache = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let unit_id = [1, 2, 3, 4];
    let mut state = AppState::with_runtime(
        Config::ephemeral(),
        &[inventory_with_battery(unit_id, 50)],
        &[],
        &cache,
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );
    assert_eq!(
        state.device_list[0].battery.as_ref().map(|b| b.percentage),
        Some(50)
    );

    let changed =
        state.refresh_inventories(&[inventory_with_battery(unit_id, 40)], &[], &cache, &[]);

    assert!(changed, "a battery change is a change");
    assert_eq!(
        state.device_list[0].battery.as_ref().map(|b| b.percentage),
        Some(40),
        "the fresh reading must replace the stale one"
    );
}

/// The guard still exists: an identical snapshot is a no-op, so quiet cycles
/// cost no window refresh. Without this the previous test could be satisfied
/// by simply always returning `true`.
#[test]
fn an_identical_snapshot_is_still_a_no_op() {
    let cache = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let unit_id = [1, 2, 3, 4];
    let mut state = AppState::with_runtime(
        Config::ephemeral(),
        &[inventory_with_battery(unit_id, 50)],
        &[],
        &cache,
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );

    assert!(!state.refresh_inventories(&[inventory_with_battery(unit_id, 50)], &[], &cache, &[]));
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
