//! Serialized native UVC work, separate from plugin calls and the input hook.

use super::{
    ApplicationStatus, Arc, BTreeMap, BTreeSet, Capability, CapabilityEvidence, ConnectionStatus,
    DeviceIoGate, EndpointId, Handle, PeripheralError, PeripheralRecord, SessionId, set_status,
};
use openlogi_core::{
    camera::{Camera, CameraState, ControlError},
    config::CameraControls,
};
use tokio::task::{Id, JoinError, JoinSet};

#[derive(Default, PartialEq)]
enum Attempt {
    #[default]
    Untried,
    Read,
    Apply(CameraControls),
}

impl From<&Option<CameraControls>> for Attempt {
    fn from(settings: &Option<CameraControls>) -> Self {
        settings
            .as_ref()
            .map_or(Self::Read, |settings| Self::Apply(settings.clone()))
    }
}

struct CameraEntry {
    record: PeripheralRecord,
    attempted: Attempt,
    pending: Option<Id>,
}

impl CameraEntry {
    fn reconcile_selection(&mut self, context: Context<'_>) -> bool {
        let Context {
            desired,
            catalog,
            gate,
            ..
        } = context;
        let selected = select(catalog, desired, &self.record);
        let registration = match selected {
            Ok(registration) => registration,
            Err(error) => {
                self.attempted = Attempt::Untried;
                self.record.driver_error = Some(error.clone());
                set_status(
                    &mut self.record,
                    desired.revision,
                    &ApplicationStatus::Failed(error),
                );
                return false;
            }
        };
        if self.record.driver != registration.selection {
            self.record.driver = registration.selection.clone();
            self.attempted = Attempt::Untried;
        }
        self.record.model = registration.descriptor.model().clone();
        self.record.driver_error = None;
        match desired.config.peripheral_rule(&self.record) {
            Ok(Some(rule)) if !rule.enabled => {
                self.attempted = Attempt::Untried;
                set_status(
                    &mut self.record,
                    desired.revision,
                    &ApplicationStatus::Disabled,
                );
                return false;
            }
            Err(error) => {
                self.attempted = Attempt::Untried;
                self.record.driver_error = Some(error.clone());
                set_status(
                    &mut self.record,
                    desired.revision,
                    &ApplicationStatus::Failed(error),
                );
                return false;
            }
            _ => {}
        }
        if !gate.allows_io() {
            self.attempted = Attempt::Untried;
            set_status(
                &mut self.record,
                desired.revision,
                &ApplicationStatus::Failed(PeripheralError::Suspended),
            );
            return false;
        }
        true
    }
}

pub(super) struct Completion {
    session: SessionId,
    revision: u64,
    discovery: u64,
    state: Option<CameraState>,
    result: Result<(), PeripheralError>,
}

type CameraWork = dyn Fn(
        &Camera,
        Option<&CameraControls>,
        &dyn Fn() -> Result<(), PeripheralError>,
        &mut Option<CameraState>,
    ) -> Result<(), PeripheralError>
    + Send
    + Sync;

#[derive(Clone, Copy)]
pub(super) struct Context<'a> {
    pub(super) handle: &'a Handle,
    pub(super) desired: &'a super::super::Desired,
    pub(super) gate: &'a DeviceIoGate,
    pub(super) catalog: &'a super::super::catalog::Catalog,
}

pub(super) struct Cameras {
    entries: BTreeMap<EndpointId, CameraEntry>,
    work: JoinSet<Completion>,
    discovery: u64,
    io: Arc<CameraWork>,
}

impl Default for Cameras {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            work: JoinSet::new(),
            discovery: 0,
            io: Arc::new(apply),
        }
    }
}

impl Cameras {
    pub(super) fn records(&self) -> impl Iterator<Item = PeripheralRecord> + '_ {
        self.entries.values().map(|entry| entry.record.clone())
    }

    pub(super) fn reconcile(&mut self, context: Context<'_>, generation: &mut u64) {
        let Context {
            handle,
            desired,
            gate,
            ..
        } = context;
        let discovery = handle.cameras.borrow().clone();
        let Some(result) = discovery.result else {
            return;
        };
        let cameras = match result {
            Ok(cameras) => cameras,
            Err(error) => {
                for entry in self.entries.values_mut() {
                    entry.record.connection = ConnectionStatus::Unavailable;
                    set_status(
                        &mut entry.record,
                        desired.revision,
                        &ApplicationStatus::Failed(error.clone()),
                    );
                }
                return;
            }
        };
        if discovery.revision != self.discovery {
            for entry in self.entries.values_mut() {
                entry.attempted = Attempt::Untried;
            }
            self.discovery = discovery.revision;
        }
        let mut seen = BTreeSet::new();
        for camera in cameras {
            let canonical = camera.config_key();
            let key = desired
                .config
                .legacy_camera_key(&canonical, &camera.unique_id)
                .unwrap_or(canonical);
            let entry = observe(&mut self.entries, &camera, generation);
            seen.insert(entry.record.session.endpoint.clone());
            entry.record.connection = ConnectionStatus::Online;
            if !entry.reconcile_selection(context) {
                continue;
            }
            let settings = desired
                .config
                .device_enabled(&key)
                .then(|| desired.config.camera_controls(&key))
                .flatten();
            if entry.pending.is_some() || entry.attempted == Attempt::from(&settings) {
                continue;
            }
            if self.work.len() >= 16 {
                set_status(
                    &mut entry.record,
                    desired.revision,
                    &ApplicationStatus::Failed(PeripheralError::ResourceLimit(
                        "camera work queue is full".into(),
                    )),
                );
                continue;
            }
            entry.attempted = Attempt::from(&settings);
            set_status(
                &mut entry.record,
                desired.revision,
                &ApplicationStatus::Pending,
            );
            let session = entry.record.session.clone();
            let revision = desired.revision;
            let observed = self.discovery;
            let handle = handle.clone();
            let gate = gate.clone();
            let io = Arc::clone(&self.io);
            let task = self.work.spawn_blocking(move || {
                let check = || {
                    gate.ensure_allowed()
                        .map_err(|_| PeripheralError::Suspended)?;
                    if handle.desired.borrow().revision != revision
                        || handle.cameras.borrow().revision != observed
                    {
                        return Err(PeripheralError::StaleSession);
                    }
                    Ok(())
                };
                let mut state = None;
                let result = io(&camera, settings.as_ref(), &check, &mut state);
                Completion {
                    session,
                    revision,
                    discovery: observed,
                    state,
                    result,
                }
            });
            entry.pending = Some(task.id());
        }
        self.disconnect_absent(&seen, desired);
    }

    fn disconnect_absent(&mut self, seen: &BTreeSet<EndpointId>, desired: &super::super::Desired) {
        for (id, entry) in &mut self.entries {
            if !seen.contains(id) {
                entry.record.connection = ConnectionStatus::Offline;
                for capability in &mut entry.record.capabilities {
                    capability.evidence = CapabilityEvidence::LastKnown;
                }
                set_status(
                    &mut entry.record,
                    desired.revision,
                    &ApplicationStatus::Failed(PeripheralError::Offline),
                );
            }
        }
        self.entries.retain(|_, entry| {
            entry.record.connection != ConnectionStatus::Offline
                || entry.pending.is_some()
                || metadata(&entry.record)
                    .is_some_and(|camera| desired.config.devices.contains_key(&camera.config_key()))
        });
    }

    pub(super) async fn next(&mut self) -> Result<(Id, Completion), JoinError> {
        match self.work.join_next_with_id().await {
            Some(completion) => completion,
            None => std::future::pending().await,
        }
    }

    pub(super) fn complete(
        &mut self,
        completion: Result<(Id, Completion), JoinError>,
        handle: &Handle,
    ) {
        let (id, completion) = match completion {
            Ok(completion) => completion,
            Err(error) => {
                for entry in self
                    .entries
                    .values_mut()
                    .filter(|entry| entry.pending == Some(error.id()))
                {
                    entry.pending = None;
                    set_status(
                        &mut entry.record,
                        handle.desired.borrow().revision,
                        &ApplicationStatus::Failed(PeripheralError::ReadFailed(error.to_string())),
                    );
                }
                return;
            }
        };
        let Some(entry) = self
            .entries
            .get_mut(&completion.session.endpoint)
            .filter(|entry| entry.pending == Some(id))
        else {
            return;
        };
        entry.pending = None;
        if entry.record.session != completion.session
            || handle.desired.borrow().revision != completion.revision
            || handle.cameras.borrow().revision != completion.discovery
        {
            entry.attempted = Attempt::Untried;
            return;
        }
        if let Some(capability) = entry.record.capabilities.first_mut() {
            if let Capability::Camera(camera) = &mut capability.capability {
                camera.state = completion.state;
            }
            if matches!(&capability.capability, Capability::Camera(camera) if camera.state.is_some())
            {
                capability.evidence = CapabilityEvidence::Probed;
            }
        }
        entry.record.driver_error = completion.result.as_ref().err().cloned();
        set_status(
            &mut entry.record,
            completion.revision,
            &match completion.result {
                Ok(()) if entry.attempted == Attempt::Read => ApplicationStatus::Disabled,
                Ok(()) => ApplicationStatus::Applied,
                Err(error) => ApplicationStatus::Failed(error),
            },
        );
    }

    pub(super) fn retry(&mut self, session: &SessionId) -> bool {
        let Some(entry) = self
            .entries
            .get_mut(&session.endpoint)
            .filter(|entry| entry.record.session == *session)
        else {
            return false;
        };
        entry.attempted = Attempt::Untried;
        true
    }

    pub(super) async fn stop(&mut self) {
        // Native calls own their buffers and handles. Replacement waits for submitted calls to finish.
        while self.work.join_next().await.is_some() {}
    }
}

fn select(
    catalog: &super::super::catalog::Catalog,
    desired: &super::super::Desired,
    record: &PeripheralRecord,
) -> Result<super::super::catalog::Registration, PeripheralError> {
    use super::super::catalog::SelectionContext;
    let selected = catalog
        .select_with_config(
            &record.endpoints,
            &desired.config,
            SelectionContext {
                session: &record.session,
                current: Some(record),
                builtin_owner: Some(&record.driver.driver),
            },
        )?
        .ok_or_else(|| {
            PeripheralError::DriverUnavailable("camera registration is unavailable".into())
        })?;
    if openlogi_core::peripheral::builtin::BuiltinDriver::find(
        selected.registration.selection.driver.as_ref(),
    ) != Some(openlogi_core::peripheral::builtin::BuiltinDriver::Camera)
    {
        return Err(PeripheralError::Unsupported(
            "this plugin requires a UVC host service that V1 does not expose".into(),
        ));
    }
    Ok(selected.registration.clone())
}

fn observe<'a>(
    entries: &'a mut BTreeMap<EndpointId, CameraEntry>,
    camera: &Camera,
    generation: &mut u64,
) -> &'a mut CameraEntry {
    let mut record = super::super::builtins::camera(camera.clone(), *generation);
    let id = record.session.endpoint.clone();
    match entries.entry(id) {
        std::collections::btree_map::Entry::Occupied(mut slot) => {
            if slot.get().record.connection == ConnectionStatus::Offline
                || metadata(&slot.get().record).is_none_or(|old| {
                    (old.vendor_id, old.product_id) != (camera.vendor_id, camera.product_id)
                })
            {
                *generation += 1;
                record.session.generation = *generation;
                slot.get_mut().record = record;
                slot.get_mut().attempted = Attempt::Untried;
            } else {
                let entry = slot.get_mut();
                entry.record.name.clone_from(&camera.name);
                entry.record.physical = record.physical;
                entry.record.scopes = record.scopes;
                if let Some(capability) = entry.record.capabilities.first_mut()
                    && let Capability::Camera(metadata) = &mut capability.capability
                {
                    metadata.camera = camera.clone();
                    capability.scopes.clone_from(&entry.record.scopes);
                }
            }
            slot.into_mut()
        }
        std::collections::btree_map::Entry::Vacant(slot) => {
            *generation += 1;
            record.session.generation = *generation;
            slot.insert(CameraEntry {
                record,
                attempted: Attempt::Untried,
                pending: None,
            })
        }
    }
}

fn metadata(record: &PeripheralRecord) -> Option<&Camera> {
    record
        .capabilities
        .iter()
        .find_map(|c| match &c.capability {
            Capability::Camera(camera) => Some(&camera.camera),
            _ => None,
        })
}

fn apply(
    camera: &Camera,
    settings: Option<&CameraControls>,
    check: &dyn Fn() -> Result<(), PeripheralError>,
    state: &mut Option<CameraState>,
) -> Result<(), PeripheralError> {
    check()?;
    let observed = openlogi_camera::read_camera_state(&camera.unique_id).map_err(read_error)?;
    let Some(settings) = settings else {
        *state = Some(observed);
        return check();
    };
    let changes = observed
        .changes(settings)
        .map_err(|e| PeripheralError::InvalidSettings(e.to_string()))?;
    *state = Some(observed);
    if changes.is_empty() {
        return check();
    }
    check()?;
    let result =
        openlogi_camera::apply_settings(&camera.unique_id, &changes.autos, &changes.values)
            .map_err(|e| PeripheralError::WriteFailed(e.to_string()));
    // A partial batch can change auto modes before a later write fails. Always reread that state.
    *state = None;
    let observed = openlogi_camera::read_camera_state(&camera.unique_id).map_err(read_error)?;
    let matches = observed
        .changes(settings)
        .map_err(|e| PeripheralError::InvalidSettings(e.to_string()))?
        .is_empty();
    *state = Some(observed);
    check()?;
    result?;
    if !matches {
        return Err(PeripheralError::ReadbackMismatch);
    }
    Ok(())
}

fn read_error(error: ControlError) -> PeripheralError {
    match error {
        ControlError::NotFound => PeripheralError::Offline,
        ControlError::Unsupported => PeripheralError::Unsupported("camera controls".into()),
        error => PeripheralError::ReadFailed(error.to_string()),
    }
}

#[cfg(test)]
mod tests;
