//! Startup construction: everything built *before* arming.
//!
//! [`bootstrap`] assembles the [`Core`] — pure construction plus the IPC
//! socket bind; the watcher fleets spawn later, at arming. The ladder itself
//! is `crate::lifecycle`.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures::StreamExt as _;
use futures::stream::{self, Stream};

use openlogi_agent_core::action_ring::ActionRingManager;
use openlogi_agent_core::event_monitor::EventMonitor;
use openlogi_agent_core::observable::ObservableState;
use openlogi_agent_core::orchestrator::{Orchestrator, SharedRuntime};
use openlogi_agent_core::runtime::scroll::{ScrollInputHandle, ScrollRuntime};
use openlogi_agent_core::runtime::{ActionDispatcher, ActionRuntime};
use openlogi_agent_core::touchpad_monitor::TouchpadMonitor;
use openlogi_agent_core::watchers::{
    self,
    gesture::{GestureOutputs, GestureWatcher},
};
use openlogi_core::config::Config;
use openlogi_core::device::DeviceInventory;
use openlogi_core::device_order::DeviceIdentity;
use openlogi_hid::session::gesture::{CaptureSessionMode, CaptureSpec};
use openlogi_hid::{
    CaptureSessionOutcome, CaptureSessionRequest, DeviceRoute, FileTouchpadJournalStore,
    run_capture_session,
};
#[cfg(target_os = "macos")]
use openlogi_hook::Hook;
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::server::{AgentMonitors, AgentServer};
use crate::{pairing, server};

/// Everything the lifecycle keeps alive after [`bootstrap`]: the shared state
/// plus the running IPC server's handles.
pub(crate) struct Core {
    pub(crate) orchestrator: Arc<Mutex<Orchestrator>>,
    pub(crate) shared: SharedRuntime,
    pub(crate) observable: Arc<ObservableState>,
    pub(crate) event_monitor: Arc<EventMonitor>,
    pub(crate) touchpad_monitor: Arc<TouchpadMonitor>,
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
    let touchpad_monitor = Arc::new(TouchpadMonitor::default());
    tokio::spawn(Arc::clone(&touchpad_monitor).run_idle_janitor());

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
        Arc::clone(&touchpad_monitor),
        &inputs,
    );
    Some(Core {
        orchestrator,
        shared,
        observable,
        event_monitor,
        touchpad_monitor,
        inputs,
        ring_haptics,
        demand,
    })
}

/// Resolve durable raw-touchpad records before the dormancy gate can let the
/// process exit. This pass is deliberately recovery-only: no watcher starts,
/// no normal device setting is applied, and macOS never asks for permission.
pub(crate) async fn recover_pending_touchpads(shared: &SharedRuntime) {
    let journal = match FileTouchpadJournalStore::in_state_dir() {
        Ok(journal) => Arc::new(journal),
        Err(error) => {
            warn!(error = %error, "could not open touchpad raw-mode journal — startup recovery deferred");
            return;
        }
    };
    let pending = match journal.pending_ids() {
        Ok(ids) => ids.into_iter().collect::<HashSet<_>>(),
        Err(error) => {
            warn!(error = %error, "could not inspect touchpad raw-mode journal — startup recovery deferred");
            return;
        }
    };
    if pending.is_empty() {
        return;
    }

    #[cfg(target_os = "macos")]
    if !openlogi_hid::permissions::has_access() {
        info!(
            pending = pending.len(),
            "Input Monitoring is unavailable — touchpad raw-mode recovery deferred"
        );
        return;
    }

    let Some(_lease) = shared.receiver_access.try_acquire_for_session() else {
        debug!("receiver is busy — touchpad raw-mode startup recovery deferred");
        return;
    };
    let backend = openlogi_hid::host::backend();
    let inventories = match openlogi_hid::inventory::enumerate(Arc::clone(&backend)).await {
        Ok(inventories) => inventories,
        Err(error) => {
            warn!(error = ?error, "could not enumerate for touchpad raw-mode startup recovery");
            return;
        }
    };
    let targets = touchpad_recovery_targets(&inventories, &pending);
    let channel = Arc::new(RwLock::new(None));
    for target in targets {
        let (sink, _inputs) = mpsc::unbounded_channel();
        let (_stop, shutdown) = oneshot::channel();
        let spec = CaptureSpec {
            mode: CaptureSessionMode::TouchpadRecovery,
            touchpad_journal_id: Some(target.journal_id.clone()),
            ..CaptureSpec::default()
        };
        match run_capture_session(
            &*backend,
            target.route,
            CaptureSessionRequest {
                spec,
                touchpad_journal: Some(journal.clone()),
                sink,
                shutdown,
                channel_slot: Arc::clone(&channel),
                device_io: shared.device_io.clone(),
                // Recovery only restores; nothing subscribes to presses.
                native_button: shared.native_button.clone(),
            },
        )
        .await
        {
            Ok(CaptureSessionOutcome::Restored) => info!(
                device_id = target.journal_id,
                "touchpad raw-mode startup recovery complete"
            ),
            Ok(CaptureSessionOutcome::RestorePending(_)) => warn!(
                device_id = target.journal_id,
                "touchpad raw-mode startup recovery left unrelated control restoration pending"
            ),
            Err(error) => warn!(
                device_id = target.journal_id,
                error = %error,
                "touchpad raw-mode startup recovery deferred"
            ),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct TouchpadRecoveryTarget {
    route: DeviceRoute,
    journal_id: String,
}

/// Match journals only to online devices whose current probe positively
/// reported raw-touchpad support and a stable physical identity.
fn touchpad_recovery_targets(
    inventories: &[DeviceInventory],
    pending: &HashSet<String>,
) -> Vec<TouchpadRecoveryTarget> {
    let mut matched = HashSet::new();
    let mut targets = Vec::new();
    for inventory in inventories {
        for device in &inventory.paired {
            if !device.online
                || !device
                    .capabilities
                    .is_some_and(|capabilities| capabilities.touchpad_raw_xy)
            {
                continue;
            }
            let Some(model) = device.model_info.as_ref() else {
                continue;
            };
            let Some(journal_id) =
                DeviceIdentity::from_parts(model.serial_number.as_deref(), model.unit_id)
                    .config_key()
            else {
                continue;
            };
            let Some(route) = DeviceRoute::device_route_for(inventory, device.slot) else {
                continue;
            };
            if !pending.contains(&journal_id) || !matched.insert(journal_id.clone()) {
                continue;
            }
            targets.push(TouchpadRecoveryTarget { route, journal_id });
        }
    }
    targets
}

fn spawn_ipc_server(
    orchestrator: Arc<Mutex<Orchestrator>>,
    shared: &SharedRuntime,
    observable: Arc<ObservableState>,
    pairing: Arc<pairing::PairingManager>,
    event_monitor: Arc<EventMonitor>,
    touchpad_monitor: Arc<TouchpadMonitor>,
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
        AgentMonitors::new(event_monitor, touchpad_monitor),
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

/// Start the HID++ background sessions that do not need Accessibility.
pub(crate) fn spawn_hidpp_watchers(
    shared: &SharedRuntime,
    inputs: &InputServices,
    touchpad_monitor: Arc<TouchpadMonitor>,
) -> GestureWatcher {
    let gesture = watchers::gesture::spawn(
        &shared.capture_plans,
        shared.receiver_access.clone(),
        shared.device_io.clone(),
        GestureOutputs::new(
            inputs.dispatcher.clone(),
            inputs.scroll_input.clone(),
            shared.hook_maps.clone(),
        ),
        touchpad_monitor,
        (
            shared.capture_channel.clone(),
            shared.channel_registry.clone(),
            shared.native_button.clone(),
        ),
    );
    watchers::host_switch::spawn(
        &shared.host_switch_links,
        shared.channel_pool.clone(),
        shared.receiver_access.clone(),
        shared.device_io.clone(),
    );
    watchers::keyboard::spawn(
        &shared.keyboard_spec,
        shared.keyboard_channel.clone(),
        shared.receiver_access.clone(),
        shared.channel_registry.clone(),
        shared.device_io.clone(),
        inputs.dispatcher.clone(),
    );
    gesture
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

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::device::{
        Capabilities, DeviceKind, DeviceModelInfo, DeviceTransports, PairedDevice, ReceiverInfo,
    };

    fn device(serial: &str, slot: u8, online: bool, touchpad_raw_xy: bool) -> PairedDevice {
        PairedDevice {
            slot,
            codename: Some(serial.to_string()),
            wpid: Some(0xb123),
            kind: DeviceKind::Touchpad,
            online,
            battery: None,
            model_info: Some(DeviceModelInfo {
                entity_count: 1,
                serial_number: Some(serial.to_string()),
                unit_id: [slot, 2, 3, 4],
                transports: DeviceTransports::default(),
                model_ids: [0xb123, 0, 0],
                extended_model_id: 1,
            }),
            capabilities: Some(Capabilities {
                touchpad_raw_xy,
                ..Capabilities::default()
            }),
        }
    }

    #[test]
    fn startup_recovery_targets_only_pending_online_probed_touchpads() {
        let casa = device("CASA-1", 1, true, true);
        let duplicate_casa = device("casa-1", 2, true, true);
        let unrelated_touchpad = device("other", 3, true, true);
        let unsupported = device("unsupported", 4, true, false);
        let offline = device("offline", 5, false, true);
        let inventory = DeviceInventory {
            receiver: ReceiverInfo {
                name: "Logi Bolt Receiver".to_string(),
                vendor_id: 0x046d,
                product_id: 0xc548,
                unique_id: Some("receiver-1".to_string()),
            },
            paired: vec![
                casa,
                duplicate_casa,
                unrelated_touchpad,
                unsupported,
                offline,
            ],
        };
        let pending = [
            "serial:casa-1".to_string(),
            "serial:unsupported".to_string(),
            "serial:offline".to_string(),
        ]
        .into_iter()
        .collect();

        assert_eq!(
            touchpad_recovery_targets(&[inventory], &pending),
            vec![TouchpadRecoveryTarget {
                route: DeviceRoute::Bolt {
                    receiver_uid: "receiver-1".to_string(),
                    slot: 1,
                },
                journal_id: "serial:casa-1".to_string(),
            }]
        );
    }
}
