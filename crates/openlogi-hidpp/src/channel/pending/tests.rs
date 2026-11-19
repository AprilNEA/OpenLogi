//! The key lifecycle, one transition per test, driven through the queue's
//! own methods with explicit instants.

use std::time::Duration;

use super::*;

const KEY: CorrelationKey = (0x01, 0x0a, 0x21);

/// A request under [`KEY`]; `question` tells same-header requests apart.
fn ask(question: u8) -> HidppMessage {
    HidppMessage::Short([KEY.0, KEY.1, KEY.2, question, 0, 0])
}

/// A reply under [`KEY`].
fn reply(answer: u8) -> HidppMessage {
    HidppMessage::Short([KEY.0, KEY.1, KEY.2, answer, 0xee, 0xee])
}

/// A request whose predicate accepts every report under its header, as the
/// protocol layers' predicates do.
fn registration(
    id: u64,
    request: HidppMessage,
    abandoned: AbandonedReply,
) -> (Registration, oneshot::Receiver<HidppMessage>) {
    let key = request.header();
    accepting(id, request, abandoned, move |reply| reply.header() == key)
}

fn accepting(
    id: u64,
    request: HidppMessage,
    abandoned: AbandonedReply,
    response_predicate: impl Fn(&HidppMessage) -> bool + Send + 'static,
) -> (Registration, oneshot::Receiver<HidppMessage>) {
    let (sender, receiver) = oneshot::channel();
    let registration = Registration {
        id,
        correlation: Correlation {
            key: request.header(),
            request,
            response_predicate: Box::new(response_predicate),
        },
        abandoned,
        sender,
    };
    (registration, receiver)
}

/// Registers a request that must find its key free or adoptable, and
/// returns where its reply arrives.
fn register_awaiting(
    queue: &mut PendingQueue,
    id: u64,
    request: HidppMessage,
    abandoned: AbandonedReply,
    now: Instant,
) -> oneshot::Receiver<HidppMessage> {
    let (registration, receiver) = registration(id, request, abandoned);
    assert!(
        queue.try_register(registration, now).is_ok(),
        "request {id} was refused"
    );
    receiver
}

/// [`register_awaiting`] for a request whose reply the test does not read.
fn register(
    queue: &mut PendingQueue,
    id: u64,
    request: HidppMessage,
    abandoned: AbandonedReply,
    now: Instant,
) {
    drop(register_awaiting(queue, id, request, abandoned, now));
}

/// The verdict for a request that must be refused.
fn refusal(
    queue: &mut PendingQueue,
    request: HidppMessage,
    abandoned: AbandonedReply,
    now: Instant,
) -> Wait {
    let (registration, _receiver) = registration(99, request, abandoned);
    match queue.try_register(registration, now) {
        Ok(()) => panic!("the request registered"),
        Err((_, wait)) => wait,
    }
}

/// [`KEY`]'s state, in a form a test can compare.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Free,
    InFlight { id: u64, extra_replies: usize },
    Stale(Stale),
}

fn seen(queue: &PendingQueue) -> Seen {
    let mut held = queue.keys.iter().filter(|held| held.correlation.key == KEY);
    let state = match held.next().map(|held| &held.state) {
        None => Seen::Free,
        Some(KeyState::InFlight(in_flight)) => Seen::InFlight {
            id: in_flight.id,
            extra_replies: in_flight.extra_replies,
        },
        Some(KeyState::Stale(stale)) => Seen::Stale(*stale),
    };
    assert!(held.next().is_none(), "{KEY:?} is held twice");
    state
}

fn stale(outstanding: usize, expires: Instant) -> Seen {
    Seen::Stale(Stale {
        outstanding: NonZeroUsize::new(outstanding).unwrap(),
        expires,
    })
}

fn parked(queue: &mut PendingQueue) -> oneshot::Receiver<()> {
    let (wake, parked) = oneshot::channel();
    queue.key_waiters.push(wake);
    parked
}

fn woken(parked: &mut oneshot::Receiver<()>) -> bool {
    matches!(parked.try_recv(), Ok(Some(())))
}

/// [`KEY`] stale owing two replies, abandoned at `now`: a request abandoned
/// once, then an adopting re-ask of it abandoned too.
fn stale_owing_two(queue: &mut PendingQueue, now: Instant) {
    register(queue, 1, ask(0), AbandonedReply::Quarantine, now);
    queue.abandon(1, now);
    register(queue, 2, ask(0), AbandonedReply::AdoptIdentical, now);
    queue.abandon(2, now);
    assert_eq!(seen(queue), stale(2, now + STALE_REPLY_GRACE));
}

/// Free → in flight. While it is, the key refuses every other request, an
/// adopting re-ask of the very same bytes included.
#[test]
fn a_free_key_goes_in_flight_and_refuses_the_next_request() {
    let now = Instant::now();
    let mut queue = PendingQueue::default();

    register(&mut queue, 1, ask(0), AbandonedReply::Quarantine, now);

    assert_eq!(
        seen(&queue),
        Seen::InFlight {
            id: 1,
            extra_replies: 0
        }
    );
    assert_eq!(queue.in_flight_count(), 1);
    assert_eq!(
        refusal(&mut queue, ask(0), AbandonedReply::AdoptIdentical, now),
        Wait::InFlight
    );
    assert_eq!(
        refusal(&mut queue, ask(1), AbandonedReply::Quarantine, now),
        Wait::InFlight
    );
}

/// In flight → free: the reply goes to the request, and parked requests are
/// woken to take the key.
#[test]
fn an_answered_key_is_free_again() {
    let now = Instant::now();
    let mut queue = PendingQueue::default();
    let mut answer = register_awaiting(&mut queue, 1, ask(0), AbandonedReply::Quarantine, now);
    let mut parked = parked(&mut queue);

    assert_eq!(queue.resolve(reply(7), now), Resolution::Answered(1));

    assert_eq!(answer.try_recv(), Ok(Some(reply(7))));
    assert_eq!(seen(&queue), Seen::Free);
    assert!(woken(&mut parked));
}

/// In flight → stale: an abandoned request's reply is owed for the grace from
/// when it was abandoned, not from when it registered.
#[test]
fn an_abandoned_key_owes_its_reply_for_the_grace() {
    let registered = Instant::now();
    let abandoned = registered + Duration::from_millis(300);
    let mut queue = PendingQueue::default();
    register(
        &mut queue,
        1,
        ask(0),
        AbandonedReply::Quarantine,
        registered,
    );
    let mut parked = parked(&mut queue);

    // An ID that is not in flight changes nothing.
    queue.abandon(2, abandoned);
    assert!(!woken(&mut parked));

    queue.abandon(1, abandoned);

    assert_eq!(seen(&queue), stale(1, abandoned + STALE_REPLY_GRACE));
    assert_eq!(queue.in_flight_count(), 0);
    assert!(woken(&mut parked));
    assert_eq!(
        refusal(&mut queue, ask(1), AbandonedReply::Quarantine, abandoned),
        Wait::Stale(abandoned + STALE_REPLY_GRACE)
    );
}

/// Stale → in flight: an identical re-ask that asks to adopts every reply the
/// key still owes. A re-ask that does not ask to, or asks something else,
/// waits out the key.
#[test]
fn an_adopting_identical_re_ask_takes_over_a_stale_key() {
    let now = Instant::now();
    let mut queue = PendingQueue::default();
    stale_owing_two(&mut queue, now);
    let expires = now + STALE_REPLY_GRACE;

    assert_eq!(
        refusal(&mut queue, ask(0), AbandonedReply::Quarantine, now),
        Wait::Stale(expires)
    );
    assert_eq!(
        refusal(&mut queue, ask(1), AbandonedReply::AdoptIdentical, now),
        Wait::Stale(expires)
    );

    register(&mut queue, 3, ask(0), AbandonedReply::AdoptIdentical, now);

    assert_eq!(
        seen(&queue),
        Seen::InFlight {
            id: 3,
            extra_replies: 2
        }
    );
}

/// In flight → stale: a request that adopted owed replies is answered by the
/// first of them, and the key stays stale, from the answer, for the rest.
#[test]
fn an_answer_with_adopted_replies_still_owed_leaves_the_key_stale() {
    let abandoned = Instant::now();
    let answered = abandoned + Duration::from_millis(300);
    let mut queue = PendingQueue::default();
    stale_owing_two(&mut queue, abandoned);
    let mut answer = register_awaiting(
        &mut queue,
        3,
        ask(0),
        AbandonedReply::AdoptIdentical,
        abandoned,
    );

    assert_eq!(queue.resolve(reply(7), answered), Resolution::Answered(3));

    assert_eq!(answer.try_recv(), Ok(Some(reply(7))));
    assert_eq!(seen(&queue), stale(2, answered + STALE_REPLY_GRACE));
}

/// Stale → stale: discarding one of several owed replies keeps the key
/// stale, with its grace unchanged, and frees nothing to wake for.
#[test]
fn a_discarded_reply_with_more_owed_keeps_the_key_stale() {
    let abandoned = Instant::now();
    let mut queue = PendingQueue::default();
    stale_owing_two(&mut queue, abandoned);
    let mut parked = parked(&mut queue);

    assert_eq!(
        queue.resolve(reply(7), abandoned + Duration::from_millis(300)),
        Resolution::Discarded
    );

    assert_eq!(seen(&queue), stale(1, abandoned + STALE_REPLY_GRACE));
    assert!(!woken(&mut parked));
}

/// Stale → free: discarding the last owed reply frees the key at once, well
/// inside the grace, and wakes the requests parked on it.
#[test]
fn the_last_owed_reply_frees_the_key() {
    let now = Instant::now();
    let mut queue = PendingQueue::default();
    register(&mut queue, 1, ask(0), AbandonedReply::Quarantine, now);
    queue.abandon(1, now);
    let mut parked = parked(&mut queue);

    assert_eq!(queue.resolve(reply(7), now), Resolution::Discarded);

    assert_eq!(seen(&queue), Seen::Free);
    assert!(woken(&mut parked));
    assert_eq!(queue.resolve(reply(7), now), Resolution::Unmatched);
    register(&mut queue, 2, ask(1), AbandonedReply::Quarantine, now);
}

/// Stale → free: a reply that never comes holds the key until its grace
/// expires — refused an instant before, registered at it, and not adopted.
#[test]
fn a_stale_key_frees_when_its_grace_runs_out() {
    let abandoned = Instant::now();
    let expires = abandoned + STALE_REPLY_GRACE;
    let mut queue = PendingQueue::default();
    register(&mut queue, 1, ask(0), AbandonedReply::Quarantine, abandoned);
    queue.abandon(1, abandoned);

    let just_before = expires.checked_sub(Duration::from_nanos(1)).unwrap();
    assert_eq!(
        refusal(&mut queue, ask(1), AbandonedReply::Quarantine, just_before),
        Wait::Stale(expires)
    );

    register(&mut queue, 2, ask(1), AbandonedReply::Quarantine, expires);

    assert_eq!(
        seen(&queue),
        Seen::InFlight {
            id: 2,
            extra_replies: 0
        }
    );
}

/// A reply goes to the oldest in-flight request that accepts it, even past
/// stale keys held longer that accept it too; with none in flight, it is
/// discarded against the key that went stale first, which keeps its place
/// while it is still owed.
#[test]
fn replies_match_in_flight_requests_before_stale_keys_oldest_first() {
    let now = Instant::now();
    let header = |key: CorrelationKey| HidppMessage::Short([key.0, key.1, key.2, 0, 0, 0]);
    let take = |queue: &mut PendingQueue, id, key, abandoned| {
        let (registration, _) = accepting(id, header(key), abandoned, |_| true);
        assert!(queue.try_register(registration, now).is_ok());
    };
    let (a, b, c, d) = ((1, 1, 1), (2, 2, 2), (3, 3, 3), (4, 4, 4));
    let mut queue = PendingQueue::default();
    take(&mut queue, 1, a, AbandonedReply::Quarantine);
    take(&mut queue, 2, b, AbandonedReply::Quarantine);
    // `a` goes stale first, owing two replies once its adopting re-ask is
    // abandoned too; then `b`; then `c` and `d` go in flight behind them.
    queue.abandon(1, now);
    take(&mut queue, 3, a, AbandonedReply::AdoptIdentical);
    queue.abandon(3, now);
    queue.abandon(2, now);
    take(&mut queue, 4, c, AbandonedReply::Quarantine);
    take(&mut queue, 5, d, AbandonedReply::Quarantine);
    let held = |queue: &PendingQueue| {
        queue
            .keys
            .iter()
            .map(|held| held.correlation.key)
            .collect::<Vec<_>>()
    };
    assert_eq!(held(&queue), [a, b, c, d]);

    assert_eq!(queue.resolve(reply(0), now), Resolution::Answered(4));
    assert_eq!(queue.resolve(reply(0), now), Resolution::Answered(5));
    assert_eq!(queue.resolve(reply(0), now), Resolution::Discarded);
    assert_eq!(
        held(&queue),
        [a, b],
        "a partly discarded key lost its place"
    );
    assert_eq!(queue.resolve(reply(0), now), Resolution::Discarded);
    assert_eq!(held(&queue), [b]);
    assert_eq!(queue.resolve(reply(0), now), Resolution::Discarded);
    assert!(held(&queue).is_empty());
}

/// One event the exhaustive walk below can apply to [`KEY`].
#[derive(Clone, Copy, Debug)]
enum Event {
    Ask(AbandonedReply, u8),
    AbandonNewest,
    Reply,
    GraceRunsOut,
}

const EVENTS: [Event; 6] = [
    Event::Ask(AbandonedReply::Quarantine, 0),
    Event::Ask(AbandonedReply::AdoptIdentical, 0),
    Event::Ask(AbandonedReply::Quarantine, 1),
    Event::AbandonNewest,
    Event::Reply,
    Event::GraceRunsOut,
];

/// A key is held at most once, whatever order requests, abandonments,
/// replies and the clock arrive in: every sequence of six events, which
/// reaches every state and every transition above, leaves at most one entry
/// for the key after each step. [`PendingQueue::hold`]'s `debug_assert`
/// guards the same invariant on every path the channel tests drive.
#[test]
fn a_key_is_held_at_most_once_whatever_the_order_of_events() {
    const DEPTH: u32 = 6;
    for walk in 0..EVENTS.len().pow(DEPTH) {
        let mut queue = PendingQueue::default();
        let mut now = Instant::now();
        let mut registered = Vec::new();
        let mut next_id = 0;
        let mut remaining = walk;
        let mut events = Vec::new();
        for _ in 0..DEPTH {
            let event = EVENTS[remaining % EVENTS.len()];
            remaining /= EVENTS.len();
            events.push(event);
            match event {
                Event::Ask(abandoned, question) => {
                    next_id += 1;
                    let (registration, receiver) = registration(next_id, ask(question), abandoned);
                    if queue.try_register(registration, now).is_ok() {
                        registered.push((next_id, receiver));
                    }
                }
                Event::AbandonNewest => {
                    if let Some((id, _receiver)) = registered.pop() {
                        queue.abandon(id, now);
                    }
                }
                Event::Reply => {
                    queue.resolve(reply(0), now);
                }
                Event::GraceRunsOut => now += STALE_REPLY_GRACE,
            }
            let held = queue
                .keys
                .iter()
                .filter(|held| held.correlation.key == KEY)
                .count();
            assert!(held <= 1, "{KEY:?} held {held} times after {events:?}");
        }
    }
}
