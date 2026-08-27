//! The agent's startup ladder: bootstrap, the dormancy gate, arming.
//!
//! [`bootstrap`] builds everything that is safe *before* the agent is armed —
//! pure construction plus the IPC socket bind, no permission prompt, no
//! device open, no helper spawn. Between it and arming sits the macOS
//! dormancy gate ([`await_demand`]): with `launch_at_login` off, a launchd
//! login start waits here for the demand signal (the first accepted IPC
//! connection) and otherwise leaves with a clean `exit(0)`. The state
//! watchers ([`spawn_state_watchers`]) spawn at arming, feeding the select
//! loop in `main`.

use std::sync::Arc;
use std::time::Duration;

use openlogi_agent_core::event_monitor::EventMonitor;
use openlogi_agent_core::observable::ObservableState;
use openlogi_agent_core::orchestrator::{Orchestrator, SharedRuntime};
use openlogi_agent_core::watchers;
use openlogi_core::config::Config;
#[cfg(target_os = "macos")]
use openlogi_hook::Hook;
use tokio::sync::Mutex;

use crate::server::AgentServer;
use crate::{InputServices, pairing, server};

/// How long a dormant agent waits for a client before leaving. Generous next
/// to the seconds a kickstarting GUI needs to connect; the only cost of the
/// window is an idle process that has opened no device and prompted for
/// nothing.
#[cfg(target_os = "macos")]
const DORMANT_DEADLINE: Duration = Duration::from_secs(60);

/// Everything [`run`] needs alive after [`bootstrap`]: the shared state plus
/// the running IPC server's handles.
pub(crate) struct Core {
    pub(crate) orchestrator: Arc<Mutex<Orchestrator>>,
    pub(crate) shared: SharedRuntime,
    pub(crate) observable: Arc<ObservableState>,
    pub(crate) event_monitor: Arc<EventMonitor>,
    pub(crate) inputs: InputServices,
    pub(crate) ring_haptics: server::RingHapticPlayer,
    /// Notified on every accepted IPC connection — the dormancy gate's
    /// demand signal.
    pub(crate) connected: Arc<tokio::sync::Notify>,
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
pub(crate) async fn bootstrap(config: Config) -> Option<Core> {
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
pub(crate) struct StateWatchers {
    pub(crate) inventory: tokio::sync::mpsc::UnboundedReceiver<watchers::inventory::InventoryEvent>,
    pub(crate) camera: tokio::sync::mpsc::UnboundedReceiver<bool>,
    pub(crate) app:
        tokio::sync::mpsc::UnboundedReceiver<watchers::foreground_app::ForegroundUpdate>,
    pub(crate) accessibility: tokio::sync::mpsc::UnboundedReceiver<bool>,
    pub(crate) input_monitoring: tokio::sync::mpsc::UnboundedReceiver<bool>,
}

pub(crate) fn spawn_state_watchers(shared: &SharedRuntime) -> StateWatchers {
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
pub(crate) async fn await_demand(
    connected: &tokio::sync::Notify,
    sigterm: &mut Option<tokio::signal::unix::Signal>,
    sigint: &mut Option<tokio::signal::unix::Signal>,
    uninstalled: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
) -> bool {
    tokio::select! {
        () = connected.notified() => true,
        () = tokio::time::sleep(DORMANT_DEADLINE) => false,
        () = crate::shutdown::shutdown_signal(sigterm, sigint) => false,
        Some(()) = uninstalled.recv() => false,
    }
}
