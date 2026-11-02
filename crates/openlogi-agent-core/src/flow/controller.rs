//! Starts and stops Flow generations as the agent arms and as the Flow
//! configuration changes.
//!
//! The orchestrator pushes the configuration and the device inventory; both
//! are `watch` values, so a burst of updates coalesces. A supervisor task,
//! spawned when the agent arms, runs at most one [`FlowRuntime`]: it starts
//! one when Flow is enabled and has a peer, and replaces it whenever the
//! configuration changes. The OS hook reaches the current generation's
//! pointer thread through [`FlowInput`].

use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use openlogi_core::config::FlowConfig;
use openlogi_hid::{ChannelPool, DeviceIoGate};
use openlogi_ipc::{FlowLinkState, FlowPeerStatus, FlowStatus};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use super::devices::InventoryDevice;
use super::pointer::PointerWaker;
use super::runtime::{FlowHardware, FlowRuntime, FlowStart};
use crate::observable::ObservableState;
use crate::receiver_access::ReceiverAccess;

#[cfg(test)]
mod tests;

/// How long a generation that failed to start waits before trying again,
/// unless the configuration changes first.
const RETRY_DELAY: Duration = Duration::from_secs(30);

/// The OS hook's entry into Flow: wakes the current generation's pointer
/// thread after cursor motion.
#[derive(Clone, Debug, Default)]
pub struct FlowInput {
    waker: Arc<RwLock<Option<PointerWaker>>>,
}

impl FlowInput {
    /// Record that the cursor moved. Never blocks, so the hook callback may
    /// call it: while a generation is being swapped the motion is dropped,
    /// and the next motion wakes the new pointer thread.
    pub fn moved(&self) {
        if let Ok(waker) = self.waker.try_read()
            && let Some(waker) = waker.as_ref()
        {
            waker.moved();
        }
    }

    fn set(&self, waker: Option<PointerWaker>) {
        *self.waker.write().unwrap_or_else(PoisonError::into_inner) = waker;
    }
}

/// Owns Flow for the agent's lifetime. Cloning shares the same controller.
#[derive(Clone)]
pub struct FlowController {
    inner: Arc<Inner>,
}

struct Inner {
    config: watch::Sender<Arc<FlowConfig>>,
    inventory: watch::Sender<Arc<[InventoryDevice]>>,
    stop: watch::Sender<bool>,
    input: FlowInput,
    observable: Arc<ObservableState>,
    hardware: FlowHardware,
    supervisor: Mutex<Option<JoinHandle<()>>>,
}

impl FlowController {
    /// A dormant controller for `config`. Nothing runs until [`Self::arm`].
    /// `receiver_access` must be the agent's shared instance, so a Flow
    /// transfer excludes the other receiver users.
    pub(crate) fn new(
        config: &FlowConfig,
        observable: Arc<ObservableState>,
        channel_pool: ChannelPool,
        receiver_access: ReceiverAccess,
        device_io: DeviceIoGate,
    ) -> Self {
        let (config, _) = watch::channel(Arc::new(config.clone()));
        let (inventory, _) = watch::channel(Arc::from(Vec::new()));
        let (stop, _) = watch::channel(false);
        Self {
            inner: Arc::new(Inner {
                config,
                inventory,
                stop,
                input: FlowInput::default(),
                observable,
                hardware: FlowHardware {
                    channel_pool,
                    receiver_access,
                    device_io,
                },
                supervisor: Mutex::new(None),
            }),
        }
    }

    /// The handle the OS hook calls on cursor motion.
    #[must_use]
    pub fn input(&self) -> FlowInput {
        self.inner.input.clone()
    }

    /// Use `config` from now on. A change restarts the running generation;
    /// an equal configuration changes nothing.
    pub(crate) fn update_config(&self, config: &FlowConfig) {
        self.inner.config.send_if_modified(|current| {
            if **current == *config {
                return false;
            }
            *current = Arc::new(config.clone());
            true
        });
    }

    /// Replace the device inventory the running generation sees.
    pub(crate) fn update_devices(&self, devices: Vec<InventoryDevice>) {
        self.inner.inventory.send_if_modified(|current| {
            if **current == *devices {
                return false;
            }
            *current = Arc::from(devices);
            true
        });
    }

    /// Start supervising Flow. The agent calls this once it arms; later calls
    /// and calls after [`Self::shutdown`] do nothing.
    pub fn arm(&self) {
        let mut supervisor = self
            .inner
            .supervisor
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if supervisor.is_some() || *self.inner.stop.borrow() {
            return;
        }
        *supervisor = Some(tokio::spawn(supervise(Arc::clone(&self.inner))));
    }

    /// Stop the running generation and the supervisor, and wait for both.
    pub async fn shutdown(&self) {
        self.inner.stop.send_replace(true);
        let supervisor = self
            .inner
            .supervisor
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(supervisor) = supervisor
            && supervisor.await.is_err()
        {
            warn!("the Flow supervisor panicked");
        }
    }
}

#[cfg(test)]
impl FlowController {
    /// The configuration the supervisor sees.
    pub(crate) fn current_config(&self) -> Arc<FlowConfig> {
        Arc::clone(&self.inner.config.borrow())
    }

    /// The inventory the running generation sees.
    pub(crate) fn current_devices(&self) -> Arc<[InventoryDevice]> {
        Arc::clone(&self.inner.inventory.borrow())
    }
}

/// Run one generation per configuration until told to stop.
async fn supervise(inner: Arc<Inner>) {
    let mut config = inner.config.subscribe();
    let mut stop = inner.stop.subscribe();
    loop {
        if *stop.borrow_and_update() {
            break;
        }
        let current = Arc::clone(&config.borrow_and_update());
        let wanted = current.enabled && !current.peers().is_empty();
        let runtime = if wanted {
            start(&inner, &current).await
        } else {
            None
        };
        if runtime.is_none() {
            publish_dormant(&inner.observable, &current);
        }
        let retry = wanted && runtime.is_none();
        let stopping = tokio::select! {
            changed = config.changed() => changed.is_err(),
            _ = stop.changed() => true,
            () = tokio::time::sleep(RETRY_DELAY), if retry => false,
        };
        if let Some(runtime) = runtime {
            inner.input.set(None);
            runtime.stop().await;
        }
        if stopping {
            break;
        }
    }
    let current = Arc::clone(&inner.config.borrow());
    publish_dormant(&inner.observable, &current);
}

async fn start(inner: &Inner, config: &Arc<FlowConfig>) -> Option<FlowRuntime> {
    let started = FlowRuntime::start(FlowStart {
        config: Arc::clone(config),
        inventory: inner.inventory.subscribe(),
        observable: Arc::clone(&inner.observable),
        hardware: inner.hardware.clone(),
    })
    .await;
    match started {
        Ok(runtime) => {
            inner.input.set(Some(runtime.pointer_waker()));
            info!(peers = config.peers().len(), "Flow started");
            Some(runtime)
        }
        Err(error) => {
            warn!(%error, "cannot start Flow; retrying later");
            None
        }
    }
}

/// Report the configured peers as unreachable while no generation runs.
fn publish_dormant(observable: &ObservableState, config: &FlowConfig) {
    observable.set_flow(FlowStatus {
        enabled: config.enabled,
        peers: config
            .peers()
            .iter()
            .map(|peer| FlowPeerStatus {
                name: peer.name.clone(),
                public_key: peer.public_key,
                state: FlowLinkState::Lost,
            })
            .collect(),
    });
}
