//! Wheel-hold mode as a per-button shape: what switching it keeps, and the
//! buttons it applies to.
use super::*;
use crate::binding::{LongPressBinding, WheelHoldBinding};

const KEY: &str = "2b042";

fn wheel_hold(click: Action) -> Binding {
    Binding::WheelHold(WheelHoldBinding::horizontal_scroll(click))
}

#[test]
fn no_button_holds_the_wheel_by_default() {
    let cfg = Config::default();
    for button in ButtonId::ALL {
        assert!(!cfg.is_wheel_hold(KEY, button), "{button:?}");
    }
}

#[test]
fn turning_wheel_hold_on_keeps_the_click_action() {
    let mut cfg = Config::default();
    cfg.set_binding(KEY, ButtonId::Back, Binding::Single(Action::Copy));

    cfg.set_wheel_hold(KEY, ButtonId::Back, true);

    assert!(cfg.is_wheel_hold(KEY, ButtonId::Back));
    assert_eq!(
        cfg.stored_bindings(KEY).get(&ButtonId::Back),
        Some(&wheel_hold(Action::Copy))
    );
}

#[test]
fn an_unbound_button_holds_the_wheel_with_its_default_click() {
    let mut cfg = Config::default();

    cfg.set_wheel_hold(KEY, ButtonId::Forward, true);

    assert_eq!(
        cfg.stored_bindings(KEY).get(&ButtonId::Forward),
        Some(&wheel_hold(default_binding(ButtonId::Forward)))
    );
}

#[test]
fn turning_wheel_hold_off_keeps_the_click_as_a_single_action() {
    let mut cfg = Config::default();
    cfg.set_binding(KEY, ButtonId::Back, wheel_hold(Action::Paste));

    cfg.set_wheel_hold(KEY, ButtonId::Back, false);

    assert!(!cfg.is_wheel_hold(KEY, ButtonId::Back));
    assert_eq!(
        cfg.stored_bindings(KEY).get(&ButtonId::Back),
        Some(&Binding::Single(Action::Paste))
    );
}

#[test]
fn turning_wheel_hold_off_leaves_a_button_without_it_untouched() {
    let mut cfg = Config::default();
    cfg.set_binding(
        KEY,
        ButtonId::Back,
        Binding::LongPress(LongPressBinding::new(Action::Copy, Action::Paste)),
    );

    cfg.set_wheel_hold(KEY, ButtonId::Back, false);
    cfg.set_wheel_hold("absent", ButtonId::Back, false);

    assert_eq!(
        cfg.stored_bindings(KEY).get(&ButtonId::Back),
        Some(&Binding::LongPress(LongPressBinding::new(
            Action::Copy,
            Action::Paste
        )))
    );
    assert!(!cfg.devices.contains_key("absent"));
}

#[test]
fn a_long_press_keeps_its_short_action_as_the_click() {
    let mut cfg = Config::default();
    cfg.set_binding(
        KEY,
        ButtonId::Back,
        Binding::LongPress(LongPressBinding::new(Action::Copy, Action::Paste)),
    );

    cfg.set_wheel_hold(KEY, ButtonId::Back, true);

    assert_eq!(
        cfg.stored_bindings(KEY).get(&ButtonId::Back),
        Some(&wheel_hold(Action::Copy))
    );
}

#[test]
fn wheel_hold_takes_a_button_out_of_gesture_mode_and_keeps_its_arms() {
    let mut cfg = Config::default();
    cfg.set_binding(KEY, ButtonId::Back, Binding::Single(Action::Paste));
    cfg.set_gesture_direction(KEY, ButtonId::Back, GestureDirection::Up, Action::Copy);
    assert!(cfg.is_gesture_mode(KEY, ButtonId::Back));

    cfg.set_wheel_hold(KEY, ButtonId::Back, true);

    assert!(!cfg.is_gesture_mode(KEY, ButtonId::Back));
    assert_eq!(
        cfg.stored_bindings(KEY).get(&ButtonId::Back),
        Some(&wheel_hold(Action::Paste)),
        "the gesture map's Click becomes the press"
    );

    cfg.set_gesture_mode(KEY, ButtonId::Back, true);

    assert!(!cfg.is_wheel_hold(KEY, ButtonId::Back));
    match cfg.stored_bindings(KEY).get(&ButtonId::Back) {
        Some(Binding::Gesture(map)) => {
            assert_eq!(map.get(&GestureDirection::Up), Some(&Action::Copy));
            assert_eq!(map.get(&GestureDirection::Click), Some(&Action::Paste));
        }
        other => panic!("expected the stashed gesture map back, got {other:?}"),
    }
}

#[test]
fn only_back_and_forward_hold_the_wheel() {
    let eligible: Vec<ButtonId> = ButtonId::ALL
        .into_iter()
        .filter(|button| button.supports_wheel_hold())
        .collect();
    assert_eq!(eligible, [ButtonId::Back, ButtonId::Forward]);
    assert!(!ButtonId::control(0x00c3).supports_wheel_hold());

    let mut cfg = Config::default();
    cfg.set_binding(KEY, ButtonId::MiddleClick, Binding::Single(Action::Copy));
    for button in ButtonId::ALL
        .into_iter()
        .filter(|button| !button.supports_wheel_hold())
    {
        cfg.set_wheel_hold(KEY, button, true);
        assert!(!cfg.is_wheel_hold(KEY, button), "{button:?}");
    }
    assert_eq!(
        cfg.stored_bindings(KEY).get(&ButtonId::MiddleClick),
        Some(&Binding::Single(Action::Copy))
    );
}
