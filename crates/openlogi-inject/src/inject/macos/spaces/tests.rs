use super::*;
use crate::inject::space_switch::Outcome;

#[test]
fn dock_swipe_encodes_both_directions_and_balanced_phases() {
    for (direction, progress, velocity) in [
        (Direction::Previous, -1.0_f64, -9999.0_f64),
        (Direction::Next, 1.0_f64, 9999.0_f64),
    ] {
        let events = swipe_events(direction).expect("CGEvent allocation");
        for (event, phase) in events.iter().zip([1, 4]) {
            assert_eq!(CGEvent::r#type(Some(event)), CGEventType(30));
            for (field, expected) in [(55, 30), (110, 23), (123, 1), (132, phase)] {
                assert_eq!(
                    CGEvent::integer_value_field(Some(event), CGEventField(field)),
                    expected
                );
            }
            assert_eq!(
                CGEvent::double_value_field(Some(event), CGEventField(124)).to_bits(),
                progress.to_bits()
            );
            assert_eq!(
                CGEvent::double_value_field(Some(event), CGEventField(129)).to_bits(),
                velocity.to_bits()
            );
            assert_eq!(
                CGEvent::integer_value_field(Some(event), CGEventField::EventSourceUserData),
                crate::inject::SYNTHETIC_EVENT_USER_DATA
            );
        }
    }
}

/// Opt-in hardware test. Run in a logged-in macOS session with Accessibility
/// granted to the test host, a next Space on the pointer's display, and no
/// concurrent trackpad/keyboard Space switching. Switches right, then left back
/// to the original Space. A failure can leave the display on the next Space;
/// never changes keyboard settings, moves the cursor, or retries.
#[test]
#[ignore = "changes the current macOS Space; requires explicit interactive execution"]
fn interactive_space_round_trip() {
    autoreleasepool(|_| {
        let display_id = cursor_display().expect("one unambiguous pointer display");
        let mut backend = Native::new(display_id).expect("Space SPI available");
        let before = backend.state().expect("read initial display state");
        let result = space_switch::run(&mut backend, Direction::Next, PostGate::for_test())
            .expect("Space switch confirmed");
        let Outcome::Reached(target) = result else {
            panic!("move to a Space with a right-hand neighbor before running this test");
        };
        let after = backend.state().expect("read final display state");
        assert_eq!(after.display, before.display);
        assert_eq!(after.current, target);
        assert_ne!(after.current, before.current);
        assert_eq!(cursor_display(), Some(display_id));
        // Give the reverse transaction its own observer and deadline. The
        // expectation is the original native ID, not the direction encoder.
        let mut reverse = Native::new(display_id).expect("Space SPI available");
        assert_eq!(
            space_switch::run(&mut reverse, Direction::Previous, PostGate::for_test())
                .expect("reverse Space switch confirmed"),
            Outcome::Reached(before.current)
        );
        assert_eq!(
            reverse.state().expect("read restored display state"),
            before
        );
        assert_eq!(cursor_display(), Some(display_id));
    });
}
