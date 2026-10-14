//! Easy-Switch membership stays explicit, device-scoped, and rollback-safe.

use super::*;
use crate::services::ipc::Command;

const KEYBOARD: &str = "unit:4f4c4402";
const MOUSE: &str = "unit:4f4c4401";
const OTHER_MOUSE: &str = "unit:4f4c4403";

fn host_switch_state(
    persistence: ConfigPersistence,
) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let profile: DeviceProfile =
        serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).expect("canonical profile parses");
    let mut inventories = profile.inventories;
    let mut other_keyboard = inventories[0].paired[2].clone();
    other_keyboard.slot = 4;
    other_keyboard.model_info.as_mut().unwrap().unit_id = [0x4f, 0x4c, 0x44, 0x05];
    inventories[0].paired.push(other_keyboard);
    inventories.push(direct_inventory([0; 4]));
    let mut config = Config::ephemeral();
    config.set_selected_device(Some(KEYBOARD.into()));
    config.set_binding(KEYBOARD, ButtonId::Back, Binding::Single(Action::Copy));
    config.set_fn_lock(KEYBOARD, true);
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &inventories,
        persistence,
        ..Sources::in_memory(config, &resolver, commands)
    });
    (state, receiver)
}

fn drain(receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>) -> Vec<Command> {
    std::iter::from_fn(|| receiver.try_recv().ok()).collect()
}

#[test]
fn easy_switch_targets_are_independent_validated_and_rollback_safe() {
    let (mut state, mut receiver) = host_switch_state(ConfigPersistence::MemoryOnly);
    drain(&mut receiver);
    assert_eq!(state.current_record().unwrap().config_key, KEYBOARD);
    assert!(state.current_host_switch_targets().is_empty());
    assert_eq!(state.current_host_switch_candidates().count(), 2);
    let source_device = state.current_record().unwrap().device_key();
    let original = state.config.devices[KEYBOARD].clone();
    let ephemeral = state
        .devices()
        .iter()
        .find(|record| !record.is_persistent())
        .expect("all-zero direct identity stays ephemeral")
        .config_key
        .clone();
    let unknown_pointer = state
        .devices()
        .iter()
        .find(|record| record.kind == DeviceKind::Mouse && record.capabilities.is_none())
        .expect("offline unprobed mouse")
        .config_key
        .clone();
    for invalid in [
        "missing",
        KEYBOARD,
        "unit:4f4c4405",
        "unit:4f4c4404",
        &ephemeral,
        &unknown_pointer,
    ] {
        assert!(state.commit_host_switch_target(invalid, true).is_empty());
    }
    assert!(state.current_host_switch_targets().is_empty());
    assert!(drain(&mut receiver).is_empty());

    let events = state.commit_host_switch_target(MOUSE, true);
    assert!(events.contains(&StateEvent::DeviceConfigChanged(source_device.clone())));
    assert_eq!(state.current_host_switch_targets(), &[MOUSE.to_string()]);
    drain(&mut receiver);
    assert!(state.commit_host_switch_target(MOUSE, true).is_empty());
    assert!(drain(&mut receiver).is_empty());
    let _ = state.commit_host_switch_target(OTHER_MOUSE, true);
    let _ = state.commit_host_switch_target(MOUSE, false);
    let mut expected = original.clone();
    expected.host_switch_targets = vec![OTHER_MOUSE.into()];
    assert_eq!(state.config.devices[KEYBOARD], expected);

    // Losing a probe or route cannot silently unlink a saved target.
    let record = state
        .devices
        .records
        .iter_mut()
        .find(|record| record.config_key == OTHER_MOUSE)
        .unwrap();
    record.online = false;
    assert!(
        state
            .current_host_switch_candidates()
            .any(|record| record.config_key == OTHER_MOUSE)
    );
    state
        .devices
        .records
        .retain(|record| record.config_key != OTHER_MOUSE);
    assert_eq!(
        state.current_host_switch_targets(),
        &[OTHER_MOUSE.to_string()]
    );
    assert!(
        state
            .commit_host_switch_target(OTHER_MOUSE, false)
            .contains(&StateEvent::DeviceConfigChanged(source_device))
    );
    assert_eq!(state.config.devices[KEYBOARD], original);

    // A non-keyboard or ephemeral keyboard cannot own a persistent link.
    let mouse_index = state
        .devices()
        .iter()
        .position(|record| record.config_key == MOUSE)
        .unwrap();
    let _ = state.select_device(mouse_index);
    assert!(state.current_host_switch_candidates().next().is_none());
    assert!(state.commit_host_switch_target(MOUSE, true).is_empty());

    let (mut read_only, mut receiver) =
        host_switch_state(ConfigPersistence::ReadOnly("read-only".into()));
    drain(&mut receiver);
    let original = read_only.config.devices[KEYBOARD].clone();
    let _ = read_only.commit_host_switch_target(MOUSE, true);
    assert!(read_only.current_host_switch_targets().is_empty());
    assert_eq!(read_only.config.devices[KEYBOARD], original);
    assert!(drain(&mut receiver).is_empty());
    assert_eq!(read_only.config_issue(), Some("read-only"));
}
