use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use super::*;

/// The sampling period under test: short, so a watcher that ignores the
/// demand samples many times within [`SETTLE`].
const TICK: Duration = Duration::from_millis(1);
/// Long enough for a watcher that ignored the demand to have sampled.
const SETTLE: Duration = Duration::from_millis(30);
/// The bound on anything that must happen.
const DEADLINE: Duration = Duration::from_secs(5);

const DESKTOP: PointerContext = PointerContext {
    app: None,
    target: PointerTarget::Desktop,
};
const UNIDENTIFIED: PointerContext = PointerContext {
    app: None,
    target: PointerTarget::Unavailable,
};

/// [`track`] on its own thread, with a sampler that counts its calls and
/// always finds the pointer over the desktop.
struct Watcher {
    demand: watch::Sender<PointerDemand>,
    context: watch::Receiver<PointerContext>,
    samples: Arc<AtomicUsize>,
    ended: mpsc::Receiver<()>,
}

impl Watcher {
    fn start(initial: PointerDemand) -> Self {
        let (demand, demands) = watch::channel(initial);
        let (tx, context) = watch::channel(UNIDENTIFIED);
        let samples = Arc::new(AtomicUsize::new(0));
        let (ended_tx, ended) = mpsc::channel();
        let counter = Arc::clone(&samples);
        thread::spawn(move || {
            track(
                &tx,
                demands,
                || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    DESKTOP
                },
                TICK,
            );
            let _ = ended_tx.send(());
        });
        Self {
            demand,
            context,
            samples,
            ended,
        }
    }

    fn samples(&self) -> usize {
        self.samples.load(Ordering::SeqCst)
    }

    fn wait_for_context(&self, expected: &PointerContext) {
        let deadline = Instant::now() + DEADLINE;
        while *self.context.borrow() != *expected {
            assert!(
                Instant::now() < deadline,
                "pointer context never became {expected:?}"
            );
            thread::sleep(TICK);
        }
    }

    /// Stop listening, and require the watcher thread to end.
    fn finish(self) {
        drop(self.context);
        self.ended
            .recv_timeout(DEADLINE)
            .expect("the watcher ends once its consumer stops listening");
    }
}

#[test]
fn an_idle_watcher_never_samples_and_still_ends_with_its_consumer() {
    let watcher = Watcher::start(PointerDemand::Idle);
    thread::sleep(SETTLE);
    assert_eq!(watcher.samples(), 0, "nothing reads the context");
    // Parked on the demand, the thread must still hear the consumer leave.
    watcher.finish();
}

#[test]
fn the_watcher_samples_only_while_wanted_and_republishes_on_resume() {
    let watcher = Watcher::start(PointerDemand::Wanted);
    watcher.wait_for_context(&DESKTOP);

    // Going idle withdraws the last sample, then stops sampling.
    watcher.demand.send_replace(PointerDemand::Idle);
    watcher.wait_for_context(&UNIDENTIFIED);
    let parked_at = watcher.samples();
    thread::sleep(SETTLE);
    assert_eq!(
        watcher.samples(),
        parked_at,
        "a parked watcher samples nothing"
    );

    // The pointer never left the desktop, yet resuming publishes it again:
    // the consumer was told the context is unknown while parked.
    watcher.demand.send_replace(PointerDemand::Wanted);
    watcher.wait_for_context(&DESKTOP);
    assert!(watcher.samples() > parked_at);
    watcher.finish();
}

#[test]
fn a_parked_watcher_outlives_a_closed_demand_until_its_consumer_leaves() {
    let watcher = Watcher::start(PointerDemand::Idle);
    let Watcher {
        demand,
        context,
        samples,
        ended,
    } = watcher;
    drop(demand);
    thread::sleep(SETTLE);
    assert_eq!(samples.load(Ordering::SeqCst), 0);
    // The consumer still listens, so the watcher keeps its channel open.
    assert_eq!(ended.try_recv(), Err(mpsc::TryRecvError::Empty));
    drop(context);
    ended
        .recv_timeout(DEADLINE)
        .expect("the watcher ends once its consumer stops listening");
}
