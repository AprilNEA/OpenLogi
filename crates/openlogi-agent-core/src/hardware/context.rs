//! Shared ownership of the agent's hardware backend and host-I/O policy.

use std::sync::Arc;

use openlogi_core::device::StandaloneDevice;
use openlogi_core::hid::{LightCommand, PairingError, WriteError};
use openlogi_hid::backend::{HidBackend, HotplugStream};
use openlogi_hid::inventory::persist::ProbeCacheStore;
use openlogi_hid::{
    ChannelPool, DeviceIoGate, DeviceRoute, Enumerator, FileProbeCacheStore, InventoryError,
    LitraModel, PairingCommand, PairingEvent, ReceiverSelector,
};
use tokio::sync::mpsc;

/// One coherent source for every backend-dependent agent hardware service.
///
/// Production uses the process-wide native backend and device-I/O gate. Tests
/// and alternate hosts inject one backend identity and gate; their enumerator,
/// hotplug stream, standalone discovery, and channel pool all derive from it.
#[derive(Clone)]
pub struct HardwareContext {
    backend: Arc<dyn HidBackend>,
    device_io: DeviceIoGate,
    channel_pool: ChannelPool,
    probe_cache: Option<Arc<dyn ProbeCacheStore>>,
}

impl HardwareContext {
    /// Build the production context over this process's native backend and
    /// lifecycle gate, with the usual file-backed probe cache when available.
    #[must_use]
    pub fn production() -> Self {
        let probe_cache = FileProbeCacheStore::in_data_dir()
            .map(|store| Arc::new(store) as Arc<dyn ProbeCacheStore>);
        Self::from_parts(
            openlogi_hid::host::backend(),
            openlogi_hid::host::device_io_gate(),
            probe_cache,
        )
    }

    /// Build an injected, memory-cache-only context over `backend` and
    /// `device_io`.
    #[must_use]
    pub fn injected(backend: Arc<dyn HidBackend>, device_io: DeviceIoGate) -> Self {
        Self::from_parts(backend, device_io, None)
    }

    /// Supply an explicit persistent probe-cache store.
    #[must_use]
    pub fn with_probe_cache(mut self, store: Arc<dyn ProbeCacheStore>) -> Self {
        self.probe_cache = Some(store);
        self
    }

    /// A read capability for the shared host-lifecycle gate.
    #[must_use]
    pub fn device_io(&self) -> DeviceIoGate {
        self.device_io.clone()
    }

    /// The shared route-opening channel pool derived from this context's
    /// backend.
    #[must_use]
    pub fn channel_pool(&self) -> ChannelPool {
        self.channel_pool.clone()
    }

    /// Apply one explicit semantic command to a standalone light through this
    /// context's backend and lifecycle gate.
    pub async fn apply_light(
        &self,
        route: &DeviceRoute,
        command: LightCommand,
    ) -> Result<(), WriteError> {
        super::light::cancel_light_reapply(route);
        super::light::apply_light(self, route, command).await
    }

    pub(crate) fn set_light_in_background(
        &self,
        target: Option<DeviceRoute>,
        light: &openlogi_core::config::LightSettings,
        capabilities: openlogi_core::device::LightCapabilities,
    ) {
        super::light::set_light_in_background(self, target, light, capabilities);
    }

    pub(crate) fn enumerator(&self) -> Enumerator {
        let enumerator = Enumerator::with_backend(Arc::clone(&self.backend));
        match &self.probe_cache {
            Some(store) => enumerator.with_probe_cache(Arc::clone(store)),
            None => enumerator,
        }
    }

    pub(crate) fn watch_hotplug(&self) -> Result<HotplugStream, InventoryError> {
        openlogi_hid::inventory::hotplug::watch_hotplug(&*self.backend)
    }

    pub(crate) async fn enumerate_standalone(
        &self,
    ) -> Result<Vec<StandaloneDevice>, InventoryError> {
        openlogi_hid::inventory::standalone::enumerate_standalone(&*self.backend).await
    }

    pub(crate) async fn run_pairing(
        &self,
        target: ReceiverSelector,
        commands: mpsc::UnboundedReceiver<PairingCommand>,
        events: mpsc::UnboundedSender<PairingEvent>,
    ) -> Result<(), PairingError> {
        openlogi_hid::pairing::run_pairing(&*self.backend, target, commands, events).await
    }

    pub(super) async fn apply_litra(
        &self,
        route: &DeviceRoute,
        model: LitraModel,
        command: LightCommand,
    ) -> Result<(), WriteError> {
        openlogi_hid::write::apply_litra(&*self.backend, route, model, command).await
    }

    fn from_parts(
        backend: Arc<dyn HidBackend>,
        device_io: DeviceIoGate,
        probe_cache: Option<Arc<dyn ProbeCacheStore>>,
    ) -> Self {
        let channel_pool = ChannelPool::with_backend(Arc::clone(&backend));
        Self {
            backend,
            device_io,
            channel_pool,
            probe_cache,
        }
    }
}
