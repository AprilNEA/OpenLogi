use std::cell::{Cell, RefCell};
use std::rc::Rc;

use core_graphics::event::CGEventFlags;

use super::{
    Installed, KEY_SLOTS, Keyboard, LEFT_COMMAND, LEFT_CONTROL, LEFT_OPTION, LEFT_SHIFT, PRODUCT,
    Pressed, REPORT_DESCRIPTOR, REPORT_LEN, Report, Routed, SendError, Sink,
    is_virtual_keyboard_product, modifier_bits, offer,
};
use crate::inject::KeyPhase;

const KEY_C: u8 = 0x06;
const KEY_V: u8 = 0x19;
const LEFT_ARROW: u8 = 0x50;
const COMMAND_USAGE: u8 = 0xe3;
const SHIFT_USAGE: u8 = 0xe1;
/// `kIOReturnNoDevice` (`0xe00002c0`).
const DEVICE_GONE: SendError = SendError(-536_870_208);

/// A sink that records every report it is asked to send, and rejects every
/// send from the `fail_from`th on, as a device that went away does.
#[derive(Clone, Default)]
struct Recorder {
    sent: Rc<RefCell<Vec<Report>>>,
    attempts: Rc<Cell<usize>>,
    fail_from: Option<usize>,
}

impl Recorder {
    fn failing_from(attempt: usize) -> Self {
        Self {
            fail_from: Some(attempt),
            ..Self::default()
        }
    }

    fn reports(&self) -> Vec<Report> {
        self.sent.borrow().clone()
    }
}

impl Sink for Recorder {
    fn send(&self, report: Report) -> Result<(), SendError> {
        let attempt = self.attempts.get();
        self.attempts.set(attempt + 1);
        if self.fail_from.is_some_and(|from| attempt >= from) {
            return Err(DEVICE_GONE);
        }
        self.sent.borrow_mut().push(report);
        Ok(())
    }
}

fn report(modifiers: u8, keys: &[u8]) -> Report {
    let mut bytes = [0; REPORT_LEN];
    bytes[0] = modifiers;
    bytes[2..2 + keys.len()].copy_from_slice(keys);
    Report(bytes)
}

fn installed(recorder: &Recorder) -> Installed<Recorder> {
    Installed {
        id: 0,
        keyboard: Keyboard::new(recorder.clone()),
    }
}

#[test]
fn a_tap_presses_its_modifiers_around_the_key() {
    let recorder = Recorder::default();
    let mut keyboard = Keyboard::new(recorder.clone());
    assert_eq!(keyboard.tap(KEY_C, LEFT_COMMAND), Routed::Sent);
    assert_eq!(
        recorder.reports(),
        [
            report(LEFT_COMMAND, &[]),
            report(LEFT_COMMAND, &[KEY_C]),
            report(LEFT_COMMAND, &[]),
            report(0, &[]),
        ]
    );
    assert!(keyboard.pressed.is_empty());
}

#[test]
fn a_tap_without_modifiers_is_one_press_and_release() {
    let recorder = Recorder::default();
    let mut keyboard = Keyboard::new(recorder.clone());
    assert_eq!(keyboard.tap(LEFT_ARROW, 0), Routed::Sent);
    assert_eq!(
        recorder.reports(),
        [report(0, &[LEFT_ARROW]), report(0, &[])]
    );
}

#[test]
fn a_tap_leaves_the_modifiers_a_held_chord_holds_down() {
    let recorder = Recorder::default();
    let mut keyboard = Keyboard::new(recorder.clone());
    assert_eq!(keyboard.edge(COMMAND_USAGE, KeyPhase::Down), Routed::Sent);
    assert_eq!(keyboard.tap(KEY_C, LEFT_COMMAND | LEFT_SHIFT), Routed::Sent);
    let both = LEFT_COMMAND | LEFT_SHIFT;
    assert_eq!(
        recorder.reports(),
        [
            report(LEFT_COMMAND, &[]),
            report(both, &[]),
            report(both, &[KEY_C]),
            report(both, &[]),
            report(LEFT_COMMAND, &[]),
        ]
    );
    assert!(keyboard.pressed.holds(COMMAND_USAGE));
}

#[test]
fn a_tap_of_a_key_the_device_holds_is_declined() {
    let recorder = Recorder::default();
    let mut keyboard = Keyboard::new(recorder.clone());
    assert_eq!(keyboard.edge(KEY_C, KeyPhase::Down), Routed::Sent);
    assert_eq!(keyboard.tap(KEY_C, LEFT_COMMAND), Routed::Declined);
    assert_eq!(recorder.reports(), [report(0, &[KEY_C])]);
}

#[test]
fn held_edges_press_and_release_in_pairs() {
    let recorder = Recorder::default();
    let mut keyboard = Keyboard::new(recorder.clone());
    for (usage, phase) in [
        (COMMAND_USAGE, KeyPhase::Down),
        (SHIFT_USAGE, KeyPhase::Down),
        (KEY_V, KeyPhase::Down),
        (KEY_V, KeyPhase::Up),
        (SHIFT_USAGE, KeyPhase::Up),
        (COMMAND_USAGE, KeyPhase::Up),
    ] {
        assert_eq!(
            keyboard.edge(usage, phase),
            Routed::Sent,
            "{usage:#04x} {phase:?}"
        );
    }
    let both = LEFT_COMMAND | LEFT_SHIFT;
    assert_eq!(
        recorder.reports(),
        [
            report(LEFT_COMMAND, &[]),
            report(both, &[]),
            report(both, &[KEY_V]),
            report(both, &[]),
            report(LEFT_COMMAND, &[]),
            report(0, &[]),
        ]
    );
}

/// A key the `CGEvent` path pressed is released there too.
#[test]
fn an_up_edge_the_device_did_not_press_is_declined() {
    let recorder = Recorder::default();
    let mut keyboard = Keyboard::new(recorder.clone());
    assert_eq!(keyboard.edge(KEY_C, KeyPhase::Up), Routed::Declined);
    assert_eq!(keyboard.edge(COMMAND_USAGE, KeyPhase::Up), Routed::Declined);
    assert!(recorder.reports().is_empty());
}

#[test]
fn a_press_past_the_rollover_is_declined_and_so_is_its_release() {
    let recorder = Recorder::default();
    let mut keyboard = Keyboard::new(recorder.clone());
    let keys: Vec<u8> = (0x04..).take(KEY_SLOTS).collect();
    for &key in &keys {
        assert_eq!(keyboard.edge(key, KeyPhase::Down), Routed::Sent);
    }
    assert_eq!(keyboard.edge(KEY_V, KeyPhase::Down), Routed::Declined);
    assert_eq!(keyboard.tap(KEY_V, 0), Routed::Declined);
    assert_eq!(keyboard.edge(KEY_V, KeyPhase::Up), Routed::Declined);
    assert_eq!(recorder.reports().last(), Some(&report(0, &keys)));
}

#[test]
fn releasing_a_key_keeps_the_others_in_press_order() {
    let mut pressed = Pressed::default();
    for key in [0x04, 0x05, 0x06] {
        assert!(pressed.press(key));
    }
    pressed.release(0x05);
    assert_eq!(pressed.report(), report(0, &[0x04, 0x06]));
    assert!(pressed.press(0x07));
    assert_eq!(pressed.report(), report(0, &[0x04, 0x06, 0x07]));
}

#[test]
fn dropping_the_keyboard_releases_every_held_key() {
    let recorder = Recorder::default();
    let mut keyboard = Keyboard::new(recorder.clone());
    assert_eq!(keyboard.edge(COMMAND_USAGE, KeyPhase::Down), Routed::Sent);
    assert_eq!(keyboard.edge(KEY_C, KeyPhase::Down), Routed::Sent);
    drop(keyboard);
    assert_eq!(recorder.reports().last(), Some(&report(0, &[])));
}

#[test]
fn dropping_an_idle_keyboard_sends_nothing() {
    let recorder = Recorder::default();
    drop(Keyboard::new(recorder.clone()));
    assert_eq!(recorder.attempts.get(), 0);
}

#[test]
fn an_empty_slot_leaves_every_edge_to_the_cgevent_path() {
    let mut slot: Option<Installed<Recorder>> = None;
    assert!(!offer(&mut slot, |keyboard| keyboard.tap(KEY_C, 0)));
}

#[test]
fn a_failed_down_edge_removes_the_device_and_falls_back() {
    let recorder = Recorder::failing_from(0);
    let mut slot = Some(installed(&recorder));
    assert!(!offer(&mut slot, |keyboard| keyboard.edge(KEY_C, KeyPhase::Down)));
    assert!(slot.is_none(), "a failed device must be removed");
    // Removal still tried to report the key released.
    assert_eq!(recorder.attempts.get(), 2);
}

#[test]
fn a_failed_up_edge_counts_as_released_by_the_removal() {
    let recorder = Recorder::failing_from(1);
    let mut slot = Some(installed(&recorder));
    assert!(offer(&mut slot, |keyboard| keyboard.edge(KEY_C, KeyPhase::Down)));
    assert!(offer(&mut slot, |keyboard| keyboard.edge(KEY_C, KeyPhase::Up)));
    assert!(slot.is_none());
}

/// A tap whose key-down never reached the device must still happen, as a
/// `CGEvent`; one whose key-down did must not happen twice.
#[test]
fn a_failed_tap_falls_back_only_when_its_key_never_went_down() {
    for (fail_from, delivered) in [(0, false), (1, false), (2, true), (3, true)] {
        let recorder = Recorder::failing_from(fail_from);
        let mut keyboard = Keyboard::new(recorder.clone());
        assert_eq!(
            keyboard.tap(KEY_C, LEFT_COMMAND),
            Routed::Failed {
                delivered,
                error: DEVICE_GONE
            },
            "failing from send {fail_from}"
        );
        let mut slot = Some(Installed { id: 0, keyboard });
        assert!(
            !offer(&mut slot, |keyboard| keyboard.tap(KEY_V, 0)),
            "the device is still failing"
        );
        assert!(slot.is_none());
    }
}

#[test]
fn flags_map_to_the_left_modifier_bits() {
    assert_eq!(modifier_bits(CGEventFlags::CGEventFlagNull), 0);
    assert_eq!(
        modifier_bits(CGEventFlags::CGEventFlagCommand | CGEventFlags::CGEventFlagShift),
        LEFT_COMMAND | LEFT_SHIFT
    );
    assert_eq!(
        modifier_bits(CGEventFlags::CGEventFlagControl | CGEventFlags::CGEventFlagAlternate),
        LEFT_CONTROL | LEFT_OPTION
    );
}

/// The descriptor must declare exactly the input report [`Report`] lays
/// out, and no output or feature report the device has no handler for.
#[test]
fn the_descriptor_declares_one_input_report_of_report_len_bytes() {
    const INPUT: u8 = 0x80;
    const OUTPUT: u8 = 0x90;
    const FEATURE: u8 = 0xb0;
    const REPORT_SIZE: u8 = 0x74;
    const REPORT_COUNT: u8 = 0x94;
    const REPORT_ID: u8 = 0x84;
    let mut items = REPORT_DESCRIPTOR.iter().copied();
    let (mut size, mut count, mut input_bits) = (0_u32, 0_u32, 0_u32);
    while let Some(prefix) = items.next() {
        let len = match prefix & 0x03 {
            3 => 4,
            n => usize::from(n),
        };
        let data = items
            .by_ref()
            .take(len)
            .enumerate()
            .fold(0_u32, |value, (i, byte)| {
                value | (u32::from(byte) << (8 * i))
            });
        match prefix & 0xfc {
            REPORT_SIZE => size = data,
            REPORT_COUNT => count = data,
            INPUT => input_bits += size * count,
            OUTPUT | FEATURE => panic!("the descriptor declares a report the device cannot answer"),
            REPORT_ID => panic!("the report layout has no report ID byte"),
            _ => {}
        }
    }
    assert_eq!(input_bits, u32::try_from(REPORT_LEN * 8).unwrap());
}

#[test]
fn only_the_virtual_keyboard_product_is_recognised() {
    assert!(is_virtual_keyboard_product(PRODUCT));
    assert!(!is_virtual_keyboard_product(
        "Apple Internal Keyboard / Trackpad"
    ));
    assert!(!is_virtual_keyboard_product(""));
}
