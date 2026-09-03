//! The agent's lifecycle as an explicit state machine.
//!
//! Every process start walks the same ladder, and each state is a type:
//!
//! ```text
//! startup::bootstrap ──► Booted ──gate──► Wanted ──arm──► Armed ──► Running ──► exit
//!         │                 │                                         │
//!         └─ init failed    └─ dormant start nobody wanted            └─ signal / uninstall
//! ```
//!
//! The moves are the type protection for these lifecycle contracts: the
//! uninstall receiver travels inside the states (gate consumes it first, then
//! the run loop — no third consumer can exist), the demand channel dies at
//! [`Wanted::arm`], and arming without settling the dormancy question is
//! unrepresentable — `arm` exists only on [`Wanted`], whose sole producer is
//! the gate. Moving `Armed` into `Running` also hands the single-consumer resume
//! stream to inventory exactly once. The gate *waits* only on macOS, where the
//! sunk launch-at-login switch makes an unwanted login start possible; Windows
//! and Linux only ever start wanted, so their gate passes unconditionally.

use std::sync::Arc;
use std::time::Duration;

#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "macos")]
use std::sync::{Mutex as StdMutex, PoisonError, TryLockError};

use futures::StreamExt as _;
use openlogi_agent_core::event_monitor::EventMonitor;
use openlogi_agent_core::observable::ObservableState;
use openlogi_agent_core::orchestrator::{Orchestrator, SharedRuntime};
use openlogi_agent_core::runtime::hook;
use openlogi_agent_core::watchers::foreground_app::ForegroundUpdate;
use openlogi_agent_core::watchers::inventory::{InventoryEvent, InventoryRefresh};
use openlogi_core::config::Config;
use openlogi_hook::Hook;
#[cfg(target_os = "macos")]
use openlogi_hook::HookStopHandle;
use tokio::sync::Mutex;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{debug, info, warn};

#[cfg(target_os = "macos")]
use openlogi_ipc::ClientKind;

#[cfg(target_os = "macos")]
use crate::binary_watch;
use crate::shutdown::{self, ShutdownSignals};
use crate::startup::{self, Core, InputServices};
use crate::{autostart, overlay, server};

/// How long a dormant agent waits before leaving — generous next to the
/// seconds a kickstarting GUI needs, and the window costs only an idle
/// process that has opened no device and prompted for nothing.
#[cfg(target_os = "macos")]
const DORMANT_DEADLINE: Duration = Duration::from_secs(60);

/// A failed event-tap install is normally transient (for example while the
/// session's Accessibility service is settling). Keep trying while the hook
/// is still wanted instead of waiting for another permission edge.
const HOOK_RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// An ordered device-I/O edge consumed by the lifecycle loop.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeviceIoTransition {
    /// The login session became inactive; release its global event tap.
    Suspended,
    /// The login session became active; reconcile the event tap prerequisites.
    Resumed,
}

#[cfg(target_os = "macos")]
impl DeviceIoTransition {
    const fn from_allowed(allowed: bool) -> Self {
        if allowed {
            Self::Resumed
        } else {
            Self::Suspended
        }
    }
}

/// Non-blocking bridge from the AppKit session callback to the hook owner.
///
/// The callback cannot wait for the lifecycle task to receive its watch
/// notification: that task may be inside native hook setup or an async lock.
/// A pending bit covers the interval before a newly-created hook registers its
/// handle; once registered, the request wakes the tap thread directly.
#[cfg(target_os = "macos")]
#[derive(Default)]
pub(crate) struct HookStopRequest {
    current: StdMutex<Option<HookStopHandle>>,
    pending: AtomicBool,
}

#[cfg(target_os = "macos")]
impl HookStopRequest {
    fn prepare_install(&self) {
        self.pending.store(false, Ordering::Release);
    }

    fn install(&self, handle: &HookStopHandle) {
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        *current = Some(handle.clone());
        if self.pending.swap(false, Ordering::AcqRel) {
            handle.request_stop();
        }
        drop(current);

        // A callback that lost the try_lock race while the slot was being
        // installed leaves `pending` set. Consume that edge after unlocking;
        // callbacks arriving later can see and call the installed handle.
        if self.pending.swap(false, Ordering::AcqRel) {
            handle.request_stop();
        }
    }

    fn clear(&self) {
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        *current = None;
    }

    pub(crate) fn request_stop(&self) {
        self.pending.store(true, Ordering::Release);
        match self.current.try_lock() {
            Ok(current) => self.request_locked(current.as_ref()),
            Err(TryLockError::Poisoned(error)) => {
                let current = error.into_inner();
                self.request_locked(current.as_ref());
            }
            Err(TryLockError::WouldBlock) => {}
        }
    }

    fn request_locked(&self, current: Option<&HookStopHandle>) {
        if let Some(handle) = current
            && self.pending.swap(false, Ordering::AcqRel)
        {
            handle.request_stop();
        }
    }
}

/// Walk the whole lifecycle: bootstrap, gate, arm, run. This is the async
/// core's entry point; `main` only decides which thread it runs on.
pub(crate) async fn run(
    config: Config,
    uninstalled: UnboundedReceiver<()>,
    #[cfg(target_os = "macos")] armed_tx: std::sync::mpsc::Sender<()>,
    #[cfg(target_os = "macos")] hook_stop: Arc<HookStopRequest>,
    device_io_gate: openlogi_hid::DeviceIoGate,
) {
    // Reconcile the agent's launch-at-login autostart and clear the legacy GUI
    // LaunchAgent, before `config` moves into the orchestrator.
    autostart::reconcile(config.app_settings.launch_at_login);

    let Some(booted) = Booted::bootstrap(
        config,
        uninstalled,
        #[cfg(target_os = "macos")]
        armed_tx,
        #[cfg(target_os = "macos")]
        hook_stop,
        device_io_gate,
    )
    .await
    else {
        return;
    };
    #[cfg(target_os = "macos")]
    let Some(wanted) = booted.gate().await else {
        return;
    };
    #[cfg(not(target_os = "macos"))]
    let wanted = booted.gate();
    wanted.arm().run().await;
}

/// A bootstrapped, not-yet-armed agent: the IPC socket is serving, nothing
/// user-visible has happened. The only ways out are [`Self::gate`] and being
/// dropped (exit).
struct Booted {
    core: Core,
    signals: ShutdownSignals,
    uninstalled: UnboundedReceiver<()>,
    /// The hook kill-switch, startup-only on purpose: flipping it requires
    /// an agent restart, which the config docs state.
    capture_mouse_events: bool,
    #[cfg(target_os = "macos")]
    launch_at_login: bool,
    /// Releases the main thread's tray loop once the agent arms.
    #[cfg(target_os = "macos")]
    armed_tx: std::sync::mpsc::Sender<()>,
    #[cfg(target_os = "macos")]
    hook_stop: Arc<HookStopRequest>,
    device_io_gate: openlogi_hid::DeviceIoGate,
}

impl Booted {
    async fn bootstrap(
        config: Config,
        uninstalled: UnboundedReceiver<()>,
        #[cfg(target_os = "macos")] armed_tx: std::sync::mpsc::Sender<()>,
        #[cfg(target_os = "macos")] hook_stop: Arc<HookStopRequest>,
        device_io_gate: openlogi_hid::DeviceIoGate,
    ) -> Option<Self> {
        // Read before `config` moves into the orchestrator.
        let capture_mouse_events = config.app_settings.capture_mouse_events;
        #[cfg(target_os = "macos")]
        let launch_at_login = config.app_settings.launch_at_login;
        let core = startup::bootstrap(config).await?;
        Some(Self {
            core,
            signals: ShutdownSignals::install(),
            uninstalled,
            capture_mouse_events,
            #[cfg(target_os = "macos")]
            launch_at_login,
            #[cfg(target_os = "macos")]
            armed_tx,
            #[cfg(target_os = "macos")]
            hook_stop,
            device_io_gate,
        })
    }

    /// The dormancy gate. The service plist always carries the login trigger
    /// (`SuccessfulExit` implies `RunAtLoad`), so preference-off plus no
    /// client in sight means "launchd ran us at login the user opted out
    /// of" — wait briefly, then leave with the `exit(0)` launchd will not
    /// respawn. Demand is a [`ClientKind::Gui`] declaration, not a mere
    /// connection: other clients are served without waking anything, and the
    /// takeover probe never declares at all.
    #[cfg(target_os = "macos")]
    async fn gate(mut self) -> Option<Wanted> {
        if self.launch_at_login {
            return Some(Wanted(self));
        }
        info!("launch_at_login is off — dormant until a client demands arming");
        // The deadline is absolute: a served-but-not-arming client does not
        // buy the dormant agent more time.
        let deadline = tokio::time::sleep(DORMANT_DEADLINE);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                Some(kind) = self.core.demand.recv() => match kind {
                    ClientKind::Gui => {
                        info!("GUI connected — arming");
                        return Some(Wanted(self));
                    }
                    kind => info!(client = ?kind, "served while dormant — not arming"),
                },
                () = &mut deadline => {
                    info!("no arming demand — exiting until wanted");
                    return None;
                }
                () = self.signals.recv() => {
                    info!("shutdown signal while dormant — exiting");
                    return None;
                }
                Some(()) = self.uninstalled.recv() => {
                    info!("uninstalled while dormant — exiting");
                    return None;
                }
            }
        }
    }

    /// Windows and Linux have no login trigger to second-guess: every start
    /// was asked for, so the gate passes unconditionally.
    #[cfg(not(target_os = "macos"))]
    fn gate(self) -> Wanted {
        Wanted(self)
    }
}

/// A booted agent whose dormancy question is settled: somebody wants it
/// running. [`Booted::gate`] is the only producer, so an agent that never
/// consulted the gate cannot arm.
struct Wanted(Booted);

impl Wanted {
    /// The arming point: the tray may show, the overlay may start,
    /// permissions may prompt, devices may open.
    fn arm(self) -> Armed {
        let Booted {
            core,
            signals,
            uninstalled,
            capture_mouse_events,
            #[cfg(target_os = "macos")]
            armed_tx,
            #[cfg(target_os = "macos")]
            hook_stop,
            device_io_gate,
            ..
        } = self.0;
        #[cfg(target_os = "macos")]
        let _ = armed_tx.send(());
        overlay::spawn();
        prompt_missing_accessibility(capture_mouse_events);

        let Core {
            orchestrator,
            shared,
            observable,
            event_monitor,
            inputs,
            ring_haptics,
            demand,
        } = core;
        let accessibility_granted =
            observable.read(|snapshot| snapshot.status.accessibility_granted);
        // Closing the channel turns post-arming declarations into no-ops in
        // the server's `declare_client` handler.
        drop(demand);
        Armed {
            running: Running {
                orchestrator,
                shared,
                observable,
                event_monitor,
                inputs,
                ring_haptics,
                signals,
                uninstalled,
                hook: None,
                capture_mouse_events,
                accessibility_granted,
                #[cfg(target_os = "macos")]
                hook_stop,
                device_io_gate,
            },
        }
    }
}

/// An armed agent ready to start its watcher fleets.
struct Armed {
    running: Running,
}

/// The live agent state into which the select loop folds events.
/// Separate from [`Armed`] so watcher startup and the steady-state event loop
/// remain distinct lifecycle phases.
struct Running {
    orchestrator: Arc<Mutex<Orchestrator>>,
    shared: SharedRuntime,
    observable: Arc<ObservableState>,
    event_monitor: Arc<EventMonitor>,
    inputs: InputServices,
    ring_haptics: server::RingHapticPlayer,
    signals: ShutdownSignals,
    uninstalled: UnboundedReceiver<()>,
    /// The OS hook, installed once Accessibility is granted and dropped on
    /// revoke or session inactivation (dropping the handle stops its thread).
    hook: Option<Hook>,
    capture_mouse_events: bool,
    accessibility_granted: bool,
    #[cfg(target_os = "macos")]
    hook_stop: Arc<HookStopRequest>,
    device_io_gate: openlogi_hid::DeviceIoGate,
}

impl Armed {
    /// Start the watcher fleets, then drain every control-plane source until
    /// told to leave (low-frequency by contract — [`startup::WatcherEvent`]).
    async fn run(self) {
        let Self { mut running } = self;
        #[cfg(target_os = "macos")]
        request_input_monitoring().await;
        let mut device_io_gate = running.device_io_gate.clone();
        let mut hook_retry = tokio::time::interval(HOOK_RETRY_INTERVAL);
        hook_retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // HID++ watchers need no Accessibility — start them up front.
        startup::spawn_hidpp_watchers(&running.shared, &running.inputs);
        let (mut watchers, inventory_refresh) = startup::spawn_state_watchers(&running.shared);

        info!("openlogi-agent started");
        loop {
            tokio::select! {
                Some(event) = watchers.next() => {
                    running.apply_watcher(event, &inventory_refresh).await;
                }
                device_io = device_io_gate.changed() => {
                    #[cfg(target_os = "macos")]
                    {
                        match device_io {
                            Some(allowed) => running
                                .apply_device_io(DeviceIoTransition::from_allowed(allowed))
                                .await,
                            None => running.shut_down("device I/O lifecycle ended"),
                        }
                    }
                    #[cfg(not(target_os = "macos"))]
                    if device_io.is_none() {
                        break;
                    }
                }
                _ = hook_retry.tick(), if running.should_retry_hook() => {
                    running.retry_hook().await;
                }
                Some(device_key) = running.inputs.triggers.recv() => {
                    running.begin_action_ring(device_key.as_deref()).await;
                }
                () = running.signals.recv() => running.shut_down("shutdown signal"),
                // Uninstalled while running — leave through the same door so
                // the event tap goes with us (#807).
                Some(()) = running.uninstalled.recv() => running.shut_down("the app was uninstalled"),
                else => break,
            }
        }
    }
}

impl Running {
    /// Retire a terminal Windows hook worker and publish that input capture is
    /// no longer installed. The native callbacks have already been cleared,
    /// so the interval before this check remains pass-through rather than
    /// suppressing input without a consumer.
    #[cfg(target_os = "windows")]
    async fn apply_hook_health(&mut self) {
        let Some(hook) = self.hook.as_ref() else {
            return;
        };
        if hook.is_running() {
            return;
        }
        warn!("Windows hook worker exited — marking input capture unavailable");
        self.stop_hook();
        self.orchestrator
            .lock()
            .await
            .set_os_mouse_hook_available(false);
        self.observable
            .set_accessibility_and_hook(Hook::has_accessibility(), false);
    }

    /// Fold one watcher event into the agent's state.
    async fn apply_watcher(
        &mut self,
        event: startup::WatcherEvent,
        inventory_refresh: &InventoryRefresh,
    ) {
        use startup::{Watcher, WatcherEvent};

        // Inventory and foreground-app samples make this a health
        // reconciliation without another timer in the control-plane loop.
        #[cfg(target_os = "windows")]
        self.apply_hook_health().await;

        match event {
            WatcherEvent::Inventory(event) => {
                self.apply_inventory(event, inventory_refresh).await;
            }
            WatcherEvent::Camera(active) => {
                self.orchestrator.lock().await.set_camera_active(active);
            }
            WatcherEvent::App(app) => self.apply_foreground(app).await,
            WatcherEvent::Accessibility(granted) => self.apply_accessibility(granted).await,
            WatcherEvent::InputMonitoring(granted) => {
                self.observable.set_input_monitoring_granted(granted);
            }
            // Watcher thread death — without a snapshot the GUI would scan
            // forever.
            WatcherEvent::Lost(Watcher::Inventory) => {
                warn!("inventory watcher channel closed — marking enumeration unavailable");
                self.orchestrator.lock().await.mark_inventory_unavailable();
            }
            WatcherEvent::Lost(Watcher::Camera) => {
                #[cfg(target_os = "macos")]
                warn!("camera watcher channel closed — disabling camera automation updates");
            }
            WatcherEvent::Lost(source) => debug!(?source, "state watcher channel closed"),
        }
    }

    /// Keep the global macOS event tap owned only by the active login session.
    /// The HID gate already protects device I/O during sleep/session changes,
    /// but an event tap is a separate machine-wide input path: leaving it live
    /// in an inactive user's agent lets that agent suppress the active user's
    /// physical wheel and post the replacement into the wrong session.
    #[cfg(target_os = "macos")]
    async fn apply_device_io(&mut self, transition: DeviceIoTransition) {
        match transition {
            DeviceIoTransition::Resumed => {
                self.apply_accessibility(self.accessibility_granted).await;
            }
            DeviceIoTransition::Suspended => {
                self.stop_hook();
                self.orchestrator
                    .lock()
                    .await
                    .set_os_mouse_hook_available(false);
                self.observable
                    .set_accessibility_and_hook(self.accessibility_granted, false);
                info!("inactive session — OS input hook released");
            }
        }
    }

    /// Whether a missing macOS hook still has all prerequisites and should be
    /// retried. The Accessibility watcher reports only stable permission
    /// changes, so a transient install failure needs this independent retry
    /// path to recover without another session or permission transition.
    fn should_retry_hook(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            hook_should_retry(
                hook_should_be_installed(
                    self.capture_mouse_events,
                    self.accessibility_granted,
                    self.device_io_gate.allows_io(),
                ),
                self.hook.as_ref().is_some_and(Hook::is_running),
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    /// Retry a missing hook using the last stable Accessibility observation.
    /// A fresh native probe can transiently return `false` while the permission
    /// service settles; feeding that sample into `apply_accessibility` would
    /// poison the cached state and disable this retry path.
    async fn retry_hook(&mut self) {
        #[cfg(target_os = "macos")]
        {
            let accessibility_granted = self.accessibility_granted;
            self.apply_accessibility(accessibility_granted).await;
        }
    }

    /// Fold one inventory-watcher event into the orchestrator.
    async fn apply_inventory(&self, event: InventoryEvent, refresh: &InventoryRefresh) {
        match event {
            InventoryEvent::Snapshot {
                inventories,
                standalone,
                hid_open_failures,
            } => {
                let mut orchestrator = self.orchestrator.lock().await;
                orchestrator.refresh_inventory(&inventories, &standalone, hid_open_failures);
                let confirm_settings = orchestrator.needs_reapply_confirmation();
                drop(orchestrator);
                if confirm_settings {
                    refresh.request_settings_confirmation();
                }
            }
            InventoryEvent::Unavailable => {
                self.orchestrator.lock().await.mark_inventory_unavailable();
            }
            // Devices likely power-cycled during the sleep; the next snapshot
            // re-applies their volatile settings (#189).
            InventoryEvent::SystemWake => {
                self.orchestrator
                    .lock()
                    .await
                    .reapply_volatile_on_next_refresh();
            }
        }
    }

    /// Publish one foreground-app change and cancel button lifecycles whose
    /// bindings were resolved against the previous app profile.
    async fn apply_foreground(&self, app: ForegroundUpdate) {
        if self.orchestrator.lock().await.set_current_app(app) {
            self.inputs.dispatcher.cancel_all_buttons();
        }
    }

    async fn begin_action_ring(&self, device_key: Option<&str>) {
        // A second trigger press while the ring is showing closes it.
        if self.inputs.ring.dismiss_active() {
            return;
        }
        if let Some(session) = self
            .orchestrator
            .lock()
            .await
            .action_ring_session(device_key)
        {
            // Re-arm the firmware haptic engine first: power transitions can
            // clear it, after which plays are accepted without feedback.
            self.ring_haptics.arm(session.haptic_route.clone());
            self.inputs.ring.begin(session);
        }
    }

    /// Fold one Accessibility-grant change into the hook, then publish the
    /// permission and the hook state it produced as one generation — no
    /// observation can claim the hook is installed without the permission it
    /// requires.
    async fn apply_accessibility(&mut self, granted: bool) {
        // Acquire the publication lock before changing the native hook. The
        // final device-I/O check below must be followed only by synchronous
        // publication; otherwise a suspend can arrive while this future is
        // waiting for the lock and leave a newly installed tap armed.
        let orchestrator = Arc::clone(&self.orchestrator);
        let mut orchestrator = orchestrator.lock().await;

        self.accessibility_granted = granted;
        let should_install = hook_should_be_installed(
            self.capture_mouse_events,
            granted,
            self.device_io_gate.allows_io(),
        );
        let hook_stopped = self.hook.as_ref().is_some_and(|hook| !hook.is_running());
        if !should_install || hook_stopped {
            self.stop_hook();
        }
        if should_install && self.hook.is_none() {
            #[cfg(target_os = "macos")]
            self.hook_stop.prepare_install();
            self.hook = self.start_hook();
        }
        // The session callback can publish a suspend while the synchronous
        // native install is in progress. Never leave a newly created global
        // tap behind once the gate has closed.
        if should_install && !self.device_io_gate.allows_io() {
            self.stop_hook();
        }
        orchestrator.set_os_mouse_hook_available(self.hook.is_some());
        self.observable
            .set_accessibility_and_hook(granted, self.hook.is_some());
    }

    /// Install the OS mouse hook, or say why it stays off.
    fn start_hook(&self) -> Option<Hook> {
        if !self.capture_mouse_events {
            info!(
                "OS mouse hook disabled by app_settings.capture_mouse_events — \
                 button remapping is off"
            );
            return None;
        }
        info!("accessibility granted — installing OS mouse hook");
        hook::start(
            self.shared.hook_maps.clone(),
            self.shared.keyboard_bindings.clone(),
            self.inputs.dispatcher.clone(),
            self.inputs.scroll_input.clone(),
            Arc::clone(&self.event_monitor),
            self.shared.device_io.clone(),
        )
        .inspect(|hook| {
            #[cfg(target_os = "macos")]
            self.hook_stop.install(&hook.stop_handle());
        })
    }

    /// Stop the hook so no new edge can race the lifecycle cancellation.
    fn stop_hook(&mut self) {
        #[cfg(target_os = "macos")]
        self.hook_stop.clear();
        self.hook = None;
        self.inputs.dispatcher.cancel_hook_buttons();
        self.inputs.scroll_input.cancel_hooks();
    }

    fn shut_down(&mut self, reason: &str) -> ! {
        #[cfg(target_os = "macos")]
        self.hook_stop.clear();
        shutdown::release_hook_and_exit(self.hook.take(), &mut self.inputs, reason)
    }
}

/// The event tap is a global input filter on macOS. It must be active only
/// when the agent is configured to capture input, has Accessibility, and its
/// login session is currently allowed to use host I/O.
const fn hook_should_be_installed(
    capture_mouse_events: bool,
    accessibility_granted: bool,
    device_io_allowed: bool,
) -> bool {
    capture_mouse_events && accessibility_granted && device_io_allowed
}

/// A failed install is retryable only while the hook is still wanted and no
/// live handle exists.
const fn hook_should_retry(hook_wanted: bool, hook_running: bool) -> bool {
    hook_wanted && !hook_running
}

/// Prompt for Accessibility when the enabled mouse hook needs it.
fn prompt_missing_accessibility(capture_mouse_events: bool) {
    // With the hook disabled the agent needs no Accessibility at all, so the
    // opt-out also silences that prompt.
    if capture_mouse_events && !Hook::has_accessibility() {
        Hook::prompt_accessibility();
    }
}

/// Request Input Monitoring before starting the HID inventory on macOS.
///
/// The agent (not the GUI) owns every HID++ device open, so it must be the
/// binary the user authorizes. A newly granted permission requires a process
/// relaunch before macOS lets the agent open HID devices.
#[cfg(target_os = "macos")]
async fn request_input_monitoring() {
    // Without this, macOS never registers a decision at all:
    // `IOHIDDeviceOpen` is silently denied, the permission never appears in
    // System Settings for the user to grant, and no HID++ device is ever
    // discovered. Wait for the blocking consent dialog before starting the
    // inventory so it cannot cache the pre-grant access state.
    if !openlogi_hid::permissions::has_access() {
        let access_after_prompt = tokio::task::spawn_blocking(|| {
            openlogi_hid::permissions::request_access();
            openlogi_hid::permissions::has_access()
        })
        .await;
        match access_after_prompt {
            Ok(true) => binary_watch::relaunch_after_input_monitoring_grant(),
            Ok(false) => {}
            Err(e) => {
                warn!(error = %e, "Input Monitoring permission request task failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{hook_should_be_installed, hook_should_retry};

    #[test]
    fn hook_requires_capture_permission_and_active_session() {
        assert!(hook_should_be_installed(true, true, true));
        assert!(!hook_should_be_installed(false, true, true));
        assert!(!hook_should_be_installed(true, false, true));
        assert!(!hook_should_be_installed(true, true, false));
    }

    #[test]
    fn failed_hook_install_is_retryable_while_prerequisites_hold() {
        assert!(hook_should_retry(true, false));
        assert!(!hook_should_retry(true, true));
        assert!(!hook_should_retry(false, false));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn fast_session_transitions_replay_suspend_before_resume() {
        use super::DeviceIoTransition::{Resumed, Suspended};

        let (signal, mut gate) = openlogi_hid::device_io_channel();
        assert!(signal.suspend());
        assert!(signal.resume());

        assert_eq!(
            super::DeviceIoTransition::from_allowed(
                gate.changed()
                    .await
                    .expect("the suspended edge should be replayed"),
            ),
            Suspended
        );
        assert_eq!(
            super::DeviceIoTransition::from_allowed(
                gate.changed()
                    .await
                    .expect("the resumed edge should follow the suspension"),
            ),
            Resumed
        );
    }
}
