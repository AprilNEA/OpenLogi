//! Which application is frontmost on Linux, and the observer that reports
//! when it changes.
//!
//! No Linux display server answers this the same way, so one
//! [`FrontmostSource`] is selected per process: the wlroots foreign-toplevel
//! protocol or the GNOME Shell extension on Wayland, X11 everywhere else, and a
//! source that always answers `None` when nothing is reachable. That one
//! source moves between the idle snapshot read and the observer worker; its
//! transport is never duplicated.

use std::collections::HashMap;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::Duration;

use thiserror::Error;
use tracing::{debug, error};

use crate::ForegroundApp;

mod gnome_shell;
mod lifecycle;
mod stop;
mod wlr_foreign_toplevel;
pub(super) mod x11;

use lifecycle::{ActiveSnapshot, Feed, ObserverLifecycle, Published, Start};
use stop::{PollResult, StopControl, StopToken, poll_source_or_stop, stop_pair};
pub(super) use x11::X11Source;

/// A backend that reports which application is currently frontmost.
///
/// Implementations are display-server / desktop specific. The string returned
/// by `frontmost_app_id` is compared against per-app profile keys by exact
/// match (`openlogi_core::Config::effective_bindings`), so its exact form
/// matters and is backend-specific. The X11 and gnome-shell backends both
/// return the `WM_CLASS` class component (e.g. "Firefox"); the wlr backend
/// returns the xdg-shell `app_id` (e.g. "org.mozilla.firefox"). These two
/// namespaces do not map onto each other by any simple string rule, so a
/// per-app profile created under wlroots will not match under GNOME/X11 and
/// vice versa. This is a known limitation: reconciling it needs a canonical-id
/// scheme or per-profile aliases rather than naive normalization, and is
/// deliberately out of scope for the backends themselves. One selected source
/// moves between the idle snapshot path and the observer worker; its transport
/// is never duplicated or independently selected.
trait FrontmostSource: Send {
    /// Opaque identifier of the frontmost application, or `None` when there is
    /// no frontmost window or it cannot be read.
    fn frontmost_app_id(&mut self) -> Option<String>;

    /// Subscribe natively, publish the subscribe-before-snapshot result and
    /// subsequent changes, and run until `stop` is requested. The source is
    /// returned so the idle snapshot path regains the same selected backend.
    fn observe(self: Box<Self>, stop: StopToken, publish: PublishAppId)
    -> Box<dyn FrontmostSource>;

    /// Short backend identifier, for diagnostics / logging only.
    fn name(&self) -> &'static str;
}

type PublishAppId = Arc<dyn Fn(Option<String>) + Send + Sync>;

/// Delay between reconnect attempts after an already-selected native backend
/// loses its transport. Normal delivery blocks on native events with no tick.
pub(super) const RECONNECT_DELAY: Duration = Duration::from_secs(2);

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Fallback used when no backend is available (e.g. a pure Wayland session
/// before any Wayland backend lands). Always reports `None`, so per-app
/// profile switching simply no-ops rather than erroring.
struct NullSource;

impl FrontmostSource for NullSource {
    fn frontmost_app_id(&mut self) -> Option<String> {
        None
    }

    fn observe(
        self: Box<Self>,
        stop: StopToken,
        publish: PublishAppId,
    ) -> Box<dyn FrontmostSource> {
        publish(None);
        stop.wait();
        self
    }

    fn name(&self) -> &'static str {
        "null"
    }
}

/// Coarse classification of the graphical session, used to order the frontmost
/// backend candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionKind {
    X11,
    Wayland,
    Unknown,
}

/// Classify the session from the environment. `XDG_SESSION_TYPE` is
/// authoritative when set to `x11` or `wayland`; otherwise fall back to the
/// presence of `WAYLAND_DISPLAY` / `DISPLAY`.
pub(super) fn detect_session_kind() -> SessionKind {
    if let Ok(kind) = std::env::var("XDG_SESSION_TYPE") {
        match kind.as_str() {
            "wayland" => return SessionKind::Wayland,
            "x11" => return SessionKind::X11,
            _ => {}
        }
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        SessionKind::Wayland
    } else if std::env::var_os("DISPLAY").is_some() {
        SessionKind::X11
    } else {
        SessionKind::Unknown
    }
}

/// A backend constructor: returns the backend if it can initialize on this
/// system, or `None` to fall through to the next candidate.
type Candidate = fn() -> Option<Box<dyn FrontmostSource>>;

fn x11_candidate() -> Option<Box<dyn FrontmostSource>> {
    X11Source::connect().map(|s| Box::new(s) as Box<dyn FrontmostSource>)
}

/// Wayland-native frontmost backends, in priority order: the wlroots
/// foreign-toplevel protocol (sway, Hyprland, river, …) and the GNOME Shell
/// D-Bus extension (Mutter). AT-SPI remains a future fallback. Compositors that
/// support none of these fall through to the X11/XWayland path (which resolves
/// XWayland windows, `None` for native Wayland apps).
fn wayland_candidates() -> Vec<Candidate> {
    vec![wlr_foreign_toplevel::candidate, gnome_shell::candidate]
}

/// Pick the frontmost backend for this session, trying each candidate in order
/// and keeping the first that initializes. Called once, lazily, per process.
fn detect_frontmost_source() -> Box<dyn FrontmostSource> {
    let session = detect_session_kind();
    debug!("frontmost: session kind = {session:?}");

    let mut candidates: Vec<Candidate> = match session {
        SessionKind::Wayland => wayland_candidates(),
        SessionKind::X11 | SessionKind::Unknown => Vec::new(),
    };
    // X11 / XWayland: the primary path on an X11 session and the universal
    // fallback everywhere else.
    candidates.push(x11_candidate);

    for candidate in candidates {
        if let Some(source) = candidate() {
            debug!("frontmost: using '{}' backend", source.name());
            // On Wayland, landing on the X11 backend means no native Wayland
            // frontmost source was available, so native Wayland windows will
            // report None (only XWayland windows resolve). Hint at the fix.
            if session == SessionKind::Wayland && source.name() == "x11" {
                debug!(
                    "frontmost: on Wayland but using the X11/XWayland backend; \
                     native Wayland windows will report None. Install the OpenLogi \
                     GNOME Shell extension (GNOME) or use a wlroots compositor."
                );
            }
            return source;
        }
    }

    debug!("frontmost: no usable backend; frontmost_app_id will return None");
    Box::new(NullSource)
}

struct DeliveryState {
    active: bool,
    version: Option<u64>,
}

struct Subscriber {
    callback: Box<dyn Fn(Option<ForegroundApp>) + Send + Sync>,
    delivery: Mutex<DeliveryState>,
}

impl Subscriber {
    fn new(callback: impl Fn(Option<ForegroundApp>) + Send + Sync + 'static) -> Self {
        Self {
            callback: Box::new(callback),
            delivery: Mutex::new(DeliveryState {
                active: true,
                version: None,
            }),
        }
    }

    fn deliver(&self, version: u64, app: Option<ForegroundApp>) {
        let mut delivery = lock_unpoisoned(&self.delivery);
        if !delivery.active || delivery.version.is_some_and(|seen| seen >= version) {
            return;
        }
        delivery.version = Some(version);
        if catch_unwind(AssertUnwindSafe(|| (self.callback)(app))).is_err() {
            error!("foreground-application callback panicked");
        }
    }

    fn deactivate(&self) {
        let mut delivery = lock_unpoisoned(&self.delivery);
        delivery.active = false;
    }
}

struct PublicationState {
    feed: Feed,
    subscribers: HashMap<u64, Arc<Subscriber>>,
}

struct Publication {
    state: Mutex<PublicationState>,
}

impl Publication {
    fn new() -> Self {
        Self {
            state: Mutex::new(PublicationState {
                feed: Feed::new(),
                subscribers: HashMap::new(),
            }),
        }
    }

    fn begin(&self) {
        lock_unpoisoned(&self.state).feed.begin();
    }

    fn finish(&self) {
        lock_unpoisoned(&self.state).feed.finish();
    }

    /// Add `subscriber` and return the change it must replay: the active
    /// session's latest, if any.
    fn subscribe(&self, id: u64, subscriber: &Arc<Subscriber>) -> Option<Published> {
        let mut state = lock_unpoisoned(&self.state);
        state.subscribers.insert(id, Arc::clone(subscriber));
        state.feed.latest().cloned()
    }

    fn unsubscribe(&self, id: u64) {
        let subscriber = lock_unpoisoned(&self.state).subscribers.remove(&id);
        if let Some(subscriber) = subscriber {
            // Wait for an in-flight callback and make any publisher that cloned
            // this subscriber before removal observe it as inactive.
            subscriber.deactivate();
        }
    }

    fn is_empty(&self) -> bool {
        lock_unpoisoned(&self.state).subscribers.is_empty()
    }

    fn publish(&self, app_id: Option<String>) {
        let (published, subscribers) = {
            let mut state = lock_unpoisoned(&self.state);
            let Some(published) = state
                .feed
                .publish(app_id.map(ForegroundApp::unnamed))
                .cloned()
            else {
                return;
            };
            (
                published,
                state.subscribers.values().cloned().collect::<Vec<_>>(),
            )
        };
        for subscriber in subscribers {
            subscriber.deliver(published.version, published.app.clone());
        }
    }

    fn active_snapshot(&self) -> ActiveSnapshot {
        lock_unpoisoned(&self.state).feed.snapshot()
    }
}

type ObserverWorkerStart = (Box<dyn FrontmostSource>, StopToken, PublishAppId);

/// A worker thread, spawned and blocked until it is handed its source.
struct PendingWorker {
    start: mpsc::SyncSender<ObserverWorkerStart>,
    stop: StopControl,
    stop_token: StopToken,
    thread: thread::JoinHandle<Box<dyn FrontmostSource>>,
}

impl PendingWorker {
    /// Spawn a worker thread that waits for its source.
    fn spawn() -> Result<Self, ForegroundApplicationObserverError> {
        let (stop, stop_token) =
            stop_pair().map_err(ForegroundApplicationObserverError::StopPipe)?;
        let (start, start_rx) = mpsc::sync_channel::<ObserverWorkerStart>(0);
        let thread = thread::Builder::new()
            .name("openlogi-frontmost".into())
            .spawn(move || match start_rx.recv() {
                Ok((source, stop_token, publish)) => source.observe(stop_token, publish),
                Err(_) => Box::new(NullSource),
            })
            .map_err(ForegroundApplicationObserverError::ThreadSpawn)?;
        Ok(Self {
            start,
            stop,
            stop_token,
            thread,
        })
    }

    /// Hand the worker `source`, which it observes with until stopped. A
    /// worker that exited before taking it is joined, and gives the source
    /// back.
    fn hand_off(
        self,
        source: Box<dyn FrontmostSource>,
        publish: PublishAppId,
    ) -> Result<ObserverWorker, Box<dyn FrontmostSource>> {
        match self.start.send((source, self.stop_token, publish)) {
            Ok(()) => Ok(ObserverWorker {
                stop: self.stop,
                thread: self.thread,
            }),
            Err(mpsc::SendError((source, _, _))) => {
                // Only a thread that dropped its receiver refuses the send,
                // so it is already returning, with nothing worth keeping.
                let _ = self.thread.join();
                Err(source)
            }
        }
    }
}

/// A worker observing with the source it was handed.
struct ObserverWorker {
    stop: StopControl,
    thread: thread::JoinHandle<Box<dyn FrontmostSource>>,
}

impl ObserverWorker {
    /// Wake the worker and join it, taking back its source. A worker that
    /// panicked lost its source, so a freshly detected one takes its place.
    fn shut_down(self) -> Box<dyn FrontmostSource> {
        self.stop.request();
        self.thread.join().unwrap_or_else(|panic| {
            error!("foreground-application worker panicked on shutdown: {panic:?}");
            detect_frontmost_source()
        })
    }
}

#[derive(Debug, Error)]
pub(crate) enum ForegroundApplicationObserverError {
    #[error("could not create the Linux foreground observer stop pipe: {0}")]
    StopPipe(io::Error),
    #[error("could not spawn the Linux foreground observer thread: {0}")]
    ThreadSpawn(io::Error),
    #[error("the Linux foreground observer thread stopped during startup")]
    WorkerStoppedDuringStartup,
}

struct FrontmostRuntime {
    lifecycle: ObserverLifecycle<Box<dyn FrontmostSource>, ObserverWorker>,
    next_subscriber_id: u64,
}

/// The process's one frontmost source and the observers sharing it.
///
/// The publication is active exactly while the lifecycle observes or stops:
/// [`Self::launch`] begins it before the worker holds the source, and
/// [`Self::remove_observer`] finishes it as the source comes back, both under
/// the runtime lock. A snapshot read that finds the publication inactive
/// under that lock, once no worker is stopping, therefore finds the source
/// idle.
pub(super) struct FrontmostController {
    runtime: Mutex<FrontmostRuntime>,
    worker_stopped: Condvar,
    publication: Arc<Publication>,
}

impl FrontmostController {
    fn new(source: Box<dyn FrontmostSource>) -> Arc<Self> {
        Arc::new(Self {
            runtime: Mutex::new(FrontmostRuntime {
                lifecycle: ObserverLifecycle::new(source),
                next_subscriber_id: 0,
            }),
            worker_stopped: Condvar::new(),
            publication: Arc::new(Publication::new()),
        })
    }

    /// Lock the runtime once no worker is stopping.
    fn settled_runtime(&self) -> MutexGuard<'_, FrontmostRuntime> {
        self.worker_stopped
            .wait_while(lock_unpoisoned(&self.runtime), |runtime| {
                runtime.lifecycle.is_stopping()
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn frontmost_app(&self) -> Option<ForegroundApp> {
        match self.publication.active_snapshot() {
            ActiveSnapshot::Active(app) => return app,
            ActiveSnapshot::Idle => {}
        }

        let mut runtime = self.settled_runtime();
        match self.publication.active_snapshot() {
            ActiveSnapshot::Active(app) => app,
            ActiveSnapshot::Idle => runtime
                .lifecycle
                .idle_source()
                .and_then(|source| source.frontmost_app_id())
                .map(ForegroundApp::unnamed),
        }
    }

    fn watch(
        self: &Arc<Self>,
        callback: impl Fn(Option<ForegroundApp>) + Send + Sync + 'static,
    ) -> Result<ForegroundApplicationObserver, ForegroundApplicationObserverError> {
        let subscriber = Arc::new(Subscriber::new(callback));
        let mut runtime = self.settled_runtime();

        let id = runtime.next_subscriber_id;
        runtime.next_subscriber_id += 1;
        // The first observer subscribes before its worker can publish, so it
        // replays nothing; a later one replays the session's latest change.
        let initial = self.publication.subscribe(id, &subscriber);
        match runtime.lifecycle.start(|source| self.launch(source)) {
            Start::Busy | Start::Started => {}
            Start::Failed(error) => {
                self.publication.unsubscribe(id);
                return Err(error);
            }
        }
        drop(runtime);

        // Do not call user code while holding the controller lock. If the
        // worker published a newer version first, Subscriber::deliver ignores
        // this older snapshot rather than reversing callback order.
        if let Some(Published { version, app }) = initial {
            subscriber.deliver(version, app);
        }

        Ok(ForegroundApplicationObserver {
            controller: Arc::clone(self),
            subscriber_id: id,
        })
    }

    /// Start a worker with the idle `source`: spawn it, open the publication
    /// before it can publish, and hand the source over. A launch that fails
    /// leaves the publication closed and gives the source back. Neither path
    /// unwinds, as [`ObserverLifecycle::start`] requires.
    fn launch(
        &self,
        source: Box<dyn FrontmostSource>,
    ) -> Result<ObserverWorker, (Box<dyn FrontmostSource>, ForegroundApplicationObserverError)>
    {
        let pending = match PendingWorker::spawn() {
            Ok(pending) => pending,
            Err(error) => return Err((source, error)),
        };
        self.publication.begin();
        let publication = Arc::clone(&self.publication);
        let publish: PublishAppId = Arc::new(move |app| publication.publish(app));
        pending.hand_off(source, publish).map_err(|source| {
            self.publication.finish();
            (
                source,
                ForegroundApplicationObserverError::WorkerStoppedDuringStartup,
            )
        })
    }

    fn check_health(&self) -> Result<(), &'static str> {
        let runtime = lock_unpoisoned(&self.runtime);
        if runtime
            .lifecycle
            .worker()
            .is_some_and(|worker| worker.thread.is_finished())
        {
            Err("Linux foreground observer worker stopped")
        } else {
            Ok(())
        }
    }

    fn remove_observer(&self, id: u64) {
        self.publication.unsubscribe(id);

        let worker = {
            let mut runtime = self.settled_runtime();
            if !self.publication.is_empty() {
                return;
            }
            let Some(worker) = runtime.lifecycle.stop() else {
                return;
            };
            worker
        };

        let source = worker.shut_down();

        let mut runtime = lock_unpoisoned(&self.runtime);
        runtime.lifecycle.stopped(source);
        self.publication.finish();
        self.worker_stopped.notify_all();
    }
}

pub(super) static FRONTMOST: LazyLock<Arc<FrontmostController>> =
    LazyLock::new(|| FrontmostController::new(detect_frontmost_source()));

/// Linux-native foreground-application observer ownership.
///
/// Dropping the final handle synchronously unsubscribes from the selected
/// backend, wakes and joins its worker, and returns the source to snapshot use.
#[must_use]
pub(crate) struct ForegroundApplicationObserver {
    controller: Arc<FrontmostController>,
    subscriber_id: u64,
}

impl ForegroundApplicationObserver {
    pub(crate) fn check_health(&self) -> Result<(), &'static str> {
        self.controller.check_health()
    }
}

impl Drop for ForegroundApplicationObserver {
    fn drop(&mut self) {
        self.controller.remove_observer(self.subscriber_id);
    }
}

/// Observe foreground-application changes from the selected native backend.
pub(crate) fn watch_frontmost_application_activations(
    callback: impl Fn(Option<ForegroundApp>) + Send + Sync + 'static,
) -> Result<ForegroundApplicationObserver, ForegroundApplicationObserverError> {
    FRONTMOST.watch(callback)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    use super::*;

    struct FakeFrontmostSource {
        stopped: Arc<AtomicBool>,
    }

    impl FrontmostSource for FakeFrontmostSource {
        fn frontmost_app_id(&mut self) -> Option<String> {
            Some("org.example.App".into())
        }

        fn observe(
            self: Box<Self>,
            stop: StopToken,
            publish: PublishAppId,
        ) -> Box<dyn FrontmostSource> {
            publish(Some("org.example.App".into()));
            stop.wait();
            self.stopped.store(true, Ordering::Release);
            self
        }

        fn name(&self) -> &'static str {
            "fake"
        }
    }

    #[test]
    fn foreground_publication_suppresses_duplicates_and_contains_panics() {
        let publication = Publication::new();
        publication.begin();
        let panic_once = Arc::new(AtomicBool::new(true));
        let callback_panic_once = Arc::clone(&panic_once);
        let panicking = Arc::new(Subscriber::new(move |_| {
            assert!(
                !callback_panic_once.swap(false, Ordering::AcqRel),
                "contained callback panic"
            );
        }));
        assert!(publication.subscribe(1, &panicking).is_none());

        let received = Arc::new(Mutex::new(Vec::new()));
        let callback_received = Arc::clone(&received);
        let recording_subscriber = Arc::new(Subscriber::new(move |app| {
            lock_unpoisoned(&callback_received).push(app.map(|app| app.id));
        }));
        assert!(publication.subscribe(2, &recording_subscriber).is_none());

        publication.publish(Some("one".into()));
        publication.publish(Some("one".into()));
        publication.publish(Some("two".into()));
        publication.publish(None);

        assert_eq!(
            *lock_unpoisoned(&received),
            vec![Some("one".into()), Some("two".into()), None]
        );
        assert!(!panic_once.load(Ordering::Acquire));
    }

    #[test]
    fn foreground_observer_drop_stops_joins_and_returns_source() {
        let stopped = Arc::new(AtomicBool::new(false));
        let controller = FrontmostController::new(Box::new(FakeFrontmostSource {
            stopped: Arc::clone(&stopped),
        }));
        let (tx, rx) = mpsc::channel();
        let observer = controller
            .watch(move |app| {
                tx.send(app).expect("test receiver remains alive");
            })
            .expect("observer starts");

        let initial = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("observer publishes its initial snapshot");
        assert_eq!(
            initial.map(|app| app.id).as_deref(),
            Some("org.example.App")
        );

        drop(observer);
        assert!(stopped.load(Ordering::Acquire));
        assert_eq!(
            controller.frontmost_app().map(|app| app.id).as_deref(),
            Some("org.example.App")
        );
    }

    #[test]
    fn a_later_observer_replays_the_latest_change_and_only_the_last_one_stops_the_worker() {
        let stopped = Arc::new(AtomicBool::new(false));
        let controller = FrontmostController::new(Box::new(FakeFrontmostSource {
            stopped: Arc::clone(&stopped),
        }));
        let (first_tx, first_rx) = mpsc::channel();
        let first = controller
            .watch(move |app| {
                first_tx.send(app).expect("test receiver remains alive");
            })
            .expect("first observer starts the worker");
        first_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("the worker publishes its initial snapshot");

        let (second_tx, second_rx) = mpsc::channel();
        let second = controller
            .watch(move |app| {
                second_tx.send(app).expect("test receiver remains alive");
            })
            .expect("second observer joins the worker");
        // The fake publishes once, so only the replay can have delivered it,
        // and `watch` replays before it returns.
        assert_eq!(
            second_rx
                .try_recv()
                .expect("the second observer replays the latest change")
                .map(|app| app.id)
                .as_deref(),
            Some("org.example.App")
        );

        drop(first);
        assert!(!stopped.load(Ordering::Acquire), "an observer remains");
        second.check_health().expect("the worker still runs");

        drop(second);
        assert!(stopped.load(Ordering::Acquire));
    }

    #[test]
    fn unsupported_foreground_source_publishes_none_and_stops_cleanly() {
        let controller = FrontmostController::new(Box::new(NullSource));
        let (tx, rx) = mpsc::channel();
        let observer = controller
            .watch(move |app| {
                tx.send(app).expect("test receiver remains alive");
            })
            .expect("observer starts");

        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1))
                .expect("null source publishes an initial snapshot"),
            None
        );
        drop(observer);
        assert_eq!(controller.frontmost_app(), None);
    }

    #[test]
    fn foreground_unsubscribe_waits_for_inflight_callback() {
        let publication = Arc::new(Publication::new());
        publication.begin();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let callback_release = Arc::clone(&release);
        let (entered_tx, entered_rx) = mpsc::channel();
        let subscriber = Arc::new(Subscriber::new(move |_| {
            entered_tx.send(()).expect("test receiver remains alive");
            let (lock, changed) = &*callback_release;
            let guard = lock_unpoisoned(lock);
            drop(
                changed
                    .wait_while(guard, |released| !*released)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
        }));
        assert!(publication.subscribe(1, &subscriber).is_none());

        let publisher = Arc::clone(&publication);
        let publish_thread = thread::spawn(move || publisher.publish(Some("one".into())));
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("callback starts");

        let unsubscriber = Arc::clone(&publication);
        let (unsubscribed_tx, unsubscribed_rx) = mpsc::channel();
        let unsubscribe_thread = thread::spawn(move || {
            unsubscriber.unsubscribe(1);
            unsubscribed_tx
                .send(())
                .expect("test receiver remains alive");
        });
        assert!(
            unsubscribed_rx
                .recv_timeout(Duration::from_millis(25))
                .is_err(),
            "unsubscribe returned while the callback was in flight"
        );

        let (lock, changed) = &*release;
        *lock_unpoisoned(lock) = true;
        changed.notify_all();
        unsubscribed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("unsubscribe returns after callback completion");
        publish_thread.join().expect("publisher thread exits");
        unsubscribe_thread.join().expect("unsubscribe thread exits");
    }
}
