//! Single owner of peripheral selections, live sessions, and host effects.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::Arc,
    task::Poll,
    time::{SystemTime, UNIX_EPOCH},
};

use openlogi_core::{
    config::Config,
    peripheral::{
        ApplicationStatus, Capability, CapabilityEvidence, CapabilityId, CatalogDiagnostic,
        ConfigScope, ConnectionStatus, ControlId, DriverId, Endpoint, EndpointId, HidUsage,
        InputSource, InputTransition, OperationStatus, PeripheralConfig, PeripheralError,
        PeripheralRecord, PeripheralSnapshot, PluginCommand, SessionId, SettingValue, TargetKind,
        VerificationStatus, native_key,
    },
};
use openlogi_hid::{
    DeviceIoGate,
    native_mapping::{MappingScope, MappingUsage, NativeMapping},
    peripheral::DiscoveredEndpoint,
};
use openlogi_plugin::{
    manifest::Permission,
    runtime::{self, HostRequest, SessionContext, wire},
};
use tokio::sync::{mpsc, oneshot};

use super::{
    Command, Handle, Paths,
    catalog::{Claims, Registration},
    mapping::{EffectKey, FileJournal, MappingManager},
    packages::Packages,
    session::{Broker, Envelope, Event, Guard, PluginSession},
    sources::Sources,
};
use crate::{
    observable::ObservableState, runtime::ActionDispatcher, watchers::shutdown::ManagerCompletion,
};

mod cameras;
mod discovery;
mod effects;
mod events;
mod lifecycle;
mod plugin;

struct Entry {
    record: PeripheralRecord,
    registration: Registration,
    roles: BTreeMap<ControlId, DiscoveredEndpoint>,
    rule: Option<PeripheralConfig>,
    state: DriverState,
    handoff: Option<HandoffTarget>,
}

#[derive(Clone, PartialEq)]
struct HandoffTarget {
    endpoints: Vec<Endpoint>,
    nodes: std::collections::HashSet<openlogi_hid::NodeId>,
    routes: Vec<openlogi_hid::DeviceRoute>,
}

struct Selection {
    registration: Registration,
    roles: BTreeMap<ControlId, DiscoveredEndpoint>,
    handoff: Option<HandoffTarget>,
}

enum DriverState {
    Idle,
    Builtin,
    Native,
    Plugin(Box<PluginSession>),
    SelectionError(PeripheralError),
    Fault(PeripheralError),
}

#[derive(Clone)]
struct DesiredEffect {
    owner: String,
    target: Option<MappingUsage>,
    session: SessionId,
    capability: CapabilityId,
    requests: Vec<u64>,
}

struct Retirement {
    effects: BTreeMap<EffectKey, String>,
    release_transport: bool,
    input_pending: bool,
    io: Option<std::sync::Weak<PeripheralRecord>>,
}

struct Controller {
    handle: Handle,
    desired: super::Desired,
    paths: Paths,
    gate: DeviceIoGate,
    observable: Arc<ObservableState>,
    dispatcher: ActionDispatcher,
    sources: Sources,
    packages: Packages,
    mappings: MappingManager<NativeMapping, FileJournal>,
    claims: Claims,
    entries: BTreeMap<EndpointId, Entry>,
    plugin_effects: BTreeMap<(EffectKey, SessionId), DesiredEffect>,
    retiring: BTreeMap<SessionId, Retirement>,
    generation: u64,
    request: u64,
    event_cursor: usize,
    cameras: cameras::Cameras,
    ownership: super::ownership::Ownership,
    registry: openlogi_hid::ChannelRegistry,
}

pub(super) async fn run(
    handle: Handle,
    mut commands: mpsc::Receiver<Command>,
    hardware: crate::hardware::HardwareContext,
    registry: openlogi_hid::ChannelRegistry,
    observable: Arc<ObservableState>,
    dispatcher: ActionDispatcher,
    mut stop: oneshot::Receiver<()>,
) -> ManagerCompletion {
    let mut gate = hardware.device_io();
    let created = Controller::new(
        handle.clone(),
        gate.clone(),
        Arc::clone(&observable),
        dispatcher,
        hardware.ownership(),
        registry,
    );
    let mut controller = match created {
        Ok(controller) => controller,
        Err(error) => {
            observable.set_peripherals(PeripheralSnapshot {
                diagnostics: vec![CatalogDiagnostic {
                    source: "agent peripheral state".into(),
                    retained: false,
                    error,
                }],
                ..PeripheralSnapshot::default()
            });
            return ManagerCompletion::Unexpected;
        }
    };
    let mut desired = handle.desired.subscribe();
    let mut discovery = handle.discovery.subscribe();
    let mut camera_discovery = handle.cameras.subscribe();
    let mut ownership = controller.ownership.changes();
    let mut inventory = observable.subscribe();
    controller.reload().await;
    loop {
        tokio::select! {
            biased;
            _ = &mut stop => {
                controller.stop_all(wire::DetachReason::Shutdown).await;
                controller.cameras.stop().await;
                controller.restore_all().await;
                return ManagerCompletion::Graceful;
            }
            result = desired.changed() => {
                if result.is_err() { break; }
                controller.reload().await;
            }
            result = discovery.changed() => {
                if result.is_err() { break; }
                controller.reconcile().await;
            }
            result = camera_discovery.changed() => {
                if result.is_err() { break; }
                controller.reconcile_cameras();
                controller.publish();
            }
            result = ownership.changed() => {
                if result.is_err() { break; }
                controller.reconcile().await;
            }
            result = inventory.changed() => {
                if result.is_err() { break; }
                controller.publish();
            }
            changed = gate.changed() => {
                if changed == Some(false) { controller.stop_all(wire::DetachReason::Suspended).await; }
                if changed.is_none() { break; }
                controller.reconcile().await;
            }
            Some(command) = commands.recv() => controller.command(command).await,
            result = controller.cameras.next() => {
                controller.cameras.complete(result, &handle);
                controller.reconcile_cameras();
                controller.publish();
            }
            (id, result) = events::next_event(&mut controller.entries, &mut controller.event_cursor) => {
                controller.event(&id, result).await;
                controller.publish();
            }
        }
    }
    controller.stop_all(wire::DetachReason::Shutdown).await;
    controller.cameras.stop().await;
    controller.restore_all().await;
    ManagerCompletion::Unexpected
}

impl Controller {
    fn new(
        handle: Handle,
        gate: DeviceIoGate,
        observable: Arc<ObservableState>,
        dispatcher: ActionDispatcher,
        ownership: super::ownership::Ownership,
        registry: openlogi_hid::ChannelRegistry,
    ) -> Result<Self, PeripheralError> {
        let paths = Paths::user()?;
        let packages = Packages::load(&paths)?;
        let generation = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(invalid)?
                .as_nanos(),
        )
        .map_err(invalid)?;
        let desired = handle.desired.borrow().clone();
        Ok(Self {
            desired,
            handle,
            gate,
            observable,
            dispatcher,
            packages,
            mappings: MappingManager::new(
                NativeMapping,
                FileJournal::new(paths.state.join("mappings.json")),
            )?,
            sources: Sources::new()?,
            paths,
            claims: Claims::default(),
            entries: BTreeMap::new(),
            plugin_effects: BTreeMap::new(),
            retiring: BTreeMap::new(),
            generation,
            request: 1,
            event_cursor: 0,
            cameras: cameras::Cameras::default(),
            ownership,
            registry,
        })
    }

    fn config(&self) -> Arc<Config> {
        Arc::clone(&self.desired.config)
    }
    fn revision(&self) -> u64 {
        self.desired.revision
    }

    async fn reload(&mut self) {
        self.desired = self.handle.desired.borrow().clone();
        let mut sources = self.sources.clone();
        let config = self.config();
        let paths = self.paths.clone();
        let grants = self.packages.grants().clone();
        let reload = async move {
            sources
                .reload(&config, &paths.descriptors, &paths.packages, &grants)
                .await;
            sources
        };
        tokio::pin!(reload);
        self.sources = loop {
            tokio::select! {
                sources = &mut reload => break sources,
                (id, event) = self.next_event() => { self.event(&id, event).await; self.publish(); }
            }
        };
        self.reconcile().await;
    }

    async fn command(&mut self, command: Command) {
        match command {
            Command::Plan(endpoints, cameras, reply) => {
                let registered = match &endpoints {
                    Ok(endpoints) => self
                        .sources
                        .observe(endpoints, cameras.as_ref().map_or(&[], Vec::as_slice)),
                    Err(error) => Err(error.clone()),
                };
                self.sources
                    .diagnostics
                    .retain(|diagnostic| diagnostic.source != "hardware catalog");
                match &registered {
                    Ok(()) => {
                        self.handle.discover(endpoints);
                        self.handle.discover_cameras(cameras);
                    }
                    Err(error) => {
                        self.diagnostic("hardware catalog".into(), error.clone());
                        self.handle.discover(Err(error.clone()));
                        self.handle.discover_cameras(Err(error.clone()));
                    }
                }
                self.reconcile().await;
                let result = registered.and_then(|()| self.driver_plan());
                let _ = reply.send(result);
            }
            Command::Resolve(rule, reply) => {
                let result = self
                    .mappings
                    .owned_effects(&rule)
                    .into_iter()
                    .try_for_each(|key| self.mappings.resolve_conflict(key));
                if result.is_ok() {
                    self.reconcile_mappings().await;
                    self.publish();
                }
                let _ = reply.send(result);
            }
            Command::Retry(session, reply) => {
                let result = if self.cameras.retry(&session) {
                    Ok(())
                } else if self
                    .entries
                    .get(&session.endpoint)
                    .is_some_and(|entry| entry.record.session == session)
                {
                    if let Some(mut entry) = self.entries.remove(&session.endpoint) {
                        if matches!(
                            entry.state,
                            DriverState::Fault(_) | DriverState::SelectionError(_)
                        ) {
                            self.stop_entry(&mut entry, wire::DetachReason::Replaced)
                                .await;
                        }
                        self.entries.insert(session.endpoint.clone(), entry);
                    }
                    Ok(())
                } else {
                    Err(PeripheralError::StaleSession)
                };
                if result.is_ok() {
                    self.reconcile().await;
                }
                let _ = reply.send(result);
            }
            Command::Plugin(command, reply) => {
                let result = self.package_command(command).await;
                let _ = reply.send(result);
            }
        }
    }

    fn publish(&self) {
        let mut diagnostics = self.sources.diagnostics.clone();
        let mut selected = Vec::new();
        for entry in self
            .entries
            .values()
            .filter(|entry| matches!(entry.state, DriverState::Builtin))
        {
            let nodes = entry
                .roles
                .values()
                .map(|endpoint| endpoint.node().id)
                .collect();
            match self.registry.routes_for_nodes(&nodes) {
                Ok(routes) => selected.extend(
                    routes
                        .into_iter()
                        .map(|route| (route, entry.record.driver.clone())),
                ),
                Err(error) => diagnostics.push(CatalogDiagnostic {
                    source: entry.record.driver.descriptor.to_string(),
                    retained: false,
                    error: PeripheralError::DiscoveryUnavailable(error.to_string()),
                }),
            }
            for endpoint in entry.roles.values() {
                let node = endpoint.node();
                let route =
                    openlogi_hid::DeviceRoute::from(&openlogi_core::device::RawDeviceAddress {
                        vendor_id: node.vendor_id,
                        product_id: node.product_id,
                        usage_page: node.usage_page,
                        usage_id: node.usage_id,
                        identity: node.identity(),
                    });
                selected.push((route, entry.record.driver.clone()));
            }
        }
        let protocols = self.observable.read(|snapshot| {
            super::builtins::inventory(
                &snapshot.inventory,
                &snapshot.standalone,
                &snapshot.peripherals.devices,
                self.generation,
                &selected,
            )
        });
        let mut plugins = self.sources.installed.clone();
        for package in &mut plugins {
            package.rollback_available = package.selection.is_selected()
                && self.packages.can_rollback(&package.driver, &package.digest);
            package.active = self.entries.values().any(|e| {
                e.record.driver.digest.as_deref() == Some(&package.digest)
                    && matches!(e.state, DriverState::Plugin(_))
            });
        }
        self.observable.set_peripherals(PeripheralSnapshot {
            devices: self
                .entries
                .values()
                .filter(|entry| {
                    !openlogi_core::peripheral::builtin::owns_inventory(&entry.record.driver.driver)
                        || !matches!(entry.state, DriverState::Builtin)
                })
                .map(|e| e.record.clone())
                .chain(protocols)
                .chain(self.cameras.records())
                .collect(),
            diagnostics,
            plugins,
        });
    }

    fn diagnostic(&mut self, source: String, error: PeripheralError) {
        self.sources.diagnostics.retain(|d| d.source != source);
        if self.sources.diagnostics.len() >= 256 {
            self.sources.diagnostics.remove(0);
        }
        self.sources.diagnostics.push(CatalogDiagnostic {
            source,
            error,
            retained: false,
        });
    }

    fn reconcile_cameras(&mut self) {
        self.cameras.reconcile(
            cameras::Context {
                handle: &self.handle,
                desired: &self.desired,
                gate: &self.gate,
                catalog: &self.sources.catalog,
            },
            &mut self.generation,
        );
    }

    fn driver_plan(&self) -> Result<super::DriverPlan, PeripheralError> {
        use openlogi_device_registry::driver::BuiltinDriver;
        let mut plan = super::DriverPlan {
            hidpp: HashSet::new(),
            standalone: Vec::new(),
        };
        for entry in self.entries.values().filter(|e| {
            e.record.connection == ConnectionStatus::Online
                && matches!(e.state, DriverState::Builtin)
        }) {
            match BuiltinDriver::find(entry.record.driver.driver.as_ref()) {
                Some(BuiltinDriver::Hidpp) => {
                    plan.hidpp.extend(entry.roles.values().map(|e| e.node().id));
                }
                Some(BuiltinDriver::Litra) => {
                    for endpoint in entry.roles.values() {
                        let node = endpoint.node();
                        let descriptor = openlogi_device_registry::litra::find_litra(
                            node.vendor_id,
                            node.product_id,
                            node.usage_page,
                            node.usage_id,
                        )
                        .ok_or_else(|| invalid("selected Litra identity is unavailable"))?;
                        plan.standalone
                            .push(openlogi_hid::inventory::standalone::describe(
                                &node, descriptor,
                            ));
                    }
                }
                _ => {}
            }
        }
        openlogi_hid::inventory::standalone::validate_no_ambiguous_nodes(&plan.standalone)
            .map_err(|error| PeripheralError::DiscoveryUnavailable(error.to_string()))?;
        Ok(plan)
    }
}

fn facts_for_roles(roles: &BTreeMap<ControlId, DiscoveredEndpoint>) -> Vec<Endpoint> {
    let mut facts: Vec<_> = roles.values().map(|e| e.endpoint.clone()).collect();
    facts.sort_by(|a, b| a.id.cmp(&b.id));
    facts.dedup();
    facts
}

fn extension_values(
    rule: Option<&PeripheralConfig>,
) -> Result<BTreeMap<String, SettingValue>, PeripheralError> {
    let mut settings = rule
        .filter(|r| r.enabled)
        .into_iter()
        .flat_map(|r| r.capabilities.iter())
        .filter(|(id, settings)| id.as_ref().starts_with("extension/") && settings.version == 1);
    let values = settings
        .next()
        .map_or_else(BTreeMap::new, |(_, settings)| settings.values.clone());
    if settings.next().is_some() {
        return Err(invalid(
            "V1 supports one extension settings capability per driver",
        ));
    }
    Ok(values)
}

fn native_keys(
    rule: Option<&PeripheralConfig>,
    permissions: &[Permission],
) -> Result<BTreeMap<ControlId, Option<u16>>, PeripheralError> {
    let mut targets = BTreeMap::new();
    for permission in permissions {
        if let Permission::NativeRemap { controls, .. } = permission {
            for control in controls {
                let mut bindings = rule
                    .filter(|r| r.enabled)
                    .into_iter()
                    .flat_map(|r| r.capabilities.values())
                    .filter(|s| s.version == 1)
                    .filter_map(|s| s.bindings.get(&control.id));
                let action = bindings.next();
                if bindings.next().is_some() {
                    return Err(invalid("native control has multiple capability bindings"));
                }
                let key = action
                    .map(native_key)
                    .transpose()?
                    .map(|key| u16::from(key.code()));
                targets.insert(control.id.clone(), key);
            }
        }
    }
    Ok(targets)
}

fn set_status(record: &mut PeripheralRecord, revision: u64, application: &ApplicationStatus) {
    for capability in record.capabilities.clone() {
        set_capability_status(record, &capability.id, revision, application.clone());
    }
}

fn set_capability_status(
    record: &mut PeripheralRecord,
    capability: &CapabilityId,
    revision: u64,
    application: ApplicationStatus,
) {
    let verification = if application == ApplicationStatus::Applied
        && record
            .capabilities
            .iter()
            .any(|c| &c.id == capability && matches!(c.capability, Capability::InputRemap(_)))
    {
        VerificationStatus::WaitingForPress
    } else {
        VerificationStatus::NotObserved
    };
    let status = OperationStatus {
        capability: capability.clone(),
        revision,
        application,
        verification,
    };
    if let Some(old) = record
        .operations
        .iter_mut()
        .find(|s| &s.capability == capability)
    {
        *old = status;
    } else {
        record.operations.push(status);
    }
}

fn invalid(error: impl std::fmt::Display) -> PeripheralError {
    PeripheralError::InvalidSettings(error.to_string())
}

fn failure_state(error: &PeripheralError) -> DriverState {
    match error {
        PeripheralError::StaleSession
        | PeripheralError::Suspended
        | PeripheralError::DiscoveryUnavailable(_) => DriverState::Idle,
        error => DriverState::Fault(error.clone()),
    }
}
