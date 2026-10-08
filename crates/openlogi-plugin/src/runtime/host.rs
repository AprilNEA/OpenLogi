//! Generated host interfaces validate every guest request before enqueueing I/O.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Instant,
};

use openlogi_core::peripheral::{
    Capability, CapabilityEvidence, CapabilityId, CapabilityRecord, ControlId, Endpoint, HidUsage,
    InputControl, InputRemapCapability, InputSource, PeripheralError, ScopeKind, SettingValue,
    TargetKind, Trigger,
};
use wasmtime::component::{Resource, ResourceTable};

use super::{
    HostRequest, SessionContext,
    bindings::openlogi::peripheral::{diagnostics, hid, native_remap, session, settings, time},
    quota::{Admission, Quota},
    wire,
};
use crate::{
    descriptor::validate_labels,
    limits,
    manifest::{HidOperation, Manifest, Permission},
};

/// An unforgeable resource confined to the component store that issued it.
pub struct EndpointHandle {
    role: ControlId,
    endpoint: Endpoint,
}

pub(super) struct HostState {
    pub(super) context: SessionContext,
    pub(super) quota: Quota,
    pub(super) revoked: bool,
    manifest: Arc<Manifest>,
    resources: ResourceTable,
    handles: usize,
    started: Instant,
    next_request: u64,
    requests: Vec<HostRequest>,
    pending_io: BTreeSet<u64>,
    pending_commands: BTreeSet<u64>,
    timers: BTreeSet<u64>,
    subscriptions: BTreeMap<ControlId, Vec<u8>>,
    capabilities: BTreeMap<CapabilityId, Capability>,
    held: BTreeSet<(CapabilityId, ControlId)>,
}

impl HostState {
    pub(super) fn new(
        manifest: Arc<Manifest>,
        context: SessionContext,
        admission: Arc<Admission>,
    ) -> Result<Self, PeripheralError> {
        if context.endpoints.len() > 64
            || context.permissions.iter().any(|p| {
                !manifest.permissions.contains(p) || !context.endpoints.contains_key(p.endpoint())
            })
        {
            return Err(PeripheralError::PermissionDenied(
                "session grants exceed package requests or claimed roles".into(),
            ));
        }
        Ok(Self {
            context,
            quota: Quota::new(admission)?,
            revoked: false,
            manifest,
            resources: ResourceTable::new(),
            handles: 0,
            started: Instant::now(),
            next_request: 1,
            requests: Vec::new(),
            pending_io: BTreeSet::new(),
            pending_commands: BTreeSet::new(),
            timers: BTreeSet::new(),
            subscriptions: BTreeMap::new(),
            capabilities: BTreeMap::new(),
            held: BTreeSet::new(),
        })
    }

    pub(super) fn take_requests(&mut self) -> Vec<HostRequest> {
        std::mem::take(&mut self.requests)
    }

    fn enqueue(&mut self, request: HostRequest) -> Result<(), PeripheralError> {
        if self.revoked {
            return Err(PeripheralError::PermissionDenied(
                "session is revoked".into(),
            ));
        }
        if self.requests.len() >= 256 {
            return Err(PeripheralError::ResourceLimit(
                "256 pending session events".into(),
            ));
        }
        self.requests.push(request);
        Ok(())
    }

    fn next_io(&mut self) -> Result<u64, PeripheralError> {
        self.quota.io()?;
        if self.revoked {
            return Err(PeripheralError::PermissionDenied(
                "session is revoked".into(),
            ));
        }
        if self.pending_io.len() >= 16 {
            return Err(PeripheralError::ResourceLimit(
                "16 outstanding I/O requests".into(),
            ));
        }
        let id = self.next_request;
        self.next_request += 1;
        self.pending_io.insert(id);
        Ok(id)
    }

    fn permit(
        &self,
        role: &ControlId,
        operation: HidOperation,
        id: u8,
        bytes: usize,
    ) -> Result<(), wire::Failure> {
        let allowed = self.context.permissions.iter().any(|p| match p {
            Permission::Hid {
                endpoint,
                operations,
                report_ids,
                max_report_bytes,
            } => {
                endpoint == role
                    && operations.contains(&operation)
                    && report_ids.contains(&id)
                    && bytes <= *max_report_bytes as usize
            }
            Permission::NativeRemap { .. } => false,
        });
        let observed = self.context.endpoints.get(role).is_some_and(|e| {
            let limit = match operation {
                HidOperation::Input => e.max_report_bytes,
                HidOperation::Output => e.max_output_report_bytes,
                HidOperation::FeatureRead | HidOperation::FeatureWrite => {
                    e.max_feature_report_bytes
                }
            };
            limit.is_none_or(|n| bytes <= n as usize)
                && (e.report_ids.is_empty() || e.report_ids.contains(&id))
        });
        if self.revoked || !allowed || !observed {
            return Err(denied(
                "HID request exceeds the session grant or endpoint limits",
            ));
        }
        Ok(())
    }

    pub(super) fn accept_capabilities(
        &mut self,
        records: &[wire::Capability],
    ) -> Result<Vec<CapabilityRecord>, PeripheralError> {
        if records.len() > limits::FIELDS {
            return Err(invalid("too many capabilities"));
        }
        let mut result = Vec::new();
        let mut native_controls = BTreeSet::new();
        let mut extension = false;
        for record in records {
            let id =
                CapabilityId::try_new(record.id.clone()).map_err(|e| invalid(&e.to_string()))?;
            let capability = if record.version == 1 {
                match &record.kind {
                    wire::CapabilityKind::InputRemap(controls)
                        if id.as_ref().starts_with("input-remap/") =>
                    {
                        if controls.is_empty() || controls.len() > limits::FIELDS {
                            return Err(invalid("invalid control count"));
                        }
                        let mut ids = BTreeSet::new();
                        let mut native = None;
                        let controls = controls.iter().map(|control| {
                        let id = ControlId::try_new(control.id.clone()).map_err(|e| invalid(&e.to_string()))?;
                        if !ids.insert(id.clone()) { return Err(invalid("duplicate control")); }
                        let labels: BTreeMap<_, _> = control.labels.iter().map(|label| (label.locale.clone(), label.text.clone())).collect();
                        if labels.len() != control.labels.len() { return Err(invalid("duplicate locale")); }
                        validate_labels(&labels).map_err(|e| invalid(&e.to_string()))?;
                        let is_native = control.source.is_some();
                        if native.is_some_and(|prior| prior != is_native) { return Err(invalid("one capability cannot mix native and synthesized controls")); }
                        native = Some(is_native);
                        let trigger = match control.trigger { wire::Trigger::ShortPress => Trigger::ShortPress, wire::Trigger::PressRelease => Trigger::PressRelease };
                        let source = if let Some(usage) = control.source {
                            if !native_controls.insert(id.clone()) { return Err(invalid("native control belongs to multiple capabilities")); }
                            let usage = HidUsage { page: usage.page, usage: usage.usage };
                            if !self.context.permissions.iter().any(|p| matches!(p, Permission::NativeRemap { controls, .. } if controls.iter().any(|c| c.id == id && c.source.usage() == usage && c.trigger == trigger))) {
                                return Err(PeripheralError::PermissionDenied("undeclared native source".into()));
                            }
                            InputSource::HidUsage(usage)
                        } else { InputSource::Logical };
                        Ok(InputControl { id, labels, source, trigger, recommended_key: None })
                    }).collect::<Result<Vec<_>, PeripheralError>>()?;
                        Capability::InputRemap(InputRemapCapability {
                            controls,
                            targets: if native == Some(true) {
                                TargetKind::KeyboardKey
                            } else {
                                TargetKind::Action
                            },
                            per_app: false,
                        })
                    }
                    wire::CapabilityKind::Extension if id.as_ref().starts_with("extension/") => {
                        if extension {
                            return Err(invalid(
                                "V1 exposes one extension settings capability per driver",
                            ));
                        }
                        extension = true;
                        Capability::Extension(self.manifest.settings.clone())
                    }
                    _ => Capability::Unsupported,
                }
            } else {
                Capability::Unsupported
            };
            let unsupported = capability == Capability::Unsupported;
            if unsupported && record.required {
                return Err(PeripheralError::Unsupported(format!(
                    "required capability {} version {}",
                    record.id, record.version
                )));
            }
            if self
                .capabilities
                .insert(id.clone(), capability.clone())
                .is_some()
            {
                return Err(invalid("duplicate capability ID"));
            }
            let scopes = if matches!(&capability, Capability::InputRemap(c) if c.targets == TargetKind::KeyboardKey)
            {
                vec![ScopeKind::Model]
            } else {
                vec![ScopeKind::Model, ScopeKind::Session]
            };
            result.push(CapabilityRecord {
                id,
                version: record.version,
                capability,
                scopes,
                unavailable: unsupported
                    .then(|| PeripheralError::Unsupported("capability version or family".into())),
                evidence: CapabilityEvidence::Declared,
                values: BTreeMap::new(),
            });
        }
        Ok(result)
    }

    pub(super) fn begin_request(&mut self, request: &wire::Request) -> Result<(), PeripheralError> {
        let id = CapabilityId::try_new(request.capability.clone())
            .map_err(|e| invalid(&e.to_string()))?;
        match (self.capabilities.get(&id), &request.command) {
            (Some(Capability::Extension(fields)), wire::Command::Settings(values)) => {
                let values = from_wire_settings(values)?;
                self.context.settings = crate::manifest::validate_values(fields, &values, true)
                    .map_err(|e| invalid(&e.to_string()))?;
            }
            (Some(Capability::InputRemap(cap)), wire::Command::Key(binding))
                if cap.targets == TargetKind::KeyboardKey =>
            {
                let control = ControlId::try_new(binding.control.clone())
                    .map_err(|e| invalid(&e.to_string()))?;
                if !cap.controls.iter().any(|c| c.id == control) {
                    return Err(invalid("undeclared native control"));
                }
                self.context.native_keys.insert(control, binding.key);
            }
            _ => return Err(invalid("command does not match the capability contract")),
        }
        if request.id == 0
            || self.pending_commands.len() >= 16
            || !self.pending_commands.insert(request.id)
        {
            return Err(invalid("duplicate or excessive command"));
        }
        Ok(())
    }

    pub(super) fn accept_event(&mut self, event: &wire::Event) -> Result<(), PeripheralError> {
        match event {
            wire::Event::Report(report) => {
                let role = ControlId::try_new(report.endpoint.clone())
                    .map_err(|e| invalid(&e.to_string()))?;
                if report.report.kind != wire::ReportKind::Input
                    || !self
                        .subscriptions
                        .get(&role)
                        .is_some_and(|ids| ids.contains(&report.report.id))
                {
                    return Err(invalid("unsubscribed input report"));
                }
                self.permit(
                    &role,
                    HidOperation::Input,
                    report.report.id,
                    report.report.payload.len() + 1,
                )
                .map_err(|error| guest_error(&error))?;
            }
            wire::Event::Completed(completion) => {
                if !self.pending_io.remove(&completion.request) {
                    return Err(invalid("unknown or duplicate host completion"));
                }
            }
            wire::Event::Timer(id) => {
                if !self.timers.remove(id) {
                    return Err(invalid("unknown or duplicate timer"));
                }
            }
            wire::Event::Resumed => {}
        }
        Ok(())
    }

    pub(super) fn accept_updates(
        &mut self,
        updates: &[wire::Update],
    ) -> Result<(), PeripheralError> {
        if updates.len() > 256 {
            return Err(PeripheralError::ResourceLimit(
                "256 guest updates per call".into(),
            ));
        }
        for update in updates {
            match update {
                wire::Update::Input(input) => {
                    let capability = CapabilityId::try_new(input.capability.clone())
                        .map_err(|e| invalid(&e.to_string()))?;
                    let control = ControlId::try_new(input.control.clone())
                        .map_err(|e| invalid(&e.to_string()))?;
                    let Some(Capability::InputRemap(cap)) = self.capabilities.get(&capability)
                    else {
                        return Err(invalid("input references an undeclared capability"));
                    };
                    let Some(source) = cap
                        .controls
                        .iter()
                        .find(|c| c.id == control && c.source == InputSource::Logical)
                    else {
                        return Err(invalid("input references an undeclared logical control"));
                    };
                    let key = (capability, control);
                    match (source.trigger, input.transition) {
                        (Trigger::ShortPress, wire::Transition::Trigger) => {}
                        (Trigger::PressRelease, wire::Transition::Press)
                            if self.held.insert(key.clone()) => {}
                        (Trigger::PressRelease, wire::Transition::Release)
                            if self.held.remove(&key) => {}
                        _ => {
                            return Err(invalid(
                                "input transition violates the declared trigger or held state",
                            ));
                        }
                    }
                }
                wire::Update::Completed(completion) => {
                    if !self.pending_commands.remove(&completion.request) {
                        return Err(invalid("unknown or duplicate command completion"));
                    }
                }
                wire::Update::Setting(setting) => {
                    let Some(field) = self.manifest.settings.get(&setting.key) else {
                        return Err(invalid("undeclared setting observation"));
                    };
                    field.validate(&from_wire_value(&setting.value))?;
                }
            }
        }
        Ok(())
    }
}

impl session::HostEndpoint for HostState {
    fn facts(
        &mut self,
        endpoint: Resource<EndpointHandle>,
    ) -> wasmtime::Result<wire::EndpointFacts> {
        let handle = self.resources.get(&endpoint)?;
        Ok(wire::EndpointFacts {
            role: handle.role.to_string(),
            vendor: handle.endpoint.vendor_id,
            product: handle.endpoint.product_id,
            collection: handle.endpoint.collection.map(|u| wire::Usage {
                page: u.page,
                usage: u.usage,
            }),
            max_report_bytes: handle.endpoint.max_report_bytes,
        })
    }
    fn drop(&mut self, endpoint: Resource<EndpointHandle>) -> wasmtime::Result<()> {
        self.resources.delete(endpoint)?;
        self.handles -= 1;
        Ok(())
    }
}

impl session::Host for HostState {
    fn endpoints(&mut self) -> wasmtime::Result<Vec<Resource<EndpointHandle>>> {
        if self.revoked || self.handles + self.context.endpoints.len() > 64 {
            return Err(PeripheralError::ResourceLimit("64 endpoint handles".into()).into());
        }
        let mut handles = Vec::new();
        for (role, endpoint) in &self.context.endpoints {
            handles.push(self.resources.push(EndpointHandle {
                role: role.clone(),
                endpoint: endpoint.clone(),
            })?);
            self.handles += 1;
        }
        Ok(handles)
    }
}

impl hid::Host for HostState {
    fn subscribe(
        &mut self,
        device: Resource<EndpointHandle>,
        report_ids: Vec<u8>,
    ) -> wasmtime::Result<Result<(), wire::Failure>> {
        let role = self.resources.get(&device)?.role.clone();
        if report_ids.is_empty() || report_ids.len() > 256 {
            return Ok(Err(denied("invalid subscription")));
        }
        for id in &report_ids {
            if let Err(error) = self.permit(&role, HidOperation::Input, *id, 1) {
                return Ok(Err(error));
            }
        }
        self.quota.io()?;
        self.subscriptions.insert(role.clone(), report_ids.clone());
        self.enqueue(HostRequest::Subscribe { role, report_ids })?;
        Ok(Ok(()))
    }
    fn submit(
        &mut self,
        device: Resource<EndpointHandle>,
        report: wire::Report,
    ) -> wasmtime::Result<Result<u64, wire::Failure>> {
        let role = self.resources.get(&device)?.role.clone();
        let operation = match report.kind {
            wire::ReportKind::Input => return Ok(Err(denied("input reports cannot be submitted"))),
            wire::ReportKind::Output => HidOperation::Output,
            wire::ReportKind::FeatureRead => HidOperation::FeatureRead,
            wire::ReportKind::FeatureWrite => HidOperation::FeatureWrite,
        };
        if let Err(error) = self.permit(&role, operation, report.id, report.payload.len() + 1) {
            return Ok(Err(error));
        }
        let request = self.next_io()?;
        self.enqueue(HostRequest::Hid {
            request,
            role,
            report,
        })?;
        Ok(Ok(request))
    }
}

impl native_remap::Host for HostState {
    fn set_key(
        &mut self,
        control: String,
        key: Option<u16>,
    ) -> wasmtime::Result<Result<u64, wire::Failure>> {
        let Ok(control) = ControlId::try_new(control) else {
            return Ok(Err(denied("invalid control")));
        };
        if self.context.native_keys.get(&control) != Some(&key)
            || !self.context.permissions.iter().any(|p| matches!(p, Permission::NativeRemap { controls, .. } if controls.iter().any(|c| c.id == control))) {
            return Ok(Err(denied("native mapping differs from the user's binding or grant")));
        }
        let request = self.next_io()?;
        self.enqueue(HostRequest::NativeRemap {
            request,
            control,
            key,
        })?;
        Ok(Ok(request))
    }
}

impl time::Host for HostState {
    fn monotonic_ms(&mut self) -> wasmtime::Result<u64> {
        Ok(u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX))
    }
    fn schedule(&mut self, after_ms: u32) -> wasmtime::Result<Result<u64, wire::Failure>> {
        if after_ms == 0 || after_ms > 3_600_000 {
            return Ok(Err(denied("timer delay must be 1–3600000 ms")));
        }
        self.quota.io()?;
        if self.timers.len() >= 32 {
            return Err(PeripheralError::ResourceLimit("32 outstanding timers".into()).into());
        }
        let id = self.next_request;
        self.next_request += 1;
        self.timers.insert(id);
        self.enqueue(HostRequest::Timer { id, after_ms })?;
        Ok(Ok(id))
    }
}

impl settings::Host for HostState {
    fn read(&mut self) -> wasmtime::Result<Vec<wire::Setting>> {
        Ok(to_wire_settings(&self.context.settings))
    }
}

impl diagnostics::Host for HostState {
    fn emit(&mut self, level: diagnostics::Level, message: String) -> wasmtime::Result<()> {
        self.quota.diagnostic()?;
        if message.len() > 4096 {
            return Err(PeripheralError::ResourceLimit("4096 diagnostic bytes".into()).into());
        }
        let message = message.chars().flat_map(char::escape_debug).collect();
        self.enqueue(HostRequest::Diagnostic { level, message })?;
        Ok(())
    }
}

impl super::bindings::openlogi::peripheral::types::Host for HostState {}

/// Encode validated scalar settings for the component ABI.
#[must_use]
pub fn to_wire_settings(values: &BTreeMap<String, SettingValue>) -> Vec<wire::Setting> {
    values
        .iter()
        .map(|(key, value)| wire::Setting {
            key: key.clone(),
            value: match value {
                SettingValue::Boolean(v) => wire::Value::Boolean(*v),
                SettingValue::Integer(v) => wire::Value::Integer(*v),
                SettingValue::Number(v) => wire::Value::Number(*v),
                SettingValue::Text(v) => wire::Value::Text(v.clone()),
            },
        })
        .collect()
}

/// Decode bounded, uniquely keyed scalar settings from the component ABI.
pub fn from_wire_settings(
    values: &[wire::Setting],
) -> Result<BTreeMap<String, SettingValue>, PeripheralError> {
    if values.len() > limits::FIELDS {
        return Err(invalid("too many settings"));
    }
    let mut result = BTreeMap::new();
    for value in values {
        if result
            .insert(value.key.clone(), from_wire_value(&value.value))
            .is_some()
        {
            return Err(invalid("duplicate setting"));
        }
    }
    Ok(result)
}

fn from_wire_value(value: &wire::Value) -> SettingValue {
    match value {
        wire::Value::Boolean(v) => SettingValue::Boolean(*v),
        wire::Value::Integer(v) => SettingValue::Integer(*v),
        wire::Value::Number(v) => SettingValue::Number(*v),
        wire::Value::Text(v) => SettingValue::Text(v.clone()),
    }
}

pub(super) fn guest_error(error: &wire::Failure) -> PeripheralError {
    let message: String = error
        .message
        .chars()
        .take(4096)
        .flat_map(char::escape_debug)
        .collect();
    match error.kind {
        wire::ErrorKind::PermissionDenied => PeripheralError::PermissionDenied(message),
        wire::ErrorKind::Unavailable => PeripheralError::Unsupported(message),
        wire::ErrorKind::ResourceLimit => PeripheralError::ResourceLimit(message),
        _ => PeripheralError::PluginFault(message),
    }
}

fn denied(message: &str) -> wire::Failure {
    wire::Failure {
        kind: wire::ErrorKind::PermissionDenied,
        message: message.into(),
    }
}
fn invalid(message: &str) -> PeripheralError {
    PeripheralError::InvalidSettings(message.into())
}
