//! Space-switch transaction policy, independent of macOS FFI for regression tests.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

const PREPARATION_TIMEOUT: Duration = Duration::from_secs(2);
const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    Previous,
    Next,
}

impl Direction {
    pub(super) fn sign(self) -> f64 {
        match self {
            Self::Previous => -1.0,
            Self::Next => 1.0,
        }
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
            return Err(Failure::Unavailable);
        }
        let index = self
            .ordered
            .iter()
            .position(|id| *id == self.current)
            .ok_or(Failure::Unavailable)?;
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

/// Sole authority to emit both phases. Cancellation and commitment arbitrate
/// atomically, so a timed-out native query cannot later produce stale output.
pub(super) struct PostGate {
    control: Arc<PostControl>,
    posted: mpsc::Sender<()>,
}

struct PostWait {
    control: Arc<PostControl>,
    receive: mpsc::Receiver<()>,
}

impl PostGate {
    #[cfg(test)]
    pub(super) fn for_test() -> Self {
        Self::new(Instant::now() + PREPARATION_TIMEOUT).0
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

    /// Only the two post calls belong in `emit`: no allocation, state queries,
    /// retries, or cancellation checks between the balanced phases.
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
/// detached; a canceled but stuck worker retains its single-flight lease.
pub(super) fn spawn_ordered(work: impl FnOnce(PostGate) + Send + 'static) -> Result<(), Failure> {
    let (gate, wait) = PostGate::new(Instant::now() + PREPARATION_TIMEOUT);
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

/// Single in-flight transaction; no queue of relative actions that can run
/// later on a different display. The owned lease also releases on spawn failure.
pub(super) struct Lease<'a>(&'a AtomicBool);

impl<'a> Lease<'a> {
    pub(super) fn acquire(busy: &'a AtomicBool) -> Option<Self> {
        busy.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Self(busy))
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests;
