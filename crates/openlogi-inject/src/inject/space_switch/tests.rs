use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use super::*;

fn state(current: u64) -> SpaceState {
    SpaceState {
        display: "second-display".into(),
        current,
        ordered: vec![91, 7, 42],
    }
}

struct Fake {
    states: VecDeque<Result<SpaceState, Failure>>,
    posts: Vec<Direction>,
    events: Rc<RefCell<Vec<&'static str>>>,
    elapsed: Duration,
    waits: usize,
    post_result: Result<(), Failure>,
}

impl Fake {
    fn new(states: impl IntoIterator<Item = SpaceState>) -> Self {
        Self {
            states: states.into_iter().map(Ok).collect(),
            posts: vec![],
            events: Rc::default(),
            elapsed: Duration::ZERO,
            waits: 0,
            post_result: Ok(()),
        }
    }
}

impl Backend for Fake {
    fn state(&mut self) -> Result<SpaceState, Failure> {
        self.events.borrow_mut().push("state");
        self.states.pop_front().expect("unexpected query")
    }
    fn post(&mut self, direction: Direction, gate: PostGate) -> Result<(), Failure> {
        self.post_result?;
        gate.commit(|| {
            self.events.borrow_mut().push("post");
            self.posts.push(direction);
        })
    }
    fn elapsed(&self) -> Duration {
        self.elapsed
    }
    fn wait_for_change(&mut self, remaining: Duration) {
        self.waits += 1;
        // First wake can be an unrelated notification, second is the deadline.
        self.elapsed += if self.waits == 1 {
            Duration::from_millis(10)
        } else {
            remaining
        };
    }
}

#[test]
fn neighbors_use_display_order_not_numeric_ids_and_do_not_wrap() {
    for (current, direction, target) in [
        (7, Direction::Next, Some(42)),
        (7, Direction::Previous, Some(91)),
        (91, Direction::Previous, None),
        (42, Direction::Next, None),
    ] {
        assert_eq!(state(current).target(direction), Ok(target));
    }
    assert_eq!(Direction::Previous.sign().to_bits(), (-1.0_f64).to_bits());
    assert_eq!(Direction::Next.sign().to_bits(), 1.0_f64.to_bits());
}

#[test]
fn boundary_or_invalid_snapshot_never_posts() {
    let mut invalid = state(7);
    invalid.ordered.push(7);
    for snapshot in [invalid, state(0), state(999)] {
        let mut backend = Fake::new([snapshot]);
        assert_eq!(
            run(&mut backend, Direction::Next, PostGate::for_test()),
            Err(Failure::Unavailable)
        );
        assert!(backend.posts.is_empty());
    }
    let mut backend = Fake::new([state(42)]);
    assert_eq!(
        run(&mut backend, Direction::Next, PostGate::for_test()),
        Ok(Outcome::Boundary)
    );
    assert!(backend.posts.is_empty());
}

#[test]
fn synchronous_completion_before_wait_is_not_lost() {
    let mut backend = Fake::new([state(7), state(7), state(91)]);
    let events = Rc::clone(&backend.events);
    assert_eq!(
        run(&mut backend, Direction::Previous, PostGate::for_test()),
        Ok(Outcome::Reached(91))
    );
    assert_eq!(*events.borrow(), ["state", "state", "post", "state"]);
    assert_eq!(backend.posts, [Direction::Previous]);
    assert_eq!(backend.waits, 0);
}

#[test]
fn unrelated_notification_is_not_success_and_final_query_handles_missed_notification() {
    let mut backend = Fake::new([state(7), state(7), state(7), state(7), state(42)]);
    assert_eq!(
        run(&mut backend, Direction::Next, PostGate::for_test()),
        Ok(Outcome::Reached(42))
    );
    assert_eq!(backend.posts, [Direction::Next]);
    assert_eq!(backend.waits, 2);
}

#[test]
fn timeout_never_resends() {
    let mut backend = Fake::new([state(7), state(7), state(7), state(7), state(7)]);
    assert_eq!(
        run(&mut backend, Direction::Next, PostGate::for_test()),
        Err(Failure::TimedOut)
    );
    assert_eq!(backend.posts, [Direction::Next]);
}

#[test]
fn preparation_race_or_pointer_move_prevents_injection() {
    let mut backend = Fake::new([state(7), state(91)]);
    assert_eq!(
        run(&mut backend, Direction::Next, PostGate::for_test()),
        Err(Failure::ContextChanged)
    );
    assert!(backend.posts.is_empty());
    let mut backend = Fake::new([state(7), state(7)]);
    backend.post_result = Err(Failure::ContextChanged);
    assert_eq!(
        run(&mut backend, Direction::Next, PostGate::for_test()),
        Err(Failure::ContextChanged)
    );
    let mut backend = Fake::new([state(7), state(7)]);
    let (expired, _wait) = PostGate::new(Instant::now());
    assert_eq!(
        run(&mut backend, Direction::Next, expired),
        Err(Failure::TimedOut)
    );
    assert!(backend.posts.is_empty());
}

#[test]
fn wrong_display_topology_change_or_opposite_switch_is_not_success() {
    let mut other = state(42);
    other.display = "first-display".into();
    let mut reordered = state(42);
    reordered.ordered.swap(0, 2);
    for observed in [other, reordered, state(91)] {
        let mut backend = Fake::new([state(7), state(7), observed]);
        assert_eq!(
            run(&mut backend, Direction::Next, PostGate::for_test()),
            Err(Failure::ContextChanged)
        );
        assert_eq!(backend.posts.len(), 1);
    }
}

#[test]
fn native_failure_stops_without_keyboard_fallback() {
    let mut backend = Fake::new([state(7), state(7)]);
    backend.post_result = Err(Failure::PostFailed);
    assert_eq!(
        run(&mut backend, Direction::Next, PostGate::for_test()),
        Err(Failure::PostFailed)
    );
    assert!(backend.posts.is_empty());
    let mut backend = Fake::new([]);
    backend.states.push_back(Err(Failure::Unavailable));
    assert_eq!(
        run(&mut backend, Direction::Next, PostGate::for_test()),
        Err(Failure::Unavailable)
    );
    assert!(backend.posts.is_empty());
}

fn lead(admission: Admission<'_>) -> Ticket<'_> {
    match admission {
        Admission::Lead(ticket) => ticket,
        Admission::Follow(_) => panic!("expected to lead, got a follow-up"),
        Admission::Refused => panic!("expected to lead, got refused"),
    }
}

fn follower(admission: &Admission<'_>) -> Follower {
    match admission {
        Admission::Follow(follower) => *follower,
        Admission::Lead(_) => panic!("expected a follow-up, got a lead"),
        Admission::Refused => panic!("expected a follow-up, got refused"),
    }
}

fn refused(admission: &Admission<'_>) -> bool {
    matches!(admission, Admission::Refused)
}

#[test]
fn every_phase_decides_every_request() {
    let deadline = Instant::now();
    let preparing = Phase::Preparing {
        flight: 1,
        display: 5,
        deadline,
    };
    let posting = Phase::Posting {
        flight: 1,
        display: 5,
    };
    let confirming = Phase::Confirming {
        flight: 1,
        display: 5,
        followed: false,
    };
    for (phase, display, chain, step) in [
        (Phase::Idle, 5, false, Step::Lead),
        (Phase::Idle, 5, true, Step::Lead),
        (preparing, 5, true, Step::WaitUntil(deadline)),
        (preparing, 5, false, Step::Refuse),
        (preparing, 6, true, Step::Refuse),
        (posting, 5, true, Step::WaitForPost),
        (posting, 5, false, Step::Refuse),
        (posting, 6, true, Step::Refuse),
        (confirming, 5, true, Step::Follow(1)),
        (confirming, 5, false, Step::Refuse),
        (confirming, 6, true, Step::Refuse),
    ] {
        assert_eq!(phase.step(display, chain), step, "{phase:?} on {display}");
    }
}

#[test]
fn only_one_transaction_can_run_and_drop_releases_the_slot() {
    let flight = Flight::new();
    let ticket = lead(flight.admit(5, false, Instant::now()));
    assert!(refused(&flight.admit(5, false, Instant::now())));
    assert!(refused(&flight.admit(6, true, Instant::now())));
    drop(ticket);
    let _next = lead(flight.admit(6, true, Instant::now()));
}

#[test]
fn follow_up_waits_for_first_post() {
    let flight = Flight::new();
    let ticket = lead(flight.admit(5, true, Instant::now()));
    std::thread::scope(|scope| {
        let waiting = scope.spawn(|| follower(&flight.admit(5, true, Instant::now())));
        // Only this post can let the waiting request through as a follow-up.
        assert!(ticket.begin_posting(Instant::now()));
        ticket.posted();
        let follower = waiting.join().unwrap();
        assert!(!ticket.followed());
        flight.followed(follower);
        assert!(ticket.followed());
    });
}

#[test]
fn follower_waits_for_committed_post_past_preparation_deadline() {
    let flight = Flight::new();
    let ticket = lead(flight.admit(5, true, Instant::now()));
    assert!(ticket.begin_posting(Instant::now()));
    // A slide that began just before the deadline is still posting after it.
    let late = ticket.deadline() + PREPARATION_TIMEOUT;
    std::thread::scope(|scope| {
        let waiting = scope.spawn(|| follower(&flight.admit(5, true, late)));
        ticket.posted();
        let follower = waiting.join().unwrap();
        flight.followed(follower);
        assert!(ticket.followed());
    });
}

#[test]
fn a_post_claimed_too_late_or_refused_by_its_gate_lets_no_follow_up_through() {
    let flight = Flight::new();
    let ticket = lead(flight.admit(5, true, Instant::now()));
    assert!(
        !ticket.begin_posting(ticket.deadline()),
        "a claim at the deadline is too late"
    );
    assert!(refused(&flight.admit(5, true, ticket.deadline())));
    assert!(ticket.begin_posting(Instant::now()));
    // The gate refused the claimed post after all.
    ticket.abandon_post();
    assert!(refused(&flight.admit(5, true, ticket.deadline())));
}

#[test]
fn a_follow_up_of_a_switch_that_never_posted_is_refused() {
    let flight = Flight::new();
    // A switch whose preparation ended without a post, its worker stuck.
    let stuck = lead(flight.admit(
        5,
        true,
        Instant::now().checked_sub(PREPARATION_TIMEOUT).unwrap(),
    ));
    assert!(stuck.deadline() <= Instant::now());
    assert!(refused(&flight.admit(5, true, Instant::now())));
    assert!(
        refused(&flight.admit(5, false, Instant::now())),
        "the stuck switch still holds the flight"
    );
    drop(stuck);
    let _next = lead(flight.admit(5, true, Instant::now()));
}

#[test]
fn a_stale_follow_up_never_marks_the_next_switch() {
    let flight = Flight::new();
    let first = lead(flight.admit(5, true, Instant::now()));
    assert!(first.begin_posting(Instant::now()));
    first.posted();
    let stale = follower(&flight.admit(5, true, Instant::now()));
    drop(first);
    let second = lead(flight.admit(5, true, Instant::now()));
    assert!(second.begin_posting(Instant::now()));
    second.posted();
    flight.followed(stale);
    assert!(
        !second.followed(),
        "the follow-up belonged to the first switch"
    );
    let current = follower(&flight.admit(5, true, Instant::now()));
    flight.followed(current);
    assert!(second.followed());
}

#[test]
fn desktop_post_rechecks_display_after_wait() {
    let turn = Mutex::new(());
    for (cursor, posts) in [(5, true), (6, false)] {
        let mut emitted = false;
        let result = post_on_display(
            &turn,
            5,
            || {
                assert!(turn.try_lock().is_err(), "the display is read in turn");
                Some(cursor)
            },
            || emitted = true,
        );
        assert_eq!(result.is_some(), posts, "cursor on {cursor}");
        assert_eq!(emitted, posts, "cursor on {cursor}");
    }
}

#[test]
fn dispatch_returns_after_posting_without_waiting_for_confirmation() {
    let (did_post, posted) = mpsc::channel();
    let (confirm, confirmation) = mpsc::channel();
    let (finished, finish) = mpsc::channel();
    spawn_ordered(Instant::now() + PREPARATION_TIMEOUT, move |gate| {
        gate.commit(|| {
            did_post.send(()).unwrap();
        })
        .unwrap();
        // Hold confirmation until the caller has returned, with no sleep.
        confirmation.recv().unwrap();
        finished.send(()).unwrap();
    })
    .unwrap();
    posted.try_recv().expect("posting precedes dispatch return");
    assert_eq!(finish.try_recv(), Err(mpsc::TryRecvError::Empty));
    confirm.send(()).unwrap();
    finish.recv().unwrap();
}

#[test]
fn early_exit_releases_the_caller_without_acknowledging_a_post() {
    for snapshot in [state(42), state(0)] {
        let (finished, finish) = mpsc::channel();
        spawn_ordered(Instant::now() + PREPARATION_TIMEOUT, move |gate| {
            let mut backend = Fake::new([snapshot]);
            let result = run(&mut backend, Direction::Next, gate);
            finished.send((backend.posts, result)).unwrap();
        })
        .unwrap();
        let (posts, result) = finish.recv().unwrap();
        assert!(posts.is_empty());
        assert!(matches!(
            result,
            Ok(Outcome::Boundary) | Err(Failure::Unavailable)
        ));
    }
}

#[test]
fn preparation_timeout_cancels_late_output_but_keeps_the_worker_lease() {
    let flight = Flight::new();
    let ticket = lead(flight.admit(5, true, Instant::now()));
    let deadline = Instant::now() + Duration::from_secs(60);
    let (gate, wait) = PostGate::new(deadline);
    let (resume, blocked) = mpsc::channel();
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            let _ticket = ticket;
            blocked.recv().unwrap();
            let mut backend = Fake::new([state(7), state(7)]);
            let result = run(&mut backend, Direction::Next, gate);
            (backend.posts, result)
        });
        // Advance the waiter's clock to its deadline while preparation is held.
        assert_eq!(wait.wait(deadline), Err(Failure::TimedOut));
        assert!(refused(&flight.admit(6, true, Instant::now())));
        // Later actions can proceed now. Releasing the stalled preparation
        // must never let that stale request emit either swipe phase.
        resume.send(()).unwrap();
        assert_eq!(worker.join().unwrap(), (vec![], Err(Failure::TimedOut)));
    });
    let _next = lead(flight.admit(6, true, Instant::now()));
}

#[test]
fn posting_wins_cancellation_and_acknowledges_only_after_both_phases() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let (gate, wait) = PostGate::new(deadline);
    let (events, sequence) = mpsc::channel();
    let (end, continue_posting) = mpsc::channel();
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            gate.commit(|| {
                events.send("begin").unwrap();
                continue_posting.recv().unwrap();
                events.send("end").unwrap();
            })
        });
        assert_eq!(sequence.recv().unwrap(), "begin");
        assert!(!wait.control.cancel(), "committed posts cannot be canceled");
        assert_eq!(wait.receive.try_recv(), Err(mpsc::TryRecvError::Empty));
        let waiter = scope.spawn(move || wait.wait(deadline));
        end.send(()).unwrap();
        assert_eq!(waiter.join().unwrap(), Ok(()));
        assert_eq!(sequence.try_recv().unwrap(), "end");
        assert_eq!(worker.join().unwrap(), Ok(()));
    });
}

#[test]
fn dropped_or_panicking_worker_releases_waiter_without_false_acknowledgement() {
    let (gate, wait) = PostGate::new(Instant::now() + Duration::from_secs(60));
    // Dropping the unstarted job is also what a failed thread spawn does.
    drop(gate);
    assert_eq!(
        wait.receive.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    );
    assert_eq!(wait.wait(Instant::now()), Ok(()));

    let (gate, wait) = PostGate::new(Instant::now() + Duration::from_secs(60));
    let panic = std::panic::catch_unwind(|| gate.commit(|| panic!("post failed")));
    assert!(panic.is_err(), "the simulated posting panic must unwind");
    assert_eq!(
        wait.receive.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    );
    assert_eq!(wait.wait(Instant::now()), Ok(()));
}

const BOTH_DIRECTIONS: [Direction; 2] = [Direction::Previous, Direction::Next];

#[test]
fn slide_posts_one_frame_per_options_plus_period() {
    for direction in BOTH_DIRECTIONS {
        let frames = slide(direction);
        assert_eq!(frames[0].at, Duration::ZERO);
        for pair in frames.windows(2) {
            assert_eq!(
                pair[1].at.checked_sub(pair[0].at),
                Some(Duration::from_micros(16_600)),
                "frames are posted in time order, one period apart"
            );
        }
        // 19 moving frames, then the release one period after the last.
        assert_eq!(frames.len(), 20);
        assert_eq!(
            frames.last().map(|frame| frame.at),
            Some(Duration::from_micros(315_400))
        );
    }
}

#[test]
fn slide_begins_once_then_changes_then_ends_once() {
    for direction in BOTH_DIRECTIONS {
        let phases: Vec<_> = slide(direction).iter().map(|frame| frame.phase).collect();
        let (first, rest) = phases.split_first().expect("a slide has frames");
        let (last, middle) = rest.split_last().expect("a slide has a release");
        assert_eq!(*first, SlidePhase::Began);
        assert!(middle.iter().all(|phase| *phase == SlidePhase::Changed));
        assert_eq!(*last, SlidePhase::Ended);
    }
}

#[test]
fn slide_progress_moves_monotonically_toward_the_neighbour() {
    for direction in BOTH_DIRECTIONS {
        let frames = slide(direction);
        let travel: Vec<f64> = frames
            .iter()
            .map(|frame| frame.progress * direction.sign())
            .collect();
        // The first frame already moves: a Began at zero would not tell the
        // Dock which way to go.
        assert!((travel[0] - 0.15).abs() < 1e-12, "{travel:?}");
        let moving = &travel[..travel.len() - 1];
        for pair in moving.windows(2) {
            assert!(pair[1] > pair[0], "travel must grow: {travel:?}");
        }
        assert!(moving.iter().all(|t| *t > 0.0 && *t < 1.0), "{travel:?}");
        // The release lets go where the last frame left off, past 95%.
        assert_eq!(travel.last(), moving.last());
        assert!(travel.last().is_some_and(|t| *t > 0.95), "{travel:?}");
    }
}

#[test]
fn slide_eases_out() {
    let frames = slide(Direction::Next);
    let steps: Vec<f64> = frames[..frames.len() - 1]
        .windows(2)
        .map(|pair| pair[1].progress - pair[0].progress)
        .collect();
    for pair in steps.windows(2) {
        // Each frame covers 15% of what is left, so every step is 85% of
        // the one before it.
        assert!((pair[1] / pair[0] - 0.85).abs() < 1e-9, "{steps:?}");
    }
}

/// A clock that moves only when the slide sleeps. One sleep can end late.
struct FakePace {
    now: Duration,
    /// The index of the sleep that ends late, and by how much.
    late: Option<(usize, Duration)>,
    sleeps: usize,
    /// Each posted frame with the clock time it went out.
    posted: Vec<(Duration, usize)>,
}

impl FakePace {
    fn starting_at(now: Duration) -> Self {
        Self {
            now,
            late: None,
            sleeps: 0,
            posted: vec![],
        }
    }
}

impl Pace<usize> for FakePace {
    fn now(&self) -> Duration {
        self.now
    }
    fn sleep(&mut self, duration: Duration) {
        self.now += duration;
        if let Some((index, by)) = self.late
            && index == self.sleeps
        {
            self.now += by;
        }
        self.sleeps += 1;
    }
    fn post(&mut self, frame: &usize) {
        self.posted.push((self.now, *frame));
    }
}

/// The slide's frame times, each with its index as the frame.
fn indexed_slide() -> Vec<(Duration, usize)> {
    slide(Direction::Next)
        .iter()
        .enumerate()
        .map(|(index, frame)| (frame.at, index))
        .collect()
}

#[test]
fn pace_posts_every_frame_at_its_time_from_the_start() {
    let frames = indexed_slide();
    let start = Duration::from_secs(5);
    let mut out = FakePace::starting_at(start);
    pace(&frames, &mut out);
    let expected: Vec<_> = frames
        .iter()
        .map(|(at, index)| (start + *at, *index))
        .collect();
    assert_eq!(out.posted, expected);
    // The first frame is due at once; every later one waits one period.
    assert_eq!(out.sleeps, frames.len() - 1);
}

#[test]
fn a_late_wakeup_delays_only_the_frames_already_due() {
    let frames = indexed_slide();
    let start = Duration::from_secs(5);
    let mut out = FakePace::starting_at(start);
    // The sleep before frame 3 (due at 49.8 ms) ends 40 ms late, so the
    // clock reads 89.8 ms when it wakes.
    out.late = Some((2, Duration::from_millis(40)));
    pace(&frames, &mut out);
    let woke = start + Duration::from_micros(89_800);
    let posted_at: Vec<_> = out.posted.iter().map(|(at, _)| *at).collect();
    for (index, (at, _)) in frames.iter().enumerate() {
        let expected = match index {
            // Frames 3, 4 (66.4 ms) and 5 (83.0 ms) are due by the late
            // wakeup and go out at once, with no sleep between them.
            3..=5 => woke,
            // Every other frame keeps its time from the start, so the
            // slide still ends on time.
            _ => start + *at,
        };
        assert_eq!(posted_at[index], expected, "frame {index}: {posted_at:?}");
    }
    let order: Vec<_> = out.posted.iter().map(|(_, index)| *index).collect();
    assert_eq!(order, (0..frames.len()).collect::<Vec<_>>());
    assert_eq!(out.sleeps, frames.len() - 3);
}
