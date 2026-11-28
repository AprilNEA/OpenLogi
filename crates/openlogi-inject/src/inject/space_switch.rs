//! Space-switch transaction policy, independent of macOS FFI for regression tests.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, Instant};

const PREPARATION_TIMEOUT: Duration = Duration::from_secs(2);
const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(2);

/// Logi Options+ 2.8 drives its three-finger horizontal swipe from a timer of
/// this period (`injectionPeriodInMs`, 16.6 ms), one frame per tick.
const SLIDE_PERIOD: Duration = Duration::from_micros(16_600);
/// Each frame travels this share of the distance still to go, so the slide
/// eases out the way Options+ consumes its pending swipe motion per tick.
const SLIDE_STEP: f64 = 0.15;
/// The slide lets go once less than this share of a desktop width remains;
/// the Dock finishes the switch from there.
const SLIDE_RELEASE: f64 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    Previous,
    Next,
}

impl Direction {
    /// One Space in this direction, counted toward Next.
    const fn step(self) -> i32 {
        match self {
            Self::Previous => -1,
            Self::Next => 1,
        }
    }

    /// The direction of a net count of [`Self::step`]s, or `None` when they
    /// cancel out.
    const fn toward(steps: i32) -> Option<Self> {
        match steps {
            1.. => Some(Self::Next),
            ..0 => Some(Self::Previous),
            0 => None,
        }
    }

    pub(super) fn sign(self) -> f64 {
        f64::from(self.step())
    }
}

/// The phase one frame of a slide reports to the Dock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SlidePhase {
    /// The first frame: the Dock starts moving the neighbouring Space in.
    Began,
    /// A later frame: the Dock follows to its progress.
    Changed,
    /// The release: the Dock finishes the switch.
    Ended,
}

/// One frame of a slide to the neighbouring Space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SlideFrame {
    pub phase: SlidePhase,
    /// Travel since the slide began, in desktop widths, signed like
    /// [`Direction::sign`].
    pub progress: f64,
    /// When to post the frame, counted from the first one.
    pub at: Duration,
}

/// The frames that slide one Space over in `direction`, in posting order.
///
/// One frame per [`SLIDE_PERIOD`], each covering [`SLIDE_STEP`] of the
/// distance left, until less than [`SLIDE_RELEASE`] of it remains. The
/// release then follows one period later at the same progress.
pub(super) fn slide(direction: Direction) -> Vec<SlideFrame> {
    let mut frames = Vec::new();
    let mut remaining = 1.0;
    let mut at = Duration::ZERO;
    while remaining >= SLIDE_RELEASE {
        remaining *= 1.0 - SLIDE_STEP;
        let phase = if frames.is_empty() {
            SlidePhase::Began
        } else {
            SlidePhase::Changed
        };
        frames.push(SlideFrame {
            phase,
            progress: direction.sign() * (1.0 - remaining),
            at,
        });
        at += SLIDE_PERIOD;
    }
    frames.push(SlideFrame {
        phase: SlidePhase::Ended,
        progress: direction.sign() * (1.0 - remaining),
        at,
    });
    frames
}

/// The clock a slide is paced by and the sink its frames go to.
pub(super) trait Pace<T> {
    /// Time since a fixed point of this clock.
    fn now(&self) -> Duration;
    /// Block for `duration`. A sleep can end late.
    fn sleep(&mut self, duration: Duration);
    /// Post one frame.
    fn post(&mut self, frame: &T);
}

/// Post `frames` in order, each at its time from the start of the call.
///
/// The wait targets a frame's time from the start, not from the frame
/// before, so a late wakeup does not delay the frames after it. A frame
/// that is already due goes out at once.
pub(super) fn pace<T>(frames: &[(Duration, T)], out: &mut impl Pace<T>) {
    let start = out.now();
    for (at, frame) in frames {
        let wait = start.saturating_add(*at).saturating_sub(out.now());
        if !wait.is_zero() {
            out.sleep(wait);
        }
        out.post(frame);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SpaceState {
    pub display: String,
    pub current: u64,
    /// Mission Control order, including fullscreen Spaces. Never sort by ID.
    pub ordered: Vec<u64>,
}

impl SpaceState {
    fn target(&self, direction: Direction) -> Result<Option<u64>, Failure> {
        if self.display.is_empty()
            || self.current == 0
            || self
                .ordered
                .iter()
                .enumerate()
                .any(|(i, id)| *id == 0 || self.ordered[..i].contains(id))
        {
            // #1086: this rejection needs no multi-display topology at all —
            // a zero/empty id or a duplicate in `ordered` fails it on a
            // single display too, unlike the display-match miss #1694
            // already logs in the macOS backend.
            tracing::debug!(
                display = %self.display,
                current = self.current,
                ordered = ?self.ordered,
                "Space target: the read Space list failed validation"
            );
            return Err(Failure::Unavailable);
        }
        let Some(index) = self.ordered.iter().position(|id| *id == self.current) else {
            // The current Space passed validation above (nonzero, no
            // duplicates) but isn't in `ordered` at all — e.g. a Space kind
            // the `type` filter upstream excludes (Stage Manager, a kind
            // other than the plain/fullscreen 0|4 it keeps).
            tracing::debug!(
                display = %self.display,
                current = self.current,
                ordered = ?self.ordered,
                "Space target: the current Space is not in the ordered list"
            );
            return Err(Failure::Unavailable);
        };
        let neighbor = match direction {
            Direction::Previous => index.checked_sub(1),
            Direction::Next => index.checked_add(1),
        };
        Ok(neighbor.and_then(|i| self.ordered.get(i)).copied())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Unavailable,
    ContextChanged,
    PostFailed,
    TimedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Boundary,
    Reached(u64),
}

/// One native transaction owns its observer and monotonic start time.
pub(super) trait Backend {
    fn state(&mut self) -> Result<SpaceState, Failure>;
    /// Finish allocation and cursor checks before committing the gate. Never
    /// put native preparation inside the committed, non-cancellable output.
    fn post(&mut self, direction: Direction, gate: PostGate) -> Result<(), Failure>;
    fn elapsed(&self) -> Duration;
    /// Wake on a notification or the deadline, without losing notifications
    /// between `state` and this call. A wakeup alone never proves success.
    fn wait_for_change(&mut self, remaining: Duration);
}

#[repr(u8)]
enum PostState {
    Preparing,
    Posting,
    Canceled,
}

struct PostControl {
    state: AtomicU8,
    deadline: Instant,
}

impl PostControl {
    /// False means posting already won; its native calls cannot be canceled.
    fn cancel(&self) -> bool {
        match self.state.compare_exchange(
            PostState::Preparing as u8,
            PostState::Canceled as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => true,
            Err(state) => state == PostState::Canceled as u8,
        }
    }
}

/// Sole authority to emit a switch's frames. Cancellation and commitment
/// arbitrate atomically, so a timed-out native query cannot later produce
/// stale output.
pub(super) struct PostGate {
    control: Arc<PostControl>,
    posted: mpsc::Sender<()>,
}

struct PostWait {
    control: Arc<PostControl>,
    receive: mpsc::Receiver<()>,
}

impl PostGate {
    /// A gate no caller waits on, for a step queued behind a switch. Its
    /// preparation keeps the same deadline, so a stalled query still cannot
    /// post late.
    pub(super) fn unobserved() -> Self {
        Self::preparing().0
    }

    /// A gate whose preparation must commit within [`PREPARATION_TIMEOUT`].
    fn preparing() -> (Self, PostWait) {
        Self::new(Instant::now() + PREPARATION_TIMEOUT)
    }

    fn new(deadline: Instant) -> (Self, PostWait) {
        let control = Arc::new(PostControl {
            state: AtomicU8::new(PostState::Preparing as u8),
            deadline,
        });
        let (posted, receive) = mpsc::channel();
        (
            Self {
                control: Arc::clone(&control),
                posted,
            },
            PostWait { control, receive },
        )
    }

    /// Only the post calls belong in `emit`, plus the fixed wait between a
    /// slide's frames: no allocation, state queries, retries, or cancellation
    /// checks between the balanced phases. The caller waits until `emit`
    /// returns, so no later action is posted in the middle of a slide.
    pub(super) fn commit(self, emit: impl FnOnce()) -> Result<(), Failure> {
        if Instant::now() >= self.control.deadline {
            self.control.cancel();
            return Err(Failure::TimedOut);
        }
        self.control
            .state
            .compare_exchange(
                PostState::Preparing as u8,
                PostState::Posting as u8,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .map_err(|_| Failure::TimedOut)?;
        emit();
        let _ = self.posted.send(());
        Ok(())
    }
}

impl PostWait {
    fn wait(self, now: Instant) -> Result<(), Failure> {
        let remaining = self.control.deadline.saturating_duration_since(now);
        if self.receive.recv_timeout(remaining) == Err(mpsc::RecvTimeoutError::Timeout) {
            if self.control.cancel() {
                return Err(Failure::TimedOut);
            }
            // Posting won the race. Do not release later actions before both
            // native calls return; a second timeout would allow a late swipe.
            let _ = self.receive.recv();
        }
        // Disconnection is cancellation/worker exit, not evidence of success.
        // After unwinding it can also mean partial output; never retry it.
        Ok(())
    }
}

/// Bound preparation, not native event posting. Return after irrevocable
/// cancellation, posting acknowledgement, or worker exit. Confirmation stays
/// detached; a canceled but stuck worker retains its [`Run`].
pub(super) fn spawn_ordered(work: impl FnOnce(PostGate) + Send + 'static) -> Result<(), Failure> {
    let (gate, wait) = PostGate::preparing();
    std::thread::Builder::new()
        .name("openlogi-spaces".into())
        .spawn(move || work(gate))
        .map_err(|error| {
            tracing::warn!(%error, "Space switch worker unavailable");
            Failure::Unavailable
        })?;
    wait.wait(Instant::now())
}

pub(super) fn run(
    backend: &mut impl Backend,
    direction: Direction,
    gate: PostGate,
) -> Result<Outcome, Failure> {
    let initial = backend.state()?;
    let Some(target) = initial.target(direction)? else {
        return Ok(Outcome::Boundary);
    };
    // Topology or active-Space changes during preparation invalidate the plan.
    if backend.state()? != initial {
        return Err(Failure::ContextChanged);
    }
    backend.post(direction, gate)?;
    loop {
        let state = backend.state()?;
        if state.display != initial.display || state.ordered != initial.ordered {
            return Err(Failure::ContextChanged);
        }
        if state.current == target {
            return Ok(Outcome::Reached(target));
        }
        if state.current != initial.current {
            return Err(Failure::ContextChanged);
        }
        // Read BEFORE checking the deadline: a missed/coalesced notification
        // still gets one authoritative final query. Never resend on timeout.
        let Some(remaining) = CONFIRMATION_TIMEOUT
            .checked_sub(backend.elapsed())
            .filter(|remaining| !remaining.is_zero())
        else {
            return Err(Failure::TimedOut);
        };
        backend.wait_for_change(remaining);
    }
}

/// One switch at a time, and the presses that arrive during it. A press for
/// the display being switched is kept as a step and taken once the switch
/// reaches its Space, so pressing three times moves three Spaces. A press
/// for another display is skipped: no step can run later on a display the
/// cursor has left. The caller never waits for a switch under way: the
/// worker that executes a press also releases held chords.
pub(super) struct Queue(Mutex<Option<Pending>>);

/// The display a [`Run`] switches and the net steps still to take there,
/// counted toward Next, so opposite presses cancel out.
struct Pending {
    display: u32,
    steps: i32,
}

impl Pending {
    fn take(&mut self) -> Option<Direction> {
        let direction = Direction::toward(self.steps)?;
        self.steps -= direction.step();
        Some(direction)
    }
}

/// Why a press started no switch of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Busy {
    /// A switch on the same display is under way: the press is a step after it.
    Queued,
    /// A switch on another display is under way: the press is dropped.
    Skipped,
}

impl Queue {
    pub(super) const fn new() -> Self {
        Self(Mutex::new(None))
    }

    /// A [`Run`] for `display` when no switch is under way. The caller
    /// switches first and hands the run to the worker that takes its steps.
    pub(super) fn admit(&self, display: u32, direction: Direction) -> Result<Run<'_>, Busy> {
        let mut pending = self.lock();
        match pending.as_mut() {
            None => {
                *pending = Some(Pending { display, steps: 0 });
                Ok(Run {
                    queue: self,
                    ended: false,
                })
            }
            Some(under_way) if under_way.display == display => {
                under_way.steps += direction.step();
                Err(Busy::Queued)
            }
            Some(_) => Err(Busy::Skipped),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<Pending>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The right to switch the queue's display. It ends when a switch misses its
/// Space or no step is left, and when dropped by a failed spawn or an
/// unwinding worker. Steps not taken by then are discarded.
#[must_use]
pub(super) struct Run<'a> {
    queue: &'a Queue,
    ended: bool,
}

impl Run<'_> {
    /// Switch `first`, then every step queued while a switch was under way,
    /// until one does not reach its Space.
    pub(super) fn drain(
        mut self,
        first: Direction,
        gate: PostGate,
        mut switch: impl FnMut(Direction, PostGate) -> Result<Outcome, Failure>,
    ) {
        let mut result = switch(first, gate);
        while matches!(result, Ok(Outcome::Reached(_))) {
            let Some(direction) = self.next() else {
                return;
            };
            result = switch(direction, PostGate::unobserved());
        }
        // A miss: dropping the run discards the steps left.
    }

    /// The next step, or `None` after ending the run under the same lock, so
    /// a press cannot slip in between the last step and the end.
    fn next(&mut self) -> Option<Direction> {
        let mut pending = self.queue.lock();
        let step = pending.as_mut().and_then(Pending::take);
        if step.is_none() {
            *pending = None;
            self.ended = true;
        }
        step
    }
}

impl Drop for Run<'_> {
    fn drop(&mut self) {
        if !self.ended {
            *self.queue.lock() = None;
        }
    }
}

#[cfg(test)]
mod tests;
