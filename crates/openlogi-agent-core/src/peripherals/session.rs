//! Per-session HID brokers, cancellation, and bounded ordered guest input.

use std::{
    collections::BTreeMap,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use openlogi_core::peripheral::{
    ControlId, PeripheralConfig, PeripheralError, PeripheralRecord, SessionId,
};
use openlogi_hid::{
    DeviceIoGate,
    peripheral::{DiscoveredEndpoint, Report},
};
use openlogi_plugin::{
    manifest::{HidOperation, Permission},
    runtime::{HostRequest, Session, wire},
};
use tokio::{
    sync::{mpsc, watch},
    task::{AbortHandle, JoinSet},
};

use super::Handle;

#[derive(Clone)]
pub(super) struct Guard {
    handle: Handle,
    record: Arc<PeripheralRecord>,
    rule: Option<PeripheralConfig>,
    gate: DeviceIoGate,
    halted: watch::Sender<Option<PeripheralError>>,
}

impl Guard {
    pub(super) fn new(
        handle: Handle,
        record: &PeripheralRecord,
        rule: Option<PeripheralConfig>,
        gate: DeviceIoGate,
    ) -> Self {
        Self {
            handle,
            record: Arc::new(record.clone()),
            rule,
            gate,
            halted: watch::channel(None).0,
        }
    }

    pub(super) fn check(&self) -> Result<(), PeripheralError> {
        if let Some(error) = &*self.halted.borrow() {
            return Err(error.clone());
        }
        self.gate
            .ensure_allowed()
            .map_err(|_| PeripheralError::Suspended)?;
        let desired = self.handle.desired.borrow();
        if let Some(digest) = &self.record.driver.digest
            && desired
                .config
                .plugins
                .get(&self.record.driver.driver)
                .is_none_or(|s| !s.enabled || &s.digest != digest)
        {
            return Err(PeripheralError::StaleSession);
        }
        let discovery = self.handle.discovery.borrow();
        match &discovery.result {
            Some(Ok(endpoints))
                if self
                    .record
                    .endpoints
                    .iter()
                    .all(|target| endpoints.iter().any(|e| &e.endpoint == target)) =>
            {
                Ok(())
            }
            Some(Err(error)) => Err(error.clone()),
            _ => Err(PeripheralError::StaleSession),
        }
    }

    pub(super) fn check_desired(&self) -> Result<(), PeripheralError> {
        self.check()?;
        if self
            .handle
            .desired
            .borrow()
            .config
            .peripheral_rule(&self.record)?
            != self.rule.as_ref()
        {
            return Err(PeripheralError::StaleSession);
        }
        Ok(())
    }

    fn fail(&self, error: PeripheralError) {
        self.halted.send_if_modified(|current| {
            if current.is_some() {
                false
            } else {
                *current = Some(error);
                true
            }
        });
    }

    async fn stopped(&self) {
        let mut receiver = self.halted.subscribe();
        let _ = receiver.wait_for(Option::is_some).await;
    }
}

pub(super) enum Event {
    Guest(wire::Event),
    Deadline(u64),
}

pub(super) struct Envelope {
    pub(super) session: SessionId,
    pub(super) event: Event,
}

#[derive(Clone)]
struct Queue {
    sender: mpsc::Sender<Envelope>,
    guard: Guard,
}

impl Queue {
    fn send(&self, event: Event) {
        if self.guard.halted.borrow().is_some() {
            return;
        }
        let envelope = Envelope {
            session: self.guard.record.session.clone(),
            event,
        };
        if self.sender.try_send(envelope).is_err() {
            self.guard.fail(PeripheralError::ResourceLimit(
                "256 queued peripheral events".into(),
            ));
        }
    }

    async fn complete_hid(
        &self,
        request: u64,
        operation: impl std::future::Future<Output = Result<Option<wire::Report>, PeripheralError>>,
    ) {
        tokio::pin!(operation);
        if let Ok(outcome) = tokio::time::timeout(Duration::from_secs(3), &mut operation).await {
            self.send(Event::Guest(wire::Event::Completed(wire::IoCompletion {
                request,
                outcome: outcome.map_err(|error| failure(&error)),
            })));
        } else {
            self.guard.fail(PeripheralError::WriteFailed(
                "HID deadline expired; operation outcome is uncertain".into(),
            ));
            // Retain the claim and native buffers until the submitted operation finishes.
            let result = operation.await;
            tracing::debug!(result = ?result.map(|_| ()), "native HID operation finished after its deadline");
        }
    }
}

pub(super) struct PluginSession {
    pub(super) input: crate::runtime::PeripheralInput,
    pub(super) guest: Session,
    pub(super) broker: Broker,
    pub(super) pending: BTreeMap<u64, (openlogi_core::peripheral::CapabilityId, u64)>,
}

pub(super) struct Broker {
    endpoints: BTreeMap<ControlId, DiscoveredEndpoint>,
    reports: BTreeMap<ControlId, Arc<tokio::sync::Mutex<openlogi_hid::peripheral::ReportDevice>>>,
    permissions: Vec<Permission>,
    guard: Guard,
    queue: Queue,
    receiver: mpsc::Receiver<Envelope>,
    tasks: JoinSet<()>,
    io: JoinSet<()>,
    subscriptions: BTreeMap<ControlId, AbortHandle>,
}

impl Broker {
    pub(super) fn new(
        endpoints: BTreeMap<ControlId, DiscoveredEndpoint>,
        permissions: Vec<Permission>,
        guard: Guard,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(256);
        let queue = Queue {
            sender,
            guard: guard.clone(),
        };
        let mut tasks = JoinSet::new();
        let halted = guard.clone();
        // Wake poll even when a faulted native request is still draining.
        tasks.spawn(async move { halted.stopped().await });
        Self {
            endpoints,
            reports: BTreeMap::new(),
            permissions,
            guard,
            queue,
            receiver,
            tasks,
            io: JoinSet::new(),
            subscriptions: BTreeMap::new(),
        }
    }

    pub(super) fn check(&self) -> Result<(), PeripheralError> {
        self.guard.check_desired()
    }

    pub(super) fn configure(&mut self, rule: Option<PeripheralConfig>) {
        self.guard.rule.clone_from(&rule);
        self.queue.guard.rule = rule;
    }

    pub(super) fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Result<Envelope, PeripheralError>> {
        for tasks in [&mut self.tasks, &mut self.io] {
            while let Poll::Ready(Some(result)) = tasks.poll_join_next(cx) {
                if let Err(error) = result
                    && !error.is_cancelled()
                {
                    return Poll::Ready(Err(PeripheralError::PluginFault(format!(
                        "host I/O task: {error}"
                    ))));
                }
            }
        }
        if let Some(error) = &*self.guard.halted.borrow() {
            return Poll::Ready(Err(error.clone()));
        }
        match self.receiver.poll_recv(cx) {
            Poll::Ready(Some(envelope)) => Poll::Ready(Ok(envelope)),
            Poll::Ready(None) => Poll::Ready(Err(PeripheralError::PluginFault(
                "device broker stopped".into(),
            ))),
            Poll::Pending => Poll::Pending,
        }
    }

    /// Return native effects to the one agent journal owner; all other operations stay on this broker.
    pub(super) fn submit(
        &mut self,
        requests: Vec<HostRequest>,
    ) -> Result<Vec<HostRequest>, PeripheralError> {
        self.guard.check_desired()?;
        let mut native = Vec::new();
        for request in requests {
            match request {
                HostRequest::Subscribe { role, report_ids } => self.subscribe(role, report_ids)?,
                HostRequest::Hid {
                    request,
                    role,
                    report,
                } => self.hid(request, &role, report)?,
                HostRequest::Timer { id, after_ms } => {
                    let queue = self.queue.clone();
                    self.tasks.spawn(async move {
                        tokio::select! {
                            () = queue.guard.stopped() => {},
                            () = tokio::time::sleep(Duration::from_millis(u64::from(after_ms))) => queue.send(Event::Guest(wire::Event::Timer(id))),
                        }
                    });
                }
                HostRequest::Diagnostic { level, message } => {
                    use openlogi_plugin::runtime::bindings::openlogi::peripheral::diagnostics::Level;
                    let driver = &self.guard.record.driver.driver;
                    match level {
                        Level::Debug => tracing::debug!(%driver, %message, "peripheral plugin"),
                        Level::Info => tracing::info!(%driver, %message, "peripheral plugin"),
                        Level::Warning => tracing::warn!(%driver, %message, "peripheral plugin"),
                        Level::Error => tracing::error!(%driver, %message, "peripheral plugin"),
                    }
                }
                request @ HostRequest::NativeRemap { .. } => native.push(request),
            }
        }
        Ok(native)
    }

    fn subscribe(&mut self, role: ControlId, report_ids: Vec<u8>) -> Result<(), PeripheralError> {
        let limit = self.limit(&role, HidOperation::Input)?;
        if report_ids.contains(&0) && report_ids.len() > 1 {
            return Err(PeripheralError::InvalidSettings(
                "one collection cannot mix numbered and unnumbered framing".into(),
            ));
        }
        let device = self.endpoint(&role)?.report_device(self.guard.gate.clone());
        if let Some(previous) = self.subscriptions.remove(&role) {
            previous.abort();
        }
        let queue = self.queue.clone();
        let subscription = role.clone();
        let task = self.tasks.spawn(async move {
            let read = async {
                queue.guard.check()?;
                let mut input = tokio::time::timeout(
                    Duration::from_secs(2),
                    device.input(!report_ids.contains(&0)),
                )
                .await
                .map_err(|_| {
                    PeripheralError::ReadFailed("HID input open deadline expired".into())
                })??;
                loop {
                    let report = input.next().await?;
                    queue.guard.check()?;
                    if !report_ids.contains(&report.id) {
                        continue;
                    }
                    if report.payload.len() + 1 > limit {
                        return Err::<(), _>(PeripheralError::ResourceLimit(
                            "input exceeds the granted report length".into(),
                        ));
                    }
                    queue.send(Event::Guest(wire::Event::Report(wire::DeviceReport {
                        endpoint: role.to_string(),
                        report: wire::Report {
                            kind: wire::ReportKind::Input,
                            id: report.id,
                            payload: report.payload,
                        },
                    })));
                }
            };
            tokio::select! {
                () = queue.guard.stopped() => {},
                result = read => { if let Err(error) = result { queue.guard.fail(error); } },
            }
        });
        self.subscriptions.insert(subscription, task);
        Ok(())
    }

    fn hid(
        &mut self,
        request: u64,
        role: &ControlId,
        report: wire::Report,
    ) -> Result<(), PeripheralError> {
        let operation = match report.kind {
            wire::ReportKind::Output => HidOperation::Output,
            wire::ReportKind::FeatureRead => HidOperation::FeatureRead,
            wire::ReportKind::FeatureWrite => HidOperation::FeatureWrite,
            wire::ReportKind::Input => {
                return Err(PeripheralError::PermissionDenied(
                    "cannot submit an input report".into(),
                ));
            }
        };
        let limit = self.limit(role, operation)?;
        if report.payload.len() + 1 > limit {
            return Err(PeripheralError::PermissionDenied(
                "report exceeds its grant".into(),
            ));
        }
        if !self.reports.contains_key(role) {
            let device = self.endpoint(role)?.report_device(self.guard.gate.clone());
            self.reports
                .insert(role.clone(), Arc::new(tokio::sync::Mutex::new(device)));
        }
        let device = Arc::clone(
            self.reports
                .get(role)
                .ok_or(PeripheralError::StaleSession)?,
        );
        let queue = self.queue.clone();
        self.io.spawn(async move {
            let operation = async {
                queue.guard.check_desired()?;
                let mut device = device.lock().await;
                queue.guard.check_desired()?;
                let payload = Report {
                    id: report.id,
                    payload: report.payload,
                };
                let result = match report.kind {
                    wire::ReportKind::Output => device
                        .output(&payload, || queue.guard.check_desired())
                        .await
                        .map(|()| None),
                    wire::ReportKind::FeatureWrite => device
                        .feature_write(&payload, || queue.guard.check_desired())
                        .await
                        .map(|()| None),
                    wire::ReportKind::FeatureRead => {
                        let guard = queue.guard.clone();
                        device
                            .feature_read(payload.id, payload.payload.len() + 1, move || {
                                guard.check_desired()
                            })
                            .await
                            .map(Some)
                    }
                    wire::ReportKind::Input => unreachable!("validated by hid submission"),
                }?;
                queue.guard.check_desired()?;
                if result
                    .as_ref()
                    .is_some_and(|r| r.id != payload.id || r.payload.len() + 1 > limit)
                {
                    return Err(PeripheralError::ReadFailed(
                        "feature response differs from the granted report".into(),
                    ));
                }
                Ok(result.map(|r| wire::Report {
                    kind: wire::ReportKind::FeatureRead,
                    id: r.id,
                    payload: r.payload,
                }))
            };
            queue.complete_hid(request, operation).await;
        });
        Ok(())
    }

    fn endpoint(&self, role: &ControlId) -> Result<&DiscoveredEndpoint, PeripheralError> {
        self.endpoints
            .get(role)
            .ok_or_else(|| PeripheralError::PermissionDenied("unclaimed endpoint role".into()))
    }

    fn limit(&self, role: &ControlId, operation: HidOperation) -> Result<usize, PeripheralError> {
        self.permissions
            .iter()
            .find_map(|permission| match permission {
                Permission::Hid {
                    endpoint,
                    operations,
                    max_report_bytes,
                    ..
                } if endpoint == role && operations.contains(&operation) => {
                    Some(*max_report_bytes as usize)
                }
                _ => None,
            })
            .ok_or_else(|| PeripheralError::PermissionDenied("HID operation has no grant".into()))
    }

    pub(super) fn completed(&self, request: u64, result: Result<(), PeripheralError>) {
        self.queue
            .send(Event::Guest(wire::Event::Completed(wire::IoCompletion {
                request,
                outcome: result.map(|()| None).map_err(|error| failure(&error)),
            })));
    }

    pub(super) fn command_deadline(&mut self, request: u64) {
        let queue = self.queue.clone();
        self.tasks.spawn(async move {
            tokio::select! {
                () = queue.guard.stopped() => {},
                () = tokio::time::sleep(Duration::from_secs(5)) => queue.send(Event::Deadline(request)),
            }
        });
    }

    pub(super) async fn stop(&mut self) -> std::sync::Weak<PeripheralRecord> {
        self.guard.fail(PeripheralError::StaleSession);
        self.io.detach_all();
        self.tasks.abort_all();
        while let Some(result) = self.tasks.join_next().await {
            if let Err(error) = result
                && !error.is_cancelled()
            {
                tracing::warn!(%error, "peripheral broker cleanup");
            }
        }
        // Submitted native operations retain their Guard until native handles and buffers are released.
        Arc::downgrade(&self.guard.record)
    }
}

fn failure(error: &PeripheralError) -> wire::Failure {
    let kind = match error {
        PeripheralError::PermissionDenied(_) => wire::ErrorKind::PermissionDenied,
        PeripheralError::ResourceLimit(_) => wire::ErrorKind::ResourceLimit,
        PeripheralError::WriteFailed(_) => wire::ErrorKind::Uncertain,
        PeripheralError::Offline | PeripheralError::StaleSession | PeripheralError::Suspended => {
            wire::ErrorKind::Unavailable
        }
        _ => wire::ErrorKind::Io,
    };
    wire::Failure {
        kind,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests;
