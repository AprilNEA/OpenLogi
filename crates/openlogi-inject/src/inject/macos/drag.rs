//! Trackpad gestures driven by a held button's drag: a two-finger swipe
//! (continuous, phased scroll with its gesture companion and momentum on
//! release) and a three-finger swipe (a Dock swipe whose progress follows the
//! hand). The event layouts follow Mac Mouse Fix's `GestureScrollSimulator`
//! and `TouchSimulator`, which have driven these gestures from a mouse for
//! years: <https://github.com/noah-nuebling/mac-mouse-fix>.

use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use core_graphics::display::CGDisplay;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGEvent, CGEventField, CGEventTapLocation, CGEventType};
use openlogi_core::binding::HeldDrag;

use super::spaces::{self, DockAxis};

/// Points of gesture travel per raw-XY sensor count. Raw XY reports the
/// sensor's own counts, unaccelerated; one count per point makes the content
/// follow the hand at the sensor resolutions MX mice ship with.
const POINTS_PER_COUNT: f64 = 1.0;

/// How much faster a two-finger swipe's gesture deltas run than its scroll
/// deltas. Mac Mouse Fix found 1.67 makes swiping between pages in Safari as
/// easy to trigger as on a trackpad.
const GESTURE_SCALE: f64 = 1.67;

/// Points per scroll line, `CGEventSource`'s default.
const POINTS_PER_LINE: f64 = 10.0;

/// Horizontal Dock-swipe progress per point, times the screen width: how far
/// one Space is in a swipe. Mac Mouse Fix measured `1 + 1 / (spaces - 1)` per
/// `width + 63` points; 1.5 sits between two and three Spaces.
const SPACE_OFFSET: f64 = 1.5;
const SPACE_SEPARATOR_WIDTH: f64 = 63.0;

/// A release later than this after the last motion starts no momentum: the
/// hand had stopped.
const MOMENTUM_MAX_IDLE: Duration = Duration::from_millis(50);
/// Momentum decelerates as `dv/dt = -DRAG * v^DRAG_EXPONENT` (points/s), and
/// stops below `STOP_SPEED`. Mac Mouse Fix's constants for its swipes.
const DRAG: f64 = 30.0;
const DRAG_EXPONENT: f64 = 0.7;
const STOP_SPEED: f64 = 1.0;
const MOMENTUM_FRAME: Duration = Duration::from_micros(8_333);

// IOHIDEventPhaseBits.
const PHASE_NONE: i64 = 0;
const PHASE_BEGAN: i64 = 1;
const PHASE_CHANGED: i64 = 2;
const PHASE_ENDED: i64 = 4;
const PHASE_CANCELLED: i64 = 8;
const PHASE_MAY_BEGIN: i64 = 128;
// CGMomentumScrollPhase.
const MOMENTUM_NONE: i64 = 0;
const MOMENTUM_BEGIN: i64 = 1;
const MOMENTUM_CONTINUE: i64 = 2;
const MOMENTUM_END: i64 = 3;

/// Bumped by every new two-finger drag, so a momentum glide still running
/// from the previous one stops: a fresh touch catches the content.
static MOMENTUM_GENERATION: AtomicU64 = AtomicU64::new(0);

/// One held-button drag in progress.
pub(in crate::inject) enum Drag {
    Scroll(ScrollDrag),
    Dock(DockDrag),
}

impl Drag {
    /// Begin `kind` with the first motion, in sensor counts.
    pub(in crate::inject) fn begin(kind: HeldDrag, dx: f64, dy: f64) -> Self {
        let (dx, dy) = (dx * POINTS_PER_COUNT, dy * POINTS_PER_COUNT);
        match kind {
            HeldDrag::TwoFingerSwipe => Self::Scroll(ScrollDrag::begin(dx, dy)),
            HeldDrag::ThreeFingerSwipe => Self::Dock(DockDrag::begin(dx, dy)),
        }
    }

    /// Follow further motion, in sensor counts.
    pub(in crate::inject) fn moved(&mut self, dx: f64, dy: f64) {
        let (dx, dy) = (dx * POINTS_PER_COUNT, dy * POINTS_PER_COUNT);
        match self {
            Self::Scroll(drag) => drag.moved(dx, dy),
            Self::Dock(drag) => drag.moved(dx, dy),
        }
    }

    /// The button was released.
    pub(in crate::inject) fn end(self) {
        match self {
            Self::Scroll(drag) => drag.end(),
            Self::Dock(drag) => drag.end(),
        }
    }
}

/// A two-finger swipe: continuous scroll that follows the hand.
pub(in crate::inject) struct ScrollDrag {
    natural: bool,
    pixels: SubPixel,
    last: (f64, f64),
    last_at: Instant,
    previous_at: Instant,
}

impl ScrollDrag {
    fn begin(dx: f64, dy: f64) -> Self {
        MOMENTUM_GENERATION.fetch_add(1, Ordering::Relaxed);
        let now = Instant::now();
        let mut drag = Self {
            natural: spaces::natural_scrolling(),
            pixels: SubPixel::default(),
            last: (0.0, 0.0),
            last_at: now,
            previous_at: now,
        };
        drag.post(dx, dy, PHASE_BEGAN);
        drag
    }

    fn moved(&mut self, dx: f64, dy: f64) {
        self.post(dx, dy, PHASE_CHANGED);
    }

    fn post(&mut self, dx: f64, dy: f64, phase: i64) {
        let (px, py) = self.pixels.take(dx, dy);
        if phase == PHASE_CHANGED && px == 0.0 && py == 0.0 {
            return;
        }
        post_scroll_frame(px, py, phase, MOMENTUM_NONE, self.natural);
        self.previous_at = self.last_at;
        self.last_at = Instant::now();
        self.last = (px, py);
    }

    fn end(self) {
        post_scroll_frame(0.0, 0.0, PHASE_ENDED, MOMENTUM_NONE, self.natural);
        let interval = self.last_at.duration_since(self.previous_at).as_secs_f64();
        if self.last_at.elapsed() > MOMENTUM_MAX_IDLE || interval <= 0.0 {
            return;
        }
        let velocity = (self.last.0 / interval, self.last.1 / interval);
        let generation = MOMENTUM_GENERATION.load(Ordering::Relaxed);
        let natural = self.natural;
        let spawned = thread::Builder::new()
            .name("openlogi-drag-momentum".into())
            .spawn(move || glide(velocity, generation, natural));
        if let Err(error) = spawned {
            tracing::warn!(?error, "two-finger drag: momentum thread failed to start");
        }
    }
}

/// Post the momentum glide after a two-finger swipe, as a trackpad driver
/// does: apps that leave momentum to the OS (Safari) keep moving, and the
/// glide ends with the MayBegin/Cancelled pair a trackpad sends at rest.
fn glide(velocity: (f64, f64), generation: u64, natural: bool) {
    let speed = velocity.0.hypot(velocity.1);
    if speed <= STOP_SPEED {
        return;
    }
    let unit = (velocity.0 / speed, velocity.1 / speed);
    let mut speed = speed;
    let mut pixels = SubPixel::default();
    let mut phase = MOMENTUM_BEGIN;
    let dt = MOMENTUM_FRAME.as_secs_f64();
    loop {
        thread::sleep(MOMENTUM_FRAME);
        if MOMENTUM_GENERATION.load(Ordering::Relaxed) != generation {
            post_scroll_frame(0.0, 0.0, PHASE_NONE, MOMENTUM_END, natural);
            return;
        }
        speed -= DRAG * speed.powf(DRAG_EXPONENT) * dt;
        if speed <= STOP_SPEED {
            break;
        }
        let (px, py) = pixels.take(unit.0 * speed * dt, unit.1 * speed * dt);
        post_scroll_frame(px, py, PHASE_NONE, phase, natural);
        phase = MOMENTUM_CONTINUE;
    }
    post_scroll_frame(0.0, 0.0, PHASE_NONE, MOMENTUM_END, natural);
    post_scroll_frame(0.0, 0.0, PHASE_MAY_BEGIN, MOMENTUM_NONE, natural);
    post_scroll_frame(0.0, 0.0, PHASE_CANCELLED, MOMENTUM_NONE, natural);
}

/// Whole-point deltas out of fractional travel, carrying the remainder.
#[derive(Default)]
struct SubPixel {
    x: f64,
    y: f64,
}

impl SubPixel {
    fn take(&mut self, dx: f64, dy: f64) -> (f64, f64) {
        self.x += dx;
        self.y += dy;
        let (px, py) = (self.x.trunc(), self.y.trunc());
        self.x -= px;
        self.y -= py;
        (px, py)
    }
}

/// One continuous scroll event plus, inside a gesture phase, the gesture
/// event that marks it as trackpad input. Positive deltas move the content
/// right and down, with the hand.
fn post_scroll_frame(px: f64, py: f64, phase: i64, momentum: i64, natural: bool) {
    let Some(scroll) = CGEvent::new(None) else {
        return;
    };
    CGEvent::set_type(Some(&scroll), CGEventType::ScrollWheel);
    let line = |points: f64| points / POINTS_PER_LINE;
    // Native continuous events report a sub-line delta as one line.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a frame scrolls a few lines"
    )]
    let line_int = |points: f64| {
        let lines = line(points);
        let whole = if lines.abs() <= 1.0 {
            lines.signum() * lines.abs().ceil()
        } else {
            lines.trunc()
        };
        whole as i64
    };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "16.16 fixed point of a few lines"
    )]
    let fixed = |points: f64| (line(points) * 65_536.0).round() as i64;
    #[expect(clippy::cast_possible_truncation, reason = "whole points of one frame")]
    let point = |points: f64| points as i64;
    for (field, value) in [
        (88, 1),                   // kCGScrollWheelEventIsContinuous
        (137, i64::from(natural)), // directionInvertedFromDevice
        (11, line_int(py)),
        (96, point(py)),
        (93, fixed(py)),
        (12, line_int(px)),
        (97, point(px)),
        (94, fixed(px)),
        (99, phase),
        (123, momentum),
    ] {
        CGEvent::set_integer_value_field(Some(&scroll), CGEventField(field), value);
    }
    tag(&scroll);
    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&scroll));
    if phase == PHASE_NONE {
        return;
    }
    let Some(gesture) = CGEvent::new(None) else {
        return;
    };
    CGEvent::set_type(Some(&gesture), CGEventType(29));
    CGEvent::set_integer_value_field(Some(&gesture), CGEventField(110), 6); // kIOHIDEventTypeScroll
    // Native events carry -0.0 on a still axis.
    let gesture_delta = |points: f64| {
        if points == 0.0 {
            -0.0
        } else {
            points * GESTURE_SCALE
        }
    };
    CGEvent::set_double_value_field(Some(&gesture), CGEventField(116), gesture_delta(px));
    CGEvent::set_double_value_field(Some(&gesture), CGEventField(119), gesture_delta(py));
    CGEvent::set_integer_value_field(Some(&gesture), CGEventField(132), phase);
    tag(&gesture);
    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&gesture));
}

fn tag(event: &CFRetained<CGEvent>) {
    CGEvent::set_integer_value_field(
        Some(event),
        CGEventField::EventSourceUserData,
        super::super::SYNTHETIC_EVENT_USER_DATA,
    );
}

/// A three-finger swipe: a Dock swipe whose progress follows the hand. The
/// first motion picks the axis — sideways switches Spaces, vertical opens
/// Mission Control (up) or App Exposé (down) — and the release lets the Dock
/// finish or spring back from where the hand left it.
pub(in crate::inject) struct DockDrag {
    axis: DockAxis,
    scale: f64,
    natural: bool,
    progress: f64,
    last: f64,
}

impl DockDrag {
    fn begin(dx: f64, dy: f64) -> Self {
        let bounds = CGDisplay::main().bounds();
        let (axis, scale) = if dx.abs() >= dy.abs() {
            (
                DockAxis::Horizontal,
                -SPACE_OFFSET / (bounds.size.width + SPACE_SEPARATOR_WIDTH),
            )
        } else {
            (DockAxis::Vertical, 1.0 / bounds.size.height)
        };
        let mut drag = Self {
            axis,
            scale,
            natural: spaces::natural_scrolling(),
            progress: 0.0,
            last: 0.0,
        };
        drag.post(dx, dy, PHASE_BEGAN);
        drag
    }

    fn moved(&mut self, dx: f64, dy: f64) {
        self.post(dx, dy, PHASE_CHANGED);
    }

    fn post(&mut self, dx: f64, dy: f64, phase: i64) {
        let delta = match self.axis {
            DockAxis::Horizontal => dx,
            DockAxis::Vertical => dy,
        } * self.scale;
        if phase == PHASE_CHANGED && delta == 0.0 {
            return;
        }
        self.progress += delta;
        self.last = delta;
        spaces::post_dock_drag(
            self.axis,
            phase_u32(phase),
            self.progress,
            None,
            self.natural,
        );
    }

    fn end(self) {
        // The swipe completes when the hand was still moving the way it went;
        // a hand moving back cancels it, as on a trackpad.
        let phase = if self.last.signum() == self.progress.signum() {
            PHASE_ENDED
        } else {
            PHASE_CANCELLED
        };
        spaces::post_dock_drag(
            self.axis,
            phase_u32(phase),
            self.progress,
            Some(self.last * 100.0),
            self.natural,
        );
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "phase bits fit"
)]
const fn phase_u32(phase: i64) -> u32 {
    phase as u32
}
