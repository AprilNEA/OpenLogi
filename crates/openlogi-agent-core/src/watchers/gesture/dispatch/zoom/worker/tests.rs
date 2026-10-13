use super::*;
use openlogi_core::binding::Action;
use openlogi_core::config::ThumbwheelSensitivity;
use openlogi_inject::ZoomPhase;
use std::cell::RefCell;
use std::sync::Mutex;

#[derive(Debug)]
struct Event(ZoomPhase, f64);
thread_local! {
    static EVENTS: RefCell<Option<mpsc::Sender<Event>>> = const { RefCell::new(None) };
}

struct Recorder;
impl Output for Recorder {
    fn begin(_: ZoomContext, delta: f64) -> Option<Self> {
        record(ZoomPhase::Began, 0.0);
        record(ZoomPhase::Changed, delta);
        Some(Self)
    }
    fn change(&mut self, delta: f64, _: (f64, f64)) {
        record(ZoomPhase::Changed, delta);
    }
    fn finish(&mut self, phase: ZoomPhase) {
        record(phase, 0.0);
    }
}
fn record(phase: ZoomPhase, delta: f64) {
    EVENTS.with_borrow(|sender| {
        sender.as_ref().unwrap().send(Event(phase, delta)).unwrap();
    });
}

struct Gate {
    entered: mpsc::Sender<()>,
    released: mpsc::Receiver<()>,
}
struct FakeEnvironment {
    permitted: Arc<AtomicBool>,
    clock: Arc<Mutex<Instant>>,
    pointer: PointerTarget,
    events: mpsc::Sender<Event>,
    gate: Option<Gate>,
    processed: mpsc::Sender<()>,
    observed_app: Arc<Mutex<Option<String>>>,
    live_app: String,
}
impl Environment for FakeEnvironment {
    type Gesture = Recorder;
    fn permitted(&self) -> bool {
        self.permitted.load(Ordering::Acquire)
    }
    fn observed_app(&self) -> Option<String> {
        self.observed_app.lock().unwrap().clone()
    }
    fn now(&self) -> Instant {
        *self.clock.lock().unwrap()
    }
    fn context(&mut self, _: Option<PointerTarget>) -> Option<LiveContext> {
        EVENTS.with_borrow_mut(|sender| *sender = Some(self.events.clone()));
        if let Some(gate) = self.gate.take() {
            gate.entered.send(()).unwrap();
            gate.released.recv().unwrap();
        }
        Some(LiveContext {
            app_id: self.live_app.clone(),
            foreground_pid: Some(42),
            pointer: self.pointer,
            output: ZoomContext {
                x: 1.0,
                y: 2.0,
                process_id: Some(42),
                window_id: Some(7),
            },
        })
    }
    fn foreground(&mut self) -> (Option<String>, Option<i32>) {
        (Some(self.live_app.clone()), Some(42))
    }
    fn processed(&self) {
        let _ = self.processed.send(());
    }
}

struct Harness {
    worker: Worker,
    events: mpsc::Receiver<Event>,
    entered: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
    clock: Arc<Mutex<Instant>>,
    processed: mpsc::Receiver<()>,
}
fn environment() -> (
    FakeEnvironment,
    mpsc::Receiver<Event>,
    mpsc::Receiver<()>,
    mpsc::Sender<()>,
    mpsc::Receiver<()>,
) {
    let (events, recorded) = mpsc::channel();
    let (entered, wait) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (processed, checkpoint) = mpsc::channel();
    (
        FakeEnvironment {
            permitted: Arc::new(AtomicBool::new(true)),
            clock: Arc::new(Mutex::new(Instant::now())),
            pointer: pointer(),
            events,
            gate: Some(Gate { entered, released }),
            processed,
            observed_app: Arc::new(Mutex::new(Some("app-a".into()))),
            live_app: "app-a".into(),
        },
        recorded,
        wait,
        release,
        checkpoint,
    )
}
fn harness() -> Harness {
    let (environment, events, entered, release, processed) = environment();
    let clock = Arc::clone(&environment.clock);
    Harness {
        worker: Worker::spawn_with(environment).unwrap(),
        events,
        entered,
        release,
        clock,
        processed,
    }
}
fn pointer() -> PointerTarget {
    PointerTarget::Window {
        process_id: 42,
        window_id: 7,
    }
}
fn session(epoch: u64) -> HidppSessionId {
    HidppSessionId::with_epoch("mouse-a", epoch)
}
fn input(now: Instant) -> Input {
    Input {
        configuration: WheelConfiguration {
            up: Action::ZoomIn,
            down: Action::ZoomOut,
            sensitivity: ThumbwheelSensitivity::DEFAULT,
        },
        pointer: Some(pointer()),
        foreground_app: Some("app-a".into()),
        rotation: WheelRotation::from_increments(1).unwrap(),
        magnitude: 1.0,
        now,
        stopped: false,
    }
}
fn wait(receiver: &mpsc::Receiver<()>) {
    receiver.recv_timeout(Duration::from_secs(2)).unwrap();
}
fn event(receiver: &mpsc::Receiver<Event>) -> Event {
    receiver.recv_timeout(Duration::from_secs(2)).unwrap()
}
fn finish(worker: &mut Worker) {
    worker.shutdown();
    worker.thread.take().unwrap().join().unwrap();
}

#[test]
fn blocked_live_lookup_does_not_block_cancel_pause_or_shutdown_and_never_posts_after_them() {
    for reason in 0..3 {
        let mut test = harness();
        let now = *test.clock.lock().unwrap();
        test.worker.motion(&session(1), input(now));
        wait(&test.entered);
        // These run on the producer while the worker is still blocked.
        match reason {
            0 => test.worker.cancel(&session(1)),
            1 => test.worker.pause(),
            _ => test.worker.shutdown(),
        }
        assert!(test.worker.leases.is_empty());
        test.release.send(()).unwrap();
        wait(&test.processed);
        assert!(test.events.try_iter().next().is_none());
        finish(&mut test.worker);
        assert!(test.events.try_iter().next().is_none());
    }
}

#[test]
fn config_replacement_revokes_a_blocked_report_and_starts_with_new_filter_state() {
    let mut test = harness();
    let now = *test.clock.lock().unwrap();
    test.worker.motion(&session(1), input(now));
    wait(&test.entered);
    let mut replacement = input(now);
    replacement.configuration.sensitivity = ThumbwheelSensitivity::MIN;
    replacement.configuration.up = Action::ZoomOut;
    test.worker.motion(&session(1), replacement);
    test.release.send(()).unwrap();
    assert_eq!(event(&test.events).0, ZoomPhase::Began);
    let changed = event(&test.events);
    assert_eq!(changed.0, ZoomPhase::Changed);
    assert!((changed.1 + 0.003_125).abs() < f64::EPSILON);
    finish(&mut test.worker);
    assert_eq!(event(&test.events).0, ZoomPhase::Cancelled);
    assert!(test.events.try_iter().next().is_none());
}

#[test]
fn old_epoch_cancellation_cannot_remove_a_queued_successor() {
    let mut test = harness();
    let now = *test.clock.lock().unwrap();
    test.worker.motion(&session(1), input(now));
    wait(&test.entered);
    test.worker.cancel(&session(1));
    test.worker.motion(&session(2), input(now));
    test.worker.cancel(&session(1));
    test.release.send(()).unwrap();
    assert_eq!(event(&test.events).0, ZoomPhase::Began);
    assert_eq!(event(&test.events).0, ZoomPhase::Changed);
    finish(&mut test.worker);
    assert_eq!(event(&test.events).0, ZoomPhase::Cancelled);
    assert!(test.events.try_iter().next().is_none());
}

#[test]
fn saturated_queue_revokes_even_the_report_currently_blocked_in_a_lookup() {
    let mut test = harness();
    let now = *test.clock.lock().unwrap();
    test.worker.motion(&session(1), input(now));
    wait(&test.entered);
    for _ in 0..=CAPACITY {
        test.worker.motion(&session(1), input(now));
    }
    assert!(
        test.worker.leases.is_empty(),
        "overflow must invalidate out of band"
    );
    test.release.send(()).unwrap();
    finish(&mut test.worker);
    assert!(test.events.try_iter().next().is_none());
}

#[test]
fn permission_revoked_while_lookup_is_blocked_prevents_output_without_a_native_permission_call() {
    let (mut environment, events, entered, release, _) = environment();
    let permission = Arc::clone(&environment.permitted);
    let input = input(environment.now());
    let lease = Arc::new(Lease {
        configuration: input.configuration.clone(),
        pointer: input.pointer,
        foreground_app: input.foreground_app.clone(),
        valid: AtomicBool::new(true),
    });
    let task = thread::spawn(move || {
        let mut state = State::<Recorder>::default();
        state.report(
            &Report {
                session: session(1),
                lease,
                input,
            },
            &AtomicBool::new(false),
            &mut environment,
        );
        assert!(state.sessions.deadline().is_none());
    });
    wait(&entered);
    permission.store(false, Ordering::Release);
    release.send(()).unwrap();
    task.join().unwrap();
    assert!(events.try_iter().next().is_none());
}

#[test]
fn live_pointer_mismatch_rejects_the_captured_binding_without_reselecting_a_profile() {
    let (mut environment, events, _, _, _) = environment();
    environment.gate = None;
    environment.pointer = PointerTarget::Window {
        process_id: 42,
        window_id: 8,
    };
    let input = input(environment.now());
    let lease = Arc::new(Lease {
        configuration: input.configuration.clone(),
        pointer: input.pointer,
        foreground_app: input.foreground_app.clone(),
        valid: AtomicBool::new(true),
    });
    let mut state = State::<Recorder>::default();
    state.report(
        &Report {
            session: session(1),
            lease,
            input,
        },
        &AtomicBool::new(false),
        &mut environment,
    );
    assert!(state.sessions.deadline().is_none());
    assert!(events.try_iter().next().is_none());
}

#[test]
fn pause_resume_and_stop_expiry_keep_fifo_and_balanced_terminal_ownership() {
    let mut test = harness();
    let now = *test.clock.lock().unwrap();
    test.worker.motion(&session(1), input(now));
    wait(&test.entered);
    test.worker.pause();
    test.worker.motion(&session(1), input(now));
    assert!(test.worker.leases.is_empty());
    test.worker.resume();
    test.worker.motion(&session(1), input(now));
    test.release.send(()).unwrap();
    assert_eq!(event(&test.events).0, ZoomPhase::Began);
    assert_eq!(event(&test.events).0, ZoomPhase::Changed);
    test.worker.stop(&session(1), now);
    let resumed = now + super::super::END_DELAY / 2;
    *test.clock.lock().unwrap() = resumed;
    test.worker.motion(&session(1), input(resumed));
    assert_eq!(
        event(&test.events).0,
        ZoomPhase::Changed,
        "stop/resume must not begin a second gesture"
    );
    *test.clock.lock().unwrap() = resumed + IDLE;
    test.worker.wake();
    assert_eq!(event(&test.events).0, ZoomPhase::Ended);
    finish(&mut test.worker);
    test.worker.resume();
    test.worker.motion(&session(1), input(now));
    assert!(test.events.try_iter().next().is_none());
}

#[test]
fn queued_report_older_than_idle_deadline_cannot_begin_even_if_lookup_succeeds() {
    let (mut environment, events, _, _, _) = environment();
    environment.gate = None;
    let input = input(environment.now().checked_sub(IDLE).unwrap());
    let lease = Arc::new(Lease {
        configuration: input.configuration.clone(),
        pointer: input.pointer,
        foreground_app: input.foreground_app.clone(),
        valid: AtomicBool::new(true),
    });
    let mut state = State::<Recorder>::default();
    state.report(
        &Report {
            session: session(1),
            lease,
            input,
        },
        &AtomicBool::new(false),
        &mut environment,
    );
    assert!(state.sessions.deadline().is_none());
    assert!(events.try_iter().next().is_none());
}

#[test]
fn focused_policy_never_retargets_queued_reports_to_a_different_or_unobserved_app() {
    for mismatch in 0..3 {
        let (mut environment, events, _, _, _) = environment();
        environment.gate = None;
        let mut input = input(environment.now());
        input.pointer = None;
        match mismatch {
            0 => *environment.observed_app.lock().unwrap() = Some("app-b".into()),
            1 => environment.live_app = "app-b".into(),
            _ => input.foreground_app = None,
        }
        let lease = Arc::new(Lease {
            configuration: input.configuration.clone(),
            pointer: input.pointer,
            foreground_app: input.foreground_app.clone(),
            valid: AtomicBool::new(true),
        });
        let mut state = State::<Recorder>::default();
        state.report(
            &Report {
                session: session(1),
                lease,
                input,
            },
            &AtomicBool::new(false),
            &mut environment,
        );
        assert!(state.sessions.deadline().is_none());
        assert!(events.try_iter().next().is_none());
    }
}

#[test]
fn cached_focus_change_during_a_blocked_lookup_invalidates_its_result() {
    let (environment, events, entered, release, processed) = environment();
    let observed = Arc::clone(&environment.observed_app);
    let now = environment.now();
    let mut worker = Worker::spawn_with(environment).unwrap();
    let mut focused = input(now);
    focused.pointer = None;
    worker.motion(&session(1), focused);
    wait(&entered);
    *observed.lock().unwrap() = Some("app-b".into());
    release.send(()).unwrap();
    wait(&processed);
    assert!(events.try_iter().next().is_none());
    finish(&mut worker);
}
