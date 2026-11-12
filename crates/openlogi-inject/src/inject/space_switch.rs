//! Space-switch transaction policy, independent of macOS FFI for regression tests.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
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
    pub(super) fn sign(self) -> f64 {
        match self {
            Self::Previous => -1.0,
            Self::Next => 1.0,
        }
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

/// Bound preparation by `deadline`, not native event posting. Return after
/// irrevocable cancellation, posting acknowledgement, or worker exit.
/// Confirmation stays detached; a canceled but stuck worker keeps its
/// [`Ticket`].
pub(super) fn spawn_ordered(
    deadline: Instant,
    work: impl FnOnce(PostGate) + Send + 'static,
) -> Result<(), Failure> {
    let (gate, wait) = PostGate::new(deadline);
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

/// Where the one switch in flight stands. There is never more than one: no
/// queue of relative switches that could land later on another display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// No switch in flight.
    Idle,
    /// Switch `flight` on `display` reads its Spaces and builds its events.
    /// Nothing may follow it yet. Its preparation ends at `deadline`, which
    /// its post gate enforces.
    Preparing {
        flight: u64,
        display: u32,
        deadline: Instant,
    },
    /// Switch `flight` on `display` claimed its post before its deadline and
    /// is posting. Nothing may follow it until it has posted, however long
    /// that takes: native posting can't be canceled, so its caller waits for
    /// it too.
    Posting { flight: u64, display: u32 },
    /// Switch `flight` on `display` has posted and waits for its Space.
    /// `followed` once a follow-up was posted after it.
    Confirming {
        flight: u64,
        display: u32,
        followed: bool,
    },
}

/// What a request may do, decided from the phase alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// Start a switch.
    Lead,
    /// Post at once after switch `flight`, which has posted.
    Follow(u64),
    /// Wait for the switch in flight to claim its post, until `deadline`.
    WaitUntil(Instant),
    /// Wait for the switch in flight to finish posting.
    WaitForPost,
    /// Drop the request.
    Refuse,
}

impl Phase {
    /// The step for a request on `display`. `chain` is the host's choice to
    /// post a request that finds a switch in flight on its display.
    fn step(self, display: u32, chain: bool) -> Step {
        match self {
            Self::Idle => Step::Lead,
            Self::Preparing {
                display: busy,
                deadline,
                ..
            } if chain && busy == display => Step::WaitUntil(deadline),
            Self::Posting { display: busy, .. } if chain && busy == display => Step::WaitForPost,
            Self::Confirming {
                flight,
                display: busy,
                ..
            } if chain && busy == display => Step::Follow(flight),
            Self::Preparing { .. } | Self::Posting { .. } | Self::Confirming { .. } => Step::Refuse,
        }
    }

    fn flight(self) -> Option<u64> {
        match self {
            Self::Idle => None,
            Self::Preparing { flight, .. }
            | Self::Posting { flight, .. }
            | Self::Confirming { flight, .. } => Some(flight),
        }
    }
}

struct FlightState {
    phase: Phase,
    /// The identity the next switch takes, so a late call about an earlier
    /// switch never changes its successor.
    next: u64,
}

/// The single-flight rule for desktop switches and the one owner of its
/// transitions. A request leads a switch only when none is in flight. With
/// chaining on, a request on the same display follows the switch in flight,
/// but only once that switch has posted: posted earlier, it would move the
/// Space under the first switch's preparation, which then gives up, and two
/// requests would make one switch.
pub(super) struct Flight {
    state: Mutex<FlightState>,
    changed: Condvar,
}

/// What [`Flight::admit`] grants a request.
pub(super) enum Admission<'a> {
    /// Start a switch. The ticket holds the flight until it drops.
    Lead(Ticket<'a>),
    /// Post at once after the switch in flight, which has posted.
    Follow(Follower),
    /// Drop the request: a switch it may not follow is in flight, or the one
    /// it waited for did not post before its preparation ended.
    Refused,
}

/// A follow-up's claim on the switch it follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Follower {
    flight: u64,
}

impl Flight {
    pub(super) const fn new() -> Self {
        Self {
            state: Mutex::new(FlightState {
                phase: Phase::Idle,
                next: 0,
            }),
            changed: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, FlightState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Admit a request on `display` arriving at `now`. One that may follow a
    /// switch still preparing waits for it to claim its post, but never past
    /// that switch's preparation deadline, so a stuck switch holds no caller
    /// longer than it holds its own. Once the post is claimed, it waits for
    /// the post to finish.
    pub(super) fn admit(&self, display: u32, chain: bool, mut now: Instant) -> Admission<'_> {
        let mut state = self.lock();
        loop {
            match state.phase.step(display, chain) {
                Step::Lead => {
                    let flight = state.next;
                    state.next = flight.wrapping_add(1);
                    let deadline = now + PREPARATION_TIMEOUT;
                    state.phase = Phase::Preparing {
                        flight,
                        display,
                        deadline,
                    };
                    return Admission::Lead(Ticket {
                        owner: self,
                        flight,
                        deadline,
                    });
                }
                Step::Follow(flight) => return Admission::Follow(Follower { flight }),
                Step::Refuse => return Admission::Refused,
                Step::WaitForPost => {
                    state = self
                        .changed
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                    now = Instant::now();
                }
                Step::WaitUntil(deadline) => {
                    let remaining = deadline.saturating_duration_since(now);
                    if remaining.is_zero() {
                        return Admission::Refused;
                    }
                    state = self
                        .changed
                        .wait_timeout(state, remaining)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                    now = Instant::now();
                }
            }
        }
    }

    /// Record that `follower` posted. Only the switch it followed counts it;
    /// a switch that started since does not.
    pub(super) fn followed(&self, follower: Follower) {
        let mut state = self.lock();
        if let Phase::Confirming {
            flight, followed, ..
        } = &mut state.phase
            && *flight == follower.flight
        {
            *followed = true;
        }
    }
}

/// A leading switch's hold on the flight. Dropping it ends the flight,
/// however the switch ended, so a stuck worker keeps it until it exits.
pub(super) struct Ticket<'a> {
    owner: &'a Flight,
    flight: u64,
    deadline: Instant,
}

impl Ticket<'_> {
    /// When this switch's preparation ends; its post gate enforces it.
    pub(super) fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Claim this switch's post, if its preparation has not ended by `now`.
    /// Decided under the flight's lock against the same deadline a waiting
    /// follow-up checks, so a follow-up is refused exactly when no post was
    /// claimed in time. Once claimed, follow-ups wait for the post itself.
    pub(super) fn begin_posting(&self, now: Instant) -> bool {
        let mut state = self.owner.lock();
        match state.phase {
            Phase::Preparing {
                flight,
                display,
                deadline,
            } if flight == self.flight && now < deadline => {
                state.phase = Phase::Posting { flight, display };
                true
            }
            _ => false,
        }
    }

    /// Give up a claimed post its gate refused, so waiting follow-ups see
    /// the preparation deadline again.
    pub(super) fn abandon_post(&self) {
        let mut state = self.owner.lock();
        if let Phase::Posting { flight, display } = state.phase
            && flight == self.flight
        {
            state.phase = Phase::Preparing {
                flight,
                display,
                deadline: self.deadline,
            };
            self.owner.changed.notify_all();
        }
    }

    /// Record that this switch has posted, letting follow-ups on its display
    /// post. Call it after the posting lock is released: a follow-up takes
    /// that lock next.
    pub(super) fn posted(&self) {
        let mut state = self.owner.lock();
        if let Phase::Posting { flight, display } = state.phase
            && flight == self.flight
        {
            state.phase = Phase::Confirming {
                flight,
                display,
                followed: false,
            };
            self.owner.changed.notify_all();
        }
    }

    /// Whether a follow-up was posted after this switch.
    pub(super) fn followed(&self) -> bool {
        matches!(
            self.owner.lock().phase,
            Phase::Confirming { flight, followed: true, .. } if flight == self.flight
        )
    }
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        let mut state = self.owner.lock();
        if state.phase.flight() == Some(self.flight) {
            state.phase = Phase::Idle;
            self.owner.changed.notify_all();
        }
    }
}

/// Run `emit` holding `turn`, if `cursor` still reports `display` once
/// `turn` is held. A request can wait for its turn behind another switch's
/// slide, and the pointer can leave the display meanwhile, so only a check
/// made after that wait counts. `None` when the pointer left.
pub(super) fn post_on_display<T>(
    turn: &Mutex<()>,
    display: u32,
    cursor: impl FnOnce() -> Option<u32>,
    emit: impl FnOnce() -> T,
) -> Option<T> {
    let _turn = turn.lock().unwrap_or_else(PoisonError::into_inner);
    (cursor() == Some(display)).then(emit)
}

#[cfg(test)]
mod tests;
