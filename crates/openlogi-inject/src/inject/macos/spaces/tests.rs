use objc2_foundation::NSArray;

use super::*;
use crate::inject::space_switch::Outcome;

#[test]
fn dock_swipe_encodes_both_directions_and_balanced_phases() {
    for (direction, progress, velocity) in [
        (Direction::Previous, -1.0_f64, -9999.0_f64),
        (Direction::Next, 1.0_f64, 9999.0_f64),
    ] {
        let events = field_swipe_events(direction).expect("CGEvent allocation");
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

type CopyHidEvent = unsafe extern "C" fn(*const CGEvent) -> *mut AnyObject;

/// `CGEventCopyIOHIDEvent`, when this macOS has every HID swipe API. Older
/// macOS lacks them, and then reads the field layout.
fn hid_swipe_reader() -> Option<CopyHidEvent> {
    let setter_available = app_services::sky_light_symbol(c"SLEventSetIOHIDEvent").is_some();
    let class_available = AnyClass::get(c"HIDEvent").is_some();
    let copy_hid = app_services_symbol(c"CGEventCopyIOHIDEvent");
    eprintln!(
        "HID swipe test availability: class={class_available}, setter={setter_available}, copy={}",
        copy_hid.is_some()
    );
    let Some(copy_hid) = copy_hid.filter(|_| class_available && setter_available) else {
        assert_eq!(
            *DOCK_SWIPE_LAYOUT,
            DockSwipeLayout::Fields,
            "the HID-event layout requires the HID swipe APIs"
        );
        eprintln!("HID swipe assertions skipped: private APIs unavailable on this older macOS");
        return None;
    };
    // SAFETY: CGEventCopyIOHIDEvent returns a +1 HIDEvent, as documented
    // by Mac Mouse Fix's CGEventHIDEventBridge.m at f92d2d53a.
    Some(unsafe { std::mem::transmute::<*mut c_void, CopyHidEvent>(copy_hid) })
}

/// What a Dock swipe's attached `IOHIDEvent` carries.
#[derive(Debug)]
struct HidSwipeReading {
    phase: u32,
    progress: f64,
    /// The X and Y of the velocity child, which only an end phase has.
    velocity: Option<(f64, f64)>,
}

fn read_hid_swipe(copy_hid: CopyHidEvent, event: &CGEvent) -> HidSwipeReading {
    assert_eq!(CGEvent::r#type(Some(event)), CGEventType(30));
    assert_eq!(
        CGEvent::integer_value_field(Some(event), CGEventField::EventSourceUserData),
        crate::inject::SYNTHETIC_EVENT_USER_DATA
    );
    // SAFETY: event is live; the Copy API returns a retained HIDEvent or null.
    let hid = unsafe { copy_hid(&raw const *event) };
    // SAFETY: adopt the Copy-rule reference once; Retained releases the HIDEvent.
    let hid = unsafe { Retained::from_raw(hid) }.expect("attached HIDEvent");
    // SAFETY: HIDEvent.options returns uint32_t.
    let options: u32 = unsafe { msg_send![&*hid, options] };
    assert_eq!(options & ((1 << 24) - 1), 0, "only the phase is encoded");
    // SAFETY: HIDEvent's integer getter returns NSInteger for this field.
    let motion: isize = unsafe { msg_send![&*hid, integerValueForField: HID_FIELD_MOTION] };
    // SAFETY: HIDEvent's integer getter returns NSInteger for this field.
    let flavor: isize = unsafe { msg_send![&*hid, integerValueForField: HID_FIELD_FLAVOR] };
    assert_eq!(motion, HID_MOTION_HORIZONTAL);
    assert_eq!(flavor, HID_FLAVOR_DOCK_PRIMARY);
    // SAFETY: HIDEvent's progress getter returns double.
    let progress: f64 = unsafe { msg_send![&*hid, doubleValueForField: HID_FIELD_PROGRESS] };
    // SAFETY: HIDEvent.children returns a nullable NSArray of HIDEvent objects.
    let children: Option<Retained<NSArray<AnyObject>>> = unsafe { msg_send![&*hid, children] };
    assert!(
        children.as_ref().map_or(0, |children| children.len()) <= 1,
        "at most one velocity child"
    );
    let velocity = children
        .and_then(|children| children.firstObject())
        .map(|velocity| {
            // SAFETY: the child is a live HIDEvent; its X getter returns double.
            let vx: f64 =
                unsafe { msg_send![&*velocity, doubleValueForField: HID_FIELD_VELOCITY_X] };
            // SAFETY: the child is a live HIDEvent; its Y getter returns double.
            let vy: f64 =
                unsafe { msg_send![&*velocity, doubleValueForField: HID_FIELD_VELOCITY_Y] };
            (vx, vy)
        });
    HidSwipeReading {
        phase: options >> 24,
        progress,
        velocity,
    }
}

#[test]
fn hid_dock_swipe_attaches_balanced_phases_for_both_scroll_directions() {
    autoreleasepool(|_| {
        let Some(copy_hid) = hid_swipe_reader() else {
            return;
        };
        for (natural, direction, expected_progress, expected_velocity) in [
            (true, Direction::Previous, 1.0_f64, 9999.0_f64),
            (true, Direction::Next, -1.0, -9999.0),
            (false, Direction::Previous, -1.0, -9999.0),
            (false, Direction::Next, 1.0, 9999.0),
        ] {
            let events = hid_swipe_events(direction, natural).expect("HIDEvent-backed CGEvents");
            for (event, phase) in events.iter().zip([1_u32, 4_u32]) {
                let reading = read_hid_swipe(copy_hid, event);
                assert_eq!(reading.phase, phase, "encoded begin/end phase");
                assert_eq!(
                    reading.progress.to_bits(),
                    expected_progress.to_bits(),
                    "natural={natural}, direction={direction:?}"
                );
                assert_eq!(
                    reading.velocity.map(|(x, y)| (x.to_bits(), y.to_bits())),
                    (phase == 4)
                        .then_some((expected_velocity.to_bits(), expected_velocity.to_bits())),
                    "only the end phase carries one velocity child"
                );
            }
        }
        eprintln!("HID swipe assertions executed: both directions, both preferences, both phases");
    });
}

#[test]
fn hid_slide_frames_carry_the_flipped_progress_and_an_exit_velocity_on_end() {
    autoreleasepool(|_| {
        let Some(copy_hid) = hid_swipe_reader() else {
            return;
        };
        for natural in [true, false] {
            let flip = if natural { -1.0 } else { 1.0 };
            for direction in [Direction::Previous, Direction::Next] {
                let frames = space_switch::slide(direction);
                let events = hid_slide_events(&frames, natural).expect("HIDEvent-backed CGEvents");
                assert_eq!(events.len(), frames.len());
                for ((at, event), frame) in events.iter().zip(&frames) {
                    assert_eq!(*at, frame.at);
                    let reading = read_hid_swipe(copy_hid, event);
                    assert_eq!(reading.phase, slide_phase(frame.phase), "{frame:?}");
                    // The attached event may round the progress it stores.
                    let expected = flip * frame.progress;
                    assert!(
                        (reading.progress - expected).abs() < 1e-4,
                        "natural={natural}: {reading:?} vs {expected}"
                    );
                    let exit =
                        (frame.phase == SlidePhase::Ended).then(|| expected.signum() * 9999.0);
                    assert_eq!(
                        reading.velocity.map(|(x, y)| (x.to_bits(), y.to_bits())),
                        exit.map(|v: f64| (v.to_bits(), v.to_bits())),
                        "natural={natural}: {frame:?}"
                    );
                }
            }
        }
        eprintln!("HID slide assertions executed: both directions, both preferences");
    });
}

#[test]
fn slide_frames_carry_progress_phase_and_an_exit_velocity_on_end() {
    for (phase, value) in [
        (SlidePhase::Began, 1),
        (SlidePhase::Changed, 2),
        (SlidePhase::Ended, 4),
    ] {
        let frame = SlideFrame {
            phase,
            progress: -0.25,
            at: Duration::ZERO,
        };
        let event = slide_event(frame).expect("CGEvent allocation");
        assert_eq!(CGEvent::r#type(Some(&event)), CGEventType(30));
        for (field, expected) in [(110, 23), (123, 1), (165, 1), (132, value), (134, value)] {
            assert_eq!(
                CGEvent::integer_value_field(Some(&event), CGEventField(field)),
                expected
            );
        }
        assert_eq!(
            CGEvent::double_value_field(Some(&event), CGEventField(124)).to_bits(),
            (-0.25_f64).to_bits()
        );
        assert_eq!(
            CGEvent::integer_value_field(Some(&event), CGEventField(135)),
            i64::from((-0.25_f32).to_bits())
        );
        assert_eq!(
            CGEvent::integer_value_field(Some(&event), CGEventField::EventSourceUserData),
            crate::inject::SYNTHETIC_EVENT_USER_DATA
        );
        let velocity = CGEvent::double_value_field(Some(&event), CGEventField(129));
        if phase == SlidePhase::Ended {
            assert_eq!(velocity.to_bits(), (-9999.0_f64).to_bits());
        } else {
            assert_eq!(velocity.to_bits(), 0.0_f64.to_bits());
        }
    }
}

#[test]
fn an_instant_switch_keeps_its_two_frames() {
    for direction in [Direction::Previous, Direction::Next] {
        let output =
            Output::prepare(DesktopSwitch::Instant, direction).expect("CGEvent allocation");
        assert!(matches!(output, Output::Instant(_)), "{direction:?}");
    }
}

#[test]
fn the_host_choice_decides_how_the_next_switch_posts() {
    // A switch reads the choice when it starts. No other test reads or sets
    // it, so this test may change it, and it ends on the default.
    assert_eq!(
        desktop_switch(),
        DesktopSwitch::Instant,
        "switches stay instant until the host chooses"
    );
    set_desktop_switch(DesktopSwitch::Animated);
    assert_eq!(desktop_switch(), DesktopSwitch::Animated);
    set_desktop_switch(DesktopSwitch::Instant);
    assert_eq!(desktop_switch(), DesktopSwitch::Instant);
}

#[test]
fn an_animated_switch_prepares_every_slide_frame_with_its_time() {
    // Posting at those times is `space_switch::pace`, tested with a fake clock.
    // With the user's Space shortcut enabled the slide is its fallback, so
    // either way the switch carries every frame.
    for direction in [Direction::Previous, Direction::Next] {
        let output =
            Output::prepare(DesktopSwitch::Animated, direction).expect("CGEvent allocation");
        let frames = match output {
            Output::Slide(frames) => frames,
            Output::Shortcut { fallback, .. } => fallback,
            Output::Instant(_) => panic!("an animated switch slides: {direction:?}"),
        };
        let expected = space_switch::slide(direction);
        assert_eq!(frames.len(), expected.len());
        for ((at, event), frame) in frames.iter().zip(&expected) {
            assert_eq!(*at, frame.at);
            if *DOCK_SWIPE_LAYOUT == DockSwipeLayout::HidEvent {
                // The HID slide test reads the attached events.
                continue;
            }
            assert_eq!(
                CGEvent::integer_value_field(Some(event), CGEventField(132)),
                i64::from(slide_phase(frame.phase))
            );
            // The event keeps its progress at f32 precision, as the Dock reads it.
            let progress = CGEvent::double_value_field(Some(event), CGEventField(124));
            assert!(
                (progress - frame.progress).abs() < 1e-6,
                "{progress} vs {frame:?}"
            );
            assert_eq!(
                CGEvent::integer_value_field(Some(event), CGEventField(135)),
                i64::from(slide_progress_bits(frame.progress))
            );
        }
    }
}

/// Opt-in hardware test. Run in a logged-in macOS session with Accessibility
/// granted to the test host, a next Space on the pointer's display, and no
/// concurrent trackpad/keyboard Space switching. For each switch style,
/// switches right, then left back to the original Space. A failure can leave
/// the display on the next Space; never changes keyboard settings, moves the
/// cursor, or retries.
#[test]
#[ignore = "changes the current macOS Space; requires explicit interactive execution"]
fn interactive_space_round_trip() {
    for switch in [DesktopSwitch::Instant, DesktopSwitch::Animated] {
        autoreleasepool(|_| {
            let display_id = cursor_display().expect("one unambiguous pointer display");
            let mut backend = Native::new(display_id, switch).expect("Space SPI available");
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
            let mut reverse = Native::new(display_id, switch).expect("Space SPI available");
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
}
