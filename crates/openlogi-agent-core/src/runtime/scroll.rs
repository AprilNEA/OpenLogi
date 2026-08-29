//! Traditional wheel output owned by one dedicated worker.
//!
//! Hook callbacks submit typed wheel impulses through [`ScrollInputHandle`]
//! without blocking. The worker either scales and emits them directly or
//! evaluates finite smooth motion from absolute timestamps. Pixel-precise input
//! never enters this runtime, so native trackpad and continuous wheel streams
//! cannot be mixed with wheel ticks.

mod worker;

pub use worker::{ScrollInputHandle, ScrollPreferences, ScrollRuntime};

use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use openlogi_core::scroll::ScrollDelta;
use openlogi_inject::SmoothScrollPhase;

use crate::runtime::HidppSessionId;

/// Output cadence. Position is evaluated from absolute time, so delayed wakes
/// do not slow or lengthen the animation.
const FRAME_INTERVAL: Duration = Duration::from_millis(8);
/// Sliding window over which a source's recent tick distance becomes its
/// wheel rate for acceleration. Sized so a notched wheel emitting one full
/// tick every `dt < ACCEL_WINDOW` reproduces the classic per-interval gain.
const ACCEL_WINDOW: Duration = Duration::from_millis(70);
/// Numerator of the tick-rate acceleration curve, in milliseconds: a source
/// whose window holds one tick per `dt` ms gains `(1 + ACCEL_RATE_MS/dt) / 2`,
/// clamped between 1 and the configured cap.
const ACCEL_RATE_MS: f64 = 30.0;
/// Upper bound on remembered window entries; overflow drops the oldest, which
/// can only understate the rate. 128 entries cover ~1.8 kHz reporting across
/// the full window — beyond any real wheel.
const ACCEL_WINDOW_MAX_TICKS: usize = 128;
/// Ratio of the pulse curve's viscous tail to its damped-force head.
const PULSE_TAIL_RATIO: f64 = 3.0;
/// Upper bound on in-flight pulses per source; free-spin bursts merge into
/// the newest pulse past this (see [`ActiveMotion::add_tick`]), keeping frame
/// evaluation O(1)-ish.
const MAX_PULSES: usize = 64;
/// How long the OS acceleration baked into pre-accelerated input takes to
/// decay after the wheel changes direction. Measured on macOS 15.7 with an
/// MX Master 4: a flip 68 ms after the opposing stream arrived with its
/// first tick 14× the cold-start magnitude, while flips ≥ 435 ms after it
/// started cold like every from-rest burst.
const REVERSAL_COOLDOWN: Duration = Duration::from_millis(400);
/// Raw opposing distance (in the input's own units) a flip must accumulate
/// before the tracked direction changes. Free-spin wheels jitter a fraction
/// of a tick backwards at the end of a flick; below this floor the opposing
/// ticks are attenuated but the resumed main direction stays untouched.
const REVERSAL_COMMIT: f64 = 0.5;
/// Per-tick line magnitude above which a corrective burst is compressed.
/// The OS acceleration curve reaches ~8 lines per tick within half a second
/// even from a cold start; that speed reads as intended travel when the
/// direction is sustained but as overshoot when it follows a reversal
/// (measured 2026-08-29: physically identical bursts were flagged as
/// overshoot only in the reversal context).
const REVERSAL_KNEE: f64 = 3.0;
/// Compression slope above [`REVERSAL_KNEE`] at the moment of the flip.
const REVERSAL_KNEE_RATIO: f64 = 4.0;
/// How long after the first opposing tick the corrective compression takes
/// to fade back to unity, so a reversal that turns into sustained scrolling
/// regains full throughput.
const REVERSAL_RECOVERY: Duration = Duration::from_millis(2000);

/// Normalized two-phase pulse: a damped-force head (`u − 1 + e^(−u)`) blending
/// C¹-continuously into an exponential viscous tail at one part head to
/// [`PULSE_TAIL_RATIO`] parts tail, rescaled so `P(0) = 0` and `P(1) = 1`.
/// Monotone in between; clamped outside.
fn pulse_curve(progress: f64) -> f64 {
    fn raw(u: f64) -> f64 {
        if u < 1.0 {
            u - 1.0 + (-u).exp()
        } else {
            let head_end = (-1.0_f64).exp();
            head_end + (1.0 - head_end) * (1.0 - (1.0 - u).exp())
        }
    }
    let scale = 1.0 + PULSE_TAIL_RATIO;
    (raw(progress.clamp(0.0, 1.0) * scale) / raw(scale)).clamp(0.0, 1.0)
}

/// Amplitude gain for a source whose ticks summed to `window_ticks` of raw
/// wheel distance across the trailing [`ACCEL_WINDOW`]. Rate-based rather
/// than interval-based: a high-resolution or free-spin wheel reports many
/// tiny deltas milliseconds apart, so the gap between events says nothing
/// about how fast the wheel is actually turning — only the distance covered
/// per unit time does. A notched wheel (one full tick per report, `dt`
/// apart) fills the window with `ACCEL_WINDOW/dt` ticks and lands on the
/// classic `(1 + ACCEL_RATE_MS/dt) / 2` within one tick's worth of rate.
/// Deliberately deterministic so traces are exactly testable.
fn accel_gain(window_ticks: f64, max_gain: f64) -> f64 {
    let rate_per_ms = window_ticks / (ACCEL_WINDOW.as_secs_f64() * 1000.0);
    f64::midpoint(1.0, ACCEL_RATE_MS * rate_per_ms).clamp(1.0, max_gain)
}

/// Motion settings captured per accepted tick, so a live settings change
/// affects only ticks after it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MotionTuning {
    /// Amplitude multiplier per wheel tick, in native lines.
    pub(crate) step: f64,
    /// Animation length of one tick's pulse.
    pub(crate) duration: Duration,
    /// Cap on [`accel_gain`]; `1.0` disables acceleration.
    pub(crate) max_gain: f64,
    /// Whether the source's distances already carry an OS acceleration curve
    /// keyed on unsigned wheel speed. Such input stays hot across a quick
    /// direction flip, so opposing ticks get a cold start re-imposed via
    /// [`ReversalCooldown`].
    pub(crate) preaccelerated: bool,
}

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

    /// Scale each axis by its own factor.
    fn scale_axes(self, factors: Self) -> Self {
        Self {
            x: self.x * factors.x,
            y: self.y * factors.y,
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
}

impl ScrollFrame {
    fn new(delta: WheelDelta, phase: SmoothScrollPhase) -> Self {
        Self { delta, phase }
    }

    fn post(self) {
        openlogi_inject::post_smooth_scroll(self.delta.into(), self.phase);
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

/// One tick's finite animation: `amplitude × pulse_curve(elapsed/duration)`.
struct Pulse {
    amplitude: WheelDelta,
    started_at: Instant,
    duration: Duration,
    /// Arrival of the first tick this pulse absorbed. The frame-window merge
    /// in [`ActiveMotion::add_tick`] is measured from here rather than from
    /// `started_at`, which a merge moves forward — so a steady stream still
    /// hands off to a fresh pulse one frame after each pulse began instead of
    /// restarting the same pulse forever.
    anchor: Instant,
}

impl Pulse {
    fn position_at(&self, at: Instant) -> WheelDelta {
        let elapsed = at.saturating_duration_since(self.started_at);
        let progress = elapsed.as_secs_f64() / self.duration.as_secs_f64();
        self.amplitude.scale(pulse_curve(progress))
    }

    fn ends_at(&self) -> Instant {
        self.started_at + self.duration
    }

    fn is_complete_at(&self, at: Instant) -> bool {
        at >= self.ends_at()
    }
}

/// One axis of cold-start attenuation for quick direction reversals of
/// pre-accelerated input.
///
/// The OS acceleration baked into such input is keyed on unsigned wheel
/// speed, so the first ticks after a fast flip arrive as hot as the stream
/// they interrupt instead of ramping from rest — amplified by the step
/// multiplier, that is felt as reversal overshoot. Scaling an opposing tick
/// by the time elapsed since the departed direction's last tick, relative to
/// [`REVERSAL_COOLDOWN`], reproduces the measured cold-start ramp on quick
/// flips and leaves leisurely reversals (which arrive cold anyway) exactly
/// unscaled.
#[derive(Default)]
struct ReversalCooldown {
    /// Sign of the committed scroll direction; `0` before any input.
    dir: i8,
    /// Arrival of the committed direction's most recent tick.
    last_at: Option<Instant>,
    /// Last tick time of the direction scrolled away from, kept while a flip
    /// is still recovering.
    flip_ref: Option<Instant>,
    /// Arrival of the pending flip's first opposing tick — the corrective
    /// burst's own age, from which the knee compression fades.
    flip_started: Option<Instant>,
    /// Raw opposing distance accumulated towards [`REVERSAL_COMMIT`].
    pending: f64,
    /// Whether the pending flip has crossed [`REVERSAL_COMMIT`] and `dir`
    /// now points the new way.
    committed: bool,
}

impl ReversalCooldown {
    fn clear_flip(&mut self) {
        self.flip_ref = None;
        self.flip_started = None;
        self.pending = 0.0;
        self.committed = false;
    }

    /// Scale one tick of a reversal in progress: the cooldown ramp re-imposes
    /// a cold start relative to the departed direction's last tick (the OS
    /// acceleration stays hot across a quick flip), and the knee compression
    /// tames the corrective burst's peak speed, fading out over
    /// [`REVERSAL_RECOVERY`] from the burst's first opposing tick.
    fn rescale(value: f64, flip_ref: Instant, flip_started: Instant, at: Instant) -> f64 {
        let ramp = (at.saturating_duration_since(flip_ref).as_secs_f64()
            / REVERSAL_COOLDOWN.as_secs_f64())
        .min(1.0);
        let recovery = (at.saturating_duration_since(flip_started).as_secs_f64()
            / REVERSAL_RECOVERY.as_secs_f64())
        .min(1.0);
        let magnitude = value.abs() * ramp;
        let compressed = if magnitude > REVERSAL_KNEE {
            REVERSAL_KNEE + (magnitude - REVERSAL_KNEE) / REVERSAL_KNEE_RATIO
        } else {
            magnitude
        };
        value.signum() * (compressed + (magnitude - compressed) * recovery)
    }

    fn attenuate(&mut self, value: f64, at: Instant) -> f64 {
        if value == 0.0 {
            return value;
        }
        let sign: i8 = if value > 0.0 { 1 } else { -1 };
        if self.dir == 0 {
            self.dir = sign;
            self.last_at = Some(at);
            return value;
        }
        if sign == self.dir {
            self.last_at = Some(at);
            return match (self.flip_ref, self.flip_started) {
                // Committed reversal still recovering: keep rescaling until
                // the compression has fully faded.
                (Some(flip_ref), Some(flip_started)) if self.committed => {
                    if at.saturating_duration_since(flip_started) >= REVERSAL_RECOVERY {
                        self.clear_flip();
                        value
                    } else {
                        Self::rescale(value, flip_ref, flip_started, at)
                    }
                }
                // The opposing ticks never committed — free-spin jitter —
                // and the main direction resumed; drop the pending flip.
                (Some(_), _) => {
                    self.clear_flip();
                    value
                }
                _ => value,
            };
        }
        // Opposing tick: keep the pending flip's references, or start a
        // fresh flip from the departed direction's last arrival.
        let (flip_ref, flip_started) = match (self.flip_ref, self.flip_started) {
            (Some(flip_ref), Some(flip_started)) if !self.committed => (flip_ref, flip_started),
            _ => {
                let flip_ref = self.last_at.unwrap_or(at);
                self.flip_ref = Some(flip_ref);
                self.flip_started = Some(at);
                self.pending = 0.0;
                self.committed = false;
                (flip_ref, at)
            }
        };
        self.pending += value.abs();
        if self.pending >= REVERSAL_COMMIT {
            self.dir = sign;
            self.last_at = Some(at);
            self.committed = true;
        }
        Self::rescale(value, flip_ref, flip_started, at)
    }
}

/// A source exists in the state map only while it has in-flight pulses.
///
/// Overlapping pulses superpose: the source's position is the settled distance
/// of completed pulses plus every live pulse's current contribution. Opposite
/// ticks superpose too — free-spin wheels jitter a tick backwards at the end
/// of a flick, and a finite pulse set already bounds how long any direction
/// change takes to win.
struct ActiveMotion {
    pulses: Vec<Pulse>,
    /// Sum of completed pulses' full amplitudes, so pruning never moves the
    /// position.
    settled: WheelDelta,
    emitted: WheelDelta,
    next_frame: Instant,
    /// Trailing [`ACCEL_WINDOW`] of accepted ticks — `(arrival, unsigned raw
    /// wheel distance per axis)` — from which [`accel_gain`] derives each
    /// axis's own rate, so a fast spin on one axis never accelerates slow
    /// movement on the other.
    recent_ticks: VecDeque<(Instant, WheelDelta)>,
    reversal_x: ReversalCooldown,
    reversal_y: ReversalCooldown,
}

impl ActiveMotion {
    fn new(impulse: WheelDelta, at: Instant, tuning: MotionTuning) -> Self {
        let mut motion = Self {
            pulses: Vec::new(),
            settled: WheelDelta::ZERO,
            emitted: WheelDelta::ZERO,
            next_frame: at + FRAME_INTERVAL,
            recent_ticks: VecDeque::new(),
            reversal_x: ReversalCooldown::default(),
            reversal_y: ReversalCooldown::default(),
        };
        let impulse = motion.cooled(impulse, at, tuning);
        let gain = motion.windowed_gain(impulse, at, tuning.max_gain);
        motion.pulses.push(Pulse {
            amplitude: impulse.scale(tuning.step).scale_axes(gain),
            started_at: at,
            duration: tuning.duration,
            anchor: at,
        });
        motion
    }

    /// Record one tick in the rate window and return each axis's amplitude
    /// gain.
    fn windowed_gain(&mut self, impulse: WheelDelta, at: Instant, max_gain: f64) -> WheelDelta {
        while self
            .recent_ticks
            .front()
            .is_some_and(|(tick_at, _)| at.saturating_duration_since(*tick_at) > ACCEL_WINDOW)
        {
            self.recent_ticks.pop_front();
        }
        self.recent_ticks.push_back((
            at,
            WheelDelta {
                x: impulse.x.abs(),
                y: impulse.y.abs(),
            },
        ));
        if self.recent_ticks.len() > ACCEL_WINDOW_MAX_TICKS {
            self.recent_ticks.pop_front();
        }
        let window_ticks = self
            .recent_ticks
            .iter()
            .fold(WheelDelta::ZERO, |sum, (_, ticks)| sum.plus(*ticks));
        WheelDelta {
            x: accel_gain(window_ticks.x, max_gain),
            y: accel_gain(window_ticks.y, max_gain),
        }
    }

    /// Attenuate quick direction reversals of pre-accelerated input; raw
    /// sources pass through untouched.
    fn cooled(&mut self, impulse: WheelDelta, at: Instant, tuning: MotionTuning) -> WheelDelta {
        if !tuning.preaccelerated {
            return impulse;
        }
        let cooled = WheelDelta {
            x: self.reversal_x.attenuate(impulse.x, at),
            y: self.reversal_y.attenuate(impulse.y, at),
        };
        if cooled != impulse {
            tracing::trace!(
                raw_x = impulse.x,
                raw_y = impulse.y,
                cooled_x = cooled.x,
                cooled_y = cooled.y,
                "reversal cooldown attenuated a tick"
            );
        }
        cooled
    }

    /// Superpose one tick's pulse and evaluate the position at its timestamp.
    ///
    /// Ticks landing within one frame of the newest pulse's first tick merge
    /// into that pulse when both run the same duration — but never by
    /// inheriting its clock. The merge settles whatever the pulse has
    /// delivered so far and restarts its remainder together with the new tick
    /// from `at`: position is continuous, net distance is conserved, and the
    /// tick neither starts partially progressed nor ends at an older pulse's
    /// deadline. A tick accepted under a freshly reloaded duration gets its
    /// own pulse instead, so a reload never re-times motion accepted under
    /// the previous configuration. Only the [`MAX_PULSES`] bound merges
    /// unconditionally; that is the one place the model approximates, and it
    /// still conserves distance and continuity.
    fn add_tick(&mut self, impulse: WheelDelta, at: Instant, tuning: MotionTuning) -> MotionUpdate {
        let impulse = self.cooled(impulse, at, tuning);
        let gain = self.windowed_gain(impulse, at, tuning.max_gain);
        let amplitude = impulse.scale(tuning.step).scale_axes(gain);
        let merge = self.pulses.len() >= MAX_PULSES
            || self.pulses.last().is_some_and(|pulse| {
                pulse.duration == tuning.duration
                    && at.saturating_duration_since(pulse.anchor) < FRAME_INTERVAL
            });
        if merge && let Some(last) = self.pulses.last_mut() {
            let delivered = last.position_at(at);
            self.settled = self.settled.plus(delivered);
            let remainder = last.amplitude.minus(delivered).plus(amplitude);
            if remainder.is_zero() {
                self.pulses.pop();
            } else {
                last.amplitude = remainder;
                last.started_at = at;
                last.duration = tuning.duration;
            }
        } else {
            self.pulses.push(Pulse {
                amplitude,
                started_at: at,
                duration: tuning.duration,
                anchor: at,
            });
        }
        let update = self.evaluate(at);
        if !update.is_finished() {
            // Never re-evaluate before the tick's own timestamp: an already
            // due frame deadline would read an earlier position and emit an
            // opposing delta.
            self.next_frame = at + FRAME_INTERVAL;
        }
        update
    }

    /// Evaluate the position at `at` and report whether the source remains
    /// active after this update.
    fn advance(&mut self, at: Instant) -> MotionUpdate {
        let update = self.evaluate(at);
        if !update.is_finished() {
            while self.next_frame <= at {
                self.next_frame += FRAME_INTERVAL;
            }
            if let Some(ends_at) = self.ends_at() {
                self.next_frame = self.next_frame.min(ends_at);
            }
        }
        update
    }

    fn evaluate(&mut self, at: Instant) -> MotionUpdate {
        let position = self.position_at(at);
        let delta = self.delta_to(position);
        self.prune(at);
        if self.pulses.is_empty() {
            MotionUpdate::Finished(delta)
        } else {
            MotionUpdate::Active(delta)
        }
    }

    fn position_at(&self, at: Instant) -> WheelDelta {
        self.pulses
            .iter()
            .fold(self.settled, |sum, pulse| sum.plus(pulse.position_at(at)))
    }

    fn prune(&mut self, at: Instant) {
        self.pulses.retain(|pulse| {
            if pulse.is_complete_at(at) {
                self.settled = self.settled.plus(pulse.amplitude);
                false
            } else {
                true
            }
        });
    }

    fn ends_at(&self) -> Option<Instant> {
        self.pulses.iter().map(Pulse::ends_at).max()
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
#[derive(Default)]
enum OutputStream {
    #[default]
    Idle,
    Active,
}

impl OutputStream {
    fn progress(&mut self, delta: WheelDelta, emit: &mut impl FnMut(ScrollFrame)) {
        if delta.is_zero() {
            return;
        }
        let phase = match self {
            Self::Idle => {
                *self = Self::Active;
                SmoothScrollPhase::Began
            }
            Self::Active => SmoothScrollPhase::Changed,
        };
        emit(ScrollFrame::new(delta, phase));
    }

    fn finish(&mut self, delta: WheelDelta, emit: &mut impl FnMut(ScrollFrame)) {
        match self {
            Self::Idle if !delta.is_zero() => {
                emit(ScrollFrame::new(delta, SmoothScrollPhase::Began));
                emit(ScrollFrame::new(WheelDelta::ZERO, SmoothScrollPhase::Ended));
            }
            Self::Active => emit(ScrollFrame::new(delta, SmoothScrollPhase::Ended)),
            Self::Idle => {}
        }
        *self = Self::Idle;
    }

    fn cancel(&mut self, emit: &mut impl FnMut(ScrollFrame)) {
        if matches!(self, Self::Active) {
            emit(ScrollFrame::new(
                WheelDelta::ZERO,
                SmoothScrollPhase::Cancelled,
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
    output: OutputStream,
}

impl ScrollEngine {
    fn impulse(
        &mut self,
        source: ScrollSource,
        impulse: WheelDelta,
        at: Instant,
        tuning: MotionTuning,
        emit: &mut impl FnMut(ScrollFrame),
    ) {
        let update = match self.active.entry(source) {
            Entry::Occupied(mut entry) => {
                let update = entry.get_mut().add_tick(impulse, at, tuning);
                if update.is_finished() {
                    entry.remove();
                }
                Some(update)
            }
            Entry::Vacant(entry) => {
                entry.insert(ActiveMotion::new(impulse, at, tuning));
                None
            }
        };
        if let Some(update) = update {
            self.emit_update(update, emit);
        }
    }

    fn advance_due(&mut self, at: Instant, emit: &mut impl FnMut(ScrollFrame)) {
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
            self.emit_update(update, emit);
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.active.values().map(|motion| motion.next_frame).min()
    }

    fn cancel_source(&mut self, source: &ScrollSource, emit: &mut impl FnMut(ScrollFrame)) {
        if self.active.remove(source).is_some() && self.active.is_empty() {
            self.output.cancel(emit);
        }
    }

    fn cancel_all(&mut self, emit: &mut impl FnMut(ScrollFrame)) {
        self.active.clear();
        self.output.cancel(emit);
    }

    fn emit_update(&mut self, update: MotionUpdate, emit: &mut impl FnMut(ScrollFrame)) {
        match update {
            MotionUpdate::Finished(delta) if self.active.is_empty() => {
                self.output.finish(delta, emit);
            }
            MotionUpdate::Active(delta) | MotionUpdate::Finished(delta) => {
                self.output.progress(delta, emit);
            }
        }
    }
}

#[cfg(test)]
mod tests;
