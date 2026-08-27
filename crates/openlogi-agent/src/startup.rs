//! The agent's startup construction: everything built *before* arming.
//!
//! [`bootstrap`] assembles the [`Core`] — pure construction plus the IPC
//! socket bind, no permission prompt, no device open, no helper spawn. The
//! watcher fleets ([`spawn_hidpp_watchers`], [`spawn_state_watchers`]) spawn
//! later, at arming. The ladder itself — bootstrap, the dormancy gate,
//! arming, the select loop — is `crate::lifecycle`.

use std::sync::Arc;
use std::time::Duration;

use openlogi_agent_core::action_ring::ActionRingManager;
use openlogi_agent_core::event_monitor::EventMonitor;
use openlogi_agent_core::observable::ObservableState;
use openlogi_agent_core::orchestrator::{Orchestrator, SharedRuntime};
use openlogi_agent_core::runtime::scroll::{ScrollInputHandle, ScrollRuntime};
use openlogi_agent_core::runtime::{ActionDispatcher, ActionRuntime};
use openlogi_agent_core::watchers::{self, gesture::GestureOutputs};
use openlogi_core::config::Config;
#[cfg(target_os = "macos")]
use openlogi_hook::Hook;
use tokio::sync::Mutex;
use tracing::warn;

use crate::server::AgentServer;
use crate::{pairing, server};

/// Everything the lifecycle keeps alive after [`bootstrap`]: the shared state
/// plus the running IPC server's handles.
pub(crate) struct Core {
    pub(crate) orchestrator: Arc<Mutex<Orchestrator>>,
    pub(crate) shared: SharedRuntime,
    pub(crate) observable: Arc<ObservableState>,
    pub(crate) event_monitor: Arc<EventMonitor>,
    pub(crate) inputs: InputServices,
    pub(crate) ring_haptics: server::RingHapticPlayer,
    /// Client declarations forwarded by the IPC server's `declare_client`
    /// handler — the dormancy gate's demand channel. The channel buffers, so
    /// a declaration that lands before the gate starts listening is not
    /// lost; unbounded is safe because declarations are one per connection.
    pub(crate) demand: tokio::sync::mpsc::UnboundedReceiver<openlogi_ipc::ClientKind>,
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

    let (ring_haptics, demand) = spawn_ipc_server(
        Arc::clone(&orchestrator),
        &shared,
        Arc::clone(&observable),
        Arc::clone(&pairing),
        Arc::clone(&event_monitor),
        &inputs,
    );
    Some(Core {
        orchestrator,
        shared,
        observable,
        event_monitor,
        inputs,
        ring_haptics,
        demand,
    })
}

fn spawn_ipc_server(
    orchestrator: Arc<Mutex<Orchestrator>>,
    shared: &SharedRuntime,
    observable: Arc<ObservableState>,
    pairing: Arc<pairing::PairingManager>,
    event_monitor: Arc<EventMonitor>,
    inputs: &InputServices,
) -> (
    server::RingHapticPlayer,
    tokio::sync::mpsc::UnboundedReceiver<openlogi_ipc::ClientKind>,
) {
    let (server, demand) = AgentServer::new(
        orchestrator,
        shared.clone(),
        observable,
        pairing,
        event_monitor,
        Arc::clone(&inputs.ring),
        inputs.dispatcher.clone(),
    );
    let ring_haptics = server.ring_haptics.clone();
    tokio::spawn(server::run(server));
    (ring_haptics, demand)
}

/// The input-action runtimes: the Actions Ring, the button-lifecycle worker,
/// and the smooth-scroll worker. Started inside [`bootstrap`] — they are pure
/// in-process workers that touch no device until an action is dispatched.
pub(crate) struct InputServices {
    pub(crate) ring: Arc<ActionRingManager>,
    pub(crate) triggers: tokio::sync::mpsc::UnboundedReceiver<Option<String>>,
    pub(crate) dispatcher: ActionDispatcher,
    action_runtime: ActionRuntime,
    pub(crate) scroll_input: ScrollInputHandle,
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

    pub(crate) fn shutdown(&mut self) {
        self.scroll_runtime.shutdown();
        self.action_runtime.shutdown();
    }
}

/// Start the HID++ background sessions that do not need Accessibility.
pub(crate) fn spawn_hidpp_watchers(shared: &SharedRuntime, inputs: &InputServices) {
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

/// The per-source state watchers the select loop drains, spawned at arming.
///
/// Everything in here — and everything else the lifecycle's select loop
/// listens to — is low-frequency by contract: second-scale polls and one-shot
/// signals. That contract is what makes the unbounded channels safe. The
/// input hot path (hook → dispatcher → inject) never passes through the
/// select loop; do not route a high-rate source through it.
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
