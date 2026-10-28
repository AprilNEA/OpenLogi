//! Event haptics: the haptics card's switch persists a per-device setting and
//! reloads the agent, which plays the haptics itself.
use super::fn_lock::drain;
use super::*;
use crate::services::ipc::Command;

/// The known mouse, with or without a haptic engine.
fn mouse_state(haptic_feedback: bool) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    inventory.paired[0]
        .capabilities
        .as_mut()
        .expect("the fixture mouse has capabilities")
        .haptic_feedback = haptic_feedback;
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    drain(&mut receiver);
    (state, receiver)
}

#[test]
fn event_haptics_are_off_by_default_and_the_switch_saves_and_reloads_only() {
    let (mut state, mut receiver) = mouse_state(true);
    assert!(state.current_haptic_feedback_supported());
    assert!(!state.current_event_haptics_config().enabled);
    let key = state.current_record().expect("selected").device_key();

    let events = state.commit_event_haptics(true);
    assert_eq!(events, [StateEvent::DeviceConfigChanged(key.clone())]);
    assert!(state.config.event_haptics(KNOWN_MOUSE_KEY).enabled);
    assert!(state.current_event_haptics_config().enabled);
    assert!(matches!(
        drain(&mut receiver).as_slice(),
        [Command::ReloadConfig(_)]
    ));

    let _ = state.commit_event_haptics(false);
    assert!(!state.current_event_haptics_config().enabled);
    assert!(matches!(
        drain(&mut receiver).as_slice(),
        [Command::ReloadConfig(_)]
    ));
}

#[test]
fn committing_the_current_value_saves_and_reloads_nothing() {
    let (mut state, mut receiver) = mouse_state(true);
    let _ = state.commit_event_haptics(false);
    assert!(drain(&mut receiver).is_empty());
}

#[test]
fn a_mouse_without_a_haptic_engine_reports_no_support() {
    let (state, _receiver) = mouse_state(false);
    assert!(!state.current_haptic_feedback_supported());
}

#[test]
fn each_button_and_the_wheel_save_their_own_feedback() {
    use openlogi_core::binding::ButtonId;
    use openlogi_core::config::{EventHapticFeedback, EventHapticInput};

    let (mut state, mut receiver) = mouse_state(true);
    let _ = state.commit_event_haptic(
        EventHapticInput::Button(ButtonId::Back),
        EventHapticFeedback::SharpCollision,
    );
    let _ = state.commit_event_haptic(EventHapticInput::Wheel, EventHapticFeedback::Off);
    let config = state.current_event_haptics_config();
    assert_eq!(
        config.button(ButtonId::Back),
        EventHapticFeedback::SharpCollision
    );
    assert_eq!(
        config.button(ButtonId::LeftClick),
        EventHapticFeedback::WhisperCollision,
        "the other buttons keep theirs"
    );
    assert_eq!(config.wheel, EventHapticFeedback::Off);
    // The choice that plays is previewed on the mouse; turning one off is not.
    assert!(matches!(
        drain(&mut receiver).as_slice(),
        [
            Command::PlayHaptic(crate::services::ipc::PlayHaptic {
                feedback: EventHapticFeedback::SharpCollision,
                ..
            }),
            Command::ReloadConfig(_),
            Command::ReloadConfig(_),
        ]
    ));

    let _ = state.commit_event_haptic(EventHapticInput::Wheel, EventHapticFeedback::Off);
    assert!(
        drain(&mut receiver).is_empty(),
        "an unchanged choice reloads nothing"
    );
}

#[test]
fn a_chosen_strength_saves_and_reloads_once_whether_event_haptics_are_on_or_not() {
    use openlogi_core::hid::HapticStrength;

    let (mut state, mut receiver) = mouse_state(true);
    assert_eq!(
        state.current_haptic_strength(),
        None,
        "the device keeps its own"
    );
    let strong = HapticStrength::try_new(90).expect("valid strength");

    let _ = state.commit_haptic_strength(strong);
    assert_eq!(state.current_haptic_strength(), Some(strong));
    assert_eq!(state.config.haptic_strength(KNOWN_MOUSE_KEY), Some(strong));
    assert!(!state.current_event_haptics_config().enabled);
    assert!(matches!(
        drain(&mut receiver).as_slice(),
        [Command::ReloadConfig(_)]
    ));

    let _ = state.commit_haptic_strength(strong);
    assert!(drain(&mut receiver).is_empty(), "the same strength again");
}
