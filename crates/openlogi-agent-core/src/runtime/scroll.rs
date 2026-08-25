//! Finite smooth-scroll animation owned by one dedicated worker.
//!
//! Hook callbacks submit typed wheel impulses through [`ScrollInputHandle`]
//! without blocking. The worker evaluates motion from absolute timestamps,
//! retargets an active segment when more input arrives, and emits balanced
//! phased output. Pixel-precise input never enters this runtime, so native
//! trackpad and continuous wheel streams cannot be mixed with wheel ticks.

mod worker;

pub use worker::{ScrollInputHandle, ScrollRuntime};

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use openlogi_core::scroll::ScrollDelta;
use openlogi_inject::SmoothScrollPhase;

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
/// mouse; macOS and Windows use one global callback thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ScrollSource {
    OsHook(ThreadId),
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
/// target. `output_started` records whether a macOS `Began` phase needs to be
/// emitted before its terminal phase.
struct ActiveMotion {
    segment: MotionSegment,
    emitted: WheelDelta,
    next_frame: Instant,
    output_started: bool,
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
            output_started: false,
        }
    }

    /// Evaluate the old segment at the impulse timestamp, then restart toward
    /// the cumulative target. Returns `false` when opposing input exactly
    /// consumes the remaining target and this motion is finished immediately.
    fn retarget(
        &mut self,
        impulse: WheelDelta,
        at: Instant,
        emit: &mut impl FnMut(ScrollFrame),
    ) -> bool {
        let position = self.segment.position_at(at);
        let target = self.segment.target.plus(impulse);
        if target == position {
            self.finish_at(position, emit);
            return false;
        }

        self.emit_progress(position, emit);
        self.segment = MotionSegment {
            from: position,
            target,
            started_at: at,
        };
        self.next_frame = at + FRAME_INTERVAL;
        true
    }

    /// Emit the position at `at`. Returns whether the segment is complete.
    fn advance(&mut self, at: Instant, emit: &mut impl FnMut(ScrollFrame)) -> bool {
        let complete = self.segment.is_complete_at(at);
        let position = self.segment.position_at(at);
        if complete {
            self.finish_at(position, emit);
        } else {
            self.emit_progress(position, emit);
            while self.next_frame <= at {
                self.next_frame += FRAME_INTERVAL;
            }
            self.next_frame = self.next_frame.min(self.segment.ends_at());
        }
        complete
    }

    fn emit_progress(&mut self, position: WheelDelta, emit: &mut impl FnMut(ScrollFrame)) {
        let delta = position.minus(self.emitted);
        if delta.is_zero() {
            return;
        }
        let phase = if self.output_started {
            SmoothScrollPhase::Changed
        } else {
            self.output_started = true;
            SmoothScrollPhase::Began
        };
        self.emitted = position;
        emit(ScrollFrame::new(delta, phase));
    }

    fn finish_at(&mut self, position: WheelDelta, emit: &mut impl FnMut(ScrollFrame)) {
        let delta = position.minus(self.emitted);
        if self.output_started {
            self.emitted = position;
            emit(ScrollFrame::new(delta, SmoothScrollPhase::Ended));
        } else if !delta.is_zero() {
            self.emitted = position;
            self.output_started = true;
            emit(ScrollFrame::new(delta, SmoothScrollPhase::Began));
            emit(ScrollFrame::new(WheelDelta::ZERO, SmoothScrollPhase::Ended));
        }
    }

    fn cancel(self, emit: &mut impl FnMut(ScrollFrame)) {
        if self.output_started {
            emit(ScrollFrame::new(
                WheelDelta::ZERO,
                SmoothScrollPhase::Cancelled,
            ));
        }
    }
}

/// Pure per-source state machine. Absence from the map represents idle, so an
/// idle source cannot accidentally retain a target or scheduled deadline.
#[derive(Default)]
struct ScrollEngine {
    active: HashMap<ScrollSource, ActiveMotion>,
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
            completed.advance(at, emit);
        }

        match self.active.entry(source) {
            Entry::Occupied(mut entry) => {
                if !entry.get_mut().retarget(impulse, at, emit) {
                    entry.remove();
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(ActiveMotion::new(impulse, at));
            }
        }
    }

    fn advance_due(&mut self, at: Instant, emit: &mut impl FnMut(ScrollFrame)) {
        let due: Vec<ScrollSource> = self
            .active
            .iter()
            .filter_map(|(source, motion)| (motion.next_frame <= at).then_some(*source))
            .collect();
        for source in due {
            let complete = self
                .active
                .get_mut(&source)
                .is_some_and(|motion| motion.advance(at, emit));
            if complete {
                self.active.remove(&source);
            }
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.active.values().map(|motion| motion.next_frame).min()
    }

    fn cancel_all(&mut self, emit: &mut impl FnMut(ScrollFrame)) {
        for (_, motion) in self.active.drain() {
            motion.cancel(emit);
        }
    }
}

#[cfg(test)]
mod tests;
