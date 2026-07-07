use super::*;

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

struct DropRecorder(Rc<RefCell<Vec<&'static str>>>);

impl Drop for DropRecorder {
    fn drop(&mut self) {
        self.0.borrow_mut().push("listener");
    }
}

#[tokio::test]
async fn graceful_teardown_clears_then_drops_listener_then_disarms() {
    let events = Rc::new(RefCell::new(Vec::new()));
    teardown_capture(
        {
            let events = Rc::clone(&events);
            move || events.borrow_mut().push("clear")
        },
        DropRecorder(Rc::clone(&events)),
        CaptureStop::Graceful,
        {
            let events = Rc::clone(&events);
            move || async move { events.borrow_mut().push("disarm") }
        },
    )
    .await;

    assert_eq!(*events.borrow(), ["clear", "listener", "disarm"]);
}

#[tokio::test]
async fn revoked_teardown_clears_then_drops_listener_without_disarm() {
    let events = Rc::new(RefCell::new(Vec::new()));
    teardown_capture(
        {
            let events = Rc::clone(&events);
            move || events.borrow_mut().push("clear")
        },
        DropRecorder(Rc::clone(&events)),
        CaptureStop::Revoked,
        {
            let events = Rc::clone(&events);
            move || async move { events.borrow_mut().push("disarm") }
        },
    )
    .await;

    assert_eq!(*events.borrow(), ["clear", "listener"]);
}

#[test]
fn capture_slot_cleanup_recovers_a_poisoned_writer() {
    let slot: CaptureChannel = Arc::new(RwLock::new(None));
    let poison = Arc::clone(&slot);
    let _ = catch_unwind(AssertUnwindSafe(move || {
        let _guard = poison.write().unwrap_or_else(PoisonError::into_inner);
        panic!("poison capture slot");
    }));

    replace_capture_slot(&slot, None);

    assert!(
        slot.read()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none()
    );
}

#[tokio::test]
async fn registry_miss_does_not_fall_back_to_opening_the_route() {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb35b,
    };
    let registry = ChannelRegistry::default();
    let (sink, _events) = mpsc::unbounded_channel();
    let (_stop, shutdown) = oneshot::channel();
    let slot: CaptureChannel = Arc::new(RwLock::new(None));

    let result =
        run_capture_session_with_registry(route, false, false, sink, shutdown, slot, &registry)
            .await;

    let Err(error) = result else {
        panic!("an empty Agent registry must fail before any route open");
    };

    assert!(matches!(error, GestureError::DeviceNotFound));
}

fn press() -> RawControlEvent {
    RawControlEvent::DivertedButtons([reprog_controls::GESTURE_BUTTON_CID, 0, 0, 0])
}

fn release() -> RawControlEvent {
    RawControlEvent::DivertedButtons([0, 0, 0, 0])
}

#[test]
fn quick_tap_is_a_click_even_while_the_cursor_moves() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), &[], &[], &[], &tx);
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        &[],
        &[],
        &[],
        &tx,
    );
    handle_reprog(&mut acc, release(), &[], &[], &[], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(GestureDirection::Click))
    );
    assert!(
        rx.try_recv().is_err(),
        "a quick tap emits exactly one click"
    );
}

#[test]
fn a_held_gesture_commits_a_swipe_and_does_not_also_click() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), &[], &[], &[], &tx);
    // Pretend the button has been held well past the swipe gate.
    acc.swipe.backdate_hold_for_test();
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        &[],
        &[],
        &[],
        &tx,
    );

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(GestureDirection::Right))
    );

    handle_reprog(&mut acc, release(), &[], &[], &[], &tx);
    assert!(
        rx.try_recv().is_err(),
        "a committed swipe must not also click on release"
    );
}

#[test]
fn a_held_dpi_button_presses_once_on_the_rising_edge() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let dpi = reprog_controls::DPI_MODE_SHIFT_CIDS[0];
    let down = RawControlEvent::DivertedButtons([dpi, 0, 0, 0]);

    handle_reprog(&mut acc, down, &[dpi], &[], &[], &tx);
    handle_reprog(&mut acc, down, &[dpi], &[], &[], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle, None))
    );
    assert!(rx.try_recv().is_err(), "a held DPI button presses once");
}

#[test]
fn a_dpi_button_re_presses_after_a_release() {
    // Rising-edge detection must re-arm: press → release → press is two
    // distinct presses. The release (a frame without the CID) is what resets
    // the edge; without it a re-press would be swallowed as "still held".
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let dpi = reprog_controls::DPI_MODE_SHIFT_CIDS[0];
    let down = RawControlEvent::DivertedButtons([dpi, 0, 0, 0]);
    let up = RawControlEvent::DivertedButtons([0, 0, 0, 0]);

    handle_reprog(&mut acc, down, &[dpi], &[], &[], &tx);
    handle_reprog(&mut acc, up, &[dpi], &[], &[], &tx);
    handle_reprog(&mut acc, down, &[dpi], &[], &[], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle, None))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle, None)),
        "a release re-arms the rising edge"
    );
    assert!(rx.try_recv().is_err());
}
