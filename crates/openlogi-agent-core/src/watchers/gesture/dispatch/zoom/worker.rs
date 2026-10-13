//! Bounded native Zoom output worker. The capture manager alone revokes leases;
//! live context reads, motion, expiry and terminal cleanup stay on this thread.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tokio::sync::watch;

use openlogi_hook::PointerTarget;
use openlogi_inject::ZoomContext;

use super::super::{WheelAdmission, WheelConfiguration, wheel::WheelRotation};
use super::{Admission, IDLE, Motion, Output, Sessions, Target};
use crate::runtime::HidppSessionId;

const CAPACITY: usize = 32;
const CANCEL_POLL: Duration = Duration::from_millis(25);

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkerCompletion {
    Running,
    Cleaned,
}

/// Report-time binding snapshot, never resolved again against a later plan.
pub(in super::super) struct Input {
    pub(in super::super) configuration: WheelConfiguration,
    pub(in super::super) pointer: Option<PointerTarget>,
    pub(in super::super) foreground_app: Option<String>,
    pub(in super::super) rotation: WheelRotation,
    pub(in super::super) magnitude: f64,
    pub(in super::super) now: Instant,
    pub(in super::super) stopped: bool,
}

struct Lease {
    configuration: WheelConfiguration,
    pointer: Option<PointerTarget>,
    foreground_app: Option<String>,
    valid: AtomicBool,
}

impl Lease {
    fn revoke(&self) {
        self.valid.store(false, Ordering::Release);
    }
    fn valid(&self) -> bool {
        self.valid.load(Ordering::Acquire)
    }
}

struct Report {
    session: HidppSessionId,
    lease: Arc<Lease>,
    input: Input,
}

enum Command {
    Motion(Report),
    Stop {
        session: HidppSessionId,
        lease: Arc<Lease>,
        now: Instant,
    },
    Wake,
}

struct LiveContext {
    app_id: String,
    foreground_pid: Option<i32>,
    pointer: PointerTarget,
    output: ZoomContext,
}

/// Native calls are replaceable in tests; permission reads use only the cached
/// observable status and are repeated after a potentially blocking context read.
trait Environment: Send + 'static {
    type Gesture: Output + Send;
    fn permitted(&self) -> bool;
    fn observed_app(&self) -> Option<String>;
    fn context(&mut self, captured: Option<PointerTarget>) -> Option<LiveContext>;
    fn foreground(&mut self) -> (Option<String>, Option<i32>);
    #[cfg(test)]
    fn processed(&self) {}
    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[cfg(target_os = "macos")]
struct NativeEnvironment(super::super::super::GestureOutputs);

#[cfg(target_os = "macos")]
impl Environment for NativeEnvironment {
    type Gesture = openlogi_inject::ZoomGesture;

    fn permitted(&self) -> bool {
        self.0.accessibility_granted()
    }

    fn observed_app(&self) -> Option<String> {
        self.0.foreground_app_id()
    }

    fn context(&mut self, captured: Option<PointerTarget>) -> Option<LiveContext> {
        let pointer = captured.map_or(PointerTarget::Unavailable, |_| {
            openlogi_hook::pointer_context().target
        });
        let app = openlogi_hook::frontmost_application()?;
        let foreground_pid = openlogi_inject::zoom_process_id();
        let (x, y) = openlogi_inject::cursor_location()?;
        let (process_id, window_id) = match captured {
            Some(PointerTarget::Window {
                process_id,
                window_id,
            }) => (Some(process_id), Some(window_id)),
            _ => (foreground_pid, None),
        };
        Some(LiveContext {
            app_id: app.id,
            foreground_pid,
            pointer,
            output: ZoomContext {
                x,
                y,
                process_id,
                window_id,
            },
        })
    }

    fn foreground(&mut self) -> (Option<String>, Option<i32>) {
        (
            openlogi_hook::frontmost_application().map(|app| app.id),
            openlogi_inject::zoom_process_id(),
        )
    }
}

/// The manager's single non-blocking producer. Revocation is out of band, so
/// cancellation cannot be lost behind a full queue or a blocked context read.
/// Normal watcher teardown awaits the cleanup acknowledgement. Drop only
/// requests cleanup, so input-path cancellation never waits for native I/O.
pub(in super::super) struct Worker {
    commands: mpsc::SyncSender<Command>,
    leases: HashMap<HidppSessionId, Arc<Lease>>,
    admission: Admission,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    completion: watch::Receiver<WorkerCompletion>,
}

impl Worker {
    #[cfg(target_os = "macos")]
    pub(in super::super) fn spawn(
        outputs: super::super::super::GestureOutputs,
    ) -> io::Result<Self> {
        Self::spawn_with(NativeEnvironment(outputs))
    }

    fn spawn_with(mut environment: impl Environment) -> io::Result<Self> {
        let (commands, receiver) = mpsc::sync_channel(CAPACITY);
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let (completed, completion) = watch::channel(WorkerCompletion::Running);
        let thread = thread::Builder::new()
            .name("openlogi-native-zoom".into())
            .spawn(move || {
                run(&receiver, &worker_shutdown, &mut environment);
                drop(environment);
                completed.send_replace(WorkerCompletion::Cleaned);
            })?;
        Ok(Self {
            commands,
            leases: HashMap::new(),
            admission: Admission::Accepting,
            shutdown,
            thread: Some(thread),
            completion,
        })
    }

    pub(in super::super) fn motion(&mut self, session: &HidppSessionId, input: Input) {
        if self.admission != Admission::Accepting {
            return;
        }
        if input.foreground_app.is_none() {
            self.cancel(session);
            return;
        }
        let compatible = self.leases.get(session).is_some_and(|lease| {
            lease.valid()
                && lease.configuration == input.configuration
                && lease.pointer == input.pointer
                && lease.foreground_app == input.foreground_app
        });
        if !compatible {
            self.cancel(session);
            self.leases.insert(
                session.clone(),
                Arc::new(Lease {
                    configuration: input.configuration.clone(),
                    pointer: input.pointer,
                    foreground_app: input.foreground_app.clone(),
                    valid: AtomicBool::new(true),
                }),
            );
        }
        let Some(lease) = self.leases.get(session).cloned() else {
            return;
        };
        self.enqueue(Command::Motion(Report {
            session: session.clone(),
            lease,
            input,
        }));
    }

    pub(in super::super) fn stop(&mut self, session: &HidppSessionId, now: Instant) {
        if let Some(lease) = self.leases.get(session).cloned() {
            self.enqueue(Command::Stop {
                session: session.clone(),
                lease,
                now,
            });
        }
    }

    pub(in super::super) fn cancel(&mut self, session: &HidppSessionId) {
        if let Some(lease) = self.leases.remove(session) {
            lease.revoke();
            self.wake();
        }
    }

    fn cancel_all(&mut self) {
        for lease in self.leases.drain().map(|(_, lease)| lease) {
            lease.revoke();
        }
        self.wake();
    }

    pub(in super::super) fn pause(&mut self) {
        if self.admission != Admission::Shutdown {
            self.admission = Admission::Paused;
        }
        self.cancel_all();
    }

    pub(in super::super) fn resume(&mut self) {
        if self.admission == Admission::Paused {
            self.admission = Admission::Accepting;
        }
    }

    pub(in super::super) fn shutdown(&mut self) {
        self.admission = Admission::Shutdown;
        self.shutdown.store(true, Ordering::Release);
        self.cancel_all();
    }

    /// Revoke immediately, then return an independent completion wait so the
    /// manager can restore firmware without borrowing this producer. Repeated
    /// waits observe the same barrier, even if an earlier wait was dropped.
    pub(in super::super) fn shutdown_and_wait(
        &mut self,
    ) -> impl std::future::Future<Output = ()> + use<> {
        self.shutdown();
        wait_for_cleanup(self.completion.clone())
    }

    fn wake(&self) {
        let _ = self.commands.try_send(Command::Wake);
    }

    fn enqueue(&mut self, command: Command) {
        match self.commands.try_send(command) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                // Never replay old motion after a slow native lookup. An
                // overflow resets continuity; subsequent reports start fresh.
                self.cancel_all();
                tracing::warn!("native Zoom queue full — pending gestures cancelled");
            }
            Err(mpsc::TrySendError::Disconnected(_)) => self.shutdown(),
        }
    }
}

async fn wait_for_cleanup(mut completion: watch::Receiver<WorkerCompletion>) {
    if completion
        .wait_for(|state| *state == WorkerCompletion::Cleaned)
        .await
        .is_err()
    {
        tracing::warn!("native Zoom worker ended without confirming terminal cleanup");
        // No independent timeout: replacement cannot report a clean stop, and
        // the process lifecycle alone bounds an unconfirmed terminal exit.
        std::future::pending::<()>().await;
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
        // Native calls cannot be interrupted. The worker retains its output
        // owners until they return and performs terminal cleanup before exit.
        drop(self.thread.take());
    }
}

struct State<O: Output> {
    sessions: Sessions<O>,
    // Capability identity, not a second active-output ledger. Even a failed
    // begin retains its lease until the manager replaces or revokes it.
    leases: HashMap<HidppSessionId, Arc<Lease>>,
}

impl<O: Output> Default for State<O> {
    fn default() -> Self {
        Self {
            sessions: Sessions::default(),
            leases: HashMap::new(),
        }
    }
}

impl<O: Output> State<O> {
    fn synchronize(&mut self) {
        self.leases.retain(|session, lease| {
            if lease.valid() {
                return true;
            }
            self.sessions.cancel(session);
            false
        });
    }

    fn report(
        &mut self,
        report: &Report,
        shutdown: &AtomicBool,
        environment: &mut impl Environment<Gesture = O>,
    ) {
        if !report.lease.valid() || shutdown.load(Ordering::Acquire) {
            return;
        }
        if self
            .leases
            .get(&report.session)
            .is_some_and(|lease| !Arc::ptr_eq(lease, &report.lease))
        {
            self.sessions.cancel(&report.session);
        }
        self.leases
            .insert(report.session.clone(), Arc::clone(&report.lease));
        if !environment.permitted() {
            self.sessions.cancel_all();
            return;
        }
        if environment.observed_app() != report.input.foreground_app {
            self.sessions.cancel(&report.session);
            return;
        }
        let context = environment.context(report.input.pointer);
        // The lookup may have waited for AppKit/WindowServer while the manager
        // cancelled, paused, replaced the plan or shut down. Its result has no
        // authority until this checkpoint. Calls already committed below may
        // finish, but invalidation prevents every subsequent queued call.
        self.synchronize();
        if !report.lease.valid() || shutdown.load(Ordering::Acquire) {
            return;
        }
        if !environment.permitted() {
            self.sessions.cancel_all();
            return;
        }
        if environment
            .now()
            .saturating_duration_since(report.input.now)
            >= IDLE
        {
            self.sessions.cancel(&report.session);
            return;
        }
        let Some(context) = context else {
            self.sessions.cancel(&report.session);
            return;
        };
        // Focused-policy reports retain the observer identity at admission;
        // neither queue delay nor a live lookup may retarget them to a new app.
        if report.input.foreground_app.as_deref() != Some(context.app_id.as_str())
            || environment.observed_app() != report.input.foreground_app
        {
            self.sessions.cancel(&report.session);
            return;
        }
        self.sessions.motion(
            &report.session,
            Motion {
                action: report.input.configuration.action(report.input.rotation),
                sensitivity: report.input.configuration.sensitivity,
                magnitude: report.input.magnitude,
                target: Target {
                    app_id: context.app_id,
                    pointer: report.input.pointer,
                    foreground_pid: context.foreground_pid,
                },
                current_pointer: context.pointer,
                context: context.output,
                now: report.input.now,
                admission: WheelAdmission::Active,
            },
        );
        if report.input.stopped {
            self.sessions.stop(&report.session, report.input.now);
        }
        self.synchronize();
    }
}

fn run<E: Environment>(
    receiver: &mpsc::Receiver<Command>,
    shutdown: &AtomicBool,
    environment: &mut E,
) {
    let mut state = State::<E::Gesture>::default();
    while !shutdown.load(Ordering::Acquire) {
        state.synchronize();
        let timeout = state.sessions.deadline().map_or(CANCEL_POLL, |deadline| {
            deadline
                .saturating_duration_since(environment.now())
                .min(CANCEL_POLL)
        });
        match receiver.recv_timeout(timeout) {
            Ok(Command::Motion(report)) => state.report(&report, shutdown, environment),
            Ok(Command::Stop {
                session,
                lease,
                now,
            }) => {
                if lease.valid()
                    && state
                        .leases
                        .get(&session)
                        .is_some_and(|current| Arc::ptr_eq(current, &lease))
                {
                    state.sessions.stop(&session, now);
                }
            }
            Ok(Command::Wake) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        #[cfg(test)]
        environment.processed();
        state.synchronize();
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        if state
            .sessions
            .deadline()
            .is_some_and(|deadline| environment.now() >= deadline)
        {
            if !environment.permitted() {
                state.sessions.cancel_all();
                continue;
            }
            let (app, pid) = environment.foreground();
            state.synchronize();
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            let observed = environment.observed_app();
            let app = app.filter(|app| Some(app) == observed.as_ref());
            state.sessions.expire(
                environment.now(),
                app.as_deref(),
                environment.permitted(),
                pid,
            );
        }
    }
    state.sessions.shutdown();
}

#[cfg(test)]
mod tests;
