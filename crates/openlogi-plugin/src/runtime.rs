//! A bounded Pulley worker pool; the hook thread never executes guest code.

mod host;
mod quota;

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use openlogi_core::peripheral::{
    CapabilityRecord, ControlId, Endpoint, ModelId, PeripheralError, SessionId, SettingValue,
};
use wasmtime::{
    Engine, Store,
    component::{Component, Linker},
};

use crate::{
    limits,
    manifest::{Manifest, Permission},
};
use host::HostState;
use quota::Admission;

/// Generated bindings are the only owner of the external ABI layout.
#[expect(
    clippy::allow_attributes,
    reason = "the generated macro raises documentation lints without crediting expectations"
)]
#[allow(
    missing_docs,
    reason = "WIT owns generated API documentation; Wasmtime also emits undocumented internal aliases"
)]
pub mod bindings {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "driver",
        imports: { default: trappable },
        with: { "openlogi:peripheral/session.endpoint": super::host::EndpointHandle },
    });
}

pub use bindings::openlogi::peripheral::types as wire;
pub use host::{from_wire_settings, to_wire_settings};

/// Sanitized facts and effective grants for exactly one device attachment.
#[derive(Clone)]
pub struct SessionContext {
    /// Complete attachment identity.
    pub session: SessionId,
    /// Selected model.
    pub model: ModelId,
    /// Only the roles claimed by this session.
    pub endpoints: BTreeMap<ControlId, Endpoint>,
    /// Already intersected, digest-bound permission grants.
    pub permissions: Vec<Permission>,
    /// Validated desired settings belonging to this plugin.
    pub settings: BTreeMap<String, SettingValue>,
    /// Host-owned desired native targets. Guests cannot choose their own effects.
    pub native_keys: BTreeMap<ControlId, Option<u16>>,
}

/// A nonblocking request for the agent's scoped device broker.
#[derive(Clone, Debug)]
pub enum HostRequest {
    /// Subscribe to a granted role's reports.
    Subscribe {
        /// Role name.
        role: ControlId,
        /// Report IDs including zero for unnumbered reports.
        report_ids: Vec<u8>,
    },
    /// Submit one deadline-bound HID operation.
    Hid {
        /// Completion identity.
        request: u64,
        /// Role name.
        role: ControlId,
        /// Explicit report framing.
        report: wire::Report,
    },
    /// Reconcile a declared native mapping through the host effect journal.
    NativeRemap {
        /// Completion identity.
        request: u64,
        /// Approved control.
        control: ControlId,
        /// User-selected keyboard usage.
        key: Option<u16>,
    },
    /// Arm a bounded one-shot timer.
    Timer {
        /// Timer identity.
        id: u64,
        /// Delay in milliseconds.
        after_ms: u32,
    },
    /// Sanitized, bounded diagnostic.
    Diagnostic {
        /// Severity.
        level: bindings::openlogi::peripheral::diagnostics::Level,
        /// Message.
        message: String,
    },
}

type Job = Box<dyn FnOnce() + Send>;
struct Workers {
    queue: mpsc::SyncSender<Job>,
}

impl Workers {
    fn new() -> Result<Self, PeripheralError> {
        let (queue, receiver) = mpsc::sync_channel::<Job>(limits::FIELDS);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..4 {
            let receiver = Arc::clone(&receiver);
            thread::Builder::new()
                .name(format!("peripheral-{index}"))
                .spawn(move || {
                    loop {
                        let job = match receiver.lock() {
                            Ok(receiver) => receiver.recv(),
                            Err(_) => return,
                        };
                        match job {
                            Ok(job) => job(),
                            Err(_) => return,
                        }
                    }
                })
                .map_err(|e| PeripheralError::PluginFault(e.to_string()))?;
        }
        Ok(Self { queue })
    }

    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, PeripheralError> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.queue
            .try_send(Box::new(move || {
                let result = operation();
                let _ = sender.send(result);
            }))
            .map_err(|e| PeripheralError::ResourceLimit(format!("plugin worker queue: {e}")))?;
        receiver
            .await
            .map_err(|e| PeripheralError::PluginFault(format!("plugin worker stopped: {e}")))
    }
}

/// Shared runtime admission, compilation serialization, and worker scheduling.
pub struct Runtime {
    engine: Engine,
    workers: Workers,
    admission: Arc<Admission>,
    compile: Arc<Mutex<()>>,
    stop: Arc<AtomicBool>,
}

impl Runtime {
    /// Create an interpreter engine without JIT or ambient system imports.
    pub fn new() -> Result<Arc<Self>, PeripheralError> {
        let mut config = wasmtime::Config::new();
        let target = if cfg!(target_pointer_width = "64") {
            if cfg!(target_endian = "little") {
                "pulley64"
            } else {
                "pulley64be"
            }
        } else if cfg!(target_endian = "little") {
            "pulley32"
        } else {
            "pulley32be"
        };
        config.target(target).map_err(|error| fault(&error))?;
        config
            .consume_fuel(true)
            .epoch_interruption(true)
            .wasm_memory64(false)
            .max_wasm_stack(512 * 1024);
        let engine = Engine::new(&config).map_err(|error| fault(&error))?;
        let workers = Workers::new()?;
        let stop = Arc::new(AtomicBool::new(false));
        let ticker_stop = Arc::clone(&stop);
        let ticker_engine = engine.clone();
        thread::Builder::new()
            .name("peripheral-epoch".into())
            .spawn(move || {
                while !ticker_stop.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(10));
                    ticker_engine.increment_epoch();
                }
            })
            .map_err(|e| PeripheralError::PluginFault(e.to_string()))?;
        Ok(Arc::new(Self {
            engine,
            workers,
            admission: Arc::default(),
            compile: Arc::default(),
            stop,
        }))
    }

    /// Validate and compile the same bytes that package admission hashed.
    pub async fn compile(self: &Arc<Self>, bytes: Vec<u8>) -> Result<Compiled, PeripheralError> {
        crate::validate_component(&bytes)
            .map_err(|e| PeripheralError::InvalidSettings(e.to_string()))?;
        let engine = self.engine.clone();
        let lock = Arc::clone(&self.compile);
        let component = self
            .workers
            .run(move || {
                let _guard = lock
                    .lock()
                    .map_err(|e| PeripheralError::PluginFault(e.to_string()))?;
                let component = Component::new(&engine, &bytes).map_err(|error| fault(&error))?;
                let mut linker = Linker::new(&engine);
                bindings::Driver::add_to_linker::<_, wasmtime::component::HasSelf<HostState>>(
                    &mut linker,
                    |state| state,
                )
                .map_err(|error| fault(&error))?;
                bindings::DriverPre::new(
                    linker
                        .instantiate_pre(&component)
                        .map_err(|error| fault(&error))?,
                )
                .map_err(|error| fault(&error))?;
                Ok(component)
            })
            .await??;
        Ok(Compiled {
            runtime: Arc::clone(self),
            component,
        })
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// An admitted component reusable across separate device stores.
#[derive(Clone)]
pub struct Compiled {
    runtime: Arc<Runtime>,
    component: Component,
}

impl Compiled {
    /// Instantiate and attach with only this session's grants and validated parameters.
    pub async fn attach(
        &self,
        manifest: Arc<Manifest>,
        context: SessionContext,
        parameters: BTreeMap<String, SettingValue>,
    ) -> Result<Session, PeripheralError> {
        let runtime = Arc::clone(&self.runtime);
        let component = self.component.clone();
        let session = context.session.clone();
        let state = self
            .runtime
            .workers
            .run(move || {
                let state = HostState::new(manifest, context, Arc::clone(&runtime.admission))?;
                let mut store = Store::new(&runtime.engine, state);
                store.limiter(|state| &mut state.quota);
                store
                    .set_fuel(quota::CALL_FUEL)
                    .map_err(|error| fault(&error))?;
                store.set_epoch_deadline(10);
                let mut linker = Linker::new(&runtime.engine);
                bindings::Driver::add_to_linker::<_, wasmtime::component::HasSelf<_>>(
                    &mut linker,
                    |state| state,
                )
                .map_err(|error| fault(&error))?;
                let driver = bindings::Driver::instantiate(&mut store, &component, &linker)
                    .map_err(|error| fault(&error))?;
                let ctx = wire::Context {
                    generation: store.data().context.session.generation,
                    model: store.data().context.model.to_string(),
                };
                let parameters = host::to_wire_settings(&parameters);
                let started = Instant::now();
                let caps = driver
                    .call_attach(&mut store, &ctx, &parameters)
                    .map_err(|error| fault(&error))?
                    .map_err(|error| host::guest_error(&error))?;
                let remaining = store.get_fuel().map_err(|error| fault(&error))?;
                store
                    .data_mut()
                    .quota
                    .charge_fuel(quota::CALL_FUEL - remaining)?;
                if started.elapsed() > Duration::from_millis(100) {
                    return Err(PeripheralError::ResourceLimit(
                        "attach exceeded 100 ms".into(),
                    ));
                }
                let capabilities = store.data_mut().accept_capabilities(&caps)?;
                Ok::<_, PeripheralError>(State {
                    store,
                    driver,
                    capabilities,
                })
            })
            .await??;
        Ok(Session {
            runtime: Arc::clone(&self.runtime),
            identity: session,
            state: Some(state),
        })
    }

    /// Migrate a settings copy without endpoints, device grants, or native targets.
    pub async fn migrate(
        &self,
        manifest: Arc<Manifest>,
        from: u32,
        settings: BTreeMap<String, SettingValue>,
    ) -> Result<BTreeMap<String, SettingValue>, PeripheralError> {
        let runtime = Arc::clone(&self.runtime);
        let component = self.component.clone();
        self.runtime
            .workers
            .run(move || {
                let context = SessionContext {
                    session: SessionId {
                        endpoint: openlogi_core::peripheral::EndpointId("migration".into()),
                        generation: 0,
                    },
                    model: ModelId::try_new("migration")
                        .map_err(|e| PeripheralError::InvalidSettings(e.to_string()))?,
                    endpoints: BTreeMap::new(),
                    permissions: Vec::new(),
                    settings: settings.clone(),
                    native_keys: BTreeMap::new(),
                };
                let state = HostState::new(manifest, context, Arc::clone(&runtime.admission))?;
                let mut store = Store::new(&runtime.engine, state);
                store.limiter(|state| &mut state.quota);
                store
                    .set_fuel(quota::CALL_FUEL)
                    .map_err(|error| fault(&error))?;
                store.set_epoch_deadline(10);
                let mut linker = Linker::new(&runtime.engine);
                bindings::Driver::add_to_linker::<_, wasmtime::component::HasSelf<_>>(
                    &mut linker,
                    |state| state,
                )
                .map_err(|error| fault(&error))?;
                let driver = bindings::Driver::instantiate(&mut store, &component, &linker)
                    .map_err(|error| fault(&error))?;
                let result = driver
                    .call_migrate_settings(&mut store, from, &host::to_wire_settings(&settings))
                    .map_err(|error| fault(&error))?
                    .map_err(|error| host::guest_error(&error))?;
                host::from_wire_settings(&result)
            })
            .await?
    }
}

struct State {
    store: Store<HostState>,
    driver: bindings::Driver,
    capabilities: Vec<CapabilityRecord>,
}

/// One attachment, one store, and at most one queued guest call.
pub struct Session {
    runtime: Arc<Runtime>,
    identity: SessionId,
    state: Option<State>,
}

/// Validated guest output plus host-service submissions, stamped by the host.
pub struct Update {
    /// Complete publication identity.
    pub session: SessionId,
    /// Only validated declared-control events and completions.
    pub updates: Vec<wire::Update>,
    /// Granted operations awaiting the host broker.
    pub requests: Vec<HostRequest>,
}

impl Session {
    /// Capabilities validated during attachment.
    #[must_use]
    pub fn capabilities(&self) -> &[CapabilityRecord] {
        self.state.as_ref().map_or(&[], |state| &state.capabilities)
    }

    /// Drain attach-time probes without running another guest call.
    pub fn pending(&mut self) -> Vec<HostRequest> {
        self.state
            .as_mut()
            .map_or_else(Vec::new, |s| s.store.data_mut().take_requests())
    }

    /// Apply a typed desired-state request.
    pub async fn apply(&mut self, request: wire::Request) -> Result<Update, PeripheralError> {
        self.call(move |state| {
            state.store.data_mut().begin_request(&request)?;
            state
                .driver
                .call_apply(&mut state.store, &request)
                .map_err(|error| fault(&error))?
                .map_err(|error| host::guest_error(&error))
        })
        .await
    }

    /// Deliver one ordered report, timer, or I/O completion.
    pub async fn event(&mut self, event: wire::Event) -> Result<Update, PeripheralError> {
        self.call(move |state| {
            state.store.data_mut().accept_event(&event)?;
            state
                .driver
                .call_event(&mut state.store, &event)
                .map_err(|error| fault(&error))?
                .map_err(|error| host::guest_error(&error))
        })
        .await
    }

    /// Attempt bounded guest cleanup. Host effect cleanup remains the caller's duty.
    pub async fn detach(&mut self, reason: wire::DetachReason) -> Result<(), PeripheralError> {
        let Some(mut state) = self.state.take() else {
            return Ok(());
        };
        self.runtime
            .workers
            .run(move || {
                state.store.data_mut().revoked = true;
                state
                    .store
                    .set_fuel(quota::CALL_FUEL)
                    .map_err(|error| fault(&error))?;
                state.store.set_epoch_deadline(10);
                state
                    .driver
                    .call_detach(&mut state.store, reason)
                    .map_err(|error| fault(&error))?
                    .map_err(|error| host::guest_error(&error))
            })
            .await?
    }

    async fn call(
        &mut self,
        operation: impl FnOnce(&mut State) -> Result<Vec<wire::Update>, PeripheralError>
        + Send
        + 'static,
    ) -> Result<Update, PeripheralError> {
        let mut state = self
            .state
            .take()
            .ok_or_else(|| PeripheralError::PluginFault("session is stopped".into()))?;
        let (returned, result) = self
            .runtime
            .workers
            .run(move || {
                let result = (|| {
                    state.store.data_mut().quota.before_call()?;
                    state
                        .store
                        .set_fuel(quota::CALL_FUEL)
                        .map_err(|error| fault(&error))?;
                    state.store.set_epoch_deadline(10);
                    let started = Instant::now();
                    let updates = operation(&mut state)?;
                    let remaining = state.store.get_fuel().map_err(|error| fault(&error))?;
                    state
                        .store
                        .data_mut()
                        .quota
                        .charge_fuel(quota::CALL_FUEL - remaining)?;
                    if started.elapsed() > Duration::from_millis(100) {
                        return Err(PeripheralError::ResourceLimit(
                            "guest call exceeded 100 ms".into(),
                        ));
                    }
                    state.store.data_mut().accept_updates(&updates)?;
                    Ok((updates, state.store.data_mut().take_requests()))
                })();
                (state, result)
            })
            .await?;
        let (updates, requests) = result?;
        self.state = Some(returned);
        Ok(Update {
            session: self.identity.clone(),
            updates,
            requests,
        })
    }
}

fn fault(error: &wasmtime::Error) -> PeripheralError {
    if let Some(error) = error.downcast_ref::<PeripheralError>() {
        return error.clone();
    }
    if matches!(
        error.downcast_ref::<wasmtime::Trap>(),
        Some(wasmtime::Trap::OutOfFuel | wasmtime::Trap::Interrupt)
    ) {
        PeripheralError::ResourceLimit(error.to_string())
    } else {
        PeripheralError::PluginFault(format!("{error:#}"))
    }
}

#[cfg(test)]
mod tests;
