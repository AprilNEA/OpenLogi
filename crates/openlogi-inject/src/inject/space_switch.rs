//! Space-switch transaction policy, independent of macOS FFI for regression tests.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

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
    /// Recheck the cursor display immediately before posting. Never warp it.
    fn post(&mut self, direction: Direction) -> Result<(), Failure>;
    fn elapsed(&self) -> Duration;
    /// Wake on a notification or the deadline, without losing notifications
    /// between `state` and this call. A wakeup alone never proves success.
    fn wait_for_change(&mut self, remaining: Duration);
}

/// Return only after the worker acknowledges posting or drops the sender on an
/// early exit. Confirmation remains on the worker, but later caller actions
/// cannot overtake posting and short-lived callers cannot exit before it.
pub(super) fn spawn_ordered(
    work: impl FnOnce(mpsc::SyncSender<()>) + Send + 'static,
) -> std::io::Result<()> {
    let (posted, receive) = mpsc::sync_channel(0);
    std::thread::Builder::new()
        .name("openlogi-spaces".into())
        .spawn(move || work(posted))?;
    // Disconnection means preparation failed, so no late post remains pending.
    let _ = receive.recv();
    Ok(())
}

pub(super) fn run(
    backend: &mut impl Backend,
    direction: Direction,
    posted: impl FnOnce(),
) -> Result<Outcome, Failure> {
    let initial = backend.state()?;
    let Some(target) = initial.target(direction)? else {
        return Ok(Outcome::Boundary);
    };
    // Topology or active-Space changes during preparation invalidate the plan.
    if backend.state()? != initial {
        return Err(Failure::ContextChanged);
    }
    if backend.elapsed() >= CONFIRMATION_TIMEOUT {
        return Err(Failure::TimedOut);
    }
    backend.post(direction)?;
    posted();
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
