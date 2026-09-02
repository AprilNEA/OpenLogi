//! Startup construction: everything built *before* arming.
//!
//! [`bootstrap`] assembles the [`Core`] — pure construction plus the IPC
//! socket bind; the watcher fleets spawn later, at arming. The ladder itself
//! is `crate::lifecycle`.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::stream::{self, Stream};

use openlogi_agent_core::action_ring::ActionRingManager;
use openlogi_agent_core::event_monitor::EventMonitor;
use openlogi_agent_core::observable::ObservableState;
use openlogi_agent_core::orchestrator::{Orchestrator, SharedRuntime};
use openlogi_agent_core::runtime::scroll::{ScrollInputHandle, ScrollRuntime};
use openlogi_agent_core::runtime::{ActionDispatcher, ActionRuntime};
use openlogi_agent_core::watchers::shutdown::{StopOutcome, WatcherHandle};
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
    /// Client declarations forwarded by the IPC server — the dormancy gate's
    /// demand channel. It buffers, so a declaration that lands before the
    /// gate listens is not lost.
    pub(crate) demand: tokio::sync::mpsc::UnboundedReceiver<openlogi_ipc::ClientKind>,
}

/// Build the shared state and start the IPC server — everything safe before
/// arming: no permission prompt, no device open, no helper spawn. Binding
/// ahead of the watchers and prompts lets a dormant agent hear demand, and
/// keeps a first-run consent dialog from blackholing the GUI's connect.
pub(crate) async fn bootstrap(config: Config) -> Option<Core> {
    // The orchestrator is shared with the IPC server and mutated by the
    // select loop, so it lives behind an async mutex; locks are brief. The
    // hook facts are published by the select loop, which owns the hook.
    let observable = Arc::new(ObservableState::new(env!("CARGO_PKG_VERSION").to_string()));
    #[cfg(target_os = "macos")]
    seed_permission_facts(&observable);
    let orchestrator = Arc::new(Mutex::new(Orchestrator::new(
        config,
        Arc::clone(&observable),
    )));
    let shared = orchestrator.lock().await.shared();
    let inputs = InputServices::start(&shared)?;

    // Shared between the hook callback (which mirrors events into it) and
    // the IPC server (which the GUI polls); the janitor turns it back off.
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

/// The input-action runtimes — pure in-process workers that touch no device
/// until an action is dispatched, so [`bootstrap`] may start them.
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
            shared.device_io.clone(),
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

/// Graceful-shutdown handles for the three firmware-owning HID++ managers.
pub(crate) struct HidppWatcherHandles {
    gesture: WatcherHandle,
    host_switch: WatcherHandle,
    keyboard: WatcherHandle,
}

impl HidppWatcherHandles {
    /// Stop all managers concurrently with the bounded policy for a terminal
    /// process exit.
    pub(crate) async fn stop_and_wait(self) {
        let _outcomes = tokio::join!(
            self.gesture.stop_and_wait("gesture"),
            self.host_switch.stop_and_wait("host-switch"),
            self.keyboard.stop_and_wait("keyboard"),
        );
    }

    /// Stop all managers concurrently and retain firmware ownership past the
    /// diagnostic deadline before starting any replacement process image.
    pub(crate) async fn stop_and_wait_confirmed(self) -> bool {
        let (gesture, host_switch, keyboard) = tokio::join!(
            self.gesture.stop_and_wait_confirmed("gesture"),
            self.host_switch.stop_and_wait_confirmed("host-switch"),
            self.keyboard.stop_and_wait_confirmed("keyboard"),
        );
        all_stopped(gesture, host_switch, keyboard)
    }
}

fn all_stopped(gesture: StopOutcome, host_switch: StopOutcome, keyboard: StopOutcome) -> bool {
    gesture.is_stopped() && host_switch.is_stopped() && keyboard.is_stopped()
}

/// Start the HID++ background sessions that do not need Accessibility.
pub(crate) fn spawn_hidpp_watchers(
    shared: &SharedRuntime,
    inputs: &InputServices,
) -> HidppWatcherHandles {
    let gesture = watchers::gesture::spawn(
        &shared.capture_plans,
        shared.capture_channel.clone(),
        shared.receiver_access.clone(),
        shared.channel_registry.clone(),
        shared.device_io.clone(),
        GestureOutputs::new(
            inputs.dispatcher.clone(),
            inputs.scroll_input.clone(),
            shared.hook_maps.clone(),
        ),
    );
    let host_switch = watchers::host_switch::spawn(
        &shared.host_switch_links,
        shared.channel_pool.clone(),
        shared.receiver_access.clone(),
        shared.device_io.clone(),
    );
    let keyboard = watchers::keyboard::spawn(
        &shared.keyboard_spec,
        shared.keyboard_channel.clone(),
        shared.receiver_access.clone(),
        shared.channel_registry.clone(),
        shared.device_io.clone(),
        inputs.dispatcher.clone(),
    );
    HidppWatcherHandles {
        gesture,
        host_switch,
        keyboard,
    }
}

/// One tagged event from the per-source state watchers.
///
/// Everything the lifecycle's select loop listens to is low-frequency by
/// contract — that is what makes the unbounded channels safe. The input hot
/// path (hook → dispatcher → inject) never passes through it; do not route a
/// high-rate source here.
pub(crate) enum WatcherEvent {
    Inventory(watchers::inventory::InventoryEvent),
    /// Camera activity flipped.
    Camera(bool),
    App(watchers::foreground_app::ForegroundUpdate),
    /// The Accessibility grant flipped.
    Accessibility(bool),
    /// The Input Monitoring grant flipped.
    InputMonitoring(bool),
    /// A watcher's channel closed (its thread died). Emitted once; the
    /// source then leaves the merge, so a dead watcher cannot busy-wake the
    /// loop.
    Lost(Watcher),
}

/// Which watcher a [`WatcherEvent::Lost`] names.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Watcher {
    Inventory,
    Camera,
    App,
    Accessibility,
    InputMonitoring,
}

/// Spawn the per-source state watchers at arming, merged into one tagged
/// stream.
pub(crate) fn spawn_state_watchers(
    shared: &SharedRuntime,
) -> (
    impl Stream<Item = WatcherEvent> + Unpin + use<>,
    watchers::inventory::InventoryRefresh,
) {
    fn tagged<T: Send + 'static>(
        rx: tokio::sync::mpsc::UnboundedReceiver<T>,
        source: Watcher,
        tag: impl Fn(T) -> WatcherEvent + Send + 'static,
    ) -> stream::BoxStream<'static, WatcherEvent> {
        stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        })
        .map(tag)
        .chain(stream::iter([WatcherEvent::Lost(source)]))
        .boxed()
    }
    let inventory = watchers::inventory::spawn_with_registry(
        shared.channel_registry.clone(),
        shared.device_io.clone(),
    );
    let streams = stream::select_all([
        tagged(
            inventory.events,
            Watcher::Inventory,
            WatcherEvent::Inventory,
        ),
        tagged(
            watchers::camera::spawn(Duration::from_secs(1)),
            Watcher::Camera,
            WatcherEvent::Camera,
        ),
        tagged(
            watchers::foreground_app::spawn(),
            Watcher::App,
            WatcherEvent::App,
        ),
        tagged(
            watchers::accessibility::spawn(Duration::from_millis(1200)),
            Watcher::Accessibility,
            WatcherEvent::Accessibility,
        ),
        tagged(
            watchers::input_monitoring::spawn(Duration::from_millis(1200)),
            Watcher::InputMonitoring,
            WatcherEvent::InputMonitoring,
        ),
    ]);
    (streams, inventory.refresh)
}

/// Seed the permission facts with non-prompting reads, so a client that
/// connects before the watchers' first tick doesn't see a default. No hook is
/// installed this early — arming is what may install one.
#[cfg(target_os = "macos")]
fn seed_permission_facts(observable: &ObservableState) {
    observable.set_accessibility_and_hook(Hook::has_accessibility(), false);
    observable.set_input_monitoring_granted(openlogi_hid::permissions::has_access());
}
