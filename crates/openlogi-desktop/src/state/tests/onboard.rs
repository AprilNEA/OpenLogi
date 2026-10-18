//! Onboard memory: the settings source a mouse with `0x8100` uses persists
//! per device, and decides whether the live DPI and lighting controls apply.

use super::fn_lock::drain;
use super::*;
use crate::services::ipc::Command;
use openlogi_core::hid::MemoryMode;

/// The known mouse on its cable, with or without onboard memory.
fn mouse_state(onboard: bool) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    inventory.paired[0]
        .capabilities
        .as_mut()
        .expect("the fixture mouse has capabilities")
        .onboard_profiles = onboard;
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    (state, receiver)
}

#[test]
fn a_mouse_with_onboard_memory_starts_on_it() {
    let (state, _receiver) = mouse_state(true);
    assert_eq!(state.current_memory_mode(), MemoryMode::Onboard);
    assert!(state.current_settings_on_mouse());
}

#[test]
fn choosing_software_persists_and_hands_the_live_controls_back() {
    let (mut state, mut receiver) = mouse_state(true);
    drain(&mut receiver);
    let key = state.current_record().expect("selected").device_key();

    let events = state.commit_memory_mode(MemoryMode::Software);

    assert!(events.contains(&StateEvent::OnboardChanged(key.clone())));
    // The pointer and lighting tabs change what they offer with it.
    assert!(events.contains(&StateEvent::DeviceConfigChanged(key.clone())));
    assert!(events.contains(&StateEvent::LightingChanged(key)));
    assert_eq!(
        state.config.memory_mode(KNOWN_MOUSE_KEY),
        MemoryMode::Software
    );
    assert!(!state.current_settings_on_mouse());

    let _ = state.commit_memory_mode(MemoryMode::Onboard);
    assert_eq!(
        state.config.memory_mode(KNOWN_MOUSE_KEY),
        MemoryMode::Onboard
    );
    assert!(state.current_settings_on_mouse());
}

#[test]
fn a_mouse_without_onboard_memory_has_no_mode() {
    let (mut state, mut receiver) = mouse_state(false);
    drain(&mut receiver);

    let _ = state.commit_memory_mode(MemoryMode::Software);

    assert!(!state.current_settings_on_mouse());
    assert_eq!(
        state
            .config
            .devices
            .get(KNOWN_MOUSE_KEY)
            .and_then(|device| device.memory_mode),
        None
    );
}

/// Whether `receiver` holds a DPI write.
fn sent_dpi(receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>) -> bool {
    std::iter::from_fn(|| receiver.try_recv().ok())
        .any(|command| matches!(command, Command::SetDpi(_)))
}

#[test]
fn openlogi_settings_edited_while_the_mouse_runs_onboard_are_only_saved() {
    let (mut state, mut receiver) = mouse_state(true);
    drain(&mut receiver);

    let _ = state.commit_dpi(openlogi_core::hid::Dpi::new(1200));

    // Saved for software mode, and shown as OpenLogi's value…
    assert_eq!(
        state
            .config
            .devices
            .get(KNOWN_MOUSE_KEY)
            .and_then(|device| device.dpi),
        Some(openlogi_core::hid::Dpi::new(1200))
    );
    assert_eq!(state.dpi_for_current(), openlogi_core::hid::Dpi::new(1200));
    // …but not written over the profile the mouse runs.
    assert!(!sent_dpi(&mut receiver));

    let _ = state.commit_memory_mode(MemoryMode::Software);
    drain(&mut receiver);
    let _ = state.commit_dpi(openlogi_core::hid::Dpi::new(1600));
    assert!(sent_dpi(&mut receiver));
}
