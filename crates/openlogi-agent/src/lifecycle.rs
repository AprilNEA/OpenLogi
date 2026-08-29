//! The agent's lifecycle as an explicit state machine.
//!
//! Every process start walks the same ladder, and each state is a type:
//!
//! ```text
//! startup::bootstrap ──► Booted ──gate──► Wanted ──arm──► Armed ──run──► exit
//!         │                 │                                    │
//!         └─ init failed    └─ dormant start nobody wanted       └─ signal / uninstall
//! ```
//!
//! The moves are the type protection for three contracts: the uninstall
//! receiver travels inside the states (gate consumes it first, then the run
//! loop — no third consumer can exist), the demand channel dies at
//! [`Wanted::arm`], and arming without settling the dormancy question is
//! unrepresentable — `arm` exists only on [`Wanted`], whose sole producer is
//! the gate. The gate *waits* only on macOS, where the sunk launch-at-login
//! switch makes an unwanted login start possible; Windows and Linux only ever
//! start wanted, so their gate passes unconditionally.

use std::sync::Arc;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "macos")]
use std::time::Duration;

use futures::StreamExt as _;
use openlogi_agent_core::event_monitor::EventMonitor;
use openlogi_agent_core::observable::ObservableState;
use openlogi_agent_core::orchestrator::{Orchestrator, SharedRuntime};
use openlogi_agent_core::runtime::hook;
use openlogi_agent_core::watchers::foreground_app::ForegroundUpdate;
use openlogi_agent_core::watchers::inventory::InventoryEvent;
use openlogi_core::config::Config;
use openlogi_hook::Hook;
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

/// Walk the whole lifecycle: bootstrap, gate, arm, run. This is the async
/// core's entry point; `main` only decides which thread it runs on.
pub(crate) async fn run(
    config: Config,
    #[cfg(any(target_os = "macos", target_os = "windows"))] resume_pending: Arc<AtomicBool>,
    uninstalled: UnboundedReceiver<()>,
    #[cfg(target_os = "macos")] armed_tx: std::sync::mpsc::Sender<()>,
) {
    // Reconcile the agent's launch-at-login autostart and clear the legacy GUI
    // LaunchAgent, before `config` moves into the orchestrator.
    autostart::reconcile(config.app_settings.launch_at_login);

    let Some(booted) = Booted::bootstrap(
        config,
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        resume_pending,
        uninstalled,
        #[cfg(target_os = "macos")]
        armed_tx,
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
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    resume_pending: Arc<AtomicBool>,
}

impl Booted {
    async fn bootstrap(
        config: Config,
        #[cfg(any(target_os = "macos", target_os = "windows"))] resume_pending: Arc<AtomicBool>,
        uninstalled: UnboundedReceiver<()>,
        #[cfg(target_os = "macos")] armed_tx: std::sync::mpsc::Sender<()>,
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
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            resume_pending,
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
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            resume_pending,
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
        // Closing the channel turns post-arming declarations into no-ops in
        // the server's `declare_client` handler.
        drop(demand);
        Armed {
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
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            resume_pending,
        }
    }
}

/// The armed agent — everything the select loop folds events into.
struct Armed {
    orchestrator: Arc<Mutex<Orchestrator>>,
    shared: SharedRuntime,
    observable: Arc<ObservableState>,
    event_monitor: Arc<EventMonitor>,
    inputs: InputServices,
    ring_haptics: server::RingHapticPlayer,
    signals: ShutdownSignals,
    uninstalled: UnboundedReceiver<()>,
    /// The OS hook, installed once Accessibility is granted and dropped on
    /// revoke (dropping the handle stops its thread).
    hook: Option<Hook>,
    capture_mouse_events: bool,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    resume_pending: Arc<AtomicBool>,
}

impl Armed {
    /// Start the watcher fleets, then drain every control-plane source until
    /// told to leave (low-frequency by contract — [`startup::WatcherEvent`]).
    async fn run(mut self) {
        #[cfg(target_os = "macos")]
        request_input_monitoring().await;

        // HID++ watchers need no Accessibility — start them up front.
        startup::spawn_hidpp_watchers(&self.shared, &self.inputs);
        let mut watchers = startup::spawn_state_watchers(&self.shared);

        info!("openlogi-agent started");
        loop {
            tokio::select! {
                Some(event) = watchers.next() => self.apply_watcher(event).await,
                Some(device_key) = self.inputs.triggers.recv() => {
                    self.begin_action_ring(device_key.as_deref()).await;
                }
                Some(device_key) = self.inputs.app_profile_cycles.recv() => {
                    let changed = self.orchestrator.lock().await.cycle_app_profile(&device_key);
                    if changed {
                        self.inputs.dispatcher.cancel_all_buttons();
                    }
                }
                () = self.signals.recv() => self.shut_down("shutdown signal"),
                // Uninstalled while running — leave through the same door so
                // the event tap goes with us (#807).
                Some(()) = self.uninstalled.recv() => self.shut_down("the app was uninstalled"),
                else => break,
            }
        }
    }

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
    async fn apply_watcher(&mut self, event: startup::WatcherEvent) {
        use startup::{Watcher, WatcherEvent};

        // Inventory and foreground-app samples make this a periodic health
        // reconciliation without another timer in the control-plane loop.
        #[cfg(target_os = "windows")]
        self.apply_hook_health().await;

        match event {
            WatcherEvent::Inventory(event) => self.apply_inventory(event).await,
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

    /// Fold one inventory-watcher event into the orchestrator.
    async fn apply_inventory(&self, event: InventoryEvent) {
        match event {
            InventoryEvent::Snapshot {
                inventories,
                standalone,
                hid_open_failures,
            } => {
                let mut orchestrator = self.orchestrator.lock().await;
                // Native suspend/resume notifications cover the sleeps the
                // polling gap misses; consume the coalesced signal at the
                // point that can replay it.
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                if self.resume_pending.swap(false, Ordering::Relaxed) {
                    info!("native resume notification — replaying volatile settings");
                    orchestrator.reapply_volatile_on_next_refresh();
                }
                orchestrator.refresh_inventory(&inventories, &standalone, hid_open_failures);
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
        if !granted {
            self.stop_hook();
        }
        if granted && self.hook.is_none() {
            self.hook = self.start_hook();
        }
        self.orchestrator
            .lock()
            .await
            .set_os_mouse_hook_available(self.hook.is_some());
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
        )
    }

    /// Stop the hook so no new edge can race the lifecycle cancellation.
    fn stop_hook(&mut self) {
        self.hook = None;
        self.inputs.dispatcher.cancel_hook_buttons();
        self.inputs.scroll_input.cancel_hooks();
    }

    fn shut_down(&mut self, reason: &str) -> ! {
        shutdown::release_hook_and_exit(self.hook.take(), &mut self.inputs, reason)
    }
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
