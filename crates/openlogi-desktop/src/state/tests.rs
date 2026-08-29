//! AppState unit tests.

use std::collections::BTreeMap;
use std::sync::Arc;

use openlogi_camera::Camera;
use openlogi_core::binding::{
    Action, ActionRingIcon, ActionRingSlot, Binding, ButtonId, RingAction,
};
use openlogi_core::config::{
    Config, DeviceIdentity, LightSettings, Lighting, ScrollResolution, ThumbwheelSensitivity,
    VerticalScrollSensitivity,
};
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, Capabilities, DeviceInventory, DeviceKind,
    DeviceModelInfo, DeviceTransports, LightCapabilities, LightValueRange, LightValueUnit,
    PairedDevice, RawDeviceAddress, ReceiverInfo, StandaloneDevice,
};
use openlogi_core::hid::{
    Dpi, SmartShiftAutoDisengage, SmartShiftMode, SmartShiftStatus, SmartShiftThreshold, WriteError,
};

use gpui::AppContext as _;
use openlogi_core::app::ForegroundApp;
use openlogi_ipc::ForegroundApps;
#[cfg(target_os = "macos")]
use openlogi_ipc::{PrimaryMouseButton, SystemMouseSettingError};

use crate::features::mouse::thumbwheel::ThumbwheelPreset;
use crate::services::assets::AssetResolver;
#[cfg(target_os = "macos")]
use crate::services::ipc::{Command, PrimaryMouseButtonCommandError};

use super::bindings::apply_thumbwheel_pair;
use super::devices::build_device_list;
use super::scroll::set_scroll_resolution_if_supported;
use super::smartshift::{
    ConfirmationOutcome, SmartShiftDeviceState, smartshift_read_is_current,
    smartshift_write_outcome,
};
use super::{AppState, ConfigPersistence, LightCommandStatus, Load};

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
    state.set_smooth_scroll(true);
    state.set_vertical_scroll_sensitivity(VerticalScrollSensitivity::from_rounded(7.0));

    assert_eq!(
        state.app_settings().thumbwheel_sensitivity,
        ThumbwheelSensitivity::DEFAULT
    );
    assert!(!state.app_settings().smooth_scroll);
    assert_eq!(
        state.app_settings().vertical_scroll_sensitivity,
        VerticalScrollSensitivity::DEFAULT
    );
    assert_eq!(state.config_issue(), Some("invalid config"));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn smooth_scroll_change_reloads_the_agent_once() {
    let cache = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::with_runtime(
        Config::ephemeral(),
        &[],
        &[],
        &cache,
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );

    state.set_smooth_scroll(true);

    assert!(state.app_settings().smooth_scroll);
    assert!(matches!(
        receiver.try_recv(),
        Ok(crate::services::ipc::Command::ReloadConfig)
    ));

    state.set_smooth_scroll(true);
    assert!(receiver.try_recv().is_err());
}

/// A live language switch runs inside `AppState::update`, and the menu rebuild
/// it schedules reads the same entity (the Device menu lists devices). Rebuilt
/// synchronously that read is re-entrant and panics ("cannot read … while it is
/// already being updated"), which crashed 0.8.0 on every language change —
/// `set_language` must defer the rebuild until the update returns the lease.
#[gpui::test]
fn language_switch_rebuilds_menus_after_the_state_update(cx: &mut gpui::TestAppContext) {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock();
    let cache = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::with_runtime(
        Config::ephemeral(),
        &[],
        &[],
        &cache,
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );

    cx.update(|cx| {
        AppState::set_global(cx.new(|_| state), cx);
        AppState::update(cx, |state, cx| {
            state.set_language(Some("zh-CN".into()), cx);
        });
    });

    cx.read(|cx| {
        assert_eq!(
            AppState::try_read(cx).and_then(AppState::language),
            Some("zh-CN")
        );
    });
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

#[cfg(target_os = "macos")]
#[test]
fn primary_mouse_button_failure_stays_visible_without_overwriting_the_snapshot() {
    let cache = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::with_runtime(
        Config::ephemeral(),
        &[],
        &[],
        &cache,
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );
    state.set_primary_mouse_button(Some(PrimaryMouseButton::Left));

    assert!(state.request_primary_mouse_button(PrimaryMouseButton::Right));
    assert!(state.primary_mouse_button_pending());
    assert!(state.primary_mouse_button_error().is_none());
    assert!(matches!(
        receiver.try_recv(),
        Ok(Command::SetPrimaryMouseButton(PrimaryMouseButton::Right))
    ));

    let error = PrimaryMouseButtonCommandError::Rejected(SystemMouseSettingError::Unavailable {
        message: "persistent write rejected".into(),
    });
    assert!(state.apply_primary_mouse_button_result(Err(error.clone())));
    assert!(!state.primary_mouse_button_pending());
    assert_eq!(state.primary_mouse_button_error(), Some(error.clone()));
    assert_eq!(
        state.primary_mouse_button(),
        Some(PrimaryMouseButton::Left),
        "a command result must not replace the agent's authoritative snapshot"
    );
    assert!(state.set_primary_mouse_button(Some(PrimaryMouseButton::Right)));
    assert_eq!(
        state.primary_mouse_button_error(),
        Some(error),
        "a matching snapshot must not hide a definitive platform rejection"
    );
    assert!(state.set_primary_mouse_button(Some(PrimaryMouseButton::Left)));

    assert!(state.request_primary_mouse_button(PrimaryMouseButton::Right));
    assert!(state.primary_mouse_button_error().is_none());
    assert!(state.apply_primary_mouse_button_result(Ok(PrimaryMouseButton::Right)));
    assert_eq!(
        state.primary_mouse_button(),
        Some(PrimaryMouseButton::Left),
        "even success waits for the observed snapshot to move the switch"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn primary_mouse_button_snapshot_reconciles_an_ambiguous_transport_failure() {
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
    state.set_primary_mouse_button(Some(PrimaryMouseButton::Left));

    assert!(state.request_primary_mouse_button(PrimaryMouseButton::Right));
    assert!(
        state.apply_primary_mouse_button_result(Err(
            PrimaryMouseButtonCommandError::AgentUnavailable,
        ))
    );
    assert_eq!(
        state.primary_mouse_button_error(),
        Some(PrimaryMouseButtonCommandError::AgentUnavailable)
    );
    assert!(state.set_primary_mouse_button(Some(PrimaryMouseButton::Right)));
    assert!(state.primary_mouse_button_error().is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn primary_mouse_button_snapshot_before_lost_reply_also_confirms_the_write() {
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
    state.set_primary_mouse_button(Some(PrimaryMouseButton::Left));

    assert!(state.request_primary_mouse_button(PrimaryMouseButton::Right));
    assert!(state.set_primary_mouse_button(Some(PrimaryMouseButton::Right)));
    assert!(
        state.apply_primary_mouse_button_result(Err(
            PrimaryMouseButtonCommandError::AgentUnavailable,
        ))
    );
    assert!(state.primary_mouse_button_error().is_none());
}

/// Config key of the mouse [`direct_inventory`] builds with a real unit id.
///
/// The transport-free identity, not the `direct:046d:b023:…` route it is
/// reached on: a device whose unit id is known resolves to its identity key,
/// which is what settings are now written under.
const KNOWN_MOUSE_KEY: &str = "unit:a393cae0";

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

/// A second, unmistakably different mouse, so a test can change the active device.
fn second_mouse_inventory() -> DeviceInventory {
    let mut inventory = direct_inventory([0x11, 0x22, 0x33, 0x44]);
    inventory.receiver.name = "MX Anywhere 3S".to_string();
    inventory.receiver.product_id = 0xb037;
    inventory
}

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
    let cache = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut config = Config::ephemeral();
    config.set_dpi("receiver:82839805:slot:1", Dpi::new(3200));
    let mut state = AppState::with_runtime(
        config,
        &[],
        &[],
        &cache,
        &[],
        ConfigPersistence::ReadOnly("simulated unwritable config.toml".to_string()),
        commands,
    );
    assert!(state.devices().is_empty(), "no inventory seen yet");

    let changed = state.refresh_inventories(&[receiver_inventory()], &[], &cache, &[]);

    assert!(
        !changed,
        "a failed fold-persist must not report a change — a caller \
         acting on `true` would treat the now-discarded `merged_list` as live"
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

const CAMERA_A_ID: &str = "0x1123000046d0893";
const CAMERA_B_ID: &str = "0x14110000046d0893";

fn serial_less_same_model_cameras() -> [Camera; 2] {
    let first = Camera {
        name: "Logitech StreamCam".to_string(),
        unique_id: CAMERA_A_ID.to_string(),
        serial_number: None,
        vendor_id: 0x046d,
        product_id: 0x0893,
        max_resolution: None,
        max_fps: None,
    };
    let second = Camera {
        unique_id: CAMERA_B_ID.to_string(),
        ..first.clone()
    };
    [first, second]
}

fn state_with_same_model_cameras(config: Config) -> AppState {
    let cameras = serial_less_same_model_cameras();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    AppState::with_runtime(
        config,
        &[],
        &[],
        &AssetResolver::new(),
        &cameras,
        ConfigPersistence::MemoryOnly,
        commands,
    )
}

fn camera_record<'a>(state: &'a AppState, capture_id: &str) -> &'a super::DeviceRecord {
    state
        .devices()
        .iter()
        .find(|record| record.capture_id.as_deref() == Some(capture_id))
        .expect("camera record")
}

#[test]
fn custom_device_name_updates_the_ui_and_can_restore_the_model_name() {
    let mut state = state_with_a_known_mouse();
    let model_name = state
        .current_record()
        .expect("known mouse")
        .model_name
        .clone();

    state.set_device_custom_name(KNOWN_MOUSE_KEY, "  Office mouse  ");

    assert_eq!(
        state
            .current_record()
            .map(|record| record.display_name.as_str()),
        Some("Office mouse")
    );
    assert_eq!(
        state.config.device_custom_name(KNOWN_MOUSE_KEY),
        Some("Office mouse")
    );

    state.set_device_custom_name(KNOWN_MOUSE_KEY, "   ");

    assert_eq!(
        state
            .current_record()
            .map(|record| record.display_name.as_str()),
        Some(model_name.as_str())
    );
    assert_eq!(state.config.device_custom_name(KNOWN_MOUSE_KEY), None);
}

#[test]
fn same_model_serial_less_cameras_keep_independent_names() {
    let mut state = state_with_same_model_cameras(Config::ephemeral());
    let second_key = camera_record(&state, CAMERA_B_ID).record_key();

    state.set_device_custom_name(&second_key, "Desk camera");

    assert_eq!(
        camera_record(&state, CAMERA_A_ID).display_name,
        "Logitech StreamCam"
    );
    assert_eq!(
        camera_record(&state, CAMERA_B_ID).display_name,
        "Desk camera"
    );

    let restored = state_with_same_model_cameras(state.config.clone());
    assert_eq!(
        camera_record(&restored, CAMERA_A_ID).display_name,
        "Logitech StreamCam"
    );
    assert_eq!(
        camera_record(&restored, CAMERA_B_ID).display_name,
        "Desk camera"
    );
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

fn ring_action(action: Action) -> RingAction {
    RingAction::new(action).expect("test action must be valid in the Actions Ring")
}

#[test]
fn an_unsaved_action_ring_profile_inherits_default_until_its_first_edit() {
    let mut state = state_with_a_known_mouse();
    let inherited = state.current_action_ring_layout();

    state.set_editing_action_ring_app(Some("com.apple.Safari".into()));

    assert_eq!(state.current_action_ring_layout(), inherited);
    assert!(
        state.current_action_ring().per_app.is_empty(),
        "selecting an application must not persist an unchanged layout"
    );

    state.commit_action_ring_slot(ActionRingSlot::Top, Some(ring_action(Action::NewTab)));

    let ring = state.current_action_ring();
    let safari = ring
        .per_app
        .get("com.apple.Safari")
        .expect("the first edit creates the application layout");
    assert_eq!(
        ring.default, inherited,
        "the default layout stays unchanged"
    );
    assert_eq!(safari.slots[&ActionRingSlot::Top].action(), &Action::NewTab);
    assert_eq!(
        safari.slots[&ActionRingSlot::Bottom],
        inherited.slots[&ActionRingSlot::Bottom],
        "the application profile starts as a complete copy of Default"
    );
}

#[test]
fn an_action_ring_icon_edit_targets_the_open_application_layout() {
    let mut state = state_with_a_known_mouse();
    state.set_editing_action_ring_app(Some("com.apple.Safari".into()));

    state.commit_action_ring_icon(ActionRingSlot::Top, Some(ActionRingIcon::Keyboard));

    let ring = state.current_action_ring();
    assert_eq!(ring.default.slots[&ActionRingSlot::Top].custom_icon(), None);
    assert_eq!(
        ring.per_app["com.apple.Safari"].slots[&ActionRingSlot::Top].custom_icon(),
        Some(ActionRingIcon::Keyboard)
    );
}

#[test]
fn removing_an_action_ring_profile_leaves_button_overrides_untouched() {
    let mut state = state_with_a_known_mouse();
    state.set_editing_app(Some("com.apple.Safari".into()));
    state.commit_binding(ButtonId::Back, Action::Undo);
    state.set_editing_action_ring_app(Some("com.apple.Safari".into()));
    state.commit_action_ring_slot(ActionRingSlot::Top, Some(ring_action(Action::NewTab)));

    state.remove_editing_action_ring_profile();

    assert_eq!(state.editing_action_ring_app(), None);
    assert!(state.current_action_ring().per_app.is_empty());
    assert_eq!(
        state
            .config
            .per_app_overrides(KNOWN_MOUSE_KEY, "com.apple.Safari"),
        Some(&BTreeMap::from([(ButtonId::Back, Action::Undo)]))
    );
    assert_eq!(
        state.editing_app(),
        Some("com.apple.Safari"),
        "the Buttons editor keeps its independent scope"
    );
}

#[test]
fn clearing_an_override_falls_back_to_the_global_binding() {
    let mut state = state_with_a_known_mouse();
    state.commit_binding(ButtonId::Back, Action::Copy);
    state.set_editing_app(Some("com.apple.Safari".into()));
    state.commit_binding(ButtonId::Back, Action::Undo);
    assert_eq!(
        state.button_bindings().get(&ButtonId::Back),
        Some(&Action::Undo)
    );

    state.clear_app_binding(ButtonId::Back);

    assert_eq!(
        state.button_bindings().get(&ButtonId::Back),
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
        state.button_bindings().get(&ButtonId::ThumbwheelScrollDown),
        Some(&Action::VolumeDown)
    );
    assert_eq!(
        state.button_bindings().get(&ButtonId::ThumbwheelScrollUp),
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
        state.gesture_bindings(),
        &global,
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
    // Overlays are per-device, so a scope must not follow the selection onto
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
        .devices()
        .iter()
        .position(|record| record.config_key != KNOWN_MOUSE_KEY)
        .expect("the fixture pairs a second device");
    let known = state
        .devices()
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
fn invalid_device_selection_preserves_the_valid_current_device() {
    let mut state = state_with_a_known_mouse();
    let selected = state.selected_device_index();

    assert_eq!(state.set_current_device(usize::MAX), None);
    assert_eq!(state.selected_device_index(), selected);
    assert!(state.current_record().is_some());
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

    state.config.edit(|config| {
        config.set_per_app_binding(
            KNOWN_MOUSE_KEY,
            "com.apple.Safari",
            ButtonId::Back,
            Some(Action::Undo),
        );
    });
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
    state.config.edit(|config| {
        config.set_per_app_binding(
            KNOWN_MOUSE_KEY,
            "com.apple.Safari",
            ButtonId::Back,
            Some(Action::Undo),
        );
    });
    state.set_foreground(ForegroundApps {
        current: Some(app(openlogi_core::brand::APP_ID, "OpenLogi")),
        recent: vec![app("com.apple.Safari", "Safari")],
    });

    assert_eq!(state.active_profile_name(), Some("Safari"));
}

#[test]
fn a_host_with_no_readable_foreground_app_reports_the_default_profile() {
    let mut state = state_with_a_known_mouse();
    state.config.edit(|config| {
        config.set_per_app_binding(
            KNOWN_MOUSE_KEY,
            "com.apple.Safari",
            ButtonId::Back,
            Some(Action::Undo),
        );
    });
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

    assert_eq!(state.devices().len(), 1);
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
    // The device's own unit id is known and online: the transport-free
    // identity key wins over the direct-route runtime key.
    assert_eq!(merged[0].config_key, "unit:a393cae0");
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
    // The device's own unit id is known and online: the transport-free
    // identity key wins over the direct-route runtime key.
    let stable_key = "unit:a393cae0";
    assert_eq!(state.devices()[0].config_key, stable_key);

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
    assert_eq!(merged[0].config_key, "unit:a393cae0");
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
    // The sibling's own unit id is known and online: the transport-free
    // identity key wins over the direct-route runtime key.
    let Some(sibling) = merged.iter().find(|r| r.config_key == "unit:02020202") else {
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
    assert_eq!(state.devices().len(), 2);

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
fn a_route_shared_by_two_online_twins_is_never_adopted() {
    // #482 corollary: `route_key` for a Direct route strips the device's own
    // identity, so two same-model direct devices online in the same
    // snapshot report the *same* route key. `Config::adopt_route` is
    // exclusive per route, so adopting it for either twin would just get it
    // stolen back by the other on the very next tick — a persist-and-reload
    // storm. The route cannot be attributed to either by route alone, so
    // neither claims it, and nothing is persisted or reloaded.
    let cache = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::with_runtime(
        Config::ephemeral(),
        &[],
        &[],
        &cache,
        &[],
        ConfigPersistence::MemoryOnly,
        commands,
    );

    state.refresh_inventories(
        &[
            direct_inventory([1, 1, 1, 1]),
            direct_inventory([2, 2, 2, 2]),
        ],
        &[],
        &cache,
        &[],
    );

    for key in ["unit:01010101", "unit:02020202"] {
        assert!(
            !state
                .config
                .devices
                .get(key)
                .is_some_and(|device| device.links.contains_key("direct:046d:b023")),
            "{key} must not claim a route its twin equally owns"
        );
    }
    assert!(
        receiver.try_recv().is_err(),
        "a route that was never adopted must not trigger a persist/reload"
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

    assert!(state.devices().is_empty());
    assert!(state.lighting_for(transient_key, transient_key).is_none());
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
        smartshift_write_outcome(expected, Some(&Load::Ready(Arc::new(expected)))),
        Some(ConfirmationOutcome::Confirmed)
    );
    assert_eq!(
        smartshift_write_outcome(
            expected,
            Some(&Load::Ready(Arc::new(SmartShiftStatus {
                auto_disengage: SmartShiftAutoDisengage::Threshold(
                    SmartShiftThreshold::from_rounded(13.0),
                ),
                ..expected
            }))),
        ),
        Some(ConfirmationOutcome::Failed)
    );
    assert_eq!(
        smartshift_write_outcome(
            expected,
            Some(&Load::<Arc<SmartShiftStatus>>::Failed(
                "timeout".to_string(),
            ))
        ),
        Some(ConfirmationOutcome::Failed)
    );
}

#[test]
fn stale_smartshift_reads_do_not_resolve_newer_writes() {
    let expected = SmartShiftStatus {
        mode: SmartShiftMode::Ratchet,
        auto_disengage: SmartShiftAutoDisengage::Threshold(SmartShiftThreshold::from_rounded(12.0)),
        tunable_torque: None,
    };
    let mut write = SmartShiftDeviceState::default();
    write.queue(expected, 2);

    assert!(
        !smartshift_read_is_current(Some(2), Some(&write)),
        "a tagged read cannot land before its confirmation request starts"
    );
    assert!(!smartshift_read_is_current(None, Some(&write)));
    assert_eq!(write.begin_confirmation(), Some(2));
    assert!(smartshift_read_is_current(Some(2), Some(&write)));
    assert!(!smartshift_read_is_current(Some(1), Some(&write)));
    assert!(!smartshift_read_is_current(None, Some(&write)));
    write.reset();
    assert!(!smartshift_read_is_current(Some(2), Some(&write)));
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
    state.config.edit(|config| {
        config.set_light(
            &key,
            LightSettings {
                enabled: false,
                auto_camera: true,
                brightness_percent: 70,
                temperature_kelvin: None,
                color: None,
            },
        );
    });

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
        state.devices()[0].battery.as_ref().map(|b| b.percentage),
        Some(50)
    );

    let changed =
        state.refresh_inventories(&[inventory_with_battery(unit_id, 40)], &[], &cache, &[]);

    assert!(changed, "a battery change is a change");
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
    AppState::with_runtime(
        config,
        &[],
        &[],
        &AssetResolver::new(),
        &[],
        persistence,
        commands,
    )
}

/// Forgetting an offline device removes both its placeholder card and its
/// persisted entry, so no later inventory refresh can reseed it.
#[test]
fn forgetting_an_offline_device_drops_its_card_and_config_entry() {
    let mut state = state_with_an_offline_identity(ConfigPersistence::MemoryOnly);
    assert_eq!(state.devices().len(), 1);
    let record_key = state.devices()[0].record_key();

    assert!(state.forget_device(&record_key));

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

    assert!(!state.forget_device(&record_key));
    assert_eq!(state.devices().len(), 1);
}

/// A save that cannot land keeps the card: the config store restores the
/// persisted revision and `forget_device` reports the failure, instead of the
/// card vanishing until the next refresh resurrects it.
#[test]
fn a_failed_save_keeps_the_forgotten_device() {
    let mut state = state_with_an_offline_identity(ConfigPersistence::ReadOnly("read-only".into()));
    let record_key = state.devices()[0].record_key();

    assert!(!state.forget_device(&record_key));

    assert_eq!(state.devices().len(), 1, "the card must stay");
    assert!(
        state
            .config
            .edit(|config| config.device_identity("2b034").is_some()),
        "the persisted entry must survive the failed save"
    );
}
