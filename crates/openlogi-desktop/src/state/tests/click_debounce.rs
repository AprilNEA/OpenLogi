//! Click debounce: the clicks card persists a per-device window and reloads
//! the agent, which filters the device's clicks in its OS input hook (#2182).
//! Turning it on needs a hook that names each click's device; turning it off
//! never does.
use openlogi_core::config::ClickDebounce;

use super::fn_lock::drain;
use super::*;
use crate::services::ipc::Command;

/// The known mouse, selected.
fn mouse_state() -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    drain(&mut receiver);
    (state, receiver)
}

fn only_a_reload(commands: &[Command]) -> bool {
    matches!(commands, [Command::ReloadConfig(_)])
}

fn window(ms: u8) -> ClickDebounce {
    ClickDebounce::try_new(ms).expect("valid test window")
}

#[test]
fn clicks_are_not_filtered_until_a_window_is_committed() {
    let (mut state, mut receiver) = mouse_state();
    assert_eq!(state.current_click_debounce(), None);
    let key = state.current_record().expect("selected").device_key();

    let events = state.commit_click_debounce(Some(window(45)));
    assert_eq!(events, [StateEvent::DeviceConfigChanged(key.clone())]);
    if !AppState::click_debounce_available() {
        assert_eq!(state.config.click_debounce(KNOWN_MOUSE_KEY), None);
        assert!(drain(&mut receiver).is_empty());
        return;
    }
    assert_eq!(
        state.config.click_debounce(KNOWN_MOUSE_KEY),
        Some(window(45))
    );
    assert_eq!(state.current_click_debounce(), Some(window(45)));
    assert!(only_a_reload(&drain(&mut receiver)));

    assert_eq!(
        state.commit_click_debounce(None),
        [StateEvent::DeviceConfigChanged(key)]
    );
    assert_eq!(state.config.click_debounce(KNOWN_MOUSE_KEY), None);
    assert!(only_a_reload(&drain(&mut receiver)));
}

#[test]
fn committing_the_current_window_saves_and_reloads_nothing() {
    let (mut state, mut receiver) = mouse_state();
    let _ = state.commit_click_debounce(None);
    assert!(drain(&mut receiver).is_empty());
    if !AppState::click_debounce_available() {
        return;
    }
    let _ = state.commit_click_debounce(Some(ClickDebounce::DEFAULT));
    drain(&mut receiver);
    let _ = state.commit_click_debounce(Some(ClickDebounce::DEFAULT));
    assert!(drain(&mut receiver).is_empty());
}

#[test]
fn a_window_set_elsewhere_can_always_be_turned_off() {
    let (mut state, mut receiver) = mouse_state();
    state
        .config
        .edit(|config| config.set_click_debounce(KNOWN_MOUSE_KEY, Some(window(60))));
    assert_eq!(state.current_click_debounce(), Some(window(60)));
    let _ = state.commit_click_debounce(None);
    assert_eq!(state.config.click_debounce(KNOWN_MOUSE_KEY), None);
    assert!(only_a_reload(&drain(&mut receiver)));
}

#[test]
fn with_no_device_selected_no_window_is_saved() {
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands));
    drain(&mut receiver);
    assert_eq!(state.current_click_debounce(), None);
    let _ = state.commit_click_debounce(Some(ClickDebounce::DEFAULT));
    assert!(drain(&mut receiver).is_empty());
    assert!(state.config.devices.is_empty());
}
