//! Drag-hold mode as a per-button shape: what switching it keeps, which
//! buttons it applies to, and how it is saved.
use super::*;
use crate::binding::{DragHoldBinding, HeldDrag, WheelHoldBinding};

const KEY: &str = "2b042";

fn drag_hold(click: Action, drag: HeldDrag) -> Binding {
    Binding::DragHold(DragHoldBinding::new(click, drag))
}

#[test]
fn no_button_drags_by_default() {
    let cfg = Config::default();
    for button in ButtonId::ALL {
        assert_eq!(cfg.drag_hold(KEY, button), None, "{button:?}");
    }
}

#[test]
fn turning_drag_hold_on_keeps_the_click_action() {
    let mut cfg = Config::default();
    cfg.set_binding(KEY, ButtonId::Back, Binding::Single(Action::Copy));

    cfg.set_drag_hold(KEY, ButtonId::Back, Some(HeldDrag::ThreeFingerSwipe));

    assert_eq!(
        cfg.drag_hold(KEY, ButtonId::Back),
        Some(HeldDrag::ThreeFingerSwipe)
    );
    assert_eq!(
        cfg.stored_bindings(KEY).get(&ButtonId::Back),
        Some(&drag_hold(Action::Copy, HeldDrag::ThreeFingerSwipe))
    );
}

#[test]
fn switching_the_swipe_keeps_the_click_action() {
    let mut cfg = Config::default();
    cfg.set_binding(
        KEY,
        ButtonId::Forward,
        drag_hold(Action::Paste, HeldDrag::TwoFingerSwipe),
    );

    cfg.set_drag_hold(KEY, ButtonId::Forward, Some(HeldDrag::ThreeFingerSwipe));

    assert_eq!(
        cfg.stored_bindings(KEY).get(&ButtonId::Forward),
        Some(&drag_hold(Action::Paste, HeldDrag::ThreeFingerSwipe))
    );
}

#[test]
fn turning_drag_hold_off_keeps_the_click_as_a_single_action() {
    let mut cfg = Config::default();
    cfg.set_binding(
        KEY,
        ButtonId::Back,
        drag_hold(Action::Paste, HeldDrag::TwoFingerSwipe),
    );

    cfg.set_drag_hold(KEY, ButtonId::Back, None);

    assert_eq!(cfg.drag_hold(KEY, ButtonId::Back), None);
    assert_eq!(
        cfg.stored_bindings(KEY).get(&ButtonId::Back),
        Some(&Binding::Single(Action::Paste))
    );
}

#[test]
fn drag_hold_replaces_wheel_hold_and_gesture_mode() {
    let mut cfg = Config::default();
    cfg.set_binding(
        KEY,
        ButtonId::Back,
        Binding::WheelHold(WheelHoldBinding::horizontal_scroll(Action::Copy)),
    );
    cfg.set_gesture_mode(KEY, ButtonId::Forward, true);

    cfg.set_drag_hold(KEY, ButtonId::Back, Some(HeldDrag::TwoFingerSwipe));
    cfg.set_drag_hold(KEY, ButtonId::Forward, Some(HeldDrag::ThreeFingerSwipe));

    assert!(!cfg.is_wheel_hold(KEY, ButtonId::Back));
    assert_eq!(
        cfg.stored_bindings(KEY).get(&ButtonId::Back),
        Some(&drag_hold(Action::Copy, HeldDrag::TwoFingerSwipe))
    );
    assert!(!cfg.is_gesture_mode(KEY, ButtonId::Forward));
    assert_eq!(
        cfg.drag_hold(KEY, ButtonId::Forward),
        Some(HeldDrag::ThreeFingerSwipe)
    );
}

#[test]
fn only_back_and_forward_drag() {
    let mut cfg = Config::default();
    for button in ButtonId::ALL {
        cfg.set_drag_hold(KEY, button, Some(HeldDrag::TwoFingerSwipe));
        let expected = matches!(button, ButtonId::Back | ButtonId::Forward)
            .then_some(HeldDrag::TwoFingerSwipe);
        assert_eq!(cfg.drag_hold(KEY, button), expected, "{button:?}");
    }
}

#[test]
fn a_drag_hold_binding_round_trips_through_the_file() {
    let mut cfg = Config::default();
    cfg.set_binding(
        KEY,
        ButtonId::Back,
        drag_hold(Action::Paste, HeldDrag::ThreeFingerSwipe),
    );
    cfg.set_binding(
        KEY,
        ButtonId::Forward,
        drag_hold(Action::Copy, HeldDrag::TwoFingerSwipe),
    );

    let read = write_and_read(&cfg);

    assert_eq!(
        read.drag_hold(KEY, ButtonId::Back),
        Some(HeldDrag::ThreeFingerSwipe)
    );
    assert_eq!(
        read.stored_bindings(KEY).get(&ButtonId::Forward),
        Some(&drag_hold(Action::Copy, HeldDrag::TwoFingerSwipe))
    );
}

#[test]
fn a_drag_hold_binding_is_written_as_click_and_drag() {
    let binding = drag_hold(Action::Paste, HeldDrag::ThreeFingerSwipe);
    let text = toml::to_string(&BTreeMap::from([("Back", &binding)])).expect("serialize");
    assert!(text.contains(r#"drag = "ThreeFingerSwipe""#), "{text}");
    let parsed: BTreeMap<String, Binding> = toml::from_str(&text).expect("parse");
    assert_eq!(parsed.get("Back"), Some(&binding));
}
