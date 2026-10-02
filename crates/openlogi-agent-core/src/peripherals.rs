//! Agent-owned catalogs, driver sessions, and durable effect recovery.

pub(crate) mod builtins;
pub mod catalog;
mod controller;
pub mod mapping;
pub mod ownership;
mod packages;
mod session;
pub mod sources;
mod storage;

use std::{path::PathBuf, sync::Arc};

use openlogi_core::{
    config::Config,
    peripheral::{PeripheralError, PluginCommand},
};
use openlogi_hid::peripheral::DiscoveredEndpoint;
use tokio::sync::{mpsc, oneshot, watch};

use crate::{
    observable::ObservableState, runtime::ActionDispatcher, watchers::shutdown::WatcherHandle,
};

/// The existing config, data, and state owners supply every extension path.
#[derive(Clone)]
pub struct Paths {
    /// Conflict-checked user configuration.
    pub config: PathBuf,
    /// Loadable device descriptors.
    pub descriptors: PathBuf,
    /// Immutable installed package content.
    pub packages: PathBuf,
    /// Effect recovery and digest-bound grants.
    pub state: PathBuf,
}

impl Paths {
    /// Resolve the current user's application directories.
    pub fn user() -> Result<Self, PeripheralError> {
        use openlogi_core::paths;
        Ok(Self {
            config: paths::config_path().map_err(config_error)?,
            descriptors: paths::device_descriptors_dir().map_err(config_error)?,
            packages: paths::plugin_packages_dir().map_err(config_error)?,
            state: paths::peripheral_state_dir().map_err(config_error)?,
        })
    }
}

#[derive(Clone)]
struct Desired {
    revision: u64,
    config: Arc<Config>,
}

#[derive(Clone, Default)]
struct Discovery {
    revision: u64,
    result: Option<Result<Vec<DiscoveredEndpoint>, PeripheralError>>,
}

#[derive(Clone, Default)]
struct CameraDiscovery {
    revision: u64,
    result: Option<Result<Vec<openlogi_core::camera::Camera>, PeripheralError>>,
}

enum Command {
    Plan(
        Result<Vec<DiscoveredEndpoint>, PeripheralError>,
        Result<Vec<openlogi_core::camera::Camera>, PeripheralError>,
        oneshot::Sender<Result<DriverPlan, PeripheralError>>,
    ),
    Plugin(
        PluginCommand,
        oneshot::Sender<Result<Option<Config>, PeripheralError>>,
    ),
    Resolve(String, oneshot::Sender<Result<(), PeripheralError>>),
    Retry(
        openlogi_core::peripheral::SessionId,
        oneshot::Sender<Result<(), PeripheralError>>,
    ),
}

/// Catalog admission for one inventory pass, produced before any protocol probe.
pub(crate) struct DriverPlan {
    pub(crate) hidpp: std::collections::HashSet<openlogi_hid::NodeId>,
    pub(crate) standalone: Vec<openlogi_core::device::StandaloneDevice>,
}

/// Coalesced desired state and discovery; no device work occurs before `spawn`.
#[derive(Clone)]
pub struct Handle {
    desired: watch::Sender<Desired>,
    discovery: watch::Sender<Discovery>,
    cameras: watch::Sender<CameraDiscovery>,
    commands: watch::Sender<Option<mpsc::Sender<Command>>>,
}

impl Handle {
    pub(crate) async fn plan(
        &self,
        endpoints: Result<Vec<DiscoveredEndpoint>, PeripheralError>,
        cameras: Result<Vec<openlogi_core::camera::Camera>, PeripheralError>,
    ) -> Result<DriverPlan, PeripheralError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Plan(endpoints, cameras, reply))?;
        response.await.map_err(unavailable)?
    }

    /// Create the control plane before agent arming.
    #[must_use]
    pub fn new(config: &Config) -> Self {
        Self {
            desired: watch::channel(Desired {
                revision: 1,
                config: Arc::new(config.clone()),
            })
            .0,
            discovery: watch::channel(Discovery::default()).0,
            cameras: watch::channel(CameraDiscovery::default()).0,
            commands: watch::channel(None).0,
        }
    }

    /// Reload descriptors and reconcile the current desired revision.
    pub fn reload(&self, config: &Config) {
        self.desired.send_modify(|state| {
            state.revision += 1;
            state.config = Arc::new(config.clone());
        });
    }

    /// Publish an authoritative scan. A failed scan does not prove disconnection.
    pub fn discover(&self, result: Result<Vec<DiscoveredEndpoint>, PeripheralError>) {
        self.discovery.send_if_modified(|state| {
            let equal = match (&state.result, &result) {
                (Some(Ok(old)), Ok(new)) => {
                    old.len() == new.len()
                        && old.iter().zip(new).all(|(a, b)| {
                            a.endpoint == b.endpoint
                                && a.builtin_owner == b.builtin_owner
                                && a.name == b.name
                        })
                }
                (Some(Err(old)), Err(new)) => old == new,
                _ => false,
            };
            if equal {
                return false;
            }
            state.revision += 1;
            state.result = Some(result);
            true
        });
    }

    /// Publish UVC metadata from the shared inventory lifecycle, without opening a media stream.
    pub fn discover_cameras(
        &self,
        result: Result<Vec<openlogi_core::camera::Camera>, PeripheralError>,
    ) {
        self.cameras.send_if_modified(|state| {
            if state.result.as_ref() == Some(&result) {
                return false;
            }
            state.revision += 1;
            state.result = Some(result);
            true
        });
    }

    /// Invalidate pending camera work and reapply volatile controls after system wake.
    pub fn wake(&self) {
        self.cameras.send_modify(|state| state.revision += 1);
    }

    /// Start the peripheral owner as part of the existing watcher fleet.
    #[must_use]
    pub fn spawn(
        &self,
        hardware: crate::hardware::HardwareContext,
        registry: openlogi_hid::ChannelRegistry,
        observable: Arc<ObservableState>,
        dispatcher: ActionDispatcher,
    ) -> WatcherHandle {
        let handle = self.clone();
        let (sender, receiver) = mpsc::channel(16);
        self.commands.send_replace(Some(sender));
        WatcherHandle::spawn("openlogi-peripherals", move |stop| {
            controller::run(
                handle, receiver, hardware, registry, observable, dispatcher, stop,
            )
        })
    }

    /// Execute a reviewed local-package operation. Return a committed config revision if changed.
    pub async fn plugin(&self, command: PluginCommand) -> Result<Option<Config>, PeripheralError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Plugin(command, reply))?;
        response.await.map_err(unavailable)?
    }

    /// Explicitly relinquish a conflicted mapping before capturing a new baseline.
    pub async fn resolve(&self, rule: String) -> Result<(), PeripheralError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Resolve(rule, reply))?;
        response.await.map_err(unavailable)?
    }

    /// Retry only the selected current attachment after a plugin fault.
    pub async fn retry(
        &self,
        session: openlogi_core::peripheral::SessionId,
    ) -> Result<(), PeripheralError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Retry(session, reply))?;
        response.await.map_err(unavailable)?
    }

    fn send(&self, command: Command) -> Result<(), PeripheralError> {
        let sender = self
            .commands
            .borrow()
            .clone()
            .ok_or_else(|| unavailable("agent is not armed"))?;
        sender.try_send(command).map_err(|error| {
            PeripheralError::ResourceLimit(format!("peripheral command queue: {error}"))
        })
    }
}

fn config_error(error: impl std::fmt::Display) -> PeripheralError {
    PeripheralError::ConfigWriteFailed(error.to_string())
}
fn unavailable(error: impl std::fmt::Display) -> PeripheralError {
    PeripheralError::DriverUnavailable(error.to_string())
}
