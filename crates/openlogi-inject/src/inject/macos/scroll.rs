//! Synthetic scroll on macOS: the one-tick scroll actions, the quantised wheel, and the continuous smooth-scroll phases.

use crate::inject::{QuantizedScroll, ScrollQuantizer, SmoothScrollPhase};
use core_graphics::event::{CGEvent, CGEventTapLocation, EventField, ScrollEventUnit};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use openlogi_core::scroll::ScrollDelta;
use std::sync::{LazyLock, Mutex};

use super::tag_synthetic;

static LINE_SCROLL_QUANTIZER: LazyLock<Mutex<ScrollQuantizer>> =
    LazyLock::new(|| Mutex::new(ScrollQuantizer::default()));
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
    let (quantizer, unit) = match delta {
        ScrollDelta::Pixels { .. } => (&PIXEL_SCROLL_QUANTIZER, ScrollEventUnit::PIXEL),
        ScrollDelta::WheelTicks { .. } => (&LINE_SCROLL_QUANTIZER, ScrollEventUnit::LINE),
    };
    let Ok(mut quantizer) = quantizer.lock() else {
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
    let Ok(ev) = CGEvent::new_scroll_event(src, unit, 2, delta.y, delta.x, 0) else {
        tracing::warn!("CGEvent::new_scroll_event failed for precise scroll");
        return;
    };
    if unit == ScrollEventUnit::PIXEL {
        set_continuous_scroll_fields(&ev, delta);
    }
    tag_synthetic(&ev);
    ev.post(CGEventTapLocation::HID);
}

pub(in crate::inject) fn post_smooth_scroll(delta: ScrollDelta, phase: SmoothScrollPhase) {
    const POINTS_PER_WHEEL_TICK: f64 = 10.0;

    let units_per_input = match delta {
        ScrollDelta::Pixels { .. } => 1.0,
        ScrollDelta::WheelTicks { .. } => POINTS_PER_WHEEL_TICK,
    };
    let Ok(mut quantizer) = SMOOTH_SCROLL_QUANTIZER.lock() else {
        tracing::warn!("macOS smooth-scroll quantizer mutex poisoned");
        return;
    };
    let delta = quantizer.quantize(delta, units_per_input);
    drop(quantizer);

    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for smooth scroll");
        return;
    };
    let Ok(ev) = CGEvent::new_scroll_event(src, ScrollEventUnit::PIXEL, 2, delta.y, delta.x, 0)
    else {
        tracing::warn!("CGEvent::new_scroll_event failed for smooth scroll");
        return;
    };
    set_continuous_scroll_fields(&ev, delta);
    ev.set_integer_value_field(SCROLL_PHASE, scroll_phase_value(phase));
    ev.set_integer_value_field(MOMENTUM_PHASE, 0);
    tag_synthetic(&ev);
    ev.post(CGEventTapLocation::HID);
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
    event.set_integer_value_field(point_field, points);
    event.set_integer_value_field(line_field, points / POINTS_PER_LINE);
    event.set_integer_value_field(fixed_field, points * FIXED_POINT_SCALE / POINTS_PER_LINE);
}
