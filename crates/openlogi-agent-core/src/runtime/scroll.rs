//! Traditional wheel output owned by one dedicated worker.
//!
//! Hook callbacks submit typed wheel impulses through [`ScrollInputHandle`]
//! without blocking. The worker either scales and emits them directly or
//! evaluates finite smooth motion from absolute timestamps. Pixel-precise input
//! never enters this runtime, so native trackpad and continuous wheel streams
//! cannot be mixed with wheel ticks.

mod worker;

pub use worker::{ScrollInputHandle, ScrollPreferences, ScrollRuntime};

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use openlogi_core::scroll::ScrollDelta;
use openlogi_inject::SmoothScrollPhase;

use crate::runtime::HidppSessionId;

/// Output cadence. Position is evaluated from absolute time, so delayed wakes
/// do not slow or lengthen the animation.
const FRAME_INTERVAL: Duration = Duration::from_millis(8);
/// Ticks separated by at least this much never gain amplitude, whatever the
/// acceleration curve would say.
const ACCEL_WINDOW: Duration = Duration::from_millis(70);
/// Numerator of the tick-rate acceleration curve, in milliseconds: a tick
/// arriving `dt` after its predecessor gains `(1 + ACCEL_RATE_MS/dt) / 2`,
/// clamped between 1 and the configured cap.
const ACCEL_RATE_MS: f64 = 30.0;
/// Ratio of the pulse curve's viscous tail to its damped-force head.
const PULSE_TAIL_RATIO: f64 = 3.0;
/// Upper bound on in-flight pulses per source; free-spin bursts coalesce into
/// the newest pulse past this, keeping frame evaluation O(1)-ish.
const MAX_PULSES: usize = 64;

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

/// Amplitude gain for a tick arriving `interval` after its source's previous
/// tick. Deliberately deterministic so traces are exactly testable.
fn accel_gain(interval: Duration, max_gain: f64) -> f64 {
    if interval >= ACCEL_WINDOW {
        return 1.0;
    }
    let millis = (interval.as_secs_f64() * 1000.0).max(1.0);
    f64::midpoint(1.0, ACCEL_RATE_MS / millis).clamp(1.0, max_gain)
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
    last_tick_at: Instant,
}

impl ActiveMotion {
    fn new(impulse: WheelDelta, at: Instant, tuning: MotionTuning) -> Self {
        Self {
            pulses: vec![Pulse {
                amplitude: impulse.scale(tuning.step),
                started_at: at,
                duration: tuning.duration,
            }],
            settled: WheelDelta::ZERO,
            emitted: WheelDelta::ZERO,
            next_frame: at + FRAME_INTERVAL,
            last_tick_at: at,
        }
    }

    /// Superpose one tick's pulse and evaluate the position at its timestamp.
    fn add_tick(&mut self, impulse: WheelDelta, at: Instant, tuning: MotionTuning) -> MotionUpdate {
        let gain = accel_gain(
            at.saturating_duration_since(self.last_tick_at),
            tuning.max_gain,
        );
        self.last_tick_at = at;
        let amplitude = impulse.scale(tuning.step * gain);
        let coalesce = self.pulses.len() >= MAX_PULSES
            || self.pulses.last().is_some_and(|pulse| {
                at.saturating_duration_since(pulse.started_at) < FRAME_INTERVAL
            });
        if coalesce && let Some(last) = self.pulses.last_mut() {
            last.amplitude = last.amplitude.plus(amplitude);
            if last.amplitude.is_zero() {
                self.pulses.pop();
            }
        } else {
            self.pulses.push(Pulse {
                amplitude,
                started_at: at,
                duration: tuning.duration,
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
