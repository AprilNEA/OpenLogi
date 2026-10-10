//! Onboard profiles: the toggle persists the choice, writes the device through the agent, and hands DPI back to the host's setting.

use super::*;
use std::sync::Arc;

use crate::services::ipc::{Command, SetOnboardProfiles};
use crate::state::load::OnboardProfilesLoad;
use crate::state::onboard_profiles::onboard_profiles_shown;
use openlogi_core::hid::Dpi;

/// A PRO X3 SUPERSTRIKE on its HID++ 2.0 receiver, with or without `0x8100`.
fn mouse_inventory(onboard_profiles: bool) -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "Lightspeed Receiver".to_string(),
            vendor_id: 0x046d,
            product_id: 0xc54f,
            unique_id: Some("695A8298".to_string()),
        },
        paired: vec![PairedDevice {
            slot: 1,
            codename: Some("PRO X3 SUPERSTRIKE".to_string()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: Some(DeviceModelInfo {
                entity_count: 7,
                serial_number: None,
                unit_id: [0x80, 0xb4, 0xe8, 0xc7],
                transports: DeviceTransports::default(),
                model_ids: [0x40be, 0xc0a9, 0],
                extended_model_id: 0,
            }),
            capabilities: Some(Capabilities {
                onboard_profiles,
                ..Capabilities::presumed_from_kind(DeviceKind::Mouse)
            }),
        }],
    }
}

fn mouse_state(
    onboard_profiles: bool,
) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[mouse_inventory(onboard_profiles)],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    (state, receiver)
}

fn drain(receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>) -> Vec<Command> {
    std::iter::from_fn(|| receiver.try_recv().ok()).collect()
}

fn config_key(state: &AppState) -> String {
    state
        .current_record()
        .and_then(DeviceRecord::persistent_config_key)
        .expect("a unit id makes the mouse persistent")
        .to_string()
}

#[test]
fn unset_onboard_profiles_show_the_factory_onboard_mode() {
    let (state, _receiver) = mouse_state(true);

    assert!(state.current_onboard_profiles_supported());
    assert_eq!(state.current_onboard_profiles_setting(), None);
    assert!(state.current_onboard_profiles_shown());
}

#[test]
fn onboard_toggle_persists_the_choice_and_writes_the_device_directly() {
    let (mut state, mut receiver) = mouse_state(true);
    drain(&mut receiver);
    let record = state.current_record().expect("the mouse is selected");
    let key = record.device_key();
    let route = record.route.clone().expect("a receiver route");
    let config_key = config_key(&state);

    let events = state.commit_onboard_profiles(false);

    assert_eq!(events, [StateEvent::OnboardProfilesChanged(key.clone())]);
    assert_eq!(state.config.onboard_profiles(&config_key), Some(false));
    assert!(!state.current_onboard_profiles_shown());
    let commands = drain(&mut receiver);
    let [
        Command::ReloadConfig(_),
        Command::SetOnboardProfiles(SetOnboardProfiles {
            route: written_route,
            onboard_profiles,
            key: written_key,
        }),
    ] = commands.as_slice()
    else {
        panic!("expected a config reload followed by one onboard-mode write");
    };
    assert_eq!(*written_route, route);
    assert!(!*onboard_profiles);
    assert_eq!(*written_key, key);
}

#[test]
fn onboard_toggle_is_inert_on_a_device_without_onboard_profiles() {
    let (mut state, mut receiver) = mouse_state(false);
    drain(&mut receiver);
    let config_key = config_key(&state);

    let _ = state.commit_onboard_profiles(false);

    assert_eq!(state.config.onboard_profiles(&config_key), None);
    assert!(drain(&mut receiver).is_empty());
}

#[test]
fn the_devices_reading_outranks_the_persisted_choice() {
    // A refused switch: the user chose host control, the device kept its
    // onboard profile. DPI controls must follow what the device reports.
    let onboard = OnboardProfilesLoad::Ready(Arc::new(true));
    assert!(onboard_profiles_shown(&onboard, Some(false)));
    assert!(!onboard_profiles_shown(
        &OnboardProfilesLoad::Ready(Arc::new(false)),
        Some(true)
    ));
    // Until a reading lands, the choice stands in, then the factory mode.
    assert!(!onboard_profiles_shown(
        &OnboardProfilesLoad::Loading,
        Some(false)
    ));
    assert!(onboard_profiles_shown(&OnboardProfilesLoad::Unknown, None));
}

#[test]
fn a_switch_holds_off_re_reads_until_the_device_answers() {
    let (mut state, _receiver) = mouse_state(true);
    let key = state.current_record().expect("mouse").device_key();

    let _ = state.commit_onboard_profiles(false);
    assert!(state.pointer.reads.onboard_profiles_write_pending(&key));

    let _ = state.apply_onboard_profiles_written(&key, Err(WriteError::AgentUnavailable));
    assert!(
        !state.pointer.reads.onboard_profiles_write_pending(&key),
        "a refused switch re-reads the device instead of trusting the request"
    );
}

#[test]
fn handing_control_to_the_host_shows_the_users_own_dpi_again() {
    let (mut state, _receiver) = mouse_state(true);
    let key = state.current_record().expect("mouse").device_key();
    let config_key = config_key(&state);
    state
        .config
        .edit(|config| config.set_dpi(&config_key, Dpi::new(3200)));
    state.pointer.dpi = Dpi::new(800);

    let events = state.apply_onboard_profiles_written(&key, Ok(false));

    assert_eq!(events, [StateEvent::OnboardProfilesChanged(key)]);
    assert_eq!(state.dpi(), Dpi::new(3200));
}

#[test]
fn a_refused_onboard_write_still_repaints_the_device_tab() {
    let (mut state, _receiver) = mouse_state(true);
    let key = state.current_record().expect("mouse").device_key();

    assert_eq!(
        state.apply_onboard_profiles_written(&key, Err(WriteError::AgentUnavailable)),
        [StateEvent::OnboardProfilesChanged(key)]
    );
}
