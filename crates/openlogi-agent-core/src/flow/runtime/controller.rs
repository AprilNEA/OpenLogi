use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use openlogi_core::config::FlowConfig;
use openlogi_hid::ChannelPool;
use openlogi_hook::edge::{
    ArmedSides, EdgeCrossing, EdgeDetector, EdgeDetectorParams, ExposedEdges,
};
use openlogi_ipc::{FlowLinkState, FlowPeerStatus, FlowStatus};
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::FlowGeneration;
use super::input::FlowInput;
use crate::flow::FlowDeviceSnapshot;
use crate::flow::config::{CompiledFlowConfig, format_public_key};
use crate::flow::handoff::start_outgoing;
use crate::observable::ObservableState;
use crate::receiver_access::ReceiverAccess;

const CONTROL_QUEUE: usize = 4;
const GEOMETRY_REFRESH: Duration = Duration::from_secs(2);

/// Cloneable owner of Flow's dormant configuration and armed network runtime.
#[derive(Clone)]
pub struct FlowController {
    inner: Arc<ControllerInner>,
}

struct ControllerInner {
    config: RwLock<FlowConfig>,
    devices: RwLock<Vec<FlowDeviceSnapshot>>,
    observable: Arc<ObservableState>,
    control_tx: mpsc::UnboundedSender<Control>,
    control_rx: Mutex<Option<mpsc::UnboundedReceiver<Control>>>,
    movement_tx: mpsc::Sender<Movement>,
    movement_rx: Mutex<Option<mpsc::Receiver<Movement>>>,
    edge_settings: Arc<RwLock<EdgeSettings>>,
    channel_pool: ChannelPool,
    receiver_access: ReceiverAccess,
    armed: AtomicBool,
    input: Arc<FlowInput>,
}

/// Bounded, callback-safe input seam for the OS hook.
#[derive(Clone)]
pub struct FlowInputHandle {
    movement: mpsc::Sender<Movement>,
    input: Arc<FlowInput>,
}

#[derive(Clone, Copy)]
struct Movement {
    control_held: bool,
}

enum Control {
    Reconfigure(FlowConfig),
    Devices(Vec<FlowDeviceSnapshot>),
    Crossing(EdgeCrossing),
}

#[derive(Clone)]
struct EdgeSettings {
    enabled: bool,
    require_modifier: bool,
    sides: ArmedSides,
    revision: u64,
}

impl FlowController {
    #[must_use]
    pub fn new(
        config: FlowConfig,
        observable: Arc<ObservableState>,
        channel_pool: ChannelPool,
        receiver_access: ReceiverAccess,
    ) -> Self {
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let (movement_tx, movement_rx) = mpsc::channel(CONTROL_QUEUE);
        let edge_settings = edge_settings(&config, 0);
        observable.set_flow(status_from_config(&config));
        Self {
            inner: Arc::new(ControllerInner {
                config: RwLock::new(config),
                devices: RwLock::new(Vec::new()),
                observable,
                control_tx,
                control_rx: Mutex::new(Some(control_rx)),
                movement_tx,
                movement_rx: Mutex::new(Some(movement_rx)),
                edge_settings: Arc::new(RwLock::new(edge_settings)),
                channel_pool,
                receiver_access,
                armed: AtomicBool::new(false),
                input: Arc::new(FlowInput::new()),
            }),
        }
    }

    /// Start Flow networking and edge processing after the agent's dormancy gate.
    pub fn arm(&self) {
        if self.inner.armed.swap(true, Ordering::AcqRel) {
            return;
        }
        let control = self
            .inner
            .control_rx
            .lock()
            .ok()
            .and_then(|mut receiver| receiver.take());
        let movement = self
            .inner
            .movement_rx
            .lock()
            .ok()
            .and_then(|mut receiver| receiver.take());
        let (Some(control), Some(movement)) = (control, movement) else {
            warn!("Flow controller channels unavailable — Flow remains disabled");
            return;
        };
        let config = self
            .inner
            .config
            .read()
            .map_or_else(|_| FlowConfig::default(), |config| config.clone());
        let devices = self
            .inner
            .devices
            .read()
            .map_or_else(|_| Vec::new(), |devices| devices.clone());
        tokio::spawn(run_controller(
            Arc::clone(&self.inner),
            control,
            config,
            devices,
        ));
        tokio::spawn(run_edge_input(
            movement,
            Arc::clone(&self.inner.edge_settings),
            self.inner.control_tx.clone(),
            Arc::clone(&self.inner.input),
        ));
        tokio::spawn(super::input::run_arrival_glide(Arc::clone(
            &self.inner.input,
        )));
        // Published whether or not Flow is enabled: a peer needs this key in
        // its config before either side can connect.
        tokio::spawn(publish_identity(Arc::clone(&self.inner.observable)));
    }

    /// Apply a live `[flow]` config reload.
    ///
    /// Every saved setting reloads the whole config, so an unchanged `[flow]`
    /// leaves the running sessions alone, and so does a change to the Control
    /// requirement alone, which the edge watcher reads on every movement.
    pub fn update_config(&self, config: &FlowConfig) {
        let previous = self
            .inner
            .config
            .write()
            .ok()
            .map(|mut current| std::mem::replace(&mut *current, config.clone()));
        if previous.as_ref() == Some(config) {
            return;
        }
        let revision = self
            .inner
            .edge_settings
            .read()
            .map_or(1, |settings| settings.revision.wrapping_add(1));
        if let Ok(mut settings) = self.inner.edge_settings.write() {
            *settings = edge_settings(config, revision);
        }
        let edge_only = previous.is_some_and(|previous| {
            FlowConfig {
                require_modifier: config.require_modifier,
                ..previous
            } == *config
        });
        if edge_only {
            return;
        }
        if self.inner.armed.load(Ordering::Acquire)
            && self
                .inner
                .control_tx
                .send(Control::Reconfigure(config.clone()))
                .is_ok()
        {
            return;
        }
        self.inner.observable.set_flow(status_from_config(config));
    }

    /// Publish a fresh inventory-derived Flow device snapshot.
    pub(crate) fn update_devices(&self, devices: Vec<FlowDeviceSnapshot>) {
        if let Ok(mut current) = self.inner.devices.write() {
            current.clone_from(&devices);
        }
        if self.inner.armed.load(Ordering::Acquire) {
            let _ = self.inner.control_tx.send(Control::Devices(devices));
        }
    }

    /// Clone the bounded input seam installed in the OS hook.
    #[must_use]
    pub fn input(&self) -> FlowInputHandle {
        FlowInputHandle {
            movement: self.inner.movement_tx.clone(),
            input: Arc::clone(&self.inner.input),
        }
    }
}

impl FlowInputHandle {
    /// Queue one pointer movement without blocking the OS hook callback.
    /// Returns whether the hook should swallow it: a handoff holds the
    /// pointer on the edge it crossed, and the burst a reconnecting mouse
    /// releases on arrival glides in instead of jumping.
    #[must_use]
    pub fn try_moved(&self, control_held: bool, delta_x: i32, delta_y: i32) -> bool {
        let _ = self.movement.try_send(Movement { control_held });
        self.input.pointer_moved(delta_x, delta_y)
    }

    /// Note one key press without blocking the OS hook callback. Returns
    /// whether the hook should swallow it: a press here after the devices were
    /// sent away means the keyboard stayed behind, and it is sent on.
    #[must_use]
    pub fn key_pressed(&self) -> bool {
        self.input.key_pressed()
    }
}

/// Load (or create) this machine's Flow identity and publish its public key.
async fn publish_identity(observable: Arc<ObservableState>) {
    match tokio::task::spawn_blocking(super::load_machine_identity).await {
        Ok(Ok(identity)) => observable.set_flow_identity(format_public_key(identity.public_key())),
        Ok(Err(error)) => warn!(%error, "Flow identity unavailable"),
        Err(error) => warn!(%error, "Flow identity task failed"),
    }
}

fn status_from_config(config: &FlowConfig) -> FlowStatus {
    CompiledFlowConfig::compile(config).map_or_else(
        |_| FlowStatus {
            enabled: config.enabled,
            peers: config
                .peers
                .iter()
                .map(|peer| FlowPeerStatus {
                    name: peer.name.clone(),
                    public_key: peer.public_key.clone(),
                    state: FlowLinkState::Lost,
                })
                .collect(),
            local_public_key: None,
        },
        |compiled| compiled.status(),
    )
}

fn edge_settings(config: &FlowConfig, revision: u64) -> EdgeSettings {
    let compiled = CompiledFlowConfig::compile(config).ok();
    let sides = compiled.as_ref().map_or(ArmedSides::NONE, |compiled| {
        ArmedSides::from_sides(compiled.layout.keys().copied())
    });
    EdgeSettings {
        enabled: compiled.as_ref().is_some_and(|compiled| compiled.enabled),
        require_modifier: config.require_modifier,
        sides,
        revision,
    }
}

async fn run_controller(
    inner: Arc<ControllerInner>,
    mut control: mpsc::UnboundedReceiver<Control>,
    initial_config: FlowConfig,
    mut devices: Vec<FlowDeviceSnapshot>,
) {
    let mut generation = start_generation(&inner, &initial_config, &devices).await;
    while let Some(command) = control.recv().await {
        match command {
            Control::Reconfigure(config) => {
                if let Some(active) = generation.take() {
                    active.shutdown().await;
                }
                generation = start_generation(&inner, &config, &devices).await;
            }
            Control::Devices(next) => {
                devices = next;
                if let Some(active) = &generation {
                    active.update_devices(&devices).await;
                }
            }
            Control::Crossing(crossing) => {
                if let Some(active) = &generation {
                    start_outgoing(Arc::clone(&active.state), crossing);
                } else {
                    inner.input.release();
                }
            }
        }
    }
    if let Some(active) = generation {
        active.shutdown().await;
    }
}

async fn start_generation(
    inner: &Arc<ControllerInner>,
    config: &FlowConfig,
    devices: &[FlowDeviceSnapshot],
) -> Option<FlowGeneration> {
    inner.observable.set_flow(status_from_config(config));
    let compiled = match CompiledFlowConfig::compile(config) {
        Ok(compiled) => Arc::new(compiled),
        Err(error) => {
            warn!(%error, "invalid Flow configuration — networking not started");
            return None;
        }
    };
    inner.observable.set_flow(compiled.status());
    if !compiled.enabled {
        return None;
    }
    match FlowGeneration::start(
        compiled,
        devices,
        Arc::clone(&inner.observable),
        inner.channel_pool.clone(),
        inner.receiver_access.clone(),
        Arc::clone(&inner.input),
    )
    .await
    {
        Ok(generation) => Some(generation),
        Err(error) => {
            warn!(%error, "Flow runtime failed to start");
            None
        }
    }
}

async fn run_edge_input(
    mut movement: mpsc::Receiver<Movement>,
    settings: Arc<RwLock<EdgeSettings>>,
    control: mpsc::UnboundedSender<Control>,
    input: Arc<FlowInput>,
) {
    let epoch = Instant::now();
    let mut detector: Option<EdgeDetector> = None;
    let mut latch_pending = false;
    let mut active_revision = u64::MAX;
    let mut refreshed_at = Instant::now()
        .checked_sub(GEOMETRY_REFRESH)
        .unwrap_or_else(Instant::now);
    while let Some(movement) = movement.recv().await {
        let current = settings.read().map_or_else(
            |_| edge_settings(&FlowConfig::default(), 0),
            |value| value.clone(),
        );
        if !current.enabled || (current.require_modifier && !movement.control_held) {
            // Rebuild on the next armed movement rather than at the next
            // geometry refresh, so pressing Control at the edge crosses at
            // once. An arrival's latch waits for that rebuild: a pointer still
            // on the edge it arrived on must leave it before crossing back.
            detector = None;
            active_revision = u64::MAX;
            continue;
        }
        let refresh = current.revision != active_revision
            || Instant::now().duration_since(refreshed_at) >= GEOMETRY_REFRESH;
        if refresh {
            let displays = openlogi_hook::display_rects();
            let edges = ExposedEdges::from_displays(&displays);
            detector = match detector.take() {
                // Same configuration: refresh the geometry only, so a latch
                // (after a crossing or an arrival) survives the periodic poll.
                Some(mut existing)
                    if current.revision == active_revision && !displays.is_empty() =>
                {
                    existing.set_edges(edges);
                    Some(existing)
                }
                _ => (!displays.is_empty()).then(|| {
                    EdgeDetector::new(edges, current.sides, EdgeDetectorParams::default())
                }),
            };
            active_revision = current.revision;
            refreshed_at = Instant::now();
        }
        latch_pending |= input.take_latch();
        if latch_pending && let Some(detector) = detector.as_mut() {
            detector.latch();
            latch_pending = false;
        }
        let (Some(detector), Some(position)) =
            (detector.as_mut(), openlogi_hook::edge_cursor_position())
        else {
            continue;
        };
        if let Some(crossing) = detector.update(position, epoch.elapsed()) {
            info!(side = ?crossing.side, "Flow edge crossed");
            input.hold();
            if control.send(Control::Crossing(crossing)).is_err() {
                input.release();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn armed(config: &FlowConfig) -> (FlowController, mpsc::UnboundedReceiver<Control>) {
        let controller = FlowController::new(
            config.clone(),
            Arc::new(ObservableState::new("test".to_owned())),
            openlogi_hid::channel_pool(),
            ReceiverAccess::default(),
        );
        controller.inner.armed.store(true, Ordering::Release);
        let control = controller
            .inner
            .control_rx
            .lock()
            .expect("control receiver lock")
            .take()
            .expect("control receiver");
        (controller, control)
    }

    #[test]
    fn a_reload_restarts_the_sessions_only_for_their_own_settings() {
        let config = FlowConfig {
            enabled: true,
            ..FlowConfig::default()
        };
        let (controller, mut control) = armed(&config);

        controller.update_config(&config);
        assert!(
            control.try_recv().is_err(),
            "an unchanged [flow] reconnected"
        );

        let holding_control = FlowConfig {
            require_modifier: true,
            ..config
        };
        controller.update_config(&holding_control);
        assert!(
            control.try_recv().is_err(),
            "the Control choice reconnected"
        );
        assert!(
            controller
                .inner
                .edge_settings
                .read()
                .expect("edge settings")
                .require_modifier
        );

        let no_clipboard = FlowConfig {
            clipboard: false,
            ..holding_control
        };
        controller.update_config(&no_clipboard);
        assert!(matches!(
            control.try_recv(),
            Ok(Control::Reconfigure(next)) if next == no_clipboard
        ));
    }
}
