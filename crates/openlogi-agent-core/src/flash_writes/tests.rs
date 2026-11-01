//! The flash-write queue and task: one job at a time, debounced, the latest
//! request winning, never cut short, and waited for at shutdown.

use std::sync::atomic::{AtomicUsize, Ordering};

use openlogi_core::binding::{FlashBinding, MouseButtonNumber};
use tokio::sync::Semaphore;

use super::*;

fn route(slot: u8) -> DeviceRoute {
    DeviceRoute::Unifying {
        receiver_uid: "c531".into(),
        slot,
    }
}

fn job(intents: &[SlotIntent]) -> FlashJob {
    FlashJob::bindings(intents.to_vec())
}

/// The job that starts for a request of `intents` alone.
fn ready(intents: &[SlotIntent]) -> G700Job {
    G700Job {
        bindings: intents.to_vec(),
        ..G700Job::default()
    }
}

fn outcome(profile: u8) -> FlashOutcome {
    let sixteen = MouseButtonNumber::try_new(16).expect("a mouse button number");
    FlashOutcome {
        profile: Some(profile),
        plan: FlashPlan {
            bindings: vec![FlashBinding::MouseButtons(sixteen.mask())],
            routed: [(sixteen, openlogi_core::binding::ButtonId::G6)].into(),
            unrouted: Vec::new(),
        },
    }
}

#[test]
fn a_request_waits_out_the_debounce() {
    let now = Instant::now();
    let mut queue = FlashQueue::default();
    queue.request(route(1), job(&[SlotIntent::Agent]), now);
    assert_eq!(queue.next_due(), Some(now + DEBOUNCE));
    assert_eq!(queue.start_due(now + DEBOUNCE / 2), None);
    assert_eq!(queue.write_of(&route(1)), Some(FlashWrite::Pending));
    assert_eq!(
        queue.start_due(now + DEBOUNCE),
        Some((route(1), ready(&[SlotIntent::Agent])))
    );
    assert_eq!(queue.write_of(&route(1)), Some(FlashWrite::Writing));
}

#[test]
fn a_burst_writes_the_latest_request_once_after_the_last() {
    let now = Instant::now();
    let later = now + DEBOUNCE / 2;
    let mut queue = FlashQueue::default();
    queue.request(route(1), job(&[SlotIntent::Agent]), now);
    queue.request(route(1), job(&[SlotIntent::Keep]), later);
    assert_eq!(
        queue.start_due(now + DEBOUNCE),
        None,
        "the second request restarts the wait"
    );
    assert_eq!(
        queue.start_due(later + DEBOUNCE),
        Some((route(1), ready(&[SlotIntent::Keep])))
    );
    queue.finish();
    assert_eq!(queue.next_due(), None);
    assert_eq!(queue.write_of(&route(1)), None);
}

#[test]
fn one_job_runs_at_a_time_across_devices() {
    let now = Instant::now();
    let mut queue = FlashQueue::default();
    queue.request(route(1), job(&[SlotIntent::Agent]), now);
    queue.request(
        route(2),
        job(&[SlotIntent::Keep]),
        now + Duration::from_millis(1),
    );
    let due = now + DEBOUNCE * 2;
    let (first, _) = queue.start_due(due).expect("the first due job");
    assert_eq!(first, route(1), "the earliest request starts first");
    assert_eq!(queue.next_due(), None, "nothing is due while a job runs");
    assert_eq!(queue.start_due(due), None);
    queue.finish();
    assert_eq!(queue.start_due(due).map(|(route, _)| route), Some(route(2)));
}

#[test]
fn a_request_during_its_devices_job_waits_for_it() {
    let now = Instant::now();
    let mut queue = FlashQueue::default();
    queue.request(route(1), job(&[SlotIntent::Agent]), now);
    queue.start_due(now + DEBOUNCE).expect("started");
    queue.request(route(1), job(&[SlotIntent::Keep]), now + DEBOUNCE);
    assert_eq!(queue.write_of(&route(1)), Some(FlashWrite::Writing));
    assert_eq!(queue.start_due(now + DEBOUNCE * 3), None);
    queue.finish();
    assert_eq!(queue.write_of(&route(1)), Some(FlashWrite::Pending));
    assert_eq!(
        queue.start_due(now + DEBOUNCE * 2),
        Some((route(1), ready(&[SlotIntent::Keep])))
    );
}

#[test]
fn leaving_drops_what_has_not_started() {
    let now = Instant::now();
    let mut queue = FlashQueue::default();
    queue.request(route(1), job(&[SlotIntent::Agent]), now);
    queue.request(route(2), job(&[SlotIntent::Agent]), now);
    queue.start_due(now + DEBOUNCE).expect("started");
    assert_eq!(queue.drop_queued(), 1);
    assert_eq!(queue.next_due(), None);
    let known = [route(1), route(2)]
        .iter()
        .filter(|route| queue.write_of(route).is_some())
        .count();
    assert_eq!(known, 1, "only the running write is left");
}

/// An executor that records each job and holds it until the test releases
/// a permit, the way a page rewrite holds the mouse for seconds.
#[derive(Clone)]
struct Gate {
    started: Arc<AtomicUsize>,
    finished: Arc<AtomicUsize>,
    running: Arc<AtomicUsize>,
    permits: Arc<Semaphore>,
    jobs: Arc<Mutex<Vec<(DeviceRoute, G700Job)>>>,
    /// How many of the next jobs fail.
    failures: Arc<AtomicUsize>,
    /// Whether a failing job fails at its first profile edit, leaving it in
    /// the job.
    fail_at_edit: bool,
}

impl Gate {
    fn new() -> Self {
        Self {
            started: Arc::default(),
            finished: Arc::default(),
            running: Arc::default(),
            permits: Arc::new(Semaphore::new(0)),
            jobs: Arc::default(),
            failures: Arc::default(),
            fail_at_edit: false,
        }
    }

    fn failing(failures: usize) -> Self {
        let gate = Self::new();
        gate.failures.store(failures, Ordering::SeqCst);
        gate
    }

    fn jobs(&self) -> Vec<(DeviceRoute, G700Job)> {
        self.jobs.lock().expect("jobs").clone()
    }
}

impl FlashExecutor for Gate {
    async fn execute(
        &self,
        route: &DeviceRoute,
        job: &mut G700Job,
    ) -> Result<FlashOutcome, String> {
        self.started.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            self.running.fetch_add(1, Ordering::SeqCst),
            0,
            "two flash jobs ran at once"
        );
        self.jobs
            .lock()
            .expect("jobs")
            .push((route.clone(), job.clone()));
        self.permits.acquire().await.expect("permits").forget();
        self.running.fetch_sub(1, Ordering::SeqCst);
        self.finished.fetch_add(1, Ordering::SeqCst);
        let failing = self
            .failures
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok();
        if failing {
            if !self.fail_at_edit {
                job.edits.clear();
            }
            Err("the mouse stopped answering".into())
        } else {
            job.edits.clear();
            Ok(outcome(1))
        }
    }
}

/// Lets the task run until it blocks again.
async fn settle() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn requests_before_start_wait_for_it() {
    let writes = FlashWrites::default();
    writes.request(route(1), job(&[SlotIntent::Agent]));
    assert_eq!(writes.state(&route(1)).write, FlashWrite::Pending);
    let gate = Gate::new();
    writes.start(gate.clone());
    tokio::time::sleep(DEBOUNCE).await;
    settle().await;
    assert_eq!(gate.started.load(Ordering::SeqCst), 1);
    assert_eq!(writes.state(&route(1)).write, FlashWrite::Writing);
    gate.permits.add_permits(1);
    settle().await;
    let state = writes.state(&route(1));
    assert_eq!(state.write, FlashWrite::Idle);
    assert_eq!(state.profile, Some(1));
    assert_eq!(state.routed, outcome(1).plan.routed);
    writes.shutdown().await;
}

/// A write that takes far longer than any request deadline is not cut
/// short: it runs to its end and its outcome is published.
#[tokio::test(start_paused = true)]
async fn a_slow_write_is_never_abandoned() {
    let writes = FlashWrites::default();
    let gate = Gate::new();
    writes.start(gate.clone());
    writes.request(route(1), job(&[SlotIntent::Agent]));
    tokio::time::sleep(DEBOUNCE).await;
    settle().await;
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(writes.state(&route(1)).write, FlashWrite::Writing);
    assert_eq!(gate.finished.load(Ordering::SeqCst), 0);
    gate.permits.add_permits(1);
    settle().await;
    assert_eq!(gate.finished.load(Ordering::SeqCst), 1);
    assert_eq!(writes.state(&route(1)).profile, Some(1));
    writes.shutdown().await;
}

/// Edits while a job runs coalesce into one more job with the latest of
/// them, and two devices never write at once.
#[tokio::test(start_paused = true)]
async fn writes_are_serialised_and_coalesced() {
    let writes = FlashWrites::default();
    let gate = Gate::new();
    writes.start(gate.clone());
    writes.request(route(1), job(&[SlotIntent::Agent]));
    tokio::time::sleep(Duration::from_millis(1)).await;
    writes.request(route(2), job(&[SlotIntent::Agent]));
    tokio::time::sleep(DEBOUNCE).await;
    settle().await;
    assert_eq!(gate.started.load(Ordering::SeqCst), 1);
    writes.request(route(1), job(&[SlotIntent::Keep]));
    writes.request(route(1), job(&[SlotIntent::Native(FlashBinding::Disabled)]));
    gate.permits.add_permits(1);
    settle().await;
    tokio::time::sleep(DEBOUNCE).await;
    settle().await;
    gate.permits.add_permits(1);
    settle().await;
    tokio::time::sleep(DEBOUNCE).await;
    settle().await;
    gate.permits.add_permits(1);
    settle().await;
    assert_eq!(
        gate.jobs(),
        [
            (route(1), ready(&[SlotIntent::Agent])),
            (route(2), ready(&[SlotIntent::Agent])),
            (
                route(1),
                ready(&[SlotIntent::Native(FlashBinding::Disabled)])
            ),
        ],
        "the superseded edit is never written"
    );
    writes.shutdown().await;
}

/// Shutdown waits for the job in progress, drops the queued ones, and
/// refuses new requests.
#[tokio::test(start_paused = true)]
async fn shutdown_waits_for_the_running_write() {
    let writes = FlashWrites::default();
    let gate = Gate::new();
    writes.start(gate.clone());
    writes.request(route(1), job(&[SlotIntent::Agent]));
    tokio::time::sleep(DEBOUNCE).await;
    settle().await;
    writes.request(route(2), job(&[SlotIntent::Agent]));
    let shutdown = tokio::spawn({
        let writes = writes.clone();
        async move { writes.shutdown().await }
    });
    tokio::time::sleep(SETTLE_BUDGET.saturating_sub(Duration::from_secs(1))).await;
    settle().await;
    assert!(
        !shutdown.is_finished(),
        "shutdown left before the write ended"
    );
    gate.permits.add_permits(1);
    shutdown.await.expect("shutdown");
    assert_eq!(gate.finished.load(Ordering::SeqCst), 1);
    assert_eq!(
        gate.started.load(Ordering::SeqCst),
        1,
        "the queued write was dropped"
    );
    writes.request(route(3), job(&[SlotIntent::Agent]));
    assert_eq!(writes.state(&route(3)).write, FlashWrite::Idle);
}

/// A failed write is reported, keeps what the last good one found, and a
/// later request replaces the failure with its own state.
#[tokio::test(start_paused = true)]
async fn a_failure_is_retried_once_then_reported_until_the_next_request() {
    let writes = FlashWrites::default();
    let gate = Gate::failing(2);
    writes.start(gate.clone());
    writes.request(route(1), job(&[SlotIntent::Agent]));
    tokio::time::sleep(DEBOUNCE).await;
    settle().await;
    gate.permits.add_permits(1);
    settle().await;
    assert_eq!(
        writes.state(&route(1)).write,
        FlashWrite::Writing,
        "the retry is still to come"
    );
    tokio::time::sleep(RETRY_DELAY).await;
    settle().await;
    gate.permits.add_permits(1);
    settle().await;
    assert_eq!(gate.started.load(Ordering::SeqCst), 2);
    assert_eq!(
        writes.state(&route(1)).write,
        FlashWrite::Failed("the mouse stopped answering".into())
    );
    assert!(writes.failed(&route(1)));
    writes.request(route(1), job(&[SlotIntent::Agent]));
    assert_eq!(writes.state(&route(1)).write, FlashWrite::Pending);
    writes.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_retry_that_succeeds_clears_the_failure() {
    let writes = FlashWrites::default();
    let gate = Gate::failing(1);
    writes.start(gate.clone());
    writes.request(route(1), job(&[SlotIntent::Agent]));
    tokio::time::sleep(DEBOUNCE).await;
    settle().await;
    gate.permits.add_permits(2);
    settle().await;
    tokio::time::sleep(RETRY_DELAY).await;
    settle().await;
    assert_eq!(gate.started.load(Ordering::SeqCst), 2);
    let state = writes.state(&route(1));
    assert_eq!(state.write, FlashWrite::Idle);
    assert_eq!(state.profile, Some(1));
    assert!(!writes.failed(&route(1)));
    assert_eq!(writes.played(&route(1)), Some(Some(1)));
    writes.shutdown().await;
}

/// A profile edit that failed may have half happened, so the retry carries
/// the rest of the job without it, and its error stays reported.
#[tokio::test(start_paused = true)]
async fn a_failed_edit_is_not_retried_but_the_rest_of_the_job_is() {
    let writes = FlashWrites::default();
    let mut gate = Gate::failing(1);
    gate.fail_at_edit = true;
    writes.start(gate.clone());
    let profile = openlogi_core::hid::ProfileIndex::new(1).expect("profile 1");
    writes.request(route(1), job(&[SlotIntent::Agent]));
    writes.request(
        route(1),
        FlashJob::edit(FlashProfileEdit::Duplicate(profile)),
    );
    tokio::time::sleep(DEBOUNCE).await;
    settle().await;
    gate.permits.add_permits(2);
    settle().await;
    tokio::time::sleep(RETRY_DELAY).await;
    settle().await;
    let jobs = gate.jobs();
    assert_eq!(jobs.len(), 2);
    assert_eq!(
        jobs[0].1.edits.len(),
        1,
        "the first attempt carries the edit"
    );
    assert!(jobs[1].1.edits.is_empty(), "the retry does not");
    assert_eq!(jobs[1].1.bindings, [SlotIntent::Agent]);
    let state = writes.state(&route(1));
    assert_eq!(
        state.write,
        FlashWrite::Failed("the mouse stopped answering".into())
    );
    assert_eq!(state.profile, Some(1), "what the retry found is kept");
    writes.shutdown().await;
}

#[test]
fn requests_merge_while_they_wait() {
    let now = Instant::now();
    let mut queue = FlashQueue::default();
    let profile = openlogi_core::hid::ProfileIndex::new(2).expect("profile 2");
    let save = |from, to| G700DpiSave {
        from: openlogi_core::hid::Dpi::new(from),
        to: openlogi_core::hid::Dpi::new(to),
    };
    queue.request(route(1), job(&[SlotIntent::Agent]), now);
    queue.request(route(1), FlashJob::dpi(save(800, 1000)), now);
    queue.request(
        route(1),
        FlashJob::edit(FlashProfileEdit::Disable(profile)),
        now,
    );
    queue.request(route(1), FlashJob::dpi(save(1000, 1200)), now);
    queue.request(route(1), FlashJob::rate(ReportRate::new(500)), now);
    queue.request(route(1), FlashJob::rate(ReportRate::new(1000)), now);

    let (_, started) = queue.start_due(now + DEBOUNCE).expect("due");

    assert_eq!(
        started,
        G700Job {
            edits: VecDeque::from([FlashProfileEdit::Disable(profile)]),
            dpi: Some(save(800, 1200)),
            rate: Some(ReportRate::new(1000)),
            bindings: vec![SlotIntent::Agent],
        }
    );
    queue.finish();
    // A request that names no bindings syncs the ones last asked.
    queue.request(route(1), FlashJob::default(), now);
    let (_, resync) = queue.start_due(now + DEBOUNCE).expect("due");
    assert_eq!(resync, ready(&[SlotIntent::Agent]));
}

/// A replacement holds the task: it waits for the write in progress, starts
/// none meanwhile, and a called-off replacement lets the queued one run.
#[tokio::test(start_paused = true)]
async fn hold_waits_for_the_running_write_and_starts_none_until_resumed() {
    let writes = FlashWrites::default();
    let gate = Gate::new();
    writes.start(gate.clone());
    writes.request(route(1), job(&[SlotIntent::Agent]));
    tokio::time::sleep(DEBOUNCE).await;
    settle().await;
    let hold = tokio::spawn({
        let writes = writes.clone();
        async move { writes.hold().await }
    });
    settle().await;
    assert!(!hold.is_finished(), "hold left before the write ended");
    writes.request(route(2), job(&[SlotIntent::Agent]));
    gate.permits.add_permits(1);
    hold.await.expect("hold");
    tokio::time::sleep(DEBOUNCE * 3).await;
    settle().await;
    assert_eq!(
        gate.started.load(Ordering::SeqCst),
        1,
        "nothing starts while held"
    );
    assert_eq!(writes.state(&route(2)).write, FlashWrite::Pending);
    writes.resume();
    settle().await;
    assert_eq!(gate.started.load(Ordering::SeqCst), 2);
    gate.permits.add_permits(1);
    writes.shutdown().await;
}

/// A job that never ends — stuck waiting for the receiver, say — keeps
/// neither the agent's exit nor its replacement waiting past the budget.
#[tokio::test(start_paused = true)]
async fn hold_and_shutdown_give_up_after_the_budget() {
    let writes = FlashWrites::default();
    let gate = Gate::new();
    writes.start(gate.clone());
    writes.request(route(1), job(&[SlotIntent::Agent]));
    tokio::time::sleep(DEBOUNCE).await;
    settle().await;
    assert_eq!(gate.started.load(Ordering::SeqCst), 1);
    let started = Instant::now();
    writes.hold().await;
    let waited = started.elapsed();
    assert!(
        (SETTLE_BUDGET..SETTLE_BUDGET + DEBOUNCE).contains(&waited),
        "hold waited the budget, not {waited:?}"
    );
    let started = Instant::now();
    writes.shutdown().await;
    let waited = started.elapsed();
    assert!(
        (SETTLE_BUDGET..SETTLE_BUDGET + DEBOUNCE).contains(&waited),
        "shutdown waited the budget, not {waited:?}"
    );
    assert_eq!(gate.finished.load(Ordering::SeqCst), 0);
}
