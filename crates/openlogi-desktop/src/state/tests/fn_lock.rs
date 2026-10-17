//! Fn-lock: the toggle persists the setting, writes the keyboard through the agent, and shows what it echoes.

use super::*;
use crate::services::ipc::{Command, SetFnLock};
use openlogi_core::hid::FnLockState;

/// An MX Keys Mini on a Bolt receiver, with or without an fn-inversion feature.
pub(super) fn keyboard_inventory(fn_lock: bool) -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "Bolt Receiver".to_string(),
            vendor_id: 0x046d,
            product_id: 0xc548,
            unique_id: Some("OL-BOLT-0001".to_string()),
        },
        paired: vec![PairedDevice {
            slot: 3,
            codename: Some("MX Keys Mini".to_string()),
            wpid: None,
            kind: DeviceKind::Keyboard,
            online: true,
            battery: None,
            settings_revision: 0,
            model_info: Some(DeviceModelInfo {
                entity_count: 1,
                serial_number: None,
                unit_id: [0x4f, 0x4c, 0x44, 0x02],
                transports: DeviceTransports::default(),
                model_ids: [0xb369, 0, 0],
                extended_model_id: 0,
            }),
            capabilities: Some(Capabilities {
                fn_lock,
                ..Capabilities::presumed_from_kind(DeviceKind::Keyboard)
            }),
        }],
    }
}

fn keyboard_state(fn_lock: bool) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[keyboard_inventory(fn_lock)],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    (state, receiver)
}

pub(super) fn drain(receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>) -> Vec<Command> {
    std::iter::from_fn(|| receiver.try_recv().ok()).collect()
}

#[test]
fn fn_lock_toggle_persists_the_setting_and_writes_the_keyboard_directly() {
    let (mut state, mut receiver) = keyboard_state(true);
    drain(&mut receiver);
    let record = state.current_record().expect("the keyboard is selected");
    let key = record.device_key();
    let route = record.route.clone().expect("a Bolt route");
    let config_key = record
        .persistent_config_key()
        .expect("a unit id makes the keyboard persistent")
        .to_string();

    let events = state.commit_fn_lock(true);

    assert_eq!(events, [StateEvent::FnLockChanged(key.clone())]);
    assert_eq!(state.config.fn_lock(&config_key), Some(true));
    // Nothing has been read from the keyboard yet, so the toggle shows the
    // setting it just wrote rather than the pre-write default.
    assert!(state.current_fn_lock_shown());
    let commands = drain(&mut receiver);
    let [
        Command::ReloadConfig(_),
        Command::SetFnLock(SetFnLock {
            route: written_route,
            fn_lock,
            key: written_key,
        }),
    ] = commands.as_slice()
    else {
        panic!("expected a config reload followed by one Fn-lock write");
    };
    assert_eq!(*written_route, route);
    assert!(*fn_lock);
    assert_eq!(*written_key, key);
}

#[test]
fn fn_lock_toggle_is_inert_on_a_keyboard_without_the_control() {
    let (mut state, mut receiver) = keyboard_state(false);
    drain(&mut receiver);
    let config_key = state
        .current_record()
        .and_then(DeviceRecord::persistent_config_key)
        .expect("persistent")
        .to_string();

    let _ = state.commit_fn_lock(true);

    assert_eq!(state.config.fn_lock(&config_key), None);
    assert!(drain(&mut receiver).is_empty());
}

#[test]
fn the_keyboards_answer_repaints_the_device_tab_either_way() {
    let (mut state, _receiver) = keyboard_state(true);
    let key = state.current_record().expect("keyboard").device_key();
    let echoed = FnLockState {
        fn_lock: true,
        default_fn_lock: Some(false),
    };

    assert_eq!(
        state.apply_fn_lock_written(&key, Ok(echoed)),
        [StateEvent::FnLockChanged(key.clone())]
    );
    assert_eq!(
        state.apply_fn_lock_written(&key, Err(WriteError::AgentUnavailable)),
        [StateEvent::FnLockChanged(key)]
    );
}

#[test]
fn a_fn_lock_toggled_on_the_keyboard_marks_it_for_a_re_read() {
    use crate::state::inventory::settings_moved;

    let resolver = AssetResolver::new();
    let (mut state, _receiver) = keyboard_state(true);
    let before = state.devices().to_vec();
    let key = before[0].device_key();
    let mut toggled = keyboard_inventory(true);
    toggled.paired[0].settings_revision = 1;

    let events = state.refresh_inventories(&[toggled.clone()], &[], &resolver, &[]);

    // The revision alone is a change worth a refresh: the shown Fn lock is
    // no longer known to be the keyboard's.
    assert_eq!(events, [StateEvent::InventoryChanged]);
    assert_eq!(state.devices()[0].settings_revision, 1);
    assert_eq!(settings_moved(&before, state.devices()), [key]);
    // The same snapshot again changes nothing.
    assert!(
        state
            .refresh_inventories(&[toggled], &[], &resolver, &[])
            .is_empty()
    );
}

#[test]
fn only_a_device_still_on_its_route_is_re_read_for_a_moved_revision() {
    use crate::state::inventory::settings_moved;

    let (state, _receiver) = keyboard_state(true);
    let old = state.devices().to_vec();
    let moved = |edit: fn(&mut DeviceRecord)| {
        let mut new = old.clone();
        edit(&mut new[0]);
        settings_moved(&old, &new)
    };

    assert!(moved(|_| {}).is_empty(), "nothing changed");
    assert_eq!(
        moved(|record| record.settings_revision += 1),
        [old[0].device_key()]
    );
    // Another route counts from its own channel, and the reroute path
    // drops the device's reads instead.
    assert!(
        moved(|record| {
            record.settings_revision += 1;
            record.route = None;
        })
        .is_empty()
    );
    // A device that was not in the previous snapshot has nothing to re-read.
    assert!(
        moved(|record| {
            record.settings_revision += 1;
            record.config_key.push_str("-other");
        })
        .is_empty()
    );
}
