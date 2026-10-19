//! Synthetic scroll on macOS: the one-tick scroll actions, the fractional wheel, and the continuous smooth-scroll phases.

use crate::inject::{QuantizedScroll, ScrollQuantizer, SmoothScrollPhase};
use core_graphics::event::{CGEvent, CGEventTapLocation, EventField, ScrollEventUnit};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use openlogi_core::scroll::ScrollDelta;
use std::sync::{LazyLock, Mutex};

use super::tag_synthetic;

static PIXEL_SCROLL_QUANTIZER: LazyLock<Mutex<ScrollQuantizer>> =
    LazyLock::new(|| Mutex::new(ScrollQuantizer::default()));
static SMOOTH_SCROLL_QUANTIZER: LazyLock<Mutex<ScrollQuantizer>> =
    LazyLock::new(|| Mutex::new(ScrollQuantizer::default()));

// `core-graphics` 0.25 does not expose these `CGEventTypes.h` fields.
const SCROLL_PHASE: u32 = 99; // kCGScrollWheelEventScrollPhase
const MOMENTUM_PHASE: u32 = 123; // kCGScrollWheelEventMomentumPhase

/// Post a synthetic scroll event for one tick in direction `(dx, dy)`. Unit
/// direction (-1/0/1) scaled by the fixed "one tick" pixel magnitude the
/// four `Scroll*`/`HorizontalScroll*` actions have always used.
pub(super) fn dispatch_scroll(dx: i8, dy: i8) {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for scroll");
        return;
    };
    let v = i32::from(dy) * 3;
    let h = i32::from(dx) * 3;
    let Ok(ev) = CGEvent::new_scroll_event(src, ScrollEventUnit::PIXEL, 2, v, h, 0) else {
        tracing::warn!("CGEvent::new_scroll_event failed");
        return;
    };
    tag_synthetic(&ev);
    ev.post(CGEventTapLocation::HID);
}

pub(in crate::inject) fn post_scroll(delta: ScrollDelta) {
    if let ScrollDelta::WheelTicks { x, y } = delta {
        if let Some(event) = wheel_scroll_event(x, y, None) {
            event.post(CGEventTapLocation::HID);
        }
        return;
    }

    let Ok(mut quantizer) = PIXEL_SCROLL_QUANTIZER.lock() else {
        tracing::warn!("macOS scroll quantizer mutex poisoned");
        return;
    };
    let delta = quantizer.quantize(delta, 1.0);
    drop(quantizer);
    if delta == QuantizedScroll::default() {
        return;
    }

    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for precise scroll");
        return;
    };
    let Ok(ev) = CGEvent::new_scroll_event(src, ScrollEventUnit::PIXEL, 2, delta.y, delta.x, 0)
    else {
        tracing::warn!("CGEvent::new_scroll_event failed for precise scroll");
        return;
    };
    set_continuous_scroll_fields(&ev, delta);
    tag_synthetic(&ev);
    ev.post(CGEventTapLocation::HID);
}

pub(in crate::inject) fn post_smooth_scroll(delta: ScrollDelta, _phase: SmoothScrollPhase) {
    post_continuous_scroll(delta, None);
}

pub(in crate::inject) fn post_phased_scroll(delta: ScrollDelta, phase: SmoothScrollPhase) {
    post_continuous_scroll(delta, Some(phase));
}

/// Post one continuous pixel event, phaseless (`None`) or stamped with a
/// gesture phase.
///
/// Phaseless is the default for smoothed wheel output: continuous events with
/// zeroed gesture/momentum phases scroll everywhere a wheel does, while a
/// Began/Changed/Ended stream declares a trackpad gesture — which switches
/// AppKit/WebKit into gesture handling (rubber-band overscroll appears on
/// wheel scrolls, and WebKit's gesture latching can starve sites that scroll
/// from JS wheel handlers). A phase is stamped only where a gesture is the
/// point, such as a horizontal swipe AppKit's recognisers must see.
fn post_continuous_scroll(delta: ScrollDelta, phase: Option<SmoothScrollPhase>) {
    if let ScrollDelta::WheelTicks { x, y } = delta {
        if phase.is_none() && x == 0.0 && y == 0.0 {
            return;
        }
        if let Some(event) = wheel_scroll_event(x, y, phase) {
            event.post(CGEventTapLocation::HID);
        }
        return;
    }

    let Ok(mut quantizer) = SMOOTH_SCROLL_QUANTIZER.lock() else {
        tracing::warn!("macOS smooth-scroll quantizer mutex poisoned");
        return;
    };
    let delta = quantizer.quantize(delta, 1.0);
    drop(quantizer);

    // Sub-pixel frames quantize to zero; posting a phaseless one would only
    // flood the event stream. A phased zero-distance frame still carries its
    // phase, and a gesture's Ended or Cancelled must always arrive.
    if phase.is_none() && delta == QuantizedScroll::default() {
        return;
    }
    if let Some(event) = continuous_scroll_event(delta, phase) {
        event.post(CGEventTapLocation::HID);
    }
}

/// Build (without posting) one tagged continuous pixel event.
fn continuous_scroll_event(
    delta: QuantizedScroll,
    phase: Option<SmoothScrollPhase>,
) -> Option<CGEvent> {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for smooth scroll");
        return None;
    };
    let Ok(ev) = CGEvent::new_scroll_event(src, ScrollEventUnit::PIXEL, 2, delta.y, delta.x, 0)
    else {
        tracing::warn!("CGEvent::new_scroll_event failed for smooth scroll");
        return None;
    };
    set_continuous_scroll_fields(&ev, delta);
    set_scroll_phase(&ev, phase);
    tag_synthetic(&ev);
    Some(ev)
}

/// Build (without posting) one tagged, non-continuous wheel event whose signed
/// 16.16 line distance keeps the fractional part instead of rounding to whole
/// lines.
fn wheel_scroll_event(x: f64, y: f64, phase: Option<SmoothScrollPhase>) -> Option<CGEvent> {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for wheel scroll");
        return None;
    };
    let Ok(ev) = CGEvent::new_scroll_event(src, ScrollEventUnit::LINE, 2, 0, 0, 0) else {
        tracing::warn!("CGEvent::new_scroll_event failed for wheel scroll");
        return None;
    };
    ev.set_double_value_field(EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_1, y);
    ev.set_double_value_field(EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_2, x);
    ev.set_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS, 0);
    set_scroll_phase(&ev, phase);
    tag_synthetic(&ev);
    Some(ev)
}

fn set_scroll_phase(event: &CGEvent, phase: Option<SmoothScrollPhase>) {
    if let Some(phase) = phase {
        event.set_integer_value_field(SCROLL_PHASE, scroll_phase_value(phase));
        event.set_integer_value_field(MOMENTUM_PHASE, 0);
    }
}

const fn scroll_phase_value(phase: SmoothScrollPhase) -> i64 {
    match phase {
        SmoothScrollPhase::Began => 1,
        SmoothScrollPhase::Changed => 2,
        SmoothScrollPhase::Ended => 4,
        SmoothScrollPhase::Cancelled => 8,
    }
}

fn set_continuous_scroll_fields(event: &CGEvent, delta: QuantizedScroll) {
    event.set_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS, 1);
    set_continuous_axis(
        event,
        delta.y,
        EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1,
        EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_1,
        EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1,
    );
    set_continuous_axis(
        event,
        delta.x,
        EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2,
        EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_2,
        EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2,
    );
}

fn set_continuous_axis(
    event: &CGEvent,
    points: i32,
    line_field: u32,
    fixed_field: u32,
    point_field: u32,
) {
    const POINTS_PER_LINE: i64 = 10;
    const FIXED_POINT_SCALE: i64 = 1 << 16;
    let points = i64::from(points);
    // Write order is load-bearing: a scroll CGEvent keeps one canonical
    // distance, and writing the coarse integer *line* field makes it the
    // canonical value — the point delta is then re-derived from it at 8
    // points per line, discarding pixel precision (measured on macOS 15.7:
    // frames under 10 points arrived as zero and the rest quantized to
    // 8-point steps). Writing the point delta last keeps it authoritative.
    event.set_integer_value_field(line_field, points / POINTS_PER_LINE);
    event.set_integer_value_field(fixed_field, points * FIXED_POINT_SCALE / POINTS_PER_LINE);
    event.set_integer_value_field(point_field, points);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn horizontal(x: i32) -> QuantizedScroll {
        QuantizedScroll { x, y: 0 }
    }

    /// Smoothed wheel output must stay phaseless: a phase declares a trackpad
    /// gesture to AppKit/WebKit.
    #[test]
    fn smoothed_wheel_events_carry_no_phase() {
        let event = continuous_scroll_event(horizontal(10), None).expect("a scroll event");
        assert_eq!(event.get_integer_value_field(SCROLL_PHASE), 0);
        assert_eq!(
            event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS),
            1
        );
    }

    /// A phased gesture's phases reach the event, terminal zero-distance
    /// frames included, or swipe recognisers never see a gesture.
    #[test]
    fn phased_gesture_events_carry_every_phase() {
        for (phase, delta, expected) in [
            (SmoothScrollPhase::Began, horizontal(10), 1),
            (SmoothScrollPhase::Changed, horizontal(-10), 2),
            (SmoothScrollPhase::Ended, horizontal(0), 4),
            (SmoothScrollPhase::Cancelled, horizontal(0), 8),
        ] {
            let event = continuous_scroll_event(delta, Some(phase)).expect("a scroll event");
            assert_eq!(
                event.get_integer_value_field(SCROLL_PHASE),
                expected,
                "{phase:?}"
            );
            assert_eq!(event.get_integer_value_field(MOMENTUM_PHASE), 0);
            assert_eq!(
                event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA),
                crate::SYNTHETIC_EVENT_USER_DATA
            );
        }
    }

    #[test]
    fn wheel_event_preserves_fractional_distance_when_phased() {
        let fields = [
            EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_1,
            EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_2,
        ];
        let phaseless = wheel_scroll_event(-0.025, 0.05, None).expect("a wheel event");
        let before = fields.map(|field| phaseless.get_double_value_field(field));
        assert!((before[0] - 0.05).abs() < 1.0e-4);
        assert!((before[1] + 0.025).abs() < 1.0e-4);
        assert_eq!(phaseless.get_integer_value_field(SCROLL_PHASE), 0);

        let phased = wheel_scroll_event(-0.025, 0.05, Some(SmoothScrollPhase::Changed))
            .expect("a wheel event");
        let after = fields.map(|field| phased.get_double_value_field(field));
        assert!((after[0] - before[0]).abs() < f64::EPSILON);
        assert!((after[1] - before[1]).abs() < f64::EPSILON);
        assert_eq!(
            phased.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS),
            0
        );
        assert_eq!(
            phased.get_integer_value_field(SCROLL_PHASE),
            scroll_phase_value(SmoothScrollPhase::Changed)
        );
        assert_eq!(
            phased.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA),
            crate::SYNTHETIC_EVENT_USER_DATA
        );
    }
}
