//! OpenLogi background agent — headless, always-on.
//!
//! Owns the CGEventTap hook and the HID++ device path (gesture capture, DPI,
//! SmartShift), serves the GUI over a Unix-socket tarpc IPC, reconciles its own
//! launchd autostart, and (macOS) hosts the menu-bar status item. The async
//! core runs on a tokio runtime; on macOS the process main thread hosts the
//! AppKit run loop the menu bar requires.

// Without this Windows runs the exe as a console app and pops a terminal
// window whenever the GUI's sibling spawn or the Run-key autostart starts the
// agent — "headless" must mean no window of any kind. Debug builds keep the
// console so logs stay visible (matching the GUI's arrangement).
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod autostart;
mod binary_watch;
mod logging;
mod overlay;
mod pairing;
#[cfg(target_os = "windows")]
mod resume_windows;
mod server;
#[cfg(target_os = "macos")]
mod status_item;
mod takeover;
#[cfg(target_os = "macos")]
mod tray;
#[cfg(target_os = "windows")]
mod tray_windows;

use std::sync::Arc;
// Only the resume-notification flag is atomic now, and that exists on the two
// platforms that have a native suspend/resume signal.
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use openlogi_agent_core::action_ring::ActionRingManager;
use openlogi_agent_core::event_monitor::EventMonitor;
use openlogi_agent_core::observable::ObservableState;
use openlogi_agent_core::orchestrator::{Orchestrator, SharedRuntime};
use openlogi_agent_core::runtime::scroll::{ScrollInputHandle, ScrollRuntime};
use openlogi_agent_core::runtime::{ActionDispatcher, ActionRuntime, hook};
use openlogi_agent_core::watchers::{self, gesture::GestureOutputs};
use openlogi_core::config::Config;
use openlogi_hook::Hook;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::server::AgentServer;

fn main() {
    logging::init();

    // Single-instance guard: the agent owns all device I/O, the CGEventTap, and
    // the IPC socket, so a second agent must never start — launchd's KeepAlive
    // racing the GUI's one-shot auto-spawn could otherwise bring up two, and the
    // loser would steal the socket and install a duplicate event tap. Held for
    // the whole process; the OS releases it on exit (crash-recovery is free).
    let _guard = match openlogi_core::single_instance::acquire("agent.lock") {
        Ok(g) => g,
        Err(openlogi_core::single_instance::InstanceError::AlreadyRunning { path }) => {
            // The holder may be a leftover from before this binary's update —
            // a pre-self-restart agent never exits on its own, and it would
            // wedge the (newer) GUI on its connecting screen forever. If it
            // provably speaks an older protocol, replace it; otherwise exit
            // as the duplicate we are.
            let Some(g) = takeover::try_replace_stale() else {
                info!(path = %path.display(), "another openlogi-agent is already running — exiting");
                return;
            };
            info!("replaced a stale agent — continuing as the new one");
            g
        }
        Err(e) => {
            warn!(error = %e, "single-instance check failed — exiting");
            return;
        }
    };

    // Watch our own executable and restart as the new image when an app update
    // replaces it — see `binary_watch`. Only the lock-holding (real) agent
    // watches, so a losing duplicate can't restart anything. The overlay is
    // spawned later, once `run` decides the agent is actually wanted — a
    // dormant agent must not bring a helper up.
    let uninstalled = binary_watch::spawn();

    let config = Config::load_or_default().unwrap_or_else(|e| {
        warn!(error = %e, "could not load config.toml; using defaults");
        Config::default()
    });

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            warn!(error = %e, "tokio runtime init failed; agent exiting");
            return;
        }
    };

    // macOS hosts the menu-bar item, which needs an NSApplication run loop on
    // the process main thread — so the async core (orchestrator, IPC, watchers,
    // hook) runs on the tokio runtime on a dedicated thread, and the main thread
    // runs AppKit. Elsewhere there is no tray, so just block on the core.
    #[cfg(target_os = "macos")]
    {
        // Read the menu-bar preference before `config` moves into the core
        // thread; the main thread hosts the tray.
        let show_in_menu_bar = config.app_settings.show_in_menu_bar;
        let app_icon = config.app_settings.app_icon;
        let resume_pending = Arc::new(AtomicBool::new(false));
        let core_resume_pending = Arc::clone(&resume_pending);
        // The tray waits for the core to declare the agent *armed*: a dormant
        // agent (launch_at_login off, started at login, no client yet) must
        // not put an icon in the menu bar only to vanish seconds later. A
        // dropped sender means the core exited without arming — fall through
        // and let the process end.
        let (armed_tx, armed_rx) = std::sync::mpsc::channel::<()>();
        if let Err(e) = std::thread::Builder::new()
            .name("openlogi-agent-core".into())
            .spawn(move || {
                runtime.block_on(run(config, core_resume_pending, uninstalled, armed_tx));
            })
        {
            warn!(error = %e, "could not spawn the agent core thread; exiting");
            return;
        }
        if armed_rx.recv().is_ok() {
            tray::run_app_loop(show_in_menu_bar, app_icon, resume_pending);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Windows hosts the notification-area icon on its own win32 thread
        // (message pump included); the async core keeps the main thread.
        #[cfg(target_os = "windows")]
        {
            tray_windows::spawn(config.app_settings.show_in_menu_bar);
            // Native resume notifications feed the same seam the macOS
            // workspace observer does: the core replays volatile settings
            // when the flag is set.
            let resume_pending = Arc::new(AtomicBool::new(false));
            resume_windows::register(Arc::clone(&resume_pending));
            runtime.block_on(run(config, resume_pending, uninstalled));
        }
        #[cfg(not(target_os = "windows"))]
        runtime.block_on(run(config, uninstalled));
    }
}

/// Start the HID++ background sessions that do not need Accessibility.
fn spawn_hidpp_watchers(shared: &SharedRuntime, inputs: &InputServices) {
    watchers::gesture::spawn(
        shared.capture_plans.clone(),
        shared.capture_channel.clone(),
        shared.receiver_access.clone(),
        GestureOutputs::new(inputs.dispatcher.clone(), inputs.scroll_input.clone()),
    );
    watchers::host_switch::spawn(
        shared.host_switch_links.clone(),
        shared.channel_pool.clone(),
        shared.receiver_access.clone(),
    );
    watchers::keyboard::spawn(
        shared.keyboard_spec.clone(),
        shared.keyboard_channel.clone(),
        shared.receiver_access.clone(),
        shared.channel_registry.clone(),
        inputs.dispatcher.clone(),
    );
}

struct InputServices {
    ring: Arc<ActionRingManager>,
    triggers: tokio::sync::mpsc::UnboundedReceiver<Option<String>>,
    dispatcher: ActionDispatcher,
    action_runtime: ActionRuntime,
    scroll_input: ScrollInputHandle,
    scroll_runtime: ScrollRuntime,
}

impl InputServices {
    fn start(shared: &SharedRuntime) -> Option<Self> {
        let ring = Arc::new(ActionRingManager::default());
        let (sender, triggers) = tokio::sync::mpsc::unbounded_channel();
        let action_runtime = match ActionRuntime::new(
            shared.dpi_cycle.clone(),
            shared.capture_channel.clone(),
            shared.channel_registry.clone(),
            shared.receiver_access.clone(),
            sender,
        ) {
            Ok(runtime) => runtime,
            Err(e) => {
                warn!(error = %e, "could not start button lifecycle worker — agent exiting");
                return None;
            }
        };
        let scroll_runtime = match ScrollRuntime::spawn(Arc::clone(&shared.scroll_preferences)) {
            Ok(runtime) => runtime,
            Err(e) => {
                warn!(error = %e, "could not start smooth-scroll worker — agent exiting");
                return None;
            }
        };
        let dispatcher = action_runtime.dispatcher();
        let scroll_input = scroll_runtime.input();
        Some(Self {
            ring,
            triggers,
            dispatcher,
            action_runtime,
            scroll_input,
            scroll_runtime,
        })
    }

    fn shutdown(&mut self) {
        self.scroll_runtime.shutdown();
        self.action_runtime.shutdown();
    }
}

/// Install the OS mouse hook now that Accessibility is granted, or say why it
/// stays off. `None` means no hook is running, which is what the observable
/// state reports either way.
fn start_hook(
    capture_mouse_events: bool,
    shared: &SharedRuntime,
    inputs: &InputServices,
    event_monitor: &Arc<EventMonitor>,
) -> Option<Hook> {
    if !capture_mouse_events {
        info!(
            "OS mouse hook disabled by app_settings.capture_mouse_events — \
             button remapping is off"
        );
        return None;
    }
    info!("accessibility granted — installing OS mouse hook");
    hook::start(
        shared.hook_maps.clone(),
        shared.keyboard_bindings.clone(),
        inputs.dispatcher.clone(),
        inputs.scroll_input.clone(),
        Arc::clone(event_monitor),
    )
}

async fn begin_action_ring(
    orchestrator: &Mutex<Orchestrator>,
    action_ring: &ActionRingManager,
    ring_haptics: &server::RingHapticPlayer,
    device_key: Option<&str>,
) {
    // A second trigger press while the ring is showing closes it.
    if action_ring.dismiss_active() {
        return;
    }
    if let Some(session) = orchestrator.lock().await.action_ring_session(device_key) {
        // Arm the firmware haptic engine before the first buzz: some power
        // transitions clear its enabled state, after which plays are accepted
        // without any physical feedback. Sequenced through the haptic worker
        // so the first hover cannot race a still-disarmed engine.
        ring_haptics.arm(session.haptic_route.clone());
        action_ring.begin(session);
    }
}

fn spawn_ipc_server(
    orchestrator: Arc<Mutex<Orchestrator>>,
    shared: &SharedRuntime,
    observable: Arc<ObservableState>,
    pairing: Arc<pairing::PairingManager>,
    event_monitor: Arc<EventMonitor>,
    inputs: &InputServices,
    connected: Arc<tokio::sync::Notify>,
) -> server::RingHapticPlayer {
    let server = AgentServer::new(
        orchestrator,
        shared.clone(),
        observable,
        pairing,
        event_monitor,
        Arc::clone(&inputs.ring),
        inputs.dispatcher.clone(),
    );
    let ring_haptics = server.ring_haptics.clone();
    tokio::spawn(server::run(server, connected));
    ring_haptics
}

/// The per-source state watchers the select loop drains, spawned at arming.
struct StateWatchers {
    inventory: tokio::sync::mpsc::UnboundedReceiver<watchers::inventory::InventoryEvent>,
    camera: tokio::sync::mpsc::UnboundedReceiver<bool>,
    app: tokio::sync::mpsc::UnboundedReceiver<watchers::foreground_app::ForegroundUpdate>,
    accessibility: tokio::sync::mpsc::UnboundedReceiver<bool>,
    input_monitoring: tokio::sync::mpsc::UnboundedReceiver<bool>,
}

fn spawn_state_watchers(shared: &SharedRuntime) -> StateWatchers {
    StateWatchers {
        inventory: watchers::inventory::spawn_with_registry(
            Duration::from_secs(2),
            shared.channel_registry.clone(),
        ),
        camera: watchers::camera::spawn(Duration::from_secs(1)),
        app: watchers::foreground_app::spawn(Duration::from_secs(1)),
        accessibility: watchers::accessibility::spawn(Duration::from_millis(1200)),
        input_monitoring: watchers::input_monitoring::spawn(Duration::from_millis(1200)),
    }
}

/// Seed the permission facts with non-prompting reads, so a client that
/// connects before the permission watchers' first tick (the IPC server starts
/// ahead of them) doesn't see a default instead of reality.
#[cfg(target_os = "macos")]
fn seed_permission_facts(observable: &ObservableState) {
    observable.set_accessibility_granted(Hook::has_accessibility());
    observable.set_input_monitoring_granted(openlogi_hid::permissions::has_access());
}

/// The dormancy gate's verdict: wait for a client to connect (the demand that
/// arms a dormant agent), giving up on the deadline, a shutdown signal, or an
/// uninstall.
#[cfg(target_os = "macos")]
async fn await_demand(
    connected: &tokio::sync::Notify,
    sigterm: &mut Option<tokio::signal::unix::Signal>,
    sigint: &mut Option<tokio::signal::unix::Signal>,
    uninstalled: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
) -> bool {
    tokio::select! {
        () = connected.notified() => true,
        () = tokio::time::sleep(DORMANT_DEADLINE) => false,
        () = shutdown_signal(sigterm, sigint) => false,
        Some(()) = uninstalled.recv() => false,
    }
}

/// A future that fires when `signal` does, or never when the handler could not
/// be installed.
#[cfg(unix)]
async fn fires(signal: &mut Option<tokio::signal::unix::Signal>) {
    match signal {
        Some(signal) => {
            signal.recv().await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// Resolves on the first signal that means *stop now*: `SIGTERM` from launchd
/// (logout, `bootout`) or from an incoming agent's takeover, `SIGINT` from a
/// dev-run Ctrl-C. Both default to killing the process where it stands, which
/// on macOS would strand an armed HID event tap in the system's tap chain.
#[cfg(unix)]
async fn shutdown_signal(
    sigterm: &mut Option<tokio::signal::unix::Signal>,
    sigint: &mut Option<tokio::signal::unix::Signal>,
) {
    tokio::select! {
        () = fires(sigterm) => {}
        () = fires(sigint) => {}
    }
}

/// No signal to wait for off unix; the arm simply never fires.
#[cfg(not(unix))]
async fn shutdown_signal(_sigterm: &mut Option<()>, _sigint: &mut Option<()>) {
    std::future::pending::<()>().await;
}

/// Install the shutdown-signal handlers, `(SIGTERM, SIGINT)`. A handler that
/// cannot be installed is `None`, which simply never fires.
#[cfg(unix)]
fn shutdown_signals() -> (
    Option<tokio::signal::unix::Signal>,
    Option<tokio::signal::unix::Signal>,
) {
    fn listen(kind: tokio::signal::unix::SignalKind) -> Option<tokio::signal::unix::Signal> {
        tokio::signal::unix::signal(kind)
            .inspect_err(|error| warn!(%error, ?kind, "could not install signal handler"))
            .ok()
    }
    (
        listen(tokio::signal::unix::SignalKind::terminate()),
        listen(tokio::signal::unix::SignalKind::interrupt()),
    )
}

#[cfg(not(unix))]
fn shutdown_signals() -> (Option<()>, Option<()>) {
    (None, None)
}

/// Release the input hook, then end the process.
///
/// Dropping the hook detaches the macOS event tap; a signal's default
/// disposition would have killed the process with the tap still armed, and so
/// would any other way of leaving that skips destructors. The agent's run loop
/// is not the process — macOS keeps the AppKit tray loop on the main thread —
/// so the exit has to be explicit.
fn release_hook_and_exit(hook: Option<Hook>, inputs: &mut InputServices, reason: &str) -> ! {
    info!(reason, "releasing the input hook and exiting");
    drop(hook);
    inputs.shutdown();
    #[expect(
        clippy::exit,
        reason = "a signalled shutdown must end the process, and the loop that observed it runs off the main thread"
    )]
    std::process::exit(0)
}

/// Stop the hook so no new edge can race the lifecycle cancellation.
fn stop_hook(hook: &mut Option<Hook>, inputs: &InputServices) {
    *hook = None;
    inputs.dispatcher.cancel_hook_buttons();
    inputs.scroll_input.cancel_hooks();
}

/// Fold one Accessibility-grant change into the hook: tear it down on a
/// revoke, install it on a grant (when capture is enabled), and publish the
/// resulting hook state — one publish for every path: revoked, installed,
/// kept, or never installed because capture is off.
fn apply_accessibility_update(
    granted: bool,
    hook: &mut Option<Hook>,
    capture_mouse_events: bool,
    observable: &ObservableState,
    shared: &SharedRuntime,
    inputs: &InputServices,
    event_monitor: &Arc<EventMonitor>,
) {
    observable.set_accessibility_granted(granted);
    if !granted {
        stop_hook(hook, inputs);
    }
    if granted && hook.is_none() {
        *hook = start_hook(capture_mouse_events, shared, inputs, event_monitor);
    }
    observable.set_hook_installed(hook.is_some());
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

/// Fold one inventory-watcher event into the orchestrator.
async fn apply_inventory_event(
    event: watchers::inventory::InventoryEvent,
    orchestrator: &Mutex<Orchestrator>,
    #[cfg(any(target_os = "macos", target_os = "windows"))] resume_pending: &AtomicBool,
) {
    match event {
        watchers::inventory::InventoryEvent::Snapshot {
            inventories,
            standalone,
            hid_open_failures,
        } => {
            let mut orchestrator = orchestrator.lock().await;
            // The portable watcher catches long sleeps from a polling gap.
            // Native notifications (macOS workspace wakes, Windows
            // suspend/resume) also cover the sleeps that gap misses; consume
            // the coalesced signal at the exact point that can replay it.
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            if resume_pending.swap(false, Ordering::Relaxed) {
                info!("native resume notification — replaying volatile settings");
                orchestrator.reapply_volatile_on_next_refresh();
            }
            orchestrator.refresh_inventory(&inventories, &standalone, hid_open_failures);
        }
        watchers::inventory::InventoryEvent::Unavailable => {
            orchestrator.lock().await.mark_inventory_unavailable();
        }
        // Devices likely power-cycled during the sleep; the next snapshot
        // re-applies their volatile settings (#189).
        watchers::inventory::InventoryEvent::SystemWake => {
            orchestrator.lock().await.reapply_volatile_on_next_refresh();
        }
    }
}

/// Publish one foreground-app change and cancel button lifecycles whose
/// bindings were resolved against the previous app profile.
async fn apply_foreground_update(
    app: watchers::foreground_app::ForegroundUpdate,
    orchestrator: &Mutex<Orchestrator>,
    dispatcher: &ActionDispatcher,
) {
    if orchestrator.lock().await.set_current_app(app) {
        dispatcher.cancel_all_buttons();
    }
}

/// How long a dormant agent waits for a client before leaving. Generous next
/// to the seconds a kickstarting GUI needs to connect; the only cost of the
/// window is an idle process that has opened no device and prompted for
/// nothing.
#[cfg(target_os = "macos")]
const DORMANT_DEADLINE: Duration = Duration::from_secs(60);

/// Everything [`run`] needs alive after [`bootstrap`]: the shared state plus
/// the running IPC server's handles.
struct Core {
    orchestrator: Arc<Mutex<Orchestrator>>,
    shared: SharedRuntime,
    observable: Arc<ObservableState>,
    event_monitor: Arc<EventMonitor>,
    inputs: InputServices,
    ring_haptics: server::RingHapticPlayer,
    /// Notified on every accepted IPC connection — the dormancy gate's
    /// demand signal.
    connected: Arc<tokio::sync::Notify>,
}

/// Build the agent's shared state and start the IPC server — everything that
/// is safe *before* arming: pure construction plus the socket bind. No
/// permission prompt, no device open, no helper spawn.
///
/// The IPC server starts here, ahead of the watchers and prompts: it is pure
/// state service over what exists so far (an empty, `Scanning` inventory),
/// binding early is what lets a dormant agent hear the demand that should
/// wake it, and a first-run Input Monitoring consent dialog no longer
/// blackholes the GUI's connect either.
async fn bootstrap(config: Config) -> Option<Core> {
    // The orchestrator is shared with the IPC server (which serves inventory /
    // reload / status) and mutated by the watcher select loop, so it lives
    // behind an async mutex. Locks are brief (a map rebuild or a clone).
    // One cell holds everything the GUI can observe. The orchestrator
    // republishes the device and config facts from its own mutators; the hook
    // facts are published by the select loop, which owns the hook.
    let observable = Arc::new(ObservableState::new(env!("CARGO_PKG_VERSION").to_string()));
    #[cfg(target_os = "macos")]
    seed_permission_facts(&observable);
    let orchestrator = Arc::new(Mutex::new(Orchestrator::new(
        config,
        Arc::clone(&observable),
    )));
    let shared = orchestrator.lock().await.shared();
    let inputs = InputServices::start(&shared)?;

    // Live event monitor: shared between the hook callback (which mirrors events
    // into it) and the IPC server (which the GUI polls). The janitor turns it
    // back off once the GUI stops polling.
    let event_monitor = Arc::new(EventMonitor::default());
    tokio::spawn(Arc::clone(&event_monitor).run_idle_janitor());

    // Pairing runs in the agent (it owns device I/O); the GUI drives it over IPC.
    let pairing = Arc::new(pairing::PairingManager::new(
        shared.clone(),
        Arc::clone(&observable),
    ));

    let connected = Arc::new(tokio::sync::Notify::new());
    let ring_haptics = spawn_ipc_server(
        Arc::clone(&orchestrator),
        &shared,
        Arc::clone(&observable),
        Arc::clone(&pairing),
        Arc::clone(&event_monitor),
        &inputs,
        Arc::clone(&connected),
    );
    Some(Core {
        orchestrator,
        shared,
        observable,
        event_monitor,
        inputs,
        ring_haptics,
        connected,
    })
}

async fn run(
    config: Config,
    #[cfg(any(target_os = "macos", target_os = "windows"))] resume_pending: Arc<AtomicBool>,
    mut uninstalled: tokio::sync::mpsc::UnboundedReceiver<()>,
    #[cfg(target_os = "macos")] armed_tx: std::sync::mpsc::Sender<()>,
) {
    // Reconcile the agent's launch-at-login autostart and clear the legacy GUI
    // LaunchAgent, before `config` moves into the orchestrator.
    autostart::reconcile(config.app_settings.launch_at_login);

    // Read the hook kill-switch before `config` moves into the orchestrator.
    // Startup-only on purpose (like `show_in_menu_bar`): flipping it requires
    // an agent restart, which the config docs state.
    let capture_mouse_events = config.app_settings.capture_mouse_events;
    #[cfg(target_os = "macos")]
    let launch_at_login = config.app_settings.launch_at_login;

    let Some(core) = bootstrap(config).await else {
        return;
    };
    let Core {
        orchestrator,
        shared,
        observable,
        event_monitor,
        mut inputs,
        ring_haptics,
        connected,
    } = core;
    // Only the macOS dormancy gate listens for demand; elsewhere the agent
    // always arms.
    #[cfg(not(target_os = "macos"))]
    drop(connected);

    let (mut sigterm, mut sigint) = shutdown_signals();

    // The sunk launch-at-login switch: the service plist always carries the
    // login trigger (supervision demands it — `SuccessfulExit` implies
    // `RunAtLoad`), so with the preference off, being started with no client
    // in sight means "launchd ran us at login the user opted out of". Wait
    // briefly for demand — a GUI kickstart connects within seconds — and
    // otherwise leave with a clean `exit(0)` launchd will not respawn. Until
    // then nothing user-visible has happened: no permission prompt, no device
    // open, no menu-bar icon, no overlay helper.
    #[cfg(target_os = "macos")]
    if !launch_at_login {
        info!("launch_at_login is off — dormant until a client connects");
        if !await_demand(&connected, &mut sigterm, &mut sigint, &mut uninstalled).await {
            info!("no client arrived — exiting until wanted");
            return;
        }
        info!("client connected — arming");
    }
    // Arming point: the tray may show, the overlay may start, permissions may
    // prompt, devices may open.
    #[cfg(target_os = "macos")]
    let _ = armed_tx.send(());
    overlay::spawn();

    prompt_missing_accessibility(capture_mouse_events);
    #[cfg(target_os = "macos")]
    request_input_monitoring().await;

    // HID++ watchers need no Accessibility permission — start them up front.
    spawn_hidpp_watchers(&shared, &inputs);

    let watchers = spawn_state_watchers(&shared);
    let StateWatchers {
        inventory: mut inventory_rx,
        camera: mut camera_rx,
        app: mut app_rx,
        accessibility: mut accessibility_rx,
        input_monitoring: mut input_monitoring_rx,
    } = watchers;

    // The CGEventTap hook is installed once Accessibility is granted and dropped
    // if it's revoked (the tap self-disables on revoke regardless; dropping the
    // handle stops its thread).
    let mut hook: Option<Hook> = None;

    info!("openlogi-agent started");
    // Set once the inventory channel closes (the watcher thread died), so the
    // select stops polling a permanently-ready closed receiver.
    let mut inventory_open = true;
    let mut camera_open = true;
    loop {
        tokio::select! {
            event = inventory_rx.recv(), if inventory_open => if let Some(event) = event {
                apply_inventory_event(
                    event,
                    &orchestrator,
                    #[cfg(any(target_os = "macos", target_os = "windows"))]
                    &resume_pending,
                )
                .await;
            } else {
                // Watcher thread death (e.g. a panic inside the HID backend's
                // enumerate) — without a snapshot the GUI would scan forever.
                warn!("inventory watcher channel closed — marking enumeration unavailable");
                orchestrator.lock().await.mark_inventory_unavailable();
                inventory_open = false;
            },
            event = camera_rx.recv(), if camera_open => if let Some(active) = event {
                orchestrator.lock().await.set_camera_active(active);
            } else {
                #[cfg(target_os = "macos")]
                warn!("camera watcher channel closed — disabling camera automation updates");
                camera_open = false;
            },
            Some(app) = app_rx.recv() => {
                apply_foreground_update(app, &orchestrator, &inputs.dispatcher).await;
            }
            Some(device_key) = inputs.triggers.recv() => {
                begin_action_ring(&orchestrator, &inputs.ring, &ring_haptics, device_key.as_deref()).await;
            }
            Some(granted) = accessibility_rx.recv() => {
                apply_accessibility_update(
                    granted,
                    &mut hook,
                    capture_mouse_events,
                    &observable,
                    &shared,
                    &inputs,
                    &event_monitor,
                );
            }
            () = shutdown_signal(&mut sigterm, &mut sigint) => {
                release_hook_and_exit(hook.take(), &mut inputs, "shutdown signal")
            }
            // The app was removed while we kept running from its bundle. Leave
            // through the same door, so the event tap goes with us (#807).
            Some(()) = uninstalled.recv() => {
                release_hook_and_exit(hook.take(), &mut inputs, "the app was uninstalled")
            }
            Some(granted) = input_monitoring_rx.recv() => {
                observable.set_input_monitoring_granted(granted);
            }
            else => break,
        }
    }
}
