//! Keep OS layout: the toggle persists an agent setting and reloads the agent;
//! the app never writes the keyboard's layout itself.
use super::fn_lock::{drain, keyboard_inventory};
use super::*;
use crate::services::ipc::Command;

/// The fn-lock test keyboard, with or without a settable key layout.
fn keyboard_state(os_layout: bool) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let mut inventory = keyboard_inventory(true);
    if let Some(capabilities) = inventory.paired[0].capabilities.as_mut() {
        capabilities.os_layout = os_layout;
    }
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    (state, receiver)
}

#[test]
fn keep_os_layout_is_on_by_default_and_the_toggle_saves_and_reloads_only() {
    let (mut state, mut receiver) = keyboard_state(true);
    drain(&mut receiver);
    assert!(state.current_os_layout_supported());
    assert!(state.current_keep_os_layout());
    let record = state.current_record().expect("the keyboard is selected");
    let key = record.device_key();
    let config_key = record
        .persistent_config_key()
        .expect("a unit id makes the keyboard persistent")
        .to_string();
    let events = state.commit_keep_os_layout(false);
    assert_eq!(events, [StateEvent::DeviceConfigChanged(key.clone())]);
    assert!(!state.config.keep_os_layout(&config_key));
    assert!(!state.current_keep_os_layout());
    let commands = drain(&mut receiver);
    assert!(
        matches!(commands.as_slice(), [Command::ReloadConfig(_)]),
        "only a reload reaches the agent, got {} commands",
        commands.len()
    );
    assert_eq!(
        state.commit_keep_os_layout(true),
        [StateEvent::DeviceConfigChanged(key)]
    );
    assert!(state.current_keep_os_layout());
}

#[test]
fn a_keyboard_without_a_settable_layout_ignores_the_toggle() {
    let (mut state, mut receiver) = keyboard_state(false);
    drain(&mut receiver);
    assert!(!state.current_os_layout_supported());
    let key = state
        .current_record()
        .expect("the keyboard is selected")
        .device_key();
    // The event still fires so the toggle re-renders the unchanged value.
    assert_eq!(
        state.commit_keep_os_layout(false),
        [StateEvent::DeviceConfigChanged(key)]
    );
    assert!(state.current_keep_os_layout());
    assert!(drain(&mut receiver).is_empty());
}
