//! Smooth wheel: the scrolling card's switch persists a per-device setting and
//! reloads the agent, which diverts the wheel itself. Turning it on needs
//! `0x2121` on the current link; turning it off never does.
use super::fn_lock::drain;
use super::*;
use crate::services::ipc::Command;

/// The known mouse, with or without a hi-res wheel on its current link.
fn mouse_state(hires_wheel: bool) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    inventory.paired[0]
        .capabilities
        .as_mut()
        .expect("the fixture mouse has capabilities")
        .hires_wheel = hires_wheel;
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

#[test]
fn the_smooth_wheel_is_off_by_default_and_the_switch_saves_and_reloads_only() {
    let (mut state, mut receiver) = mouse_state(true);
    assert!(!state.current_smooth_wheel());
    let key = state.current_record().expect("selected").device_key();

    let events = state.commit_smooth_wheel(true);
    assert_eq!(events, [StateEvent::DeviceConfigChanged(key.clone())]);
    assert!(state.config.smooth_wheel(KNOWN_MOUSE_KEY));
    assert!(state.current_smooth_wheel());
    let commands = drain(&mut receiver);
    assert!(
        only_a_reload(&commands),
        "only a reload reaches the agent, got {} commands",
        commands.len()
    );

    assert_eq!(
        state.commit_smooth_wheel(false),
        [StateEvent::DeviceConfigChanged(key)]
    );
    assert!(!state.config.smooth_wheel(KNOWN_MOUSE_KEY));
    assert!(!state.current_smooth_wheel());
    assert!(only_a_reload(&drain(&mut receiver)));
}

#[test]
fn committing_the_current_value_saves_and_reloads_nothing() {
    let (mut state, mut receiver) = mouse_state(true);
    let key = state.current_record().expect("selected").device_key();
    // The event still fires so the switch re-renders the unchanged value.
    assert_eq!(
        state.commit_smooth_wheel(false),
        [StateEvent::DeviceConfigChanged(key)]
    );
    assert!(drain(&mut receiver).is_empty());

    let _ = state.commit_smooth_wheel(true);
    drain(&mut receiver);
    let _ = state.commit_smooth_wheel(true);
    assert!(drain(&mut receiver).is_empty());
    assert!(state.current_smooth_wheel());
}

#[test]
fn a_link_without_a_hires_wheel_cannot_turn_it_on_but_can_turn_it_off() {
    let (mut state, mut receiver) = mouse_state(false);
    let _ = state.commit_smooth_wheel(true);
    assert!(!state.config.smooth_wheel(KNOWN_MOUSE_KEY));
    assert!(drain(&mut receiver).is_empty());

    // Turned on from another link, where the wheel can be diverted.
    state
        .config
        .edit(|config| config.set_smooth_wheel(KNOWN_MOUSE_KEY, true));
    assert!(state.current_smooth_wheel());
    let _ = state.commit_smooth_wheel(false);
    assert!(!state.config.smooth_wheel(KNOWN_MOUSE_KEY));
    assert!(only_a_reload(&drain(&mut receiver)));
}

#[test]
fn with_no_device_selected_nothing_is_saved() {
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands));
    drain(&mut receiver);
    assert!(!state.current_smooth_wheel());
    let _ = state.commit_smooth_wheel(true);
    assert!(drain(&mut receiver).is_empty());
    assert!(state.config.devices.is_empty());
}
