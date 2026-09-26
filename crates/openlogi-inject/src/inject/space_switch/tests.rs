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
    fn post(&mut self, direction: Direction) -> Result<(), Failure> {
        self.events.borrow_mut().push("post");
        self.posts.push(direction);
        self.post_result
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
            run(&mut backend, Direction::Next, || {}),
            Err(Failure::Unavailable)
        );
        assert!(backend.posts.is_empty());
    }
    let mut backend = Fake::new([state(42)]);
    assert_eq!(
        run(&mut backend, Direction::Next, || {}),
        Ok(Outcome::Boundary)
    );
    assert!(backend.posts.is_empty());
}

#[test]
fn synchronous_completion_before_wait_is_not_lost() {
    let mut backend = Fake::new([state(7), state(7), state(91)]);
    let events = Rc::clone(&backend.events);
    assert_eq!(
        run(&mut backend, Direction::Previous, || events
            .borrow_mut()
            .push("ack")),
        Ok(Outcome::Reached(91))
    );
    assert_eq!(*events.borrow(), ["state", "state", "post", "ack", "state"]);
    assert_eq!(backend.posts, [Direction::Previous]);
    assert_eq!(backend.waits, 0);
}

#[test]
fn unrelated_notification_is_not_success_and_final_query_handles_missed_notification() {
    let mut backend = Fake::new([state(7), state(7), state(7), state(7), state(42)]);
    assert_eq!(
        run(&mut backend, Direction::Next, || {}),
        Ok(Outcome::Reached(42))
    );
    assert_eq!(backend.posts, [Direction::Next]);
    assert_eq!(backend.waits, 2);
}

#[test]
fn timeout_never_resends() {
    let mut backend = Fake::new([state(7), state(7), state(7), state(7), state(7)]);
    assert_eq!(
        run(&mut backend, Direction::Next, || {}),
        Err(Failure::TimedOut)
    );
    assert_eq!(backend.posts, [Direction::Next]);
}

#[test]
fn preparation_race_or_pointer_move_prevents_injection() {
    let mut backend = Fake::new([state(7), state(91)]);
    assert_eq!(
        run(&mut backend, Direction::Next, || {}),
        Err(Failure::ContextChanged)
    );
    assert!(backend.posts.is_empty());
    let mut backend = Fake::new([state(7), state(7)]);
    backend.post_result = Err(Failure::ContextChanged);
    assert_eq!(
        run(&mut backend, Direction::Next, || {}),
        Err(Failure::ContextChanged)
    );
    let mut backend = Fake::new([state(7), state(7)]);
    backend.elapsed = Duration::from_secs(2);
    assert_eq!(
        run(&mut backend, Direction::Next, || {}),
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
            run(&mut backend, Direction::Next, || {}),
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
        run(&mut backend, Direction::Next, || {}),
        Err(Failure::PostFailed)
    );
    assert_eq!(backend.posts.len(), 1);
    let mut backend = Fake::new([]);
    backend.states.push_back(Err(Failure::Unavailable));
    assert_eq!(
        run(&mut backend, Direction::Next, || {}),
        Err(Failure::Unavailable)
    );
    assert!(backend.posts.is_empty());
}

#[test]
fn only_one_transaction_can_run_and_drop_releases_the_slot() {
    let busy = AtomicBool::new(false);
    let lease = Lease::acquire(&busy).unwrap();
    assert!(Lease::acquire(&busy).is_none());
    drop(lease);
    assert!(Lease::acquire(&busy).is_some());
}

#[test]
fn dispatch_returns_after_posting_without_waiting_for_confirmation() {
    let (did_post, posted) = mpsc::channel();
    let (confirm, confirmation) = mpsc::channel();
    let (finished, finish) = mpsc::channel();
    spawn_ordered(move |acknowledge| {
        let mut backend = Fake::new([state(7), state(7), state(42)]);
        let result = run(&mut backend, Direction::Next, || {
            did_post.send(()).unwrap();
            acknowledge.send(()).unwrap();
            // Hold confirmation until the caller has returned, with no sleep.
            confirmation.recv().unwrap();
        });
        finished.send((backend.posts, result)).unwrap();
    })
    .unwrap();
    posted.try_recv().expect("posting precedes dispatch return");
    assert_eq!(finish.try_recv(), Err(mpsc::TryRecvError::Empty));
    confirm.send(()).unwrap();
    assert_eq!(
        finish.recv().unwrap(),
        (vec![Direction::Next], Ok(Outcome::Reached(42)))
    );
}

#[test]
fn early_exit_releases_the_caller_without_acknowledging_a_post() {
    for snapshot in [state(42), state(0)] {
        let (finished, finish) = mpsc::channel();
        spawn_ordered(move |acknowledge| {
            let mut backend = Fake::new([snapshot]);
            let result = run(&mut backend, Direction::Next, move || {
                acknowledge.send(()).unwrap();
                panic!("must not acknowledge an unposted swipe");
            });
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
