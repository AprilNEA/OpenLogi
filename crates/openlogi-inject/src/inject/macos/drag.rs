//! Trackpad gestures driven by a held button's drag: a two-finger swipe
//! (continuous, phased scroll with its gesture companion and momentum on
//! release) and a three-finger swipe (a Dock swipe whose progress follows the
//! hand). The event layouts follow Mac Mouse Fix's `GestureScrollSimulator`
//! and `TouchSimulator`, which have driven these gestures from a mouse for
//! years: <https://github.com/noah-nuebling/mac-mouse-fix>.
//!
//! Raw-XY diversion keeps the system pointer still during the drag, as a
//! trackpad's swipes do: OpenLogi cannot tell when a page has stopped
//! scrolling, so a pointer following the hand would run off the page at its
//! end. Scroll events go where the pointer is.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use core_graphics::display::CGDisplay;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGEvent, CGEventField, CGEventTapLocation, CGEventType};
use openlogi_core::binding::HeldDrag;

use super::super::flick::FlickVelocity;
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
/// Weight of the newest raw-XY report in a three-finger drag's velocity: a
/// short average, so one stray report as the hand lets go cannot flip it.
const DOCK_VELOCITY_WEIGHT: f64 = 0.35;
/// A three-finger drag whose hand rested this long before release ends with
/// no exit velocity: the Dock then decides from how far it was dragged.
const DOCK_REST: Duration = Duration::from_millis(60);

/// Momentum decelerates as `dv/dt = -DRAG * v^DRAG_EXPONENT` (points/s), and
/// stops below `STOP_SPEED`. Mac Mouse Fix's constants for its swipes.
const DRAG: f64 = 30.0;
const DRAG_EXPONENT: f64 = 0.7;
const STOP_SPEED: f64 = 1.0;
const MOMENTUM_FRAME: Duration = Duration::from_micros(8_333);

/// Travel, in points, a two-finger drag makes before it has a heading to turn
/// back from.
const TURN_MIN_LEG: f64 = 8.0;
/// Travel, in points, back against the heading that counts as turning round
/// rather than a wobble of the hand.
const TURN_BACK: f64 = 6.0;

/// The phase a two-finger frame carries. Touch, momentum and rest frames are
/// distinct variants, so a frame can never pair a touch phase with a momentum
/// phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollPhase {
    /// The hand is on the "trackpad".
    Touch(Touch),
    /// The glide after the hand let go.
    Momentum(Momentum),
    /// The pair a trackpad sends at rest after a glide.
    Rest(Rest),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Touch {
    Began,
    Changed,
    Ended,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Momentum {
    Begin,
    Continue,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rest {
    MayBegin,
    Cancelled,
}

impl ScrollPhase {
    /// The scroll event's `(scroll phase, momentum phase)` fields: an
    /// `IOHIDEventPhaseBits` value and a `CGMomentumScrollPhase`.
    const fn fields(self) -> (i64, i64) {
        match self {
            Self::Touch(Touch::Began) => (1, 0),
            Self::Touch(Touch::Changed) => (2, 0),
            Self::Touch(Touch::Ended) => (4, 0),
            Self::Touch(Touch::Cancelled) | Self::Rest(Rest::Cancelled) => (8, 0),
            Self::Momentum(Momentum::Begin) => (0, 1),
            Self::Momentum(Momentum::Continue) => (0, 2),
            Self::Momentum(Momentum::End) => (0, 3),
            Self::Rest(Rest::MayBegin) => (128, 0),
        }
    }
}

/// The phase of a three-finger frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DockPhase {
    Began,
    Changed,
    Ended,
    Cancelled,
}

impl DockPhase {
    /// The `IOHIDEventPhaseBits` value the Dock reads.
    const fn bits(self) -> u32 {
        match self {
            Self::Began => 1,
            Self::Changed => 2,
            Self::Ended => 4,
            Self::Cancelled => 8,
        }
    }
}

/// The momentum glide a released two-finger drag launched, on its own thread.
/// Dropping it leaves the glide running; [`Self::stop`] ends it at the next
/// frame.
pub(in crate::inject) struct Glide {
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}

impl Glide {
    pub(in crate::inject) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub(in crate::inject) fn is_running(&self) -> bool {
        !self.done.load(Ordering::Relaxed)
    }
}

/// One held-button drag in progress.
pub(in crate::inject) struct Drag {
    gesture: Gesture,
}

enum Gesture {
    Scroll(ScrollDrag),
    Dock(DockDrag),
}

impl Drag {
    /// Begin `kind` with the first motion, in sensor counts, whose report
    /// arrived `at`.
    pub(in crate::inject) fn begin(kind: HeldDrag, dx: f64, dy: f64, at: Instant) -> Self {
        let bounds = CGDisplay::main().bounds();
        Self::begin_on((bounds.size.width, bounds.size.height), kind, dx, dy, at)
    }

    /// [`Self::begin`] on a main display of `screen` points (width, height):
    /// the one live lookup stays in `begin`, so tests pass a fixed size.
    fn begin_on(screen: (f64, f64), kind: HeldDrag, dx: f64, dy: f64, at: Instant) -> Self {
        let (dx, dy) = (dx * POINTS_PER_COUNT, dy * POINTS_PER_COUNT);
        let gesture = match kind {
            HeldDrag::TwoFingerSwipe => Gesture::Scroll(ScrollDrag::begin(dx, dy, at)),
            HeldDrag::ThreeFingerSwipe => Gesture::Dock(DockDrag::begin(screen, dx, dy, at)),
        };
        Self { gesture }
    }

    /// Follow further motion, in sensor counts, whose report arrived `at`.
    pub(in crate::inject) fn moved(&mut self, dx: f64, dy: f64, at: Instant) {
        let (dx, dy) = (dx * POINTS_PER_COUNT, dy * POINTS_PER_COUNT);
        match &mut self.gesture {
            Gesture::Scroll(drag) => drag.moved(dx, dy, at),
            Gesture::Dock(drag) => drag.moved(dx, dy, at),
        }
    }

    /// The button was released `at`: finish the gesture as a trackpad lift
    /// does, and hand back the glide a two-finger flick launched.
    pub(in crate::inject) fn release(self, at: Instant) -> Option<Glide> {
        match self.gesture {
            Gesture::Scroll(drag) => drag.release(at),
            Gesture::Dock(drag) => {
                drag.release(at);
                None
            }
        }
    }

    /// The press ended without a release (its capture session or binding
    /// went away): end the gesture with no glide, and let the Dock spring
    /// back.
    pub(in crate::inject) fn cancel(self) {
        match self.gesture {
            Gesture::Scroll(drag) => drag.cancel(),
            Gesture::Dock(drag) => drag.cancel(),
        }
    }
}

/// A two-finger swipe: continuous scroll that follows the hand.
struct ScrollDrag {
    natural: bool,
    pixels: SubPixel,
    flick: FlickVelocity,
    turn: Turn,
}

impl ScrollDrag {
    fn begin(dx: f64, dy: f64, at: Instant) -> Self {
        let mut drag = Self {
            natural: spaces::natural_scrolling(),
            pixels: SubPixel::default(),
            flick: FlickVelocity::default(),
            turn: Turn::default(),
        };
        drag.turn.reversed(dx, dy);
        drag.post(dx, dy, Touch::Began, at);
        drag
    }

    /// A turn back against the heading lifts the "fingers" and puts them down
    /// again. An app ties a touch to the scroll view that takes its first
    /// motion, so a drag that starts by pushing into a page's end is tied to
    /// a view that cannot move, and pulling back never scrolls the page. On
    /// a trackpad the fingers lift soon; a held button may never let go.
    fn moved(&mut self, dx: f64, dy: f64, at: Instant) {
        if self.turn.reversed(dx, dy) {
            post_scroll_frame(0.0, 0.0, ScrollPhase::Touch(Touch::Ended), self.natural);
            self.pixels = SubPixel::default();
            self.post(dx, dy, Touch::Began, at);
        } else {
            self.post(dx, dy, Touch::Changed, at);
        }
    }

    fn post(&mut self, dx: f64, dy: f64, touch: Touch, at: Instant) {
        self.flick.sample(dx, dy, at);
        let (px, py) = self.pixels.take(dx, dy);
        if touch == Touch::Changed && px == 0.0 && py == 0.0 {
            return;
        }
        post_scroll_frame(px, py, ScrollPhase::Touch(touch), self.natural);
    }

    fn release(self, at: Instant) -> Option<Glide> {
        post_scroll_frame(0.0, 0.0, ScrollPhase::Touch(Touch::Ended), self.natural);
        let velocity = self.flick.exit(at, MOMENTUM_MAX_IDLE)?;
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let (thread_stop, thread_done) = (Arc::clone(&stop), Arc::clone(&done));
        let natural = self.natural;
        let spawned = thread::Builder::new()
            .name("openlogi-drag-momentum".into())
            .spawn(move || {
                glide(velocity, &thread_stop, natural);
                thread_done.store(true, Ordering::Relaxed);
            });
        match spawned {
            Ok(_) => Some(Glide { stop, done }),
            Err(error) => {
                tracing::warn!(?error, "two-finger drag: momentum thread failed to start");
                None
            }
        }
    }

    fn cancel(self) {
        post_scroll_frame(0.0, 0.0, ScrollPhase::Touch(Touch::Cancelled), self.natural);
    }
}

/// Post the momentum glide after a two-finger swipe, as a trackpad driver
/// does: apps that leave momentum to the OS (Safari) keep moving, and the
/// glide ends with the MayBegin/Cancelled pair a trackpad sends at rest.
fn glide(velocity: (f64, f64), stop: &AtomicBool, natural: bool) {
    let speed = velocity.0.hypot(velocity.1);
    if speed <= STOP_SPEED {
        return;
    }
    let unit = (velocity.0 / speed, velocity.1 / speed);
    let mut speed = speed;
    let mut pixels = SubPixel::default();
    let mut phase = Momentum::Begin;
    let dt = MOMENTUM_FRAME.as_secs_f64();
    loop {
        thread::sleep(MOMENTUM_FRAME);
        if stop.load(Ordering::Relaxed) {
            post_scroll_frame(0.0, 0.0, ScrollPhase::Momentum(Momentum::End), natural);
            return;
        }
        speed -= DRAG * speed.powf(DRAG_EXPONENT) * dt;
        if speed <= STOP_SPEED {
            break;
        }
        let (px, py) = pixels.take(unit.0 * speed * dt, unit.1 * speed * dt);
        post_scroll_frame(px, py, ScrollPhase::Momentum(phase), natural);
        phase = Momentum::Continue;
    }
    post_scroll_frame(0.0, 0.0, ScrollPhase::Momentum(Momentum::End), natural);
    post_scroll_frame(0.0, 0.0, ScrollPhase::Rest(Rest::MayBegin), natural);
    post_scroll_frame(0.0, 0.0, ScrollPhase::Rest(Rest::Cancelled), natural);
}

/// Tells a two-finger drag turning back from one moving on or wobbling. The
/// heading is the drag's travel since its touch began. A motion against it
/// counts toward turning back, and one with it clears the count.
#[derive(Default)]
struct Turn {
    leg: (f64, f64),
    back: f64,
}

impl Turn {
    /// Take one motion; `true` when it completes a turn back, which starts a
    /// new leg heading the new way.
    fn reversed(&mut self, dx: f64, dy: f64) -> bool {
        let length = self.leg.0.hypot(self.leg.1);
        if length < TURN_MIN_LEG {
            self.leg = (self.leg.0 + dx, self.leg.1 + dy);
            return false;
        }
        let along = (dx * self.leg.0 + dy * self.leg.1) / length;
        if along >= 0.0 {
            self.back = 0.0;
            self.leg = (self.leg.0 + dx, self.leg.1 + dy);
            return false;
        }
        self.back -= along;
        if self.back < TURN_BACK {
            return false;
        }
        *self = Self {
            leg: (dx, dy),
            back: 0.0,
        };
        true
    }
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

/// One continuous scroll event plus, inside a touch or rest phase, the
/// gesture event that marks it as trackpad input. Positive deltas move the
/// content right and down, with the hand.
/// `natural` is the natural-scrolling flag a trackpad stamps on its events.
fn post_scroll_frame(px: f64, py: f64, phase: ScrollPhase, natural: bool) {
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
    let (scroll_phase, momentum_phase) = phase.fields();
    for (field, value) in [
        (88, 1),                   // kCGScrollWheelEventIsContinuous
        (137, i64::from(natural)), // directionInvertedFromDevice
        (11, line_int(py)),
        (96, point(py)),
        (93, fixed(py)),
        (12, line_int(px)),
        (97, point(px)),
        (94, fixed(px)),
        (99, scroll_phase),
        (123, momentum_phase),
    ] {
        CGEvent::set_integer_value_field(Some(&scroll), CGEventField(field), value);
    }
    tag(&scroll);
    post(&scroll);
    if matches!(phase, ScrollPhase::Momentum(_)) {
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
    CGEvent::set_integer_value_field(Some(&gesture), CGEventField(132), scroll_phase);
    tag(&gesture);
    post(&gesture);
}

/// Post one drag event at the session tap: the single exit for every event a
/// drag sends, so tests can record what a drag would post instead.
pub(super) fn post(event: &CFRetained<CGEvent>) {
    #[cfg(test)]
    if recording::capture(event) {
        return;
    }
    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(event));
}

/// Records what drags post while a test holds a [`recording::Recording`],
/// and posts nothing then.
#[cfg(test)]
pub(super) mod recording {
    use std::sync::{Mutex, MutexGuard, PoisonError};

    use objc2_core_foundation::CFRetained;
    use objc2_core_graphics::{CGEvent, CGEventField};

    /// The window-server type and scroll phase of each event posted while
    /// recording.
    static POSTED: Mutex<Option<Vec<(i64, i64)>>> = Mutex::new(None);
    /// One recording at a time: tests run in parallel.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    pub(in crate::inject) struct Recording {
        _turn: MutexGuard<'static, ()>,
    }

    impl Recording {
        pub(in crate::inject) fn start() -> Self {
            let turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
            *POSTED.lock().unwrap_or_else(PoisonError::into_inner) = Some(Vec::new());
            Self { _turn: turn }
        }

        /// The type (`kCGSEventTypeField`) of every event posted so far.
        pub(in crate::inject) fn types(&self) -> Vec<i64> {
            self.posted().into_iter().map(|(kind, _)| kind).collect()
        }

        /// The scroll phase (`kCGScrollWheelEventScrollPhase`) of every scroll
        /// event posted so far.
        pub(in crate::inject) fn scroll_phases(&self) -> Vec<i64> {
            self.posted()
                .into_iter()
                .filter(|(kind, _)| *kind == 22)
                .map(|(_, phase)| phase)
                .collect()
        }

        #[expect(
            clippy::unused_self,
            reason = "reading needs the recording held, so its events are this test's"
        )]
        fn posted(&self) -> Vec<(i64, i64)> {
            POSTED
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .unwrap_or_default()
        }
    }

    impl Drop for Recording {
        fn drop(&mut self) {
            *POSTED.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
    }

    /// Record `event` if a recording is on; `true` when it was.
    pub(super) fn capture(event: &CFRetained<CGEvent>) -> bool {
        let mut posted = POSTED.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(posted) = posted.as_mut() else {
            return false;
        };
        posted.push((
            CGEvent::integer_value_field(Some(event), CGEventField(55)),
            CGEvent::integer_value_field(Some(event), CGEventField(99)),
        ));
        true
    }
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
struct DockDrag {
    axis: DockAxis,
    scale: f64,
    natural: bool,
    progress: f64,
    /// Smoothed progress per report, signed like `progress`.
    velocity: f64,
    /// Arrival of the last report that moved the swipe.
    moved_at: Instant,
}

impl DockDrag {
    fn begin((width, height): (f64, f64), dx: f64, dy: f64, at: Instant) -> Self {
        let (axis, scale) = if dx.abs() >= dy.abs() {
            (
                DockAxis::Horizontal,
                -SPACE_OFFSET / (width + SPACE_SEPARATOR_WIDTH),
            )
        } else {
            (DockAxis::Vertical, 1.0 / height)
        };
        let mut drag = Self {
            axis,
            scale,
            natural: spaces::natural_scrolling(),
            progress: 0.0,
            velocity: 0.0,
            moved_at: at,
        };
        drag.post(dx, dy, DockPhase::Began, at);
        drag
    }

    fn moved(&mut self, dx: f64, dy: f64, at: Instant) {
        self.post(dx, dy, DockPhase::Changed, at);
    }

    fn post(&mut self, dx: f64, dy: f64, phase: DockPhase, at: Instant) {
        let delta = match self.axis {
            DockAxis::Horizontal => dx,
            DockAxis::Vertical => dy,
        } * self.scale;
        if phase == DockPhase::Changed && delta == 0.0 {
            return;
        }
        self.progress += delta;
        self.velocity += DOCK_VELOCITY_WEIGHT * (delta - self.velocity);
        self.moved_at = self.moved_at.max(at);
        spaces::post_dock_drag(self.axis, phase.bits(), self.progress, None, self.natural);
    }

    fn release(self, at: Instant) {
        // A hand that came to rest lets the Dock decide from the distance; a
        // hand still moving back cancels the swipe, as on a trackpad. The
        // velocity is averaged, so a wobble as the button lets go does not.
        let velocity = if at.saturating_duration_since(self.moved_at) > DOCK_REST {
            0.0
        } else {
            self.velocity
        };
        let phase = if velocity != 0.0 && velocity.signum() != self.progress.signum() {
            DockPhase::Cancelled
        } else {
            DockPhase::Ended
        };
        tracing::debug!(
            axis = ?self.axis,
            progress = self.progress,
            velocity,
            ?phase,
            "three-finger drag released"
        );
        spaces::post_dock_drag(
            self.axis,
            phase.bits(),
            self.progress,
            Some(velocity * 100.0),
            self.natural,
        );
    }

    fn cancel(self) {
        spaces::post_dock_drag(
            self.axis,
            DockPhase::Cancelled.bits(),
            self.progress,
            Some(0.0),
            self.natural,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `kCGEventMouseMoved`: what a drag moving the pointer posts.
    const MOUSE_MOVED: i64 = 5;

    /// A drag of `kind`: begin, two moves, a quick release, and any glide it
    /// launched, stopped after a few frames.
    fn play(kind: HeldDrag) {
        let start = Instant::now();
        let ms = |n| start + Duration::from_millis(n);
        // A fixed screen: the test never reads the Mac's display geometry.
        let mut drag = Drag::begin_on((1440.0, 900.0), kind, 20.0, 4.0, ms(0));
        drag.moved(10.0, 2.0, ms(8));
        drag.moved(12.0, 2.0, ms(16));
        if let Some(glide) = drag.release(ms(20)) {
            thread::sleep(Duration::from_millis(40));
            glide.stop();
            let deadline = Instant::now() + Duration::from_secs(2);
            while glide.is_running() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            assert!(!glide.is_running(), "the glide stops");
        }
    }

    /// The fix for the shaking pointer and the freeze after release: a drag
    /// never moves the pointer. Reverting to the pointer-following drag of
    /// 0374959d posts `MOUSE_MOVED` events here and fails.
    #[test]
    fn drag_pointer_stays_still() {
        let recording = recording::Recording::start();
        play(HeldDrag::TwoFingerSwipe);
        play(HeldDrag::ThreeFingerSwipe);
        let posted = recording.types();
        assert!(
            posted.contains(&22),
            "the two-finger drag scrolled: {posted:?}"
        );
        assert!(
            posted.contains(&30),
            "the three-finger drag swiped: {posted:?}"
        );
        assert!(
            !posted.contains(&MOUSE_MOVED),
            "the pointer moved: {posted:?}"
        );
    }

    /// The fix for a page that would not scroll back after the drag pushed
    /// past its end: turning back starts a new touch (Ended, then Began).
    /// Without it the whole drag is one touch and this fails.
    #[test]
    fn a_two_finger_drag_turning_back_starts_a_new_touch() {
        let recording = recording::Recording::start();
        let start = Instant::now();
        let ms = |n| start + Duration::from_millis(n);
        let mut drag = Drag::begin_on((1440.0, 900.0), HeldDrag::TwoFingerSwipe, 0.0, 20.0, ms(0));
        drag.moved(0.0, 40.0, ms(8));
        drag.moved(0.0, -20.0, ms(16));
        drag.moved(0.0, -20.0, ms(24));
        drag.cancel();
        // 1 Began, 2 Changed, 4 Ended, 8 Cancelled.
        assert_eq!(recording.scroll_phases(), [1, 2, 4, 1, 2, 8]);
    }

    #[test]
    fn a_turn_is_a_clear_turn_back_not_a_wobble() {
        // Moving on, or drifting sideways, never turns.
        let mut turn = Turn::default();
        assert!(!turn.reversed(0.0, 10.0));
        assert!(!turn.reversed(0.0, 30.0));
        assert!(!turn.reversed(25.0, 0.0));
        // A small wobble back is not a turn, and moving on clears it.
        assert!(!turn.reversed(0.0, -4.0));
        assert!(!turn.reversed(0.0, 2.0));
        assert!(!turn.reversed(0.0, -4.0));
        // A clear pull back is, once.
        assert!(turn.reversed(0.0, -4.0));
        assert!(!turn.reversed(0.0, -40.0));
        // Turning round again is a second turn.
        assert!(!turn.reversed(0.0, 3.0));
        assert!(turn.reversed(0.0, 5.0));

        // Before the drag has a heading, nothing is a turn.
        let mut start = Turn::default();
        assert!(!start.reversed(0.0, 3.0));
        assert!(!start.reversed(0.0, -10.0));
    }

    #[test]
    fn scroll_phases_never_mix_touch_and_momentum() {
        let all = [
            ScrollPhase::Touch(Touch::Began),
            ScrollPhase::Touch(Touch::Changed),
            ScrollPhase::Touch(Touch::Ended),
            ScrollPhase::Touch(Touch::Cancelled),
            ScrollPhase::Momentum(Momentum::Begin),
            ScrollPhase::Momentum(Momentum::Continue),
            ScrollPhase::Momentum(Momentum::End),
            ScrollPhase::Rest(Rest::MayBegin),
            ScrollPhase::Rest(Rest::Cancelled),
        ];
        for phase in all {
            let (touch, momentum) = phase.fields();
            assert!(touch == 0 || momentum == 0, "{phase:?} sets both phases");
            assert_eq!(
                momentum != 0,
                matches!(phase, ScrollPhase::Momentum(_)),
                "{phase:?}"
            );
        }
    }
}
