//! Traditional wheel output owned by one dedicated worker.
//!
//! Hook callbacks submit typed wheel impulses through [`ScrollInputHandle`]
//! without blocking. The worker either scales and emits them directly,
//! evaluates finite smooth motion from absolute timestamps, or, with smoothing
//! off, wraps a diverted horizontal wheel in a phased gesture. Pixel-precise
//! input never enters this runtime, so native trackpad and continuous wheel
//! streams cannot be mixed with wheel ticks.
//!
//! With momentum scrolling on
//! ([`MomentumScrolling`](openlogi_core::config::MomentumScrolling)), a
//! smoothed main wheel also glides: its free-spin detector tells ratchet
//! scrolling from a SmartShift free spin, the page glides off at the speed
//! the wheel was spun (the `glide` model) and slows along one
//! [`GlideCurve`], and a finger stopping the wheel ends the glide at once.
//! Off, the wheel eases exactly as any other wheel does.

mod free_spin;
mod glide;
mod reverse_lock;
mod worker;

pub use worker::{ScrollInputHandle, ScrollPreferences, ScrollRuntime};

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use openlogi_core::config::MomentumParams;
use openlogi_core::scroll::{GlideCurve, ScrollDelta};
use openlogi_inject::SmoothScrollPhase;

use self::free_spin::{FreeSpinDetector, WheelPhase};
use self::reverse_lock::{Admit, ReverseLock};
use crate::runtime::HidppSessionId;

/// Duration of every [`Easing::Smooth`] segment, including a segment
/// restarted by retargeting.
const ANIMATION_DURATION: Duration = Duration::from_millis(100);
/// Duration of every [`Easing::Crisp`] segment.
const CRISP_STEP_DURATION: Duration = Duration::from_millis(40);
/// Output cadence. Position is evaluated from absolute time, so delayed wakes
/// do not slow or lengthen the animation.
const FRAME_PERIOD: Duration = Duration::from_millis(8);
/// Quiet time after the last phased tick that ends the gesture. A wheel has no
/// release event, so idleness is the only end-of-gesture signal; the value sits
/// above the gap between ratchet ticks of a continuous spin and far below the
/// pause between separate spins.
const PHASED_IDLE: Duration = Duration::from_millis(120);
/// A push repeats onto a glide only while it is still at least this fast,
/// px/s; a push after the glide has faded is ordinary scrolling again.
const REPEAT_MIN_SPEED: f64 = 400.0;
/// A spin has peaked once the wheel has gone this long without reaching a
/// new top speed; a push after that is a repeated spin. A finger driving the
/// wheel keeps re-reaching its speed, so it stays one spin.
const FLICK_PEAKED_AFTER: Duration = Duration::from_millis(250);
/// A spin within this long of the last glide ending — caught by a finger or
/// run out — the same way, is a repeat. It spans the push that follows a
/// catch until the detector reads a free spin again.
const REPEAT_WINDOW: Duration = Duration::from_millis(600);
/// How long after a free-spinning wheel's last tick a single step the other
/// way is held back as the wheel springing back from a finger stopping it.
const SPRING_BACK: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq)]
struct WheelDelta {
    x: f64,
    y: f64,
}

impl WheelDelta {
    const ZERO: Self = Self { x: 0.0, y: 0.0 };

    fn is_zero(self) -> bool {
        self.x == 0.0 && self.y == 0.0
    }

    fn plus(self, other: Self) -> Self {
        Self {
            x: self.x + other.x,
            y: self.y + other.y,
        }
    }

    fn minus(self, other: Self) -> Self {
        Self {
            x: self.x - other.x,
            y: self.y - other.y,
        }
    }

    fn scale(self, factor: f64) -> Self {
        Self {
            x: self.x * factor,
            y: self.y * factor,
        }
    }

    fn with_vertical_scale(self, factor: f64) -> Option<Self> {
        let y = self.y * factor;
        y.is_finite().then_some(Self { x: self.x, y })
    }

    fn post(self) {
        openlogi_inject::post_scroll(self.into());
    }
}

impl TryFrom<ScrollDelta> for WheelDelta {
    type Error = ();

    fn try_from(delta: ScrollDelta) -> Result<Self, Self::Error> {
        let ScrollDelta::WheelTicks { x, y } = delta else {
            return Err(());
        };
        let delta = Self { x, y };
        if x.is_finite() && y.is_finite() && !delta.is_zero() {
            Ok(delta)
        } else {
            Err(())
        }
    }
}

impl From<WheelDelta> for ScrollDelta {
    fn from(delta: WheelDelta) -> Self {
        Self::wheel_ticks(delta.x, delta.y)
    }
}

/// One output frame from the pure motion model.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ScrollFrame {
    delta: WheelDelta,
    phase: SmoothScrollPhase,
    kind: FrameKind,
    unit: FrameUnit,
}

/// What a frame belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FrameKind {
    /// Smoothed wheel output.
    Eased,
    /// A phased gesture whose phases the application must see.
    Phased,
    /// A momentum-scrolling glide frame, emitted outside the eased output
    /// stream. Posted like any pixel frame: phaseless, since a scroll phase
    /// declares a trackpad gesture (rubber-band overscroll, and WebKit gesture
    /// latching that starves sites scrolling from JS wheel handlers).
    Glide,
}

/// What a frame's `delta` counts.
#[derive(Clone, Copy, Debug, PartialEq)]
enum FrameUnit {
    /// Wheel ticks, which each application scales its own way.
    Ticks,
    /// Pixels, `per_tick` to a wheel tick: momentum scrolling's output, sent
    /// as continuous pixel events so every application scrolls the same
    /// distance. Platforms without pixel wheel events get ticks back.
    Pixels { per_tick: f64 },
}

impl ScrollFrame {
    fn new(delta: WheelDelta, phase: SmoothScrollPhase, phased: bool) -> Self {
        Self {
            delta,
            phase,
            kind: if phased {
                FrameKind::Phased
            } else {
                FrameKind::Eased
            },
            unit: FrameUnit::Ticks,
        }
    }

    /// One phaseless eased step of momentum scrolling's smoothed wheel,
    /// `ticks` in wheel ticks posted as pixels at `per_tick`. Outside the
    /// shared output stream, so a phased gesture another wheel opened never
    /// changes its distance or stamps it with a phase.
    fn step(ticks: WheelDelta, per_tick: f64) -> Self {
        Self {
            delta: ticks.scale(per_tick),
            phase: SmoothScrollPhase::Changed,
            kind: FrameKind::Eased,
            unit: FrameUnit::Pixels { per_tick },
        }
    }

    /// One phaseless frame of glide, `delta` in pixels.
    fn momentum(delta: WheelDelta, per_tick: f64) -> Self {
        Self {
            delta,
            phase: SmoothScrollPhase::Changed,
            kind: FrameKind::Glide,
            unit: FrameUnit::Pixels { per_tick },
        }
    }

    /// Distance for backends that accept fractional wheel ticks, not pixels.
    #[cfg(any(test, not(target_os = "macos")))]
    fn wheel_delta(self) -> WheelDelta {
        match self.unit {
            FrameUnit::Ticks => self.delta,
            // Use a fixed reference, not this frame's configured distance:
            // dividing by `per_tick` would cancel the distance slider out.
            // Native applications still choose how far a wheel tick moves.
            FrameUnit::Pixels { .. } => self.delta.scale(
                f64::from(openlogi_core::config::MomentumScrolling::default().pixels_per_notch)
                    .recip(),
            ),
        }
    }

    fn post(self) {
        if self.kind == FrameKind::Phased {
            openlogi_inject::post_phased_scroll(self.delta.into(), self.phase);
            return;
        }
        let delta = match self.unit {
            FrameUnit::Ticks => self.delta.into(),
            #[cfg(target_os = "macos")]
            FrameUnit::Pixels { .. } => ScrollDelta::pixels(self.delta.x, self.delta.y),
            #[cfg(not(target_os = "macos"))]
            FrameUnit::Pixels { .. } => self.wheel_delta().into(),
        };
        openlogi_inject::post_smooth_scroll(delta, self.phase);
    }
}

/// One physical producer. Linux runs one hook callback thread per grabbed
/// mouse; macOS and Windows use one global callback thread. HID++ capture
/// sessions use their epoch-bearing identity so a restarted session cannot
/// inherit motion from the one it replaced, and within a session each wheel
/// is its own producer, so the main wheel's momentum and easing never reach
/// the thumb wheel's motion. Cancelling a session cancels both.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ScrollSource {
    OsHook(ThreadId),
    Hidpp(HidppSessionId, HidppWheel),
}

/// Which wheel of a HID++ capture session a motion belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum HidppWheel {
    /// The main wheel, diverted by its Smooth wheel switch.
    Main,
    /// The horizontal thumb wheel.
    Thumb,
}

impl ScrollSource {
    fn current_hook() -> Self {
        Self::OsHook(thread::current().id())
    }

    /// Whether this is one of `session`'s wheels.
    fn in_session(&self, session: &HidppSessionId) -> bool {
        matches!(self, Self::Hidpp(own, _) if own == session)
    }
}

/// How a segment moves from one cumulative position to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Easing {
    /// Cubic smoothstep over [`ANIMATION_DURATION`]: smoothed wheel output.
    Smooth,
    /// Linear over [`CRISP_STEP_DURATION`]: momentum scrolling's notch
    /// steps, kept short and direct so ratchet scrolling stays precise beside
    /// the glide.
    Crisp,
}

impl Easing {
    fn duration(self) -> Duration {
        match self {
            Self::Smooth => ANIMATION_DURATION,
            Self::Crisp => CRISP_STEP_DURATION,
        }
    }

    /// Normalized distance at normalized `progress`, clamped to `0..=1`.
    fn ease(self, progress: f64) -> f64 {
        let p = progress.clamp(0.0, 1.0);
        match self {
            Self::Smooth => p * p * (3.0 - 2.0 * p),
            Self::Crisp => p,
        }
    }
}

/// A finite eased segment between two cumulative positions.
struct MotionSegment {
    from: WheelDelta,
    target: WheelDelta,
    started_at: Instant,
    easing: Easing,
}

impl MotionSegment {
    fn position_at(&self, at: Instant) -> WheelDelta {
        let elapsed = at.saturating_duration_since(self.started_at);
        let progress = elapsed.as_secs_f64() / self.easing.duration().as_secs_f64();
        let eased = self.easing.ease(progress);
        self.from.plus(self.target.minus(self.from).scale(eased))
    }

    fn ends_at(&self) -> Instant {
        self.started_at + self.easing.duration()
    }

    fn is_complete_at(&self, at: Instant) -> bool {
        at >= self.ends_at()
    }
}

/// A source exists in the state map only while it has a non-zero remaining
/// target.
struct ActiveMotion {
    segment: MotionSegment,
    emitted: WheelDelta,
    next_frame: Instant,
}

impl ActiveMotion {
    fn new(impulse: WheelDelta, at: Instant, easing: Easing) -> Self {
        Self {
            segment: MotionSegment {
                from: WheelDelta::ZERO,
                target: impulse,
                started_at: at,
                easing,
            },
            emitted: WheelDelta::ZERO,
            next_frame: at + FRAME_PERIOD,
        }
    }

    /// Evaluate the old segment at the impulse timestamp, then restart toward
    /// the cumulative target.
    fn retarget(&mut self, impulse: WheelDelta, at: Instant, easing: Easing) -> MotionUpdate {
        let position = self.segment.position_at(at);
        let target = self.segment.target.plus(impulse);
        let delta = self.delta_to(position);
        if target == position {
            return MotionUpdate::Finished(delta);
        }

        self.segment = MotionSegment {
            from: position,
            target,
            started_at: at,
            easing,
        };
        self.next_frame = at + FRAME_PERIOD;
        MotionUpdate::Active(delta)
    }

    /// Evaluate the position at `at` and report whether the source remains
    /// active after this update.
    fn advance(&mut self, at: Instant) -> MotionUpdate {
        let complete = self.segment.is_complete_at(at);
        let position = self.segment.position_at(at);
        let delta = self.delta_to(position);
        if complete {
            MotionUpdate::Finished(delta)
        } else {
            while self.next_frame <= at {
                self.next_frame += FRAME_PERIOD;
            }
            self.next_frame = self.next_frame.min(self.segment.ends_at());
            MotionUpdate::Active(delta)
        }
    }

    fn delta_to(&mut self, position: WheelDelta) -> WheelDelta {
        let delta = position.minus(self.emitted);
        self.emitted = position;
        delta
    }
}

/// Result of evaluating one source-local motion.
#[derive(Clone, Copy)]
enum MotionUpdate {
    Active(WheelDelta),
    Finished(WheelDelta),
}

impl MotionUpdate {
    fn is_finished(&self) -> bool {
        matches!(self, Self::Finished(_))
    }
}

/// The one phase stream visible to the foreground application. Source-local
/// motions may overlap, but Core Graphics has no source identity with which to
/// pair multiple synthetic gestures; all distances therefore share this single
/// balanced lifecycle.
///
/// Whoever opens the stream decides whether it is a phased gesture (`phased`
/// on the opening frame), and every frame up to its end keeps that choice, so
/// the application never sees a gesture that changes kind half-way.
#[derive(Default)]
enum OutputStream {
    #[default]
    Idle,
    Active {
        phased: bool,
    },
}

impl OutputStream {
    fn progress(&mut self, delta: WheelDelta, phased: bool, emit: &mut impl FnMut(ScrollFrame)) {
        if delta.is_zero() {
            return;
        }
        let (phase, phased) = match *self {
            Self::Idle => {
                *self = Self::Active { phased };
                (SmoothScrollPhase::Began, phased)
            }
            Self::Active { phased } => (SmoothScrollPhase::Changed, phased),
        };
        emit(ScrollFrame::new(delta, phase, phased));
    }

    fn finish(&mut self, delta: WheelDelta, phased: bool, emit: &mut impl FnMut(ScrollFrame)) {
        match *self {
            Self::Idle if !delta.is_zero() => {
                emit(ScrollFrame::new(delta, SmoothScrollPhase::Began, phased));
                emit(ScrollFrame::new(
                    WheelDelta::ZERO,
                    SmoothScrollPhase::Ended,
                    phased,
                ));
            }
            Self::Active { phased } => {
                emit(ScrollFrame::new(delta, SmoothScrollPhase::Ended, phased));
            }
            Self::Idle => {}
        }
        *self = Self::Idle;
    }

    fn cancel(&mut self, emit: &mut impl FnMut(ScrollFrame)) {
        if let Self::Active { phased } = *self {
            emit(ScrollFrame::new(
                WheelDelta::ZERO,
                SmoothScrollPhase::Cancelled,
                phased,
            ));
        }
        *self = Self::Idle;
    }
}

/// Pure per-source state machine. Absence from the map represents idle, so an
/// idle source cannot accidentally retain a target or scheduled deadline. All
/// source-local distances feed one application-visible [`OutputStream`].
#[derive(Default)]
struct ScrollEngine {
    active: HashMap<ScrollSource, ActiveMotion>,
    /// Sources inside an unsmoothed phased gesture, each with the instant its
    /// idle window closes. Disjoint from `active`: smoothing on or off decides
    /// which map a source can enter, and a preference change cancels both.
    phased: HashMap<ScrollSource, Instant>,
    output: OutputStream,
    /// Smoothed main wheels under momentum scrolling, each with its device's
    /// settings, its free-spin detector and, while it glides, its glide.
    wheels: HashMap<ScrollSource, WheelState>,
}

/// One smoothed main wheel's momentum-scrolling state.
struct WheelState {
    /// The momentum-scrolling settings of the device this wheel belongs to,
    /// as its latest report carried them.
    momentum: MomentumParams,
    spring_back: ReverseLock<WheelTick>,
    detector: FreeSpinDetector,
    glide: GlideState,
    /// Spins chained onto the glide, like repeated trackpad flicks: each
    /// one speeds it up ([`glide::chain_boost`]).
    chain: u32,
}

impl WheelState {
    fn new(momentum: MomentumParams) -> Self {
        Self {
            momentum,
            spring_back: ReverseLock::default(),
            detector: FreeSpinDetector::default(),
            glide: GlideState::default(),
            chain: 0,
        }
    }

    /// The running glide, if any.
    fn gliding(&self) -> Option<&Glide> {
        match &self.glide {
            GlideState::Gliding(glide) => Some(glide),
            GlideState::Idle { .. } | GlideState::Spent { .. } => None,
        }
    }

    /// End a running glide at `at`, the way `stop` says.
    fn end_glide(&mut self, at: Instant, stop: GlideStop) {
        let GlideState::Gliding(glide) = &self.glide else {
            return;
        };
        let last = GlideEnd {
            at,
            direction: glide.direction,
            speed: glide.speed(at),
        };
        self.glide = match stop {
            GlideStop::RanOutSpinning => GlideState::Spent {
                last,
                next_check: at + FRAME_PERIOD,
            },
            GlideStop::Touched | GlideStop::RanOut => GlideState::Idle { last: Some(last) },
        };
    }
}

/// A smoothed main wheel's glide, from one free spin to the next.
enum GlideState {
    /// No glide; `last` is how the previous one ended — caught by a finger
    /// or run out — which the next spin may repeat onto.
    Idle { last: Option<GlideEnd> },
    /// A free-spinning wheel's glide.
    Gliding(Glide),
    /// This free spin's glide has run its course; the wheel spinning on must
    /// not start another. Left when the wheel next leaves free spin, still
    /// repeatable onto.
    ///
    /// Its detector is still ticked every frame ([`Self::next_tick`]), so the
    /// silence of a finger catching the wheel reads as a stop and the next
    /// stroke scrolls, however slow.
    Spent { last: GlideEnd, next_check: Instant },
}

impl GlideState {
    /// When the wheel's detector is next due a tick: every frame while it
    /// glides or its spent spin goes on, never while it is idle.
    fn next_tick(&self) -> Option<Instant> {
        match self {
            Self::Gliding(glide) => Some(glide.next_frame),
            Self::Spent { next_check, .. } => Some(*next_check),
            Self::Idle { .. } => None,
        }
    }
}

impl Default for GlideState {
    fn default() -> Self {
        Self::Idle { last: None }
    }
}

/// How a glide ended. A spin soon after, the same way, repeats onto it
/// however it ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlideStop {
    /// A finger stopped or turned the wheel.
    Touched,
    /// It ran its course with the wheel still spinning free.
    RanOutSpinning,
    /// It ran its course after the wheel had gone quiet.
    RanOut,
}

/// When, which way and how fast a glide was going as it ended.
#[derive(Clone, Copy)]
struct GlideEnd {
    at: Instant,
    direction: Direction,
    speed: f64,
}

/// Which way a glide scrolls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Up,
    Down,
}

impl Direction {
    /// The direction of a vertical distance; `None` for zero.
    fn of(y: f64) -> Option<Self> {
        if y > 0.0 {
            Some(Self::Up)
        } else if y < 0.0 {
            Some(Self::Down)
        } else {
            None
        }
    }

    fn sign(self) -> f64 {
        match self {
            Self::Up => 1.0,
            Self::Down => -1.0,
        }
    }
}

/// One main-wheel tick: the scaled impulse and the raw signed notches the
/// detector measures speed in.
#[derive(Clone, Copy)]
struct WheelTick {
    impulse: WheelDelta,
    notches: f64,
}

/// A free-spinning wheel's glide: one [`GlideCurve`], relaunched from where
/// it is when a faster spin or a repeat speeds it up. A speed-up never lands
/// in one frame: the glide eases up into its curve from the speed it was at
/// ([`glide::RISE_S`]), so its speed is the curve's less a shortfall that
/// decays away — `curve(t) − rise·e^(−t/RISE_S)` — evaluated from elapsed
/// time like the curve itself.
struct Glide {
    /// The scroll direction of the impulse that started the spin, inversion
    /// and natural scrolling included.
    direction: Direction,
    /// The vertical sensitivity multiplier the wheel's speed is scaled by.
    sensitivity: f64,
    /// The speed-up on the wheel's speed: the repeat boost for a repeated
    /// spin, else 1.
    boost: f64,
    curve: GlideCurve,
    /// How far below the curve's start speed the glide was when the curve
    /// began, px/s: the shortfall it eases out of.
    rise: f64,
    /// When the current curve began, and how much of it has been emitted.
    began: Instant,
    emitted: f64,
    flick: Flick,
    next_frame: Instant,
    /// When the glide began, which tells consecutive glides apart in tests.
    #[cfg(test)]
    launched: Instant,
}

impl Glide {
    /// A glide launching at `speed` off a page already scrolling at `pace`.
    fn new(
        direction: Direction,
        sensitivity: f64,
        boost: f64,
        (pace, speed): (f64, f64),
        momentum: &MomentumParams,
        at: Instant,
    ) -> Self {
        let curve = glide::curve(speed, momentum);
        Self {
            direction,
            sensitivity,
            boost,
            curve,
            rise: (curve.start_speed() - pace).max(0.0),
            began: at,
            emitted: 0.0,
            flick: Flick::start(at),
            next_frame: at + FRAME_PERIOD,
            #[cfg(test)]
            launched: at,
        }
    }

    /// Its speed at `at`, px/s.
    fn speed(&self, at: Instant) -> f64 {
        let elapsed = at.saturating_duration_since(self.began);
        let shortfall = self.rise * (-elapsed.as_secs_f64() / glide::RISE_S).exp();
        (self.curve.speed_at(elapsed) - shortfall).max(0.0)
    }

    /// How far it has glided along its current curve by `at`.
    fn position(&self, at: Instant) -> f64 {
        let elapsed = at.saturating_duration_since(self.began);
        let eased = 1.0 - (-elapsed.as_secs_f64() / glide::RISE_S).exp();
        self.curve.position_at(elapsed) - self.rise * glide::RISE_S * eased
    }

    /// Whether its curve has run its course by `at`.
    fn finished(&self, at: Instant) -> bool {
        at.saturating_duration_since(self.began) >= self.curve.duration()
    }

    /// The frame carrying it from where it was last emitted to `at`, if it
    /// moved.
    fn advance(&mut self, at: Instant, per_tick: f64) -> Option<ScrollFrame> {
        // Never backwards: the eased shortfall can outlast a short curve's
        // tail by a hair.
        let position = self.position(at);
        if position <= self.emitted {
            return None;
        }
        let pixels = (position - self.emitted) * self.direction.sign();
        self.emitted = position;
        pixels
            .is_finite()
            .then(|| ScrollFrame::momentum(WheelDelta { x: 0.0, y: pixels }, per_tick))
    }

    /// Glide on from `at` along a new curve starting at `speed`, easing up
    /// into it from the speed it is at now.
    fn relaunch(&mut self, speed: f64, momentum: &MomentumParams, at: Instant) {
        let now = self.speed(at);
        self.curve = glide::curve(speed, momentum);
        self.rise = (self.curve.start_speed() - now).max(0.0);
        self.began = at;
        self.emitted = 0.0;
    }
}

/// How far the spin launching a glide has got.
#[derive(Clone, Copy)]
enum Flick {
    /// Still speeding up: its top wheel speed so far, notches/s, and when
    /// that was reached. A faster wheel relaunches the glide faster.
    Rising { top: f64, at: Instant },
    /// Peaked: a push from now on is a repeated spin.
    Peaked,
}

impl Flick {
    fn start(at: Instant) -> Self {
        Self::Rising { top: 0.0, at }
    }

    /// Follow the wheel turning at `speed`: a new top speed, or peaked once
    /// none has come for [`FLICK_PEAKED_AFTER`].
    fn follow(&mut self, speed: f64, now: Instant) {
        if let Self::Rising { top, at } = *self {
            if speed > top {
                *self = Self::Rising {
                    top: speed,
                    at: now,
                };
            } else if now.saturating_duration_since(at) >= FLICK_PEAKED_AFTER {
                *self = Self::Peaked;
            }
        }
    }
}

impl ScrollEngine {
    /// Pixels per tick `source`'s frames are posted in: momentum scrolling's
    /// smoothed wheels scroll in pixels, every other source in ticks.
    fn pixels_for(&self, source: &ScrollSource) -> Option<f64> {
        self.wheels
            .get(source)
            .map(|state| state.momentum.pixels_per_notch())
    }

    /// One smoothed main-wheel impulse of `notches` raw wheel notches.
    ///
    /// `momentum` is the settings of the wheel's device, `None` while momentum
    /// scrolling is off for it. Without momentum scrolling it eases as any
    /// impulse does, and the wheel's glide state is dropped. With it, a
    /// single step back right after a finger stops a free spin is held as a
    /// possible spring-back, ratchet scrolling still eases, and once the
    /// detector sees a free spin the page glides off at the wheel's speed
    /// instead.
    fn wheel_impulse(
        &mut self,
        source: ScrollSource,
        impulse: WheelDelta,
        notches: f64,
        momentum: Option<MomentumParams>,
        at: Instant,
        emit: &mut impl FnMut(ScrollFrame),
    ) {
        let Some(momentum) = momentum else {
            // Off for this wheel's device: it eases exactly as any other
            // wheel from this tick on.
            self.wheels.remove(&source);
            self.impulse(source, impulse, at, emit);
            return;
        };
        if notches == 0.0 {
            self.impulse(source, impulse, at, emit);
            return;
        }
        let state = self
            .wheels
            .entry(source.clone())
            .and_modify(|state| state.momentum = momentum)
            .or_insert_with(|| WheelState::new(momentum));
        // Only a free-spinning wheel springs back as a finger stops it: a
        // step back while ratcheting is the user's correction.
        let lock = if state.detector.phase() == WheelPhase::Ratchet {
            Duration::ZERO
        } else {
            SPRING_BACK
        };
        match state
            .spring_back
            .admit(notches, at, lock, WheelTick { impulse, notches })
        {
            Admit::Pass(tick) => self.wheel_tick(&source, tick, at, momentum, emit),
            Admit::Hold => {}
            // Released now: replayed at their own, older instants the held
            // ticks would restart the eased motion in the past, and the page
            // would lurch to catch up.
            Admit::Release(ticks) => {
                for tick in ticks {
                    self.wheel_tick(&source, tick, at, momentum, emit);
                }
            }
        }
    }

    /// One admitted main-wheel tick: eased while ratcheting, a glide once
    /// the wheel spins freely, swallowed after a finger stop.
    fn wheel_tick(
        &mut self,
        source: &ScrollSource,
        WheelTick { impulse, notches }: WheelTick,
        at: Instant,
        momentum: MomentumParams,
        emit: &mut impl FnMut(ScrollFrame),
    ) {
        let state = self
            .wheels
            .entry(source.clone())
            .or_insert_with(|| WheelState::new(momentum));
        let phase = state.detector.report(at, notches);
        // A push the detector reads as a finger driving the wheel, landing on
        // a glide still going strong the same way, is a repeated spin: keep
        // gliding, faster once the spin before it has peaked.
        let direction = Direction::of(impulse.y);
        let pushed_while_gliding = phase == WheelPhase::Ratchet
            && state.gliding().is_some_and(|glide| {
                glide.speed(at) >= REPEAT_MIN_SPEED && direction == Some(glide.direction)
            });
        if pushed_while_gliding {
            state.detector.resume_free_spin(at);
            let wheel_speed = state.detector.speed(at);
            if let GlideState::Gliding(glide) = &mut state.glide
                && let Flick::Peaked = glide.flick
            {
                if let Some(frame) = glide.advance(at, momentum.pixels_per_notch()) {
                    emit(frame);
                }
                state.chain += 1;
                glide.boost = glide::chain_boost(&momentum, state.chain);
                let launch = glide::launch_speed(wheel_speed, glide.sensitivity, &momentum);
                glide.relaunch((launch * glide.boost).max(glide.speed(at)), &momentum, at);
                glide.flick = Flick::start(at);
            }
            return;
        }
        match (phase, &state.glide, direction) {
            (WheelPhase::Ratchet, ..) => {
                state.end_glide(at, GlideStop::Touched);
                if let GlideState::Spent { last, .. } = state.glide {
                    state.glide = GlideState::Idle { last: Some(last) };
                }
                self.impulse(source.clone(), impulse, at, emit);
            }
            (WheelPhase::FreeSpin, &GlideState::Idle { last }, Some(direction)) => {
                // `notches` is the raw count in the same direction as
                // `impulse`, so their ratio is the unsigned sensitivity.
                let sensitivity = (impulse.y / notches).abs();
                let wheel_speed = state.detector.speed(at);
                // The page is moving at the ratchet steps' pace: the glide
                // eases up from it rather than jumping to its launch speed.
                let pace = glide::pace(wheel_speed, sensitivity, &momentum);
                let launch = glide::launch_speed(wheel_speed, sensitivity, &momentum);
                // A spin soon after the last glide ended, the same way, is a
                // repeat like repeated flicks on a trackpad: it builds on
                // the chain instead of starting over, and never glides slower
                // than the glide it caught.
                let repeat = last.filter(|last| {
                    at.saturating_duration_since(last.at) <= REPEAT_WINDOW
                        && last.direction == direction
                });
                state.chain = if repeat.is_some() { state.chain + 1 } else { 0 };
                let boost = glide::chain_boost(&momentum, state.chain);
                let caught = repeat.map_or(0.0, |last| last.speed);
                let speed = (launch * boost).max(caught);
                state.glide = GlideState::Gliding(Glide::new(
                    direction,
                    sensitivity,
                    boost,
                    (pace, speed),
                    &momentum,
                    at,
                ));
                // The glide takes over from here: the eased tail still in
                // flight would double up with its first frames.
                self.drop_motion(source, emit);
            }
            (WheelPhase::FreeSpin, ..) => {}
            (WheelPhase::Stopped(_), ..) => self.stop_wheel(source, at, emit),
        }
    }

    /// Drop every pending motion of a wheel a finger just stopped.
    fn stop_wheel(
        &mut self,
        source: &ScrollSource,
        at: Instant,
        emit: &mut impl FnMut(ScrollFrame),
    ) {
        // A finger catching the wheel to spin again: the next spin may
        // repeat onto this glide.
        if let Some(state) = self.wheels.get_mut(source) {
            state.end_glide(at, GlideStop::Touched);
        }
        self.drop_motion(source, emit);
    }

    /// Drop `source`'s eased motion unfinished, cancelling the shared stream
    /// if nothing on it is left moving.
    fn drop_motion(&mut self, source: &ScrollSource, emit: &mut impl FnMut(ScrollFrame)) {
        if self.active.remove(source).is_some() && self.stream_idle() {
            self.output.cancel(emit);
        }
    }

    /// Whether nothing on the shared output stream is moving: no phased
    /// gesture, and no eased motion but momentum scrolling's smoothed
    /// wheels, whose steps stay off the stream ([`ScrollFrame::step`]).
    fn stream_idle(&self) -> bool {
        self.phased.is_empty()
            && self
                .active
                .keys()
                .all(|source| self.pixels_for(source).is_some())
    }

    /// Emit due glide frames, end the glides that ran their course or whose
    /// wheel a finger stopped, and watch spent spins for their stop.
    fn advance_glides(&mut self, at: Instant, emit: &mut impl FnMut(ScrollFrame)) {
        let ticking: Vec<ScrollSource> = self
            .wheels
            .iter()
            .filter(|(_, state)| state.glide.next_tick().is_some())
            .map(|(source, _)| source.clone())
            .collect();
        for source in ticking {
            let Some(state) = self.wheels.get_mut(&source) else {
                continue;
            };
            let momentum = state.momentum;
            let per_tick = momentum.pixels_per_notch();
            if let GlideState::Spent { last, next_check } = state.glide {
                if at >= next_check {
                    state.glide = match state.detector.tick(at) {
                        WheelPhase::FreeSpin => GlideState::Spent {
                            last,
                            next_check: at + FRAME_PERIOD,
                        },
                        // Caught or gone quiet: the next movement is a new
                        // gesture, which may repeat onto the spent glide.
                        WheelPhase::Stopped(_) | WheelPhase::Ratchet => {
                            GlideState::Idle { last: Some(last) }
                        }
                    };
                }
                continue;
            }
            let phase = state.detector.tick(at);
            let wheel_speed = match phase {
                WheelPhase::Stopped(_) => {
                    self.stop_wheel(&source, at, emit);
                    continue;
                }
                WheelPhase::Ratchet => 0.0,
                WheelPhase::FreeSpin => state.detector.speed(at),
            };
            let GlideState::Gliding(glide) = &mut state.glide else {
                continue;
            };
            if at < glide.next_frame {
                continue;
            }
            glide.next_frame = at + FRAME_PERIOD;
            if let Some(frame) = glide.advance(at, per_tick) {
                emit(frame);
            }
            // While the spin still speeds up, the glide launches off its
            // fastest measure rather than the first one; it never rides the
            // wheel back down.
            if let Flick::Rising { .. } = glide.flick {
                let launch =
                    glide::launch_speed(wheel_speed, glide.sensitivity, &momentum) * glide.boost;
                if glide::curve(launch, &momentum).start_speed() > glide.curve.start_speed() {
                    glide.relaunch(launch, &momentum, at);
                }
            }
            glide.flick.follow(wheel_speed, at);
            if glide.finished(at) {
                let stop = if phase == WheelPhase::FreeSpin {
                    GlideStop::RanOutSpinning
                } else {
                    GlideStop::RanOut
                };
                state.end_glide(at, stop);
            }
        }
    }

    fn impulse(
        &mut self,
        source: ScrollSource,
        impulse: WheelDelta,
        at: Instant,
        emit: &mut impl FnMut(ScrollFrame),
    ) {
        let pixels = self.pixels_for(&source);
        // Momentum scrolling's wheel steps crisply; every other wheel eases.
        let easing = if pixels.is_some() {
            Easing::Crisp
        } else {
            Easing::Smooth
        };
        if self
            .active
            .get(&source)
            .is_some_and(|motion| motion.segment.is_complete_at(at))
            && let Some(mut completed) = self.active.remove(&source)
        {
            let update = completed.advance(at);
            self.emit_update(update, pixels, emit);
        }

        let update = match self.active.entry(source) {
            Entry::Occupied(mut entry) => {
                let update = entry.get_mut().retarget(impulse, at, easing);
                if update.is_finished() {
                    entry.remove();
                }
                Some(update)
            }
            Entry::Vacant(entry) => {
                entry.insert(ActiveMotion::new(impulse, at, easing));
                None
            }
        };
        if let Some(update) = update {
            self.emit_update(update, pixels, emit);
        }
    }

    /// Emit one tick immediately inside a phased gesture and push back the
    /// idle deadline that ends it. The first tick of a gesture is `Began`.
    fn phased_impulse(
        &mut self,
        source: ScrollSource,
        impulse: WheelDelta,
        at: Instant,
        emit: &mut impl FnMut(ScrollFrame),
    ) {
        self.end_idle_phased(at, emit);
        self.output.progress(impulse, true, emit);
        self.phased.insert(source, at + PHASED_IDLE);
    }

    fn advance_due(&mut self, at: Instant, emit: &mut impl FnMut(ScrollFrame)) {
        self.end_idle_phased(at, emit);
        self.advance_glides(at, emit);
        self.advance_segments(at, emit);
    }

    /// Advance the segments whose frame is due.
    fn advance_segments(&mut self, at: Instant, emit: &mut impl FnMut(ScrollFrame)) {
        let due: Vec<ScrollSource> = self
            .active
            .iter()
            .filter(|(_, motion)| motion.next_frame <= at)
            .map(|(source, _)| source.clone())
            .collect();
        for source in due {
            let Some(update) = self
                .active
                .get_mut(&source)
                .map(|motion| motion.advance(at))
            else {
                continue;
            };
            if update.is_finished() {
                self.active.remove(&source);
            }
            let pixels = self.pixels_for(&source);
            self.emit_update(update, pixels, emit);
        }
    }

    /// Close the gesture once every phased source has been idle past its
    /// deadline. Sources still inside their window keep the gesture open.
    fn end_idle_phased(&mut self, at: Instant, emit: &mut impl FnMut(ScrollFrame)) {
        let before = self.phased.len();
        self.phased.retain(|_, deadline| *deadline > at);
        if self.phased.len() != before && self.stream_idle() {
            self.output.finish(WheelDelta::ZERO, true, emit);
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        let frame = self
            .active
            .values()
            .map(|motion| motion.next_frame)
            .chain(
                self.wheels
                    .values()
                    .filter_map(|state| state.glide.next_tick()),
            )
            .min();
        frame.into_iter().chain(self.phased.values().copied()).min()
    }

    fn cancel_source(&mut self, source: &ScrollSource, emit: &mut impl FnMut(ScrollFrame)) {
        self.cancel_where(|own| own == source, emit);
    }

    /// Cancel both wheels of a HID++ capture session.
    fn cancel_session(&mut self, session: &HidppSessionId, emit: &mut impl FnMut(ScrollFrame)) {
        self.cancel_where(|own| own.in_session(session), emit);
    }

    fn cancel_where(
        &mut self,
        cancelled: impl Fn(&ScrollSource) -> bool,
        emit: &mut impl FnMut(ScrollFrame),
    ) {
        self.wheels.retain(|source, _| !cancelled(source));
        let before = (self.active.len(), self.phased.len());
        self.active.retain(|source, _| !cancelled(source));
        self.phased.retain(|source, _| !cancelled(source));
        let removed = (self.active.len(), self.phased.len()) != before;
        if removed && self.stream_idle() {
            self.output.cancel(emit);
        }
    }

    fn cancel_all(&mut self, emit: &mut impl FnMut(ScrollFrame)) {
        self.wheels.clear();
        self.active.clear();
        self.phased.clear();
        self.output.cancel(emit);
    }

    fn emit_update(
        &mut self,
        update: MotionUpdate,
        pixels: Option<f64>,
        emit: &mut impl FnMut(ScrollFrame),
    ) {
        if let Some(per_tick) = pixels {
            let (MotionUpdate::Active(delta) | MotionUpdate::Finished(delta)) = update;
            if !delta.is_zero() {
                emit(ScrollFrame::step(delta, per_tick));
            }
            return;
        }
        match update {
            // A phased gesture still open (after a smoothing toggle) owns the
            // stream's end, so a motion finishing under it only progresses it.
            MotionUpdate::Finished(delta) if self.stream_idle() => {
                self.output.finish(delta, false, emit);
            }
            MotionUpdate::Active(delta) | MotionUpdate::Finished(delta) => {
                self.output.progress(delta, false, emit);
            }
        }
    }
}

#[cfg(test)]
mod tests;
