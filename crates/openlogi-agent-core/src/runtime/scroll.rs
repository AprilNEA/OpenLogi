//! Traditional wheel output owned by one dedicated worker.
//!
//! Hook callbacks submit typed wheel impulses through [`ScrollInputHandle`]
//! without blocking. The worker either scales and emits them directly,
//! evaluates finite smooth motion from absolute timestamps, or, with smoothing
//! off, wraps a diverted horizontal wheel in a phased gesture. Pixel-precise
//! input never enters this runtime, so native trackpad and continuous wheel
//! streams cannot be mixed with wheel ticks.
//!
//! With momentum scrolling on ([`MomentumScrolling`]), a smoothed main wheel
//! also glides: its free-spin detector tells ratchet scrolling from a
//! SmartShift free spin, the wheel's speed then drives pixel momentum frames,
//! and a finger stopping the wheel ends the glide at once. Off, the wheel
//! eases exactly as any other wheel does.

mod free_spin;
mod reverse_lock;
mod worker;

pub use worker::{ScrollInputHandle, ScrollPreferences, ScrollRuntime};

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use openlogi_core::config::MomentumScrolling;
use openlogi_core::scroll::{
    MOMENTUM_END_SPEED, ScrollDelta, momentum_coast, momentum_friction, momentum_output,
};
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
/// A display-paced engine falls back to its own timer when no display frame
/// has arrived for this long (displays asleep, link stalled).
const DISPLAY_FALLBACK: Duration = Duration::from_millis(40);
/// A re-flick chains only onto a glide still at least this fast, px/s; a
/// push after the glide has faded is ordinary scrolling again.
const CHAIN_MIN_VELOCITY: f64 = 400.0;
/// The most repeated flicks can multiply the glide.
const CHAIN_MAX_BOOST: f64 = 3.0;
/// A flick has peaked once the wheel has gone this long without reaching a
/// new top speed; a push after that is a fresh flick and chains. A finger
/// driving the wheel keeps re-reaching its speed, so it stays one flick.
const FLICK_PEAKED_AFTER: Duration = Duration::from_millis(250);
/// A new flick within this long of the last glide ending — run out, or
/// caught by the finger to flick again — in the same direction is a repeat,
/// like a Magic Mouse's repeated swipes, and keeps building speed.
const CHAIN_WINDOW: Duration = Duration::from_millis(600);

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
    /// The frame belongs to a phased gesture whose phases the application
    /// must see, rather than to smoothed wheel output.
    phased: bool,
    unit: FrameUnit,
    /// A momentum-scrolling glide frame, emitted outside the eased output
    /// stream. Posted like any pixel frame.
    glide: bool,
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
            phased,
            unit: FrameUnit::Ticks,
            glide: false,
        }
    }

    /// The same frame in pixels at `per_tick` (when `Some`).
    fn in_pixels(self, per_tick: Option<f64>) -> Self {
        match per_tick {
            Some(per_tick) if !self.phased => Self {
                delta: self.delta.scale(per_tick),
                unit: FrameUnit::Pixels { per_tick },
                ..self
            },
            _ => self,
        }
    }

    /// One phaseless frame of glide, `delta` in pixels.
    fn momentum(delta: WheelDelta, per_tick: f64) -> Self {
        Self {
            delta,
            phase: SmoothScrollPhase::Changed,
            phased: false,
            unit: FrameUnit::Pixels { per_tick },
            glide: true,
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
            FrameUnit::Pixels { .. } => self
                .delta
                .scale(f64::from(MomentumScrolling::default().pixels_per_notch).recip()),
        }
    }

    fn post(self) {
        if self.phased {
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
/// inherit motion from the one it replaced.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ScrollSource {
    OsHook(ThreadId),
    Hidpp(HidppSessionId),
}

impl ScrollSource {
    fn current_hook() -> Self {
        Self::OsHook(thread::current().id())
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
    /// `pixels`: post this frame in pixels at that many per tick (momentum
    /// scrolling's smoothed wheel) rather than in ticks.
    fn progress(
        &mut self,
        delta: WheelDelta,
        phased: bool,
        pixels: Option<f64>,
        emit: &mut impl FnMut(ScrollFrame),
    ) {
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
        emit(ScrollFrame::new(delta, phase, phased).in_pixels(pixels));
    }

    fn finish(
        &mut self,
        delta: WheelDelta,
        phased: bool,
        pixels: Option<f64>,
        emit: &mut impl FnMut(ScrollFrame),
    ) {
        match *self {
            Self::Idle if !delta.is_zero() => {
                emit(ScrollFrame::new(delta, SmoothScrollPhase::Began, phased).in_pixels(pixels));
                emit(
                    ScrollFrame::new(WheelDelta::ZERO, SmoothScrollPhase::Ended, phased)
                        .in_pixels(pixels),
                );
            }
            Self::Active { phased } => {
                emit(ScrollFrame::new(delta, SmoothScrollPhase::Ended, phased).in_pixels(pixels));
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
    momentum: MomentumScrolling,
    /// Smoothed main wheels under momentum scrolling, each with its
    /// free-spin detector and, while it glides, its glide.
    wheels: HashMap<ScrollSource, WheelState>,
    /// While output is paced by the display, the instant of the last display
    /// frame; `None` while the engine runs on its own frame timer.
    display_frame: Option<Instant>,
}

/// One smoothed main wheel's momentum-scrolling state.
#[derive(Default)]
struct WheelState {
    spring_back: ReverseLock<WheelTick>,
    detector: FreeSpinDetector,
    glide: Option<Glide>,
    /// Flicks chained onto the current glide, like repeated Magic Mouse
    /// swipes: each one speeds the glide up.
    chain: u32,
    /// This free spin's glide has run out; the wheel spinning on must not
    /// start another. Cleared when the wheel next leaves free spin.
    glide_done: bool,
    /// When and in which direction (±1) the last glide ended, for chaining
    /// the next flick onto it.
    last_glide: Option<(Instant, f64)>,
}

/// One main-wheel tick: the scaled impulse and the raw signed notches the
/// detector measures speed in.
#[derive(Clone, Copy)]
struct WheelTick {
    impulse: WheelDelta,
    notches: f64,
}

/// A free-spinning wheel's glide.
struct Glide {
    /// Signed output scale: the sign is the scroll direction of the impulse
    /// that started the spin (inversion and natural scrolling included), the
    /// magnitude the vertical sensitivity multiplier.
    scale: f64,
    /// Glide speed of the last frame, px/s (unsigned).
    velocity: f64,
    /// Speed multiplier from repeated flicks.
    boost: f64,
    /// The flick's top wheel speed so far, notches/s, and when it was last
    /// raised.
    peak_wheel: f64,
    peak_at: Instant,
    /// The flick has peaked: a push from now on is a fresh flick.
    peaked: bool,
    last_emit: Instant,
    next_frame: Instant,
}

impl ScrollEngine {
    /// Apply momentum-scrolling settings. Turning it off drops every glide
    /// and wheel state, so the wheel eases exactly as any other from the next
    /// tick on.
    fn set_momentum(&mut self, momentum: MomentumScrolling) {
        if !momentum.enabled {
            self.wheels.clear();
        }
        self.momentum = momentum;
    }

    /// Pixels per tick `source`'s frames are posted in: momentum scrolling's
    /// smoothed wheels scroll in pixels, every other source in ticks.
    fn pixels_for(&self, source: &ScrollSource) -> Option<f64> {
        (self.momentum.enabled && self.wheels.contains_key(source)).then(|| {
            f64::from(self.momentum.pixels_per_notch.clamp(
                MomentumScrolling::PIXELS_PER_NOTCH_MIN,
                MomentumScrolling::PIXELS_PER_NOTCH_MAX,
            ))
        })
    }

    /// Whether output should be paced by display frames: momentum scrolling
    /// is on and a segment or a glide is moving.
    fn needs_frames(&self) -> bool {
        self.momentum.enabled
            && (!self.active.is_empty() || self.wheels.values().any(|state| state.glide.is_some()))
    }

    fn display_driven(&self) -> bool {
        self.display_frame.is_some()
    }

    /// Pace output by display frames ([`Self::on_display_frame`]) or, when
    /// off, by the engine's own frame timer.
    fn set_display_driven(&mut self, on: bool, at: Instant) {
        self.display_frame = on.then_some(at);
    }

    /// One display frame: every moving segment and glide emits its frame
    /// now, so each displayed frame carries exactly one step of motion — the
    /// way macOS paces trackpad and Magic Mouse momentum.
    fn on_display_frame(&mut self, at: Instant, emit: &mut impl FnMut(ScrollFrame)) {
        if self.display_frame.is_none() {
            return;
        }
        self.display_frame = Some(at);
        self.end_idle_phased(at, emit);
        self.advance_glides(at, true, emit);
        self.advance_segments(at, true, emit);
    }

    /// One smoothed main-wheel impulse of `notches` raw wheel notches.
    ///
    /// Without momentum scrolling it eases as any impulse does. With it, a
    /// single step back right after the wheel stops is held as a possible
    /// spring-back, ratchet scrolling still eases, and once the detector sees
    /// a free spin the wheel's speed drives a glide instead.
    fn wheel_impulse(
        &mut self,
        source: ScrollSource,
        impulse: WheelDelta,
        notches: f64,
        at: Instant,
        emit: &mut impl FnMut(ScrollFrame),
    ) {
        if !self.momentum.enabled || notches == 0.0 {
            self.impulse(source, impulse, at, emit);
            return;
        }
        let lock = Duration::from_millis(u64::from(
            self.momentum
                .spring_back_ms
                .min(MomentumScrolling::SPRING_BACK_MS_MAX),
        ));
        let state = self.wheels.entry(source.clone()).or_default();
        match state
            .spring_back
            .admit(notches, at, lock, WheelTick { impulse, notches })
        {
            Admit::Pass(tick) => self.wheel_tick(&source, tick, at, emit),
            Admit::Hold => {}
            // Released now: replayed at their own, older instants the held
            // ticks would restart the eased motion in the past, and the page
            // would lurch to catch up.
            Admit::Release(ticks) => {
                for tick in ticks {
                    self.wheel_tick(&source, tick, at, emit);
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
        emit: &mut impl FnMut(ScrollFrame),
    ) {
        let boost_per_flick = f64::from(
            self.momentum
                .repeat_boost
                .clamp(100, MomentumScrolling::REPEAT_BOOST_MAX),
        ) / 100.0;
        let state = self.wheels.entry(source.clone()).or_default();
        let phase = state.detector.report(at, notches);
        // A push the detector reads as a finger driving the wheel, landing on
        // a glide still going strong the same way, is a re-flick: keep
        // gliding, faster once the flick before it has peaked.
        let pushed_while_gliding = phase == WheelPhase::Ratchet
            && state.glide.as_ref().is_some_and(|glide| {
                glide.velocity >= CHAIN_MIN_VELOCITY && glide.scale * impulse.y > 0.0
            });
        if pushed_while_gliding {
            state.detector.resume_free_spin(at);
            if let Some(glide) = state.glide.as_mut()
                && glide.peaked
            {
                state.chain += 1;
                glide.boost = chain_boost(boost_per_flick, state.chain);
                glide.peaked = false;
                glide.peak_wheel = 0.0;
                glide.peak_at = at;
            }
            return;
        }
        match phase {
            WheelPhase::Ratchet => {
                if let Some(glide) = state.glide.take() {
                    state.last_glide = Some((at, glide.scale.signum()));
                }
                state.glide_done = false;
                self.impulse(source.clone(), impulse, at, emit);
            }
            WheelPhase::FreeSpin if state.glide.is_none() && !state.glide_done => {
                // A flick soon after the last glide, the same way, is a
                // repeat: it builds on the chain instead of starting over.
                let repeat = state.last_glide.is_some_and(|(ended, direction)| {
                    at.saturating_duration_since(ended) <= CHAIN_WINDOW
                        && direction * impulse.y > 0.0
                });
                state.chain = if repeat { state.chain + 1 } else { 0 };
                // `notches` is the raw count in the same direction as
                // `impulse`, so their ratio is the unsigned sensitivity; the
                // direction comes from the impulse itself.
                state.glide = Some(Glide {
                    scale: impulse.y.signum() * (impulse.y / notches).abs(),
                    velocity: 0.0,
                    boost: chain_boost(boost_per_flick, state.chain),
                    peak_wheel: 0.0,
                    peak_at: at,
                    peaked: false,
                    last_emit: at,
                    next_frame: at + FRAME_PERIOD,
                });
                // The glide takes over from here: the eased tail still in
                // flight would double up with its first frames.
                if self.active.remove(source).is_some()
                    && self.active.is_empty()
                    && self.phased.is_empty()
                {
                    self.output.cancel(emit);
                }
            }
            WheelPhase::FreeSpin => {}
            WheelPhase::Stopped(_) => self.stop_wheel(source, at, emit),
        }
    }

    /// Drop every pending motion of a wheel a finger just stopped.
    fn stop_wheel(
        &mut self,
        source: &ScrollSource,
        at: Instant,
        emit: &mut impl FnMut(ScrollFrame),
    ) {
        if let Some(state) = self.wheels.get_mut(source)
            && let Some(glide) = state.glide.take()
        {
            // A finger catching the wheel to flick again: the next flick may
            // still chain onto this glide.
            state.last_glide = Some((at, glide.scale.signum()));
        }
        if self.active.remove(source).is_some() && self.active.is_empty() && self.phased.is_empty()
        {
            self.output.cancel(emit);
        }
    }

    /// Emit due glide frames, and end the glide of wheels that stopped.
    fn advance_glides(&mut self, at: Instant, force: bool, emit: &mut impl FnMut(ScrollFrame)) {
        let gliding: Vec<ScrollSource> = self
            .wheels
            .iter()
            .filter(|(_, state)| state.glide.is_some())
            .map(|(source, _)| source.clone())
            .collect();
        let per_tick = f64::from(self.momentum.pixels_per_notch.clamp(
            MomentumScrolling::PIXELS_PER_NOTCH_MIN,
            MomentumScrolling::PIXELS_PER_NOTCH_MAX,
        ));
        for source in gliding {
            let Some(state) = self.wheels.get_mut(&source) else {
                continue;
            };
            let phase = state.detector.tick(at);
            // `Ratchet` here means the spin's gesture ended by itself (quiet
            // wheel): the glide coasts out. A finger turning the wheel ends
            // the glide in `wheel_tick` instead, and a finger stop right here.
            let wheel_speed = match phase {
                WheelPhase::Stopped(_) => {
                    self.stop_wheel(&source, at, emit);
                    continue;
                }
                WheelPhase::Ratchet => 0.0,
                WheelPhase::FreeSpin => state.detector.speed(at),
            };
            let Some(glide) = state.glide.as_mut() else {
                continue;
            };
            if !force && at < glide.next_frame {
                continue;
            }
            let elapsed = at.saturating_duration_since(glide.last_emit).as_secs_f64();
            if !glide.peaked {
                if wheel_speed > glide.peak_wheel {
                    glide.peak_wheel = wheel_speed;
                    glide.peak_at = at;
                } else if at.saturating_duration_since(glide.peak_at) >= FLICK_PEAKED_AFTER {
                    glide.peaked = true;
                }
            }
            // The glide rides the wheel, sped up by repeated flicks, for as
            // long as the wheel turns — or, with friction, slows on its own
            // from the moment the flick peaked. It never slows faster than
            // Apple's momentum coast, so its end tapers out instead of
            // stopping with the wheel's last notch.
            let since_peak = at.saturating_duration_since(glide.peak_at).as_secs_f64();
            let target = momentum_output(
                wheel_speed,
                self.momentum.glide_speed,
                self.momentum.response,
            ) * glide.boost
                * momentum_friction(self.momentum.friction, since_peak);
            // A new glide takes over from the eased ratchet steps at once:
            // the page is already moving at about that pace.
            let velocity = if glide.velocity == 0.0 {
                target
            } else {
                momentum_coast(
                    glide.velocity,
                    target,
                    elapsed,
                    self.momentum.glide_ms.min(MomentumScrolling::GLIDE_MS_MAX),
                )
            };
            glide.last_emit = at;
            glide.next_frame = at + FRAME_PERIOD;
            // Under friction the target only approaches zero, so the glide
            // ends once both have fallen below the speed Apple's stops at.
            if velocity < MOMENTUM_END_SPEED && target < MOMENTUM_END_SPEED {
                state.last_glide = Some((at, glide.scale.signum()));
                state.glide = None;
                state.glide_done = phase == WheelPhase::FreeSpin;
                continue;
            }
            glide.velocity = velocity;
            // `scale` is ±(vertical sensitivity multiplier): it carries the
            // scroll direction and the user's sensitivity onto the pixels.
            let pixels = velocity * elapsed * glide.scale;
            if pixels != 0.0 && pixels.is_finite() {
                emit(ScrollFrame::momentum(
                    WheelDelta { x: 0.0, y: pixels },
                    per_tick,
                ));
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
        self.output.progress(impulse, true, None, emit);
        self.phased.insert(source, at + PHASED_IDLE);
    }

    fn advance_due(&mut self, at: Instant, emit: &mut impl FnMut(ScrollFrame)) {
        self.end_idle_phased(at, emit);
        // Display-paced output falls back to the timer only once display
        // frames have stopped arriving.
        if self
            .display_frame
            .is_some_and(|last| at.saturating_duration_since(last) < DISPLAY_FALLBACK)
        {
            return;
        }
        self.advance_glides(at, false, emit);
        self.advance_segments(at, false, emit);
    }

    /// Advance the segments whose frame is due — or all of them, `force`d by
    /// a display frame.
    fn advance_segments(&mut self, at: Instant, force: bool, emit: &mut impl FnMut(ScrollFrame)) {
        let due: Vec<ScrollSource> = self
            .active
            .iter()
            .filter(|(_, motion)| force || motion.next_frame <= at)
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
        if self.phased.len() != before && self.phased.is_empty() && self.active.is_empty() {
            self.output.finish(WheelDelta::ZERO, true, None, emit);
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
                    .filter_map(|state| state.glide.as_ref().map(|glide| glide.next_frame)),
            )
            .min();
        // Display-paced frames come from the display; the timer only takes
        // over once they stop arriving, and then on its own frame cadence,
        // so a stalled display never leaves an expired deadline behind.
        let frame = match self.display_frame {
            Some(last) => frame.map(|due| due.max(last + DISPLAY_FALLBACK)),
            None => frame,
        };
        frame.into_iter().chain(self.phased.values().copied()).min()
    }

    fn cancel_source(&mut self, source: &ScrollSource, emit: &mut impl FnMut(ScrollFrame)) {
        self.wheels.remove(source);
        // Both removals must run, so neither may short-circuit the other.
        let was_smooth = self.active.remove(source).is_some();
        let was_phased = self.phased.remove(source).is_some();
        if (was_smooth || was_phased) && self.active.is_empty() && self.phased.is_empty() {
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
        match update {
            // A phased gesture still open (after a smoothing toggle) owns the
            // stream's end, so a motion finishing under it only progresses it.
            MotionUpdate::Finished(delta) if self.active.is_empty() && self.phased.is_empty() => {
                self.output.finish(delta, false, pixels, emit);
            }
            MotionUpdate::Active(delta) | MotionUpdate::Finished(delta) => {
                self.output.progress(delta, false, pixels, emit);
            }
        }
    }
}

/// The glide multiplier after `chain` repeated flicks at `per_flick` each,
/// capped at [`CHAIN_MAX_BOOST`].
fn chain_boost(per_flick: f64, chain: u32) -> f64 {
    per_flick
        .powi(i32::try_from(chain).unwrap_or(i32::MAX))
        .min(CHAIN_MAX_BOOST)
}

#[cfg(test)]
mod tests;
