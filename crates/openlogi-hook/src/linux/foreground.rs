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
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{
    Arc, Condvar, LazyLock, Mutex, MutexGuard,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;
use tracing::{debug, error};
use x11rb::connection::Connection as _;
use x11rb::properties::WmClass;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, EventMask, Window,
};
use x11rb::rust_connection::RustConnection;

use super::{create_pipe, signal_pipe};
use crate::ForegroundApp;

mod gnome_shell;
mod wlr_foreign_toplevel;

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

struct StopState {
    requested: AtomicBool,
    wait_lock: Mutex<()>,
    changed: Condvar,
}

impl StopState {
    fn new() -> Self {
        Self {
            requested: AtomicBool::new(false),
            wait_lock: Mutex::new(()),
            changed: Condvar::new(),
        }
    }

    fn request(&self) {
        if self.requested.swap(true, Ordering::AcqRel) {
            return;
        }
        let _guard = lock_unpoisoned(&self.wait_lock);
        self.changed.notify_all();
    }

    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    fn wait(&self) {
        let guard = lock_unpoisoned(&self.wait_lock);
        drop(
            self.changed
                .wait_while(guard, |()| !self.is_requested())
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }

    fn wait_timeout(&self, timeout: Duration) -> bool {
        if self.is_requested() {
            return true;
        }
        let guard = lock_unpoisoned(&self.wait_lock);
        let (_guard, _) = self
            .changed
            .wait_timeout_while(guard, timeout, |()| !self.is_requested())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.is_requested()
    }
}

struct StopControl {
    state: Arc<StopState>,
    wake: OwnedFd,
}

impl StopControl {
    fn request(&self) {
        self.state.request();
        signal_pipe(&self.wake);
    }
}

pub(super) struct StopToken {
    state: Arc<StopState>,
    wake: OwnedFd,
}

impl StopToken {
    pub(super) fn is_requested(&self) -> bool {
        self.state.is_requested()
    }

    pub(super) fn wait(&self) {
        self.state.wait();
    }

    pub(super) fn wait_timeout(&self, timeout: Duration) -> bool {
        self.state.wait_timeout(timeout)
    }

    pub(super) fn wake_fd(&self) -> RawFd {
        self.wake.as_raw_fd()
    }

    fn state(&self) -> Arc<StopState> {
        Arc::clone(&self.state)
    }
}

fn stop_pair() -> io::Result<(StopControl, StopToken)> {
    let (read, write) = create_pipe()?;
    let state = Arc::new(StopState::new());
    Ok((
        StopControl {
            state: Arc::clone(&state),
            wake: write,
        },
        StopToken { state, wake: read },
    ))
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum PollResult {
    SourceReady,
    StopRequested,
    DeadlineReached,
    Error,
}

/// Wait for a native display fd, observer teardown, or an optional deadline.
/// A deadline is used only for targeted reconnect/drain work; steady-state
/// event delivery passes `None` and blocks indefinitely.
pub(super) fn poll_source_or_stop(
    source_fd: Option<RawFd>,
    stop_fd: RawFd,
    deadline: Option<Instant>,
) -> PollResult {
    let mut fds = [
        libc::pollfd {
            fd: stop_fd,
            events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
            revents: 0,
        },
        libc::pollfd {
            fd: source_fd.unwrap_or(-1),
            events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
            revents: 0,
        },
    ];

    loop {
        let timeout = deadline.map_or(-1, |deadline| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                0
            } else {
                i32::try_from(remaining.as_millis().max(1).min(i32::MAX as u128))
                    .unwrap_or(i32::MAX)
            }
        });
        // SAFETY: `fds` is a live two-element pollfd array for the whole call;
        // poll writes only each element's `revents` field.
        let result = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) };
        if result > 0 {
            if fds[0].revents != 0 {
                return PollResult::StopRequested;
            }
            if fds[1].revents != 0 {
                return PollResult::SourceReady;
            }
            continue;
        }
        if result == 0 {
            return PollResult::DeadlineReached;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            debug!("frontmost: native event poll failed: {error}");
            return PollResult::Error;
        }
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Frontmost backend backed by X11 `_NET_ACTIVE_WINDOW` + `WM_CLASS`.
///
/// Works on an X11 session, and on a Wayland session for XWayland windows;
/// native Wayland windows are invisible through this path and yield `None`.
pub(in crate::linux) struct X11Source {
    pub(in crate::linux) conn: RustConnection,
    pub(in crate::linux) root: Window,
    net_active_window: Atom,
}

impl X11Source {
    /// Connect to the X server and resolve the `_NET_ACTIVE_WINDOW` atom.
    /// Returns `None` when no X display is reachable (a Wayland session without
    /// XWayland, or `$DISPLAY` unset).
    pub(in crate::linux) fn connect() -> Option<Self> {
        let (conn, screen_num) = RustConnection::connect(None)
            .map_err(|e| debug!("X11 not available, frontmost will return None: {e}"))
            .ok()?;
        let root = conn.setup().roots[screen_num].root;
        let net_active_window = conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .ok()?
            .reply()
            .ok()?
            .atom;
        Some(Self {
            conn,
            root,
            net_active_window,
        })
    }

    fn subscribe(&self) -> bool {
        self.set_event_mask(EventMask::PROPERTY_CHANGE)
            .map_err(|e| debug!("frontmost: failed to subscribe to X11 root changes: {e}"))
            .is_ok()
    }

    fn unsubscribe(&self) {
        if let Err(error) = self.set_event_mask(EventMask::NO_EVENT) {
            debug!("frontmost: failed to unsubscribe from X11 root changes: {error}");
        }
    }

    fn set_event_mask(&self, event_mask: EventMask) -> Result<(), String> {
        let cookie = self
            .conn
            .change_window_attributes(
                self.root,
                &ChangeWindowAttributesAux::new().event_mask(event_mask),
            )
            .map_err(|error| error.to_string())?;
        cookie.check().map_err(|error| error.to_string())?;
        self.conn.flush().map_err(|error| error.to_string())
    }

    fn run_until_stopped_or_disconnected(
        &mut self,
        stop: &StopToken,
        publish: &PublishAppId,
    ) -> bool {
        if !self.subscribe() {
            return false;
        }

        // The event mask is installed before this read, so a focus change can
        // only be represented by the snapshot, a queued PropertyNotify, or both.
        publish(self.frontmost_app_id());

        loop {
            match poll_source_or_stop(Some(self.conn.stream().as_raw_fd()), stop.wake_fd(), None) {
                PollResult::StopRequested => {
                    self.unsubscribe();
                    return true;
                }
                PollResult::SourceReady => {}
                PollResult::DeadlineReached => continue,
                PollResult::Error => return false,
            }

            let mut active_window_changed = false;
            loop {
                match self.conn.poll_for_event() {
                    Ok(Some(Event::PropertyNotify(event)))
                        if event.window == self.root && event.atom == self.net_active_window =>
                    {
                        active_window_changed = true;
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(error) => {
                        debug!("frontmost: X11 connection lost: {error}");
                        return false;
                    }
                }
            }
            if active_window_changed {
                publish(self.frontmost_app_id());
            }
        }
    }
}

impl FrontmostSource for X11Source {
    fn frontmost_app_id(&mut self) -> Option<String> {
        // _NET_ACTIVE_WINDOW on the root window holds the focused window's XID.
        let window: Window = self
            .conn
            .get_property(
                false,
                self.root,
                self.net_active_window,
                AtomEnum::WINDOW,
                0,
                1,
            )
            .ok()?
            .reply()
            .ok()?
            .value32()?
            .next()?;
        if window == 0 {
            return None;
        }

        // WM_CLASS is instance_name\0class_name\0; the class component is more
        // stable across window instances and is what profiles should key on
        // (e.g. "Firefox", not "Navigator").
        let wm = WmClass::get(&self.conn, window)
            .ok()?
            .reply_unchecked()
            .ok()??;
        std::str::from_utf8(wm.class())
            .ok()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    }

    fn observe(
        mut self: Box<Self>,
        stop: StopToken,
        publish: PublishAppId,
    ) -> Box<dyn FrontmostSource> {
        loop {
            if self.run_until_stopped_or_disconnected(&stop, &publish) {
                return self;
            }
            publish(None);
            if stop.wait_timeout(RECONNECT_DELAY) {
                return self;
            }
            if let Some(reconnected) = Self::connect() {
                debug!("frontmost: X11 connection restored");
                *self = reconnected;
            }
        }
    }

    fn name(&self) -> &'static str {
        "x11"
    }
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
enum SessionKind {
    X11,
    Wayland,
    Unknown,
}

/// Classify the session from the environment. `XDG_SESSION_TYPE` is
/// authoritative when set to `x11` or `wayland`; otherwise fall back to the
/// presence of `WAYLAND_DISPLAY` / `DISPLAY`.
fn detect_session_kind() -> SessionKind {
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
    active: bool,
    initialized: bool,
    version: u64,
    current: Option<ForegroundApp>,
    subscribers: HashMap<u64, Arc<Subscriber>>,
}

struct Publication {
    state: Mutex<PublicationState>,
}

enum ActiveSnapshot {
    Idle,
    Active(Option<ForegroundApp>),
}

impl Publication {
    fn new() -> Self {
        Self {
            state: Mutex::new(PublicationState {
                active: false,
                initialized: false,
                version: 0,
                current: None,
                subscribers: HashMap::new(),
            }),
        }
    }

    fn begin(&self) {
        let mut state = lock_unpoisoned(&self.state);
        state.active = true;
        state.initialized = false;
        state.current = None;
    }

    fn finish(&self) {
        let mut state = lock_unpoisoned(&self.state);
        state.active = false;
        state.initialized = false;
        state.current = None;
    }

    fn subscribe(
        &self,
        id: u64,
        subscriber: &Arc<Subscriber>,
    ) -> Option<(u64, Option<ForegroundApp>)> {
        let mut state = lock_unpoisoned(&self.state);
        state.subscribers.insert(id, Arc::clone(subscriber));
        state
            .initialized
            .then(|| (state.version, state.current.clone()))
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
        let app = app_id.map(ForegroundApp::unnamed);
        let (version, subscribers) = {
            let mut state = lock_unpoisoned(&self.state);
            if state.initialized && state.current == app {
                return;
            }
            state.initialized = true;
            state.version += 1;
            state.current.clone_from(&app);
            (
                state.version,
                state.subscribers.values().cloned().collect::<Vec<_>>(),
            )
        };
        for subscriber in subscribers {
            subscriber.deliver(version, app.clone());
        }
    }

    fn active_snapshot(&self) -> ActiveSnapshot {
        let state = lock_unpoisoned(&self.state);
        if state.active {
            ActiveSnapshot::Active(state.current.clone())
        } else {
            ActiveSnapshot::Idle
        }
    }
}

struct ObserverWorker {
    stop: StopControl,
    thread: thread::JoinHandle<Box<dyn FrontmostSource>>,
}

type ObserverWorkerStart = (Box<dyn FrontmostSource>, StopToken, PublishAppId);

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
    source: Option<Box<dyn FrontmostSource>>,
    worker: Option<ObserverWorker>,
    stopping: bool,
    next_subscriber_id: u64,
}

pub(super) struct FrontmostController {
    runtime: Mutex<FrontmostRuntime>,
    worker_stopped: Condvar,
    publication: Arc<Publication>,
}

impl FrontmostController {
    fn new(source: Box<dyn FrontmostSource>) -> Arc<Self> {
        Arc::new(Self {
            runtime: Mutex::new(FrontmostRuntime {
                source: Some(source),
                worker: None,
                stopping: false,
                next_subscriber_id: 0,
            }),
            worker_stopped: Condvar::new(),
            publication: Arc::new(Publication::new()),
        })
    }

    pub(super) fn frontmost_app(&self) -> Option<ForegroundApp> {
        match self.publication.active_snapshot() {
            ActiveSnapshot::Active(app) => return app,
            ActiveSnapshot::Idle => {}
        }

        let mut runtime = lock_unpoisoned(&self.runtime);
        while runtime.stopping {
            runtime = self
                .worker_stopped
                .wait(runtime)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        match self.publication.active_snapshot() {
            ActiveSnapshot::Active(app) => app,
            ActiveSnapshot::Idle => runtime
                .source
                .as_mut()
                .and_then(|source| source.frontmost_app_id())
                .map(ForegroundApp::unnamed),
        }
    }

    fn watch(
        self: &Arc<Self>,
        callback: impl Fn(Option<ForegroundApp>) + Send + Sync + 'static,
    ) -> Result<ForegroundApplicationObserver, ForegroundApplicationObserverError> {
        let subscriber = Arc::new(Subscriber::new(callback));
        let mut runtime = lock_unpoisoned(&self.runtime);
        while runtime.stopping {
            runtime = self
                .worker_stopped
                .wait(runtime)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }

        let id = runtime.next_subscriber_id;
        runtime.next_subscriber_id += 1;
        let initial = if runtime.worker.is_none() {
            let (stop, stop_token) =
                stop_pair().map_err(ForegroundApplicationObserverError::StopPipe)?;
            let (start_tx, start_rx) = mpsc::sync_channel::<ObserverWorkerStart>(0);
            let thread = thread::Builder::new()
                .name("openlogi-frontmost".into())
                .spawn(move || match start_rx.recv() {
                    Ok((source, stop_token, publish)) => source.observe(stop_token, publish),
                    Err(_) => Box::new(NullSource),
                })
                .map_err(ForegroundApplicationObserverError::ThreadSpawn)?;

            self.publication.begin();
            let initial = self.publication.subscribe(id, &subscriber);
            let source = runtime
                .source
                .take()
                .unwrap_or_else(|| Box::new(NullSource));
            let publication = Arc::clone(&self.publication);
            let publish: PublishAppId = Arc::new(move |app| publication.publish(app));
            if let Err(error) = start_tx.send((source, stop_token, publish)) {
                let (source, _, _) = error.0;
                runtime.source = Some(source);
                self.publication.unsubscribe(id);
                self.publication.finish();
                drop(runtime);
                let _ = thread.join();
                return Err(ForegroundApplicationObserverError::WorkerStoppedDuringStartup);
            }
            runtime.worker = Some(ObserverWorker { stop, thread });
            initial
        } else {
            self.publication.subscribe(id, &subscriber)
        };
        drop(runtime);

        // Do not call user code while holding the controller lock. If the
        // worker published a newer version first, Subscriber::deliver ignores
        // this older snapshot rather than reversing callback order.
        if let Some((version, app)) = initial {
            subscriber.deliver(version, app);
        }

        Ok(ForegroundApplicationObserver {
            controller: Arc::clone(self),
            subscriber_id: id,
        })
    }

    fn check_health(&self) -> Result<(), &'static str> {
        let runtime = lock_unpoisoned(&self.runtime);
        if runtime
            .worker
            .as_ref()
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
            let mut runtime = lock_unpoisoned(&self.runtime);
            while runtime.stopping {
                runtime = self
                    .worker_stopped
                    .wait(runtime)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if !self.publication.is_empty() {
                return;
            }
            let Some(worker) = runtime.worker.take() else {
                return;
            };
            runtime.stopping = true;
            worker
        };

        worker.stop.request();
        let source = worker.thread.join().unwrap_or_else(|panic| {
            error!("foreground-application worker panicked on shutdown: {panic:?}");
            detect_frontmost_source()
        });

        let mut runtime = lock_unpoisoned(&self.runtime);
        runtime.source = Some(source);
        self.publication.finish();
        runtime.stopping = false;
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
