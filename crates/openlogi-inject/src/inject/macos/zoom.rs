//! Native magnification fields, isolated because they are not public SDK constants.

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGEvent, CGEventField, CGEventTapLocation, CGEventType};

use crate::inject::SYNTHETIC_EVENT_USER_DATA;
use crate::{ZoomContext, ZoomGesture, ZoomPhase};

const MAGNIFY: CGEventType = CGEventType(29);
const SUBTYPE: CGEventField = CGEventField(110);
const MAGNIFICATION: CGEventField = CGEventField(113);
const GESTURE_PHASE: CGEventField = CGEventField(132);

pub(crate) fn cursor_location() -> Option<(f64, f64)> {
    let event = CGEvent::new(None)?;
    let cursor = CGEvent::location(Some(&event));
    Some((cursor.x, cursor.y))
}

pub(crate) fn frontmost_process_id() -> Option<i32> {
    objc2::rc::autoreleasepool(|_| {
        objc2_app_kit::NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .map(|app| app.processIdentifier())
            .filter(|pid| *pid > 0)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Destination {
    Hid,
    Process(i32),
}

fn destination(context: ZoomContext, phase: ZoomPhase, current_pid: Option<i32>) -> Destination {
    if matches!(phase, ZoomPhase::Ended | ZoomPhase::Cancelled)
        && let Some(pid) = context.process_id
        && (phase == ZoomPhase::Cancelled || Some(pid) != current_pid)
    {
        Destination::Process(pid)
    } else {
        Destination::Hid
    }
}

fn phase_value(phase: ZoomPhase) -> i64 {
    match phase {
        ZoomPhase::Began => 1,
        ZoomPhase::Changed => 2,
        ZoomPhase::Ended => 4,
        ZoomPhase::Cancelled => 8,
    }
}

#[expect(
    unsafe_code,
    reason = "libc clock_gettime initializes a stack-owned timespec"
)]
fn uptime_timestamp() -> Option<u64> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: CLOCK_UPTIME_RAW is a supported macOS monotonic clock; the
    // initialized timespec is live and writable for the duration of the call.
    if unsafe { libc::clock_gettime(libc::CLOCK_UPTIME_RAW, &raw mut time) } != 0 {
        return None;
    }
    let seconds = u64::try_from(time.tv_sec).ok()?;
    let nanoseconds = u64::try_from(time.tv_nsec).ok()?;
    seconds
        .checked_mul(1_000_000_000)?
        .checked_add(nanoseconds)
        .filter(|timestamp| *timestamp != 0)
}

fn event(context: ZoomContext, delta: f64, phase: ZoomPhase) -> Option<CFRetained<CGEvent>> {
    let event = CGEvent::new(None)?;
    {
        CGEvent::set_location(
            Some(&event),
            objc2_core_foundation::CGPoint {
                x: context.x,
                y: context.y,
            },
        );
        CGEvent::set_type(Some(&event), MAGNIFY);
        CGEvent::set_integer_value_field(Some(&event), SUBTYPE, 8);
        CGEvent::set_double_value_field(Some(&event), MAGNIFICATION, delta);
        CGEvent::set_integer_value_field(Some(&event), GESTURE_PHASE, phase_value(phase));
        CGEvent::set_integer_value_field(
            Some(&event),
            CGEventField::EventSourceUserData,
            SYNTHETIC_EVENT_USER_DATA,
        );
    }
    // CGEvent::new(None) leaves timestamp zero. Explicitly stamping the Mach
    // uptime clock is required for repeated native gestures in Safari.
    CGEvent::set_timestamp(Some(&event), uptime_timestamp()?);
    Some(event)
}

pub(crate) fn post(context: ZoomContext, delta: f64, phase: ZoomPhase) -> bool {
    let Some(event) = event(context, delta, phase) else {
        tracing::warn!("native zoom event allocation failed");
        return false;
    };
    let current_pid = if matches!(phase, ZoomPhase::Ended | ZoomPhase::Cancelled) {
        frontmost_process_id()
    } else {
        None
    };
    // Motion retains native HID delivery. Terminal cleanup addresses the
    // captured receiver when cancellation or a focus change requires it.
    match destination(context, phase, current_pid) {
        Destination::Hid => CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event)),
        Destination::Process(pid) => CGEvent::post_to_pid(pid, Some(&event)),
    }
    true
}

pub(super) fn execute_tap(delta: f64) {
    if let Some((x, y)) = cursor_location() {
        let context = ZoomContext {
            x,
            y,
            process_id: frontmost_process_id(),
            window_id: None,
        };
        if let Some(mut gesture) = ZoomGesture::begin(context, delta) {
            gesture.finish(ZoomPhase::Ended);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magnification_events_preserve_fractional_motion_phases_cursor_and_marker() {
        let context = ZoomContext {
            x: 127.5,
            y: 208.25,
            process_id: Some(42),
            window_id: Some(7),
        };
        for (phase, value) in [
            (ZoomPhase::Began, 1),
            (ZoomPhase::Changed, 2),
            (ZoomPhase::Ended, 4),
            (ZoomPhase::Cancelled, 8),
        ] {
            for delta in [-0.014_062_5, 0.0, 0.014_062_5] {
                let event = event(context, delta, phase).unwrap();
                {
                    assert_eq!(CGEvent::r#type(Some(&event)), MAGNIFY);
                    assert_eq!(CGEvent::integer_value_field(Some(&event), SUBTYPE), 8);
                    assert_eq!(
                        CGEvent::integer_value_field(Some(&event), GESTURE_PHASE),
                        value
                    );
                    assert!(
                        (CGEvent::double_value_field(Some(&event), MAGNIFICATION) - delta).abs()
                            < 1e-8
                    );
                    assert_eq!(
                        CGEvent::integer_value_field(
                            Some(&event),
                            CGEventField::EventSourceUserData
                        ),
                        SYNTHETIC_EVENT_USER_DATA
                    );
                    let cursor = CGEvent::location(Some(&event));
                    assert!((cursor.x - context.x).abs() < f64::EPSILON);
                    assert!((cursor.y - context.y).abs() < f64::EPSILON);
                }
            }
        }
    }
    #[test]
    fn motion_uses_hid_and_cleanup_does_not_follow_a_new_foreground_receiver() {
        let captured = ZoomContext {
            x: 5.0,
            y: 7.0,
            process_id: Some(42),
            window_id: Some(9),
        };
        assert_eq!(
            destination(captured, ZoomPhase::Began, Some(77)),
            Destination::Hid
        );
        assert_eq!(
            destination(captured, ZoomPhase::Changed, Some(77)),
            Destination::Hid
        );
        assert_eq!(
            destination(captured, ZoomPhase::Ended, Some(42)),
            Destination::Hid
        );
        assert_eq!(
            destination(captured, ZoomPhase::Ended, Some(77)),
            Destination::Process(42)
        );
        assert_eq!(
            destination(captured, ZoomPhase::Cancelled, Some(77)),
            Destination::Process(42)
        );
        let unidentified = ZoomContext {
            process_id: None,
            ..captured
        };
        assert_eq!(
            destination(unidentified, ZoomPhase::Cancelled, Some(77)),
            Destination::Hid
        );
    }
    #[test]
    fn final_events_have_fresh_monotonic_uptime_timestamps() {
        let context = ZoomContext {
            x: 5.0,
            y: 7.0,
            process_id: None,
            window_id: None,
        };
        let mut previous = 0;
        for phase in [
            ZoomPhase::Began,
            ZoomPhase::Changed,
            ZoomPhase::Ended,
            ZoomPhase::Cancelled,
        ] {
            let before = uptime_timestamp().unwrap();
            let event = event(context, 0.0, phase).unwrap();
            let after = uptime_timestamp().unwrap();
            let timestamp = CGEvent::timestamp(Some(&event));
            assert!(timestamp > previous);
            assert!((before..=after).contains(&timestamp));
            previous = timestamp;
        }
    }
}
