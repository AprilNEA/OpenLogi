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
use tracing::warn;

use super::FlowGeneration;
use crate::flow::FlowDeviceSnapshot;
use crate::flow::config::CompiledFlowConfig;
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
}

/// Bounded, callback-safe input seam for pointer movement.
#[derive(Clone)]
pub struct FlowInputHandle {
    movement: mpsc::Sender<Movement>,
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
        ));
    }

    /// Apply a live `[flow]` config reload.
    pub fn update_config(&self, config: &FlowConfig) {
        if let Ok(mut current) = self.inner.config.write() {
            *current = config.clone();
        }
        let revision = self
            .inner
            .edge_settings
            .read()
            .map_or(1, |settings| settings.revision.wrapping_add(1));
        if let Ok(mut settings) = self.inner.edge_settings.write() {
            *settings = edge_settings(config, revision);
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
        }
    }
}

impl FlowInputHandle {
    /// Queue one pointer movement without blocking the OS hook callback.
    pub fn try_moved(&self, control_held: bool) {
        let _ = self.movement.try_send(Movement { control_held });
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
) {
    let epoch = Instant::now();
    let mut detector = None;
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
            detector = None;
            continue;
        }
        let refresh = current.revision != active_revision
            || Instant::now().duration_since(refreshed_at) >= GEOMETRY_REFRESH;
        if refresh {
            let displays = openlogi_hook::display_rects();
            detector = (!displays.is_empty()).then(|| {
                EdgeDetector::new(
                    ExposedEdges::from_displays(&displays),
                    current.sides,
                    EdgeDetectorParams::default(),
                )
            });
            active_revision = current.revision;
            refreshed_at = Instant::now();
        }
        let (Some(detector), Some(position)) =
            (detector.as_mut(), openlogi_hook::cursor_position())
        else {
            continue;
        };
        if let Some(crossing) = detector.update(position, epoch.elapsed()) {
            let _ = control.send(Control::Crossing(crossing));
        }
    }
}
