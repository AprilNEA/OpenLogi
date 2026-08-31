//! Traditional wheel output owned by one dedicated worker.
//!
//! Hook callbacks submit typed wheel impulses through [`ScrollInputHandle`]
//! without blocking. The worker either scales and emits them directly or
//! evaluates finite smooth motion from absolute timestamps. Eligible
//! pixel-precise mouse-wheel input may use the worker's direct-output path, but
//! never enters the wheel-tick smoothing model; native trackpads are rejected
//! before submission.

mod worker;

pub use worker::{ScrollInputHandle, ScrollPreferences, ScrollRuntime};

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use openlogi_core::scroll::ScrollDelta;
use openlogi_inject::SmoothScrollPhase;

use crate::runtime::HidppSessionId;

/// Duration of every segment, including a segment restarted by retargeting.
const ANIMATION_DURATION: Duration = Duration::from_millis(100);
/// Output cadence. Position is evaluated from absolute time, so delayed wakes
/// do not slow or lengthen the animation.
const FRAME_INTERVAL: Duration = Duration::from_millis(8);

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

/// A finite cubic smoothstep segment between two cumulative positions.
struct MotionSegment {
    from: WheelDelta,
    target: WheelDelta,
    started_at: Instant,
}

impl MotionSegment {
    fn position_at(&self, at: Instant) -> WheelDelta {
        let elapsed = at.saturating_duration_since(self.started_at);
        let progress = (elapsed.as_secs_f64() / ANIMATION_DURATION.as_secs_f64()).clamp(0.0, 1.0);
        let eased = progress * progress * (3.0 - 2.0 * progress);
        self.from.plus(self.target.minus(self.from).scale(eased))
    }

    fn ends_at(&self) -> Instant {
        self.started_at + ANIMATION_DURATION
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
    fn new(impulse: WheelDelta, at: Instant) -> Self {
        Self {
            segment: MotionSegment {
                from: WheelDelta::ZERO,
                target: impulse,
                started_at: at,
            },
            emitted: WheelDelta::ZERO,
            next_frame: at + FRAME_INTERVAL,
        }
    }

    /// Evaluate the old segment at the impulse timestamp, then restart toward
    /// the cumulative target.
    fn retarget(&mut self, impulse: WheelDelta, at: Instant) -> MotionUpdate {
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
        };
        self.next_frame = at + FRAME_INTERVAL;
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
                self.next_frame += FRAME_INTERVAL;
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
        emit: &mut impl FnMut(ScrollFrame),
    ) {
        if self
            .active
            .get(&source)
            .is_some_and(|motion| motion.segment.is_complete_at(at))
            && let Some(mut completed) = self.active.remove(&source)
        {
            let update = completed.advance(at);
            self.emit_update(update, emit);
        }

        let update = match self.active.entry(source) {
            Entry::Occupied(mut entry) => {
                let update = entry.get_mut().retarget(impulse, at);
                if update.is_finished() {
                    entry.remove();
                }
                Some(update)
            }
            Entry::Vacant(entry) => {
                entry.insert(ActiveMotion::new(impulse, at));
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
