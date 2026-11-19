use super::*;

/// A lifecycle whose source is a backend name and whose worker is a number,
/// so each test can tell which source or worker ended up where.
type Lifecycle = ObserverLifecycle<&'static str, u8>;

/// Where the lifecycle's source and worker are, read through its queries.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Idle(&'static str),
    Observing(u8),
    Stopping,
}

fn seen(lifecycle: &mut Lifecycle) -> Seen {
    let stopping = lifecycle.is_stopping();
    let worker = lifecycle.worker().copied();
    match (lifecycle.idle_source().copied(), worker, stopping) {
        (Some(source), None, false) => Seen::Idle(source),
        (None, Some(worker), false) => Seen::Observing(worker),
        (None, None, true) => Seen::Stopping,
        other => panic!("queries disagree on the stage: {other:?}"),
    }
}

/// Launch worker `worker`, recording the source it was handed.
fn launched(
    handed: &mut Option<&'static str>,
    worker: u8,
) -> impl FnOnce(&'static str) -> Result<u8, (&'static str, &'static str)> {
    move |source| {
        *handed = Some(source);
        Ok(worker)
    }
}

fn observing(source: &'static str, worker: u8) -> Lifecycle {
    let mut lifecycle = Lifecycle::new(source);
    let mut handed = None;
    assert_eq!(
        lifecycle.start(launched(&mut handed, worker)),
        Start::Started
    );
    lifecycle
}

fn stopping(source: &'static str, worker: u8) -> Lifecycle {
    let mut lifecycle = observing(source, worker);
    assert_eq!(lifecycle.stop(), Some(worker));
    lifecycle
}

#[test]
fn start_moves_the_idle_source_to_the_launched_worker() {
    let mut lifecycle = Lifecycle::new("x11");
    assert_eq!(seen(&mut lifecycle), Seen::Idle("x11"));

    let mut handed = None;
    assert_eq!(lifecycle.start(launched(&mut handed, 1)), Start::Started);

    assert_eq!(handed, Some("x11"));
    assert_eq!(seen(&mut lifecycle), Seen::Observing(1));
}

#[test]
fn a_failed_launch_gives_the_source_back_to_idle() {
    let mut lifecycle = Lifecycle::new("x11");

    let started = lifecycle.start(|source| Err((source, "worker exited before the hand-off")));

    assert_eq!(started, Start::Failed("worker exited before the hand-off"));
    assert_eq!(seen(&mut lifecycle), Seen::Idle("x11"));
    // The returned source is the one the next launch receives.
    let mut handed = None;
    assert_eq!(lifecycle.start(launched(&mut handed, 2)), Start::Started);
    assert_eq!(handed, Some("x11"));
    assert_eq!(seen(&mut lifecycle), Seen::Observing(2));
}

#[test]
fn start_launches_nothing_while_a_worker_holds_the_source() {
    for (mut lifecycle, expected) in [
        (observing("x11", 1), Seen::Observing(1)),
        (stopping("x11", 1), Seen::Stopping),
    ] {
        let mut handed = None;
        assert_eq!(lifecycle.start(launched(&mut handed, 2)), Start::Busy);
        assert_eq!(handed, None, "launch ran outside idle");
        assert_eq!(seen(&mut lifecycle), expected);
    }
}

#[test]
fn stop_takes_the_observing_worker_out() {
    let mut lifecycle = observing("x11", 1);

    assert_eq!(lifecycle.stop(), Some(1));

    // Neither the source nor the worker is reachable until the stop ends.
    assert_eq!(seen(&mut lifecycle), Seen::Stopping);
}

#[test]
fn stop_takes_nothing_unless_a_worker_observes() {
    for (mut lifecycle, expected) in [
        (Lifecycle::new("x11"), Seen::Idle("x11")),
        (stopping("x11", 1), Seen::Stopping),
    ] {
        assert_eq!(lifecycle.stop(), None);
        assert_eq!(seen(&mut lifecycle), expected);
    }
}

#[test]
fn stopped_returns_the_joined_workers_source_to_idle() {
    let mut lifecycle = stopping("x11", 1);

    lifecycle.stopped("x11");

    assert_eq!(seen(&mut lifecycle), Seen::Idle("x11"));
}

#[test]
fn a_panicked_workers_replacement_source_becomes_idle_and_observes_next() {
    let mut lifecycle = stopping("x11", 1);

    // The worker panicked and lost "x11"; detection selected "wlr".
    lifecycle.stopped("wlr");

    assert_eq!(seen(&mut lifecycle), Seen::Idle("wlr"));
    let mut handed = None;
    assert_eq!(lifecycle.start(launched(&mut handed, 2)), Start::Started);
    assert_eq!(handed, Some("wlr"));
}

#[test]
fn stopped_is_ignored_unless_a_worker_is_stopping() {
    for (mut lifecycle, expected) in [
        (Lifecycle::new("x11"), Seen::Idle("x11")),
        (observing("x11", 1), Seen::Observing(1)),
    ] {
        lifecycle.stopped("stray");
        assert_eq!(seen(&mut lifecycle), expected);
    }
}

fn app(id: &str) -> ForegroundApp {
    ForegroundApp::unnamed(id.into())
}

fn published(version: u64, app: Option<ForegroundApp>) -> Published {
    Published { version, app }
}

#[test]
fn an_inactive_feed_drops_a_publish() {
    let mut feed = Feed::new();

    assert_eq!(feed.publish(Some(app("late"))), None);

    assert_eq!(feed.latest(), None);
    assert_eq!(feed.snapshot(), ActiveSnapshot::Idle);
    // Nor does the dropped change surface in the next session.
    feed.begin();
    assert_eq!(feed.latest(), None);
    assert_eq!(feed.snapshot(), ActiveSnapshot::Active(None));
}

#[test]
fn an_active_feed_numbers_each_change_and_drops_a_repeat() {
    let mut feed = Feed::new();
    feed.begin();
    assert_eq!(feed.snapshot(), ActiveSnapshot::Active(None));

    // A first `None` is a change: it answers the subscribe-time snapshot.
    assert_eq!(feed.publish(None), Some(&published(1, None)));
    assert_eq!(feed.publish(None), None);
    assert_eq!(
        feed.publish(Some(app("one"))),
        Some(&published(2, Some(app("one"))))
    );
    assert_eq!(feed.publish(Some(app("one"))), None);
    assert_eq!(
        feed.publish(Some(app("two"))),
        Some(&published(3, Some(app("two"))))
    );
    assert_eq!(feed.snapshot(), ActiveSnapshot::Active(Some(app("two"))));
    assert_eq!(feed.publish(None), Some(&published(4, None)));

    assert_eq!(feed.latest(), Some(&published(4, None)));
    assert_eq!(feed.snapshot(), ActiveSnapshot::Active(None));
}

#[test]
fn finish_forgets_the_session_and_begin_starts_afresh() {
    let mut feed = Feed::new();
    feed.begin();
    assert!(feed.publish(Some(app("one"))).is_some());
    assert!(feed.publish(Some(app("two"))).is_some());

    feed.finish();
    assert_eq!(feed.latest(), None);
    assert_eq!(feed.snapshot(), ActiveSnapshot::Idle);

    feed.begin();
    assert_eq!(feed.latest(), None);
    // Versions restart, and the last session's app is no repeat here.
    assert_eq!(
        feed.publish(Some(app("two"))),
        Some(&published(1, Some(app("two"))))
    );

    // Beginning again while active also starts afresh.
    feed.begin();
    assert_eq!(feed.latest(), None);
    assert_eq!(
        feed.publish(Some(app("two"))),
        Some(&published(1, Some(app("two"))))
    );
}
