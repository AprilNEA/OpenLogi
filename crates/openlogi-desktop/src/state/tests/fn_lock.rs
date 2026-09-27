//! Saving a keyboard's Fn lock from the Keys tab, and ignoring other devices.

use super::*;
use crate::state::events::StateEvents;

fn state_with_a_known_keyboard() -> AppState {
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    let keyboard = &mut inventory.paired[0];
    keyboard.kind = DeviceKind::Keyboard;
    keyboard.capabilities = Some(Capabilities::presumed_from_kind(DeviceKind::Keyboard));
    AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    })
}

#[test]
fn fn_lock_is_saved_for_the_selected_keyboard() {
    let mut state = state_with_a_known_keyboard();
    let key = state.current_record().expect("a keyboard").device_key();

    let events = state.commit_fn_lock(true);

    assert_eq!(state.config.fn_lock(KNOWN_MOUSE_KEY), Some(true));
    assert_eq!(events, StateEvents::from(StateEvent::FnLockChanged(key)));

    let _ = state.commit_fn_lock(false);
    assert_eq!(state.config.fn_lock(KNOWN_MOUSE_KEY), Some(false));
}

#[test]
fn fn_lock_is_never_saved_for_a_mouse() {
    let mut state = state_with_a_known_mouse();

    let events = state.commit_fn_lock(true);

    assert_eq!(state.config.fn_lock(KNOWN_MOUSE_KEY), None);
    assert_eq!(events, StateEvents::none());
}
