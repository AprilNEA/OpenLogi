//! Wheel-hold: the inspector's switch for Back/Forward persists a binding
//! shape and reloads the agent; it needs measured raw-wheel support to turn
//! on, and it lives only in the global profile.
use openlogi_core::binding::{Binding, WheelHoldBinding};

use super::fn_lock::drain;
use super::*;
use crate::services::ipc::Command;

/// The known mouse, with or without raw wheel on Back/Forward.
fn mouse_state(raw_wheel: bool) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    inventory.paired[0]
        .capabilities
        .as_mut()
        .expect("the fixture mouse has capabilities")
        .virtual_thumbwheel = raw_wheel;
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    drain(&mut receiver);
    (state, receiver)
}

fn stored(state: &AppState, button: ButtonId) -> Option<Binding> {
    state
        .config
        .stored_bindings(KNOWN_MOUSE_KEY)
        .get(&button)
        .cloned()
}

#[test]
fn turning_wheel_hold_on_keeps_the_action_saves_and_reloads() {
    let (mut state, mut receiver) = mouse_state(true);
    let _ = state.commit_binding(ButtonId::Forward, Action::Copy);
    drain(&mut receiver);
    let key = state.current_record().expect("selected").device_key();

    let events = state.commit_wheel_hold(ButtonId::Forward, true);

    assert_eq!(events, [StateEvent::BindingsChanged(key)]);
    assert!(state.wheel_hold_buttons().contains(&ButtonId::Forward));
    assert_eq!(
        stored(&state, ButtonId::Forward),
        Some(Binding::WheelHold(WheelHoldBinding::horizontal_scroll(
            Action::Copy
        )))
    );
    assert_eq!(state.button_bindings()[&ButtonId::Forward], Action::Copy);
    let commands = drain(&mut receiver);
    assert!(
        matches!(commands.as_slice(), [Command::ReloadConfig(_)]),
        "only a reload reaches the agent, got {} commands",
        commands.len()
    );

    let _ = state.commit_wheel_hold(ButtonId::Forward, false);

    assert!(state.wheel_hold_buttons().is_empty());
    assert_eq!(
        stored(&state, ButtonId::Forward),
        Some(Binding::Single(Action::Copy))
    );
}

#[test]
fn picking_an_action_keeps_the_button_holding_the_wheel() {
    let (mut state, _receiver) = mouse_state(true);
    let _ = state.commit_wheel_hold(ButtonId::Back, true);

    let _ = state.commit_binding(ButtonId::Back, Action::Paste);

    assert!(state.wheel_hold_buttons().contains(&ButtonId::Back));
    assert_eq!(
        stored(&state, ButtonId::Back),
        Some(Binding::WheelHold(WheelHoldBinding::horizontal_scroll(
            Action::Paste
        )))
    );
    assert_eq!(state.button_bindings()[&ButtonId::Back], Action::Paste);
}

#[test]
fn wheel_hold_turns_on_only_where_raw_wheel_was_measured() {
    let (mut state, mut receiver) = mouse_state(false);
    let key = state.current_record().expect("selected").device_key();

    // The event still fires so the switch re-renders the unchanged value.
    assert_eq!(
        state.commit_wheel_hold(ButtonId::Back, true),
        [StateEvent::BindingsChanged(key)]
    );

    assert!(state.wheel_hold_buttons().is_empty());
    assert_eq!(stored(&state, ButtonId::Back), None);
    assert!(drain(&mut receiver).is_empty(), "nothing to reload");
}

#[test]
fn a_carried_over_wheel_hold_turns_off_without_raw_wheel() {
    let mut config = Config::ephemeral();
    config.set_wheel_hold(KNOWN_MOUSE_KEY, ButtonId::Back, true);
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        inventories: &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
        ..Sources::in_memory(config, &resolver, commands)
    });
    assert!(state.wheel_hold_buttons().contains(&ButtonId::Back));

    let _ = state.commit_wheel_hold(ButtonId::Back, false);

    assert!(state.wheel_hold_buttons().is_empty());
    assert!(!state.config.is_wheel_hold(KNOWN_MOUSE_KEY, ButtonId::Back));
}

#[test]
fn wheel_hold_is_not_editable_from_an_app_profile() {
    let (mut state, mut receiver) = mouse_state(true);
    let _ = state.set_editing_app(Some("com.apple.Safari".into()));
    drain(&mut receiver);

    let _ = state.commit_wheel_hold(ButtonId::Back, true);

    assert!(!state.config.is_wheel_hold(KNOWN_MOUSE_KEY, ButtonId::Back));
    assert!(state.wheel_hold_buttons().is_empty());
    assert!(drain(&mut receiver).is_empty());
}

#[test]
fn only_back_and_forward_offer_wheel_hold() {
    let (mut state, mut receiver) = mouse_state(true);
    for button in ButtonId::ALL
        .into_iter()
        .filter(|button| !matches!(button, ButtonId::Back | ButtonId::Forward))
    {
        let _ = state.commit_wheel_hold(button, true);
        assert!(
            !state.config.is_wheel_hold(KNOWN_MOUSE_KEY, button),
            "{button:?} held the wheel"
        );
    }
    assert!(state.wheel_hold_buttons().is_empty());
    assert!(
        drain(&mut receiver).is_empty(),
        "nothing changed, so the agent has nothing to reload"
    );
}

#[test]
fn committing_the_current_wheel_hold_mode_saves_nothing() {
    let (mut state, mut receiver) = mouse_state(true);
    let _ = state.commit_wheel_hold(ButtonId::Back, false);
    assert!(
        drain(&mut receiver).is_empty(),
        "off is already the stored mode"
    );
    let _ = state.commit_wheel_hold(ButtonId::Back, true);
    drain(&mut receiver);
    let _ = state.commit_wheel_hold(ButtonId::Back, true);
    assert!(
        drain(&mut receiver).is_empty(),
        "on is already the stored mode"
    );
}

#[test]
fn wheel_hold_takes_the_button_out_of_gesture_mode() {
    let (mut state, _receiver) = mouse_state(true);
    let _ = state.commit_gesture_mode(ButtonId::Back, true);
    assert!(state.gesture_bindings().contains_key(&ButtonId::Back));

    let _ = state.commit_wheel_hold(ButtonId::Back, true);

    assert!(!state.gesture_bindings().contains_key(&ButtonId::Back));
    assert!(state.wheel_hold_buttons().contains(&ButtonId::Back));
}

#[test]
fn the_wheel_hold_projection_follows_the_selected_device() {
    let mut config = Config::ephemeral();
    config.set_wheel_hold(KNOWN_MOUSE_KEY, ButtonId::Forward, true);
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut other = direct_inventory([0x11, 0x22, 0x33, 0x44]);
    other.receiver.product_id = 0xb037;
    let mut state = AppState::new(Sources {
        inventories: &[direct_inventory([0xa3, 0x93, 0xca, 0xe0]), other],
        ..Sources::in_memory(config, &resolver, commands)
    });
    let mut seen = Vec::new();
    for index in 0..2 {
        let _ = state.select_device(index);
        let config_key = state
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .expect("both mice are persistent")
            .to_string();
        let holds = state.wheel_hold_buttons().contains(&ButtonId::Forward);
        assert_eq!(
            holds,
            config_key == KNOWN_MOUSE_KEY,
            "{config_key}: only the configured mouse holds the wheel"
        );
        seen.push(config_key);
    }
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 2, "both mice were selected: {seen:?}");
}
