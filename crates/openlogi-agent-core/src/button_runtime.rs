//! Source-independent button lifecycle state.
//!
//! Capture backends report different raw shapes: OS hooks carry discrete
//! edges, while HID++ diverted-control reports carry complete held-control
//! snapshots. Producers normalise both into typed inputs for one worker. The
//! worker is the sole owner of active presses and emits balanced lifecycle
//! events carrying a unique [`PressId`].

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle, ThreadId};
use std::time::Duration;

use openlogi_core::binding::{Action, ButtonId};
use tracing::warn;

/// OS-hook callbacks must fail open rather than block.
const EVENT_QUEUE_CAPACITY: usize = 128;
/// Bounds how long graceful process exit waits for terminal handlers.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
/// Lets the worker observe the out-of-band shutdown channel even while idle.
const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Stable identity of one HID++ capture-session incarnation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct HidppSessionId {
    device_key: Arc<str>,
    epoch: u64,
}

impl HidppSessionId {
    pub(crate) fn new(device_key: &str, epoch: u64) -> Self {
        Self {
            device_key: Arc::from(device_key),
            epoch,
        }
    }

    pub(crate) fn device_key(&self) -> &str {
        &self.device_key
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }
}

/// Capture source that owns one physical press.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ButtonSource {
    /// Linux has one callback thread per grabbed device; macOS and Windows
    /// expose one global callback thread.
    OsHook(ThreadId),
    Hidpp(HidppSessionId),
}

impl ButtonSource {
    fn current_hook() -> Self {
        Self::OsHook(thread::current().id())
    }

    fn device_key(&self) -> Option<&str> {
        match self {
            Self::OsHook(_) => None,
            Self::Hidpp(session) => Some(session.device_key()),
        }
    }
}

/// Correlation key shared by consecutive edges from one physical control.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PressKey {
    source: ButtonSource,
    button: ButtonId,
}

impl PressKey {
    fn new(source: ButtonSource, button: ButtonId) -> Self {
        Self { source, button }
    }
}

/// Unique identity of one accepted press, including a restart of the same key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PressId(u64);

/// Capability used to run a gesture action only while its originating press
/// remains active. Future timers and repeat workers can use the same token to
/// reject work scheduled by a superseded press.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PressToken {
    id: PressId,
    key: PressKey,
    generation: u64,
}

#[cfg(test)]
impl PressToken {
    pub(crate) fn hook_for_test(id: u64, button: ButtonId) -> Self {
        Self {
            id: PressId(id),
            key: PressKey::new(ButtonSource::current_hook(), button),
            generation: 0,
        }
    }
}

/// State retained from `Down` until the exactly-once terminal event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActivePress {
    token: PressToken,
    action: Option<Action>,
}

impl ActivePress {
    pub(crate) fn button(&self) -> ButtonId {
        self.token.key.button
    }

    pub(crate) fn device_key(&self) -> Option<&str> {
        self.token.key.source.device_key()
    }

    pub(crate) fn action(&self) -> Option<&Action> {
        self.action.as_ref()
    }
}

/// Why an accepted press ended without its ordinary physical release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CancelReason {
    /// A duplicate `Down` proved that the previous release was lost.
    RepeatedDown,
    /// A gesture hold aged out before another control took ownership.
    StaleHold,
    /// The capture source stopped or could no longer guarantee its release.
    SourceEnded,
    /// Bindings, profiles, or queue generation changed under the press.
    Invalidated,
    /// The agent is exiting gracefully.
    Shutdown,
}

/// How an accepted press reached its exactly-once terminal event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndReason {
    /// The physical release edge was observed.
    Released,
    /// Capture could no longer guarantee the matching release.
    Canceled(CancelReason),
}

/// Typed output from the lifecycle worker.
pub(crate) enum ButtonRuntimeEvent {
    Started(ActivePress),
    Ended {
        press: ActivePress,
        reason: EndReason,
    },
    /// A semantic gesture action admitted only while `press` is still active.
    Triggered {
        press: ActivePress,
        action: Action,
    },
}

/// Inputs cannot represent `Up + action` or a source-authored `Cancel`.
enum ButtonInput {
    Down(ActivePress),
    Up(PressKey),
    Pulse(ActivePress),
    TriggerWhilePressed { token: PressToken, action: Action },
}

enum ButtonCommand {
    Input { generation: u64, input: ButtonInput },
    CancelStalePress(PressToken),
    CancelSource(ButtonSource),
    CancelHooks,
    Wake,
}

struct ShutdownRequest {
    done: mpsc::SyncSender<()>,
}

/// Sole owner of active press records.
#[derive(Default)]
struct ButtonState {
    active: HashMap<PressKey, ActivePress>,
}

impl ButtonState {
    fn press(&mut self, press: ActivePress) -> Option<ActivePress> {
        self.active.insert(press.token.key.clone(), press)
    }

    fn release(&mut self, key: &PressKey) -> Option<ActivePress> {
        self.active.remove(key)
    }

    fn active(&self, token: &PressToken) -> Option<&ActivePress> {
        self.active
            .get(&token.key)
            .filter(|press| press.token.id == token.id)
    }

    fn cancel_press(&mut self, token: &PressToken) -> Option<ActivePress> {
        self.active(token)?;
        self.active.remove(&token.key)
    }

    fn cancel_source(&mut self, source: &ButtonSource) -> Vec<ActivePress> {
        self.cancel_where(|key| key.source == *source)
    }

    fn cancel_hooks(&mut self) -> Vec<ActivePress> {
        self.cancel_where(|key| matches!(key.source, ButtonSource::OsHook(_)))
    }

    fn cancel_all(&mut self) -> Vec<ActivePress> {
        self.active.drain().map(|(_, press)| press).collect()
    }

    fn cancel_where(&mut self, matches: impl Fn(&PressKey) -> bool) -> Vec<ActivePress> {
        let keys: Vec<PressKey> = self
            .active
            .keys()
            .filter(|key| matches(key))
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|key| self.active.remove(&key))
            .collect()
    }
}

/// Non-blocking producer cloned into capture callbacks and watcher tasks.
#[derive(Clone)]
pub(crate) struct ButtonInputHandle {
    events: mpsc::SyncSender<ButtonCommand>,
    generation: Arc<AtomicU64>,
    accepting: Arc<AtomicBool>,
    next_press: Arc<AtomicU64>,
}

impl ButtonInputHandle {
    pub(crate) fn try_hook_down(
        &self,
        button: ButtonId,
        action: Option<&Action>,
    ) -> Option<PressToken> {
        self.try_down(ButtonSource::current_hook(), button, action)
    }

    pub(crate) fn try_hook_up(&self, button: ButtonId) -> bool {
        self.try_up(ButtonSource::current_hook(), button)
    }

    pub(crate) fn cancel_hook_thread(&self) {
        self.try_command(ButtonCommand::CancelSource(ButtonSource::current_hook()));
    }

    pub(crate) fn cancel_hooks(&self) {
        self.try_command(ButtonCommand::CancelHooks);
    }

    pub(crate) fn try_hidpp_down(
        &self,
        session: &HidppSessionId,
        button: ButtonId,
        action: Option<&Action>,
    ) -> Option<PressToken> {
        self.try_down(ButtonSource::Hidpp(session.clone()), button, action)
    }

    pub(crate) fn try_hidpp_up(&self, session: &HidppSessionId, button: ButtonId) -> bool {
        self.try_up(ButtonSource::Hidpp(session.clone()), button)
    }

    pub(crate) fn try_hidpp_pulse(
        &self,
        session: &HidppSessionId,
        button: ButtonId,
        action: Option<&Action>,
    ) -> bool {
        let generation = self.generation.load(Ordering::Acquire);
        let press = self.new_press(
            ButtonSource::Hidpp(session.clone()),
            button,
            action,
            generation,
        );
        self.try_input(generation, ButtonInput::Pulse(press))
    }

    pub(crate) fn try_trigger_while_pressed(&self, token: &PressToken, action: &Action) -> bool {
        let generation = self.generation.load(Ordering::Acquire);
        if token.generation != generation {
            return false;
        }
        self.try_input(
            generation,
            ButtonInput::TriggerWhilePressed {
                token: token.clone(),
                action: action.clone(),
            },
        )
    }

    pub(crate) fn cancel_stale_press(&self, token: &PressToken) {
        self.try_command(ButtonCommand::CancelStalePress(token.clone()));
    }

    pub(crate) fn cancel_hidpp_session(&self, session: &HidppSessionId) {
        self.try_command(ButtonCommand::CancelSource(ButtonSource::Hidpp(
            session.clone(),
        )));
    }

    pub(crate) fn invalidate_all(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        let _ = self.events.try_send(ButtonCommand::Wake);
    }

    fn try_down(
        &self,
        source: ButtonSource,
        button: ButtonId,
        action: Option<&Action>,
    ) -> Option<PressToken> {
        let generation = self.generation.load(Ordering::Acquire);
        let press = self.new_press(source, button, action, generation);
        let token = press.token.clone();
        self.try_input(generation, ButtonInput::Down(press))
            .then_some(token)
    }

    fn try_up(&self, source: ButtonSource, button: ButtonId) -> bool {
        let generation = self.generation.load(Ordering::Acquire);
        self.try_input(generation, ButtonInput::Up(PressKey::new(source, button)))
    }

    fn new_press(
        &self,
        source: ButtonSource,
        button: ButtonId,
        action: Option<&Action>,
        generation: u64,
    ) -> ActivePress {
        let id = PressId(self.next_press.fetch_add(1, Ordering::Relaxed));
        ActivePress {
            token: PressToken {
                id,
                key: PressKey::new(source, button),
                generation,
            },
            action: action.cloned(),
        }
    }

    fn try_input(&self, generation: u64, input: ButtonInput) -> bool {
        if !self.accepting.load(Ordering::Acquire) {
            return false;
        }
        self.try_command(ButtonCommand::Input { generation, input })
    }

    fn try_command(&self, command: ButtonCommand) -> bool {
        match self.events.try_send(command) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(_)) => {
                self.generation.fetch_add(1, Ordering::AcqRel);
                warn!("button lifecycle queue full — invalidating active presses");
                false
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                warn!("button lifecycle worker unavailable — event ignored");
                false
            }
        }
    }

    fn stop_accepting(&self) {
        self.accepting.store(false, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
}

/// Unique owner of the lifecycle worker and its graceful shutdown handshake.
pub(crate) struct ButtonRuntimeOwner {
    input: ButtonInputHandle,
    shutdown: mpsc::Sender<ShutdownRequest>,
    worker: Option<JoinHandle<()>>,
}

impl ButtonRuntimeOwner {
    pub(crate) fn spawn(
        on_event: impl Fn(ButtonRuntimeEvent) + Send + 'static,
    ) -> io::Result<Self> {
        let (events, event_rx) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
        let (shutdown, shutdown_rx) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let input = ButtonInputHandle {
            events,
            generation: Arc::clone(&generation),
            accepting: Arc::new(AtomicBool::new(true)),
            next_press: Arc::new(AtomicU64::new(1)),
        };
        let worker = thread::Builder::new()
            .name("openlogi-buttons".into())
            .spawn(move || run_worker(&event_rx, &shutdown_rx, &generation, &on_event))?;
        Ok(Self {
            input,
            shutdown,
            worker: Some(worker),
        })
    }

    pub(crate) fn input(&self) -> ButtonInputHandle {
        self.input.clone()
    }

    pub(crate) fn shutdown(&mut self) -> bool {
        self.shutdown_with_timeout(SHUTDOWN_TIMEOUT)
    }

    fn shutdown_with_timeout(&mut self, timeout: Duration) -> bool {
        let Some(worker) = self.worker.take() else {
            return true;
        };
        self.input.stop_accepting();
        let (done, wait) = mpsc::sync_channel(0);
        if self.shutdown.send(ShutdownRequest { done }).is_err() {
            let _ = worker.join();
            return false;
        }
        if wait.recv_timeout(timeout).is_err() {
            warn!("button lifecycle worker did not shut down before the deadline");
            // Dropping a JoinHandle detaches the worker; the queued request
            // still makes it exit if the current terminal handler returns.
            return false;
        }
        if worker.join().is_err() {
            warn!("button lifecycle worker panicked during shutdown");
            return false;
        }
        true
    }
}

impl Drop for ButtonRuntimeOwner {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn run_worker(
    events: &mpsc::Receiver<ButtonCommand>,
    shutdown: &mpsc::Receiver<ShutdownRequest>,
    shared_generation: &AtomicU64,
    emit: &impl Fn(ButtonRuntimeEvent),
) {
    let mut state = ButtonState::default();
    let mut generation = shared_generation.load(Ordering::Acquire);
    loop {
        if let Ok(request) = shutdown.try_recv() {
            emit_canceled(state.cancel_all(), CancelReason::Shutdown, emit);
            let _ = request.done.send(());
            return;
        }
        let command = match events.recv_timeout(SHUTDOWN_POLL_INTERVAL) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                emit_canceled(state.cancel_all(), CancelReason::SourceEnded, emit);
                return;
            }
        };
        let current = shared_generation.load(Ordering::Acquire);
        if current != generation {
            emit_canceled(state.cancel_all(), CancelReason::Invalidated, emit);
            generation = current;
        }
        match command {
            ButtonCommand::Input {
                generation: input_generation,
                input,
            } if input_generation == generation => process_input(&mut state, input, emit),
            ButtonCommand::Input { .. } | ButtonCommand::Wake => {}
            ButtonCommand::CancelStalePress(token) => {
                if let Some(press) = state.cancel_press(&token) {
                    emit(ButtonRuntimeEvent::Ended {
                        press,
                        reason: EndReason::Canceled(CancelReason::StaleHold),
                    });
                }
            }
            ButtonCommand::CancelSource(source) => {
                emit_canceled(
                    state.cancel_source(&source),
                    CancelReason::SourceEnded,
                    emit,
                );
            }
            ButtonCommand::CancelHooks => {
                emit_canceled(state.cancel_hooks(), CancelReason::SourceEnded, emit);
            }
        }
    }
}

fn process_input(state: &mut ButtonState, input: ButtonInput, emit: &impl Fn(ButtonRuntimeEvent)) {
    match input {
        ButtonInput::Down(press) => {
            if let Some(stale) = state.press(press.clone()) {
                emit(ButtonRuntimeEvent::Ended {
                    press: stale,
                    reason: EndReason::Canceled(CancelReason::RepeatedDown),
                });
            }
            emit(ButtonRuntimeEvent::Started(press));
        }
        ButtonInput::Up(key) => {
            if let Some(press) = state.release(&key) {
                emit(ButtonRuntimeEvent::Ended {
                    press,
                    reason: EndReason::Released,
                });
            }
        }
        ButtonInput::Pulse(press) => {
            if let Some(stale) = state.press(press.clone()) {
                emit(ButtonRuntimeEvent::Ended {
                    press: stale,
                    reason: EndReason::Canceled(CancelReason::RepeatedDown),
                });
            }
            emit(ButtonRuntimeEvent::Started(press.clone()));
            if let Some(press) = state.release(&press.token.key) {
                emit(ButtonRuntimeEvent::Ended {
                    press,
                    reason: EndReason::Released,
                });
            }
        }
        ButtonInput::TriggerWhilePressed { token, action } => {
            if let Some(press) = state.active(&token).cloned() {
                emit(ButtonRuntimeEvent::Triggered { press, action });
            }
        }
    }
}

fn emit_canceled(
    presses: Vec<ActivePress>,
    reason: CancelReason,
    emit: &impl Fn(ButtonRuntimeEvent),
) {
    for press in presses {
        emit(ButtonRuntimeEvent::Ended {
            press,
            reason: EndReason::Canceled(reason),
        });
    }
}

#[cfg(test)]
mod tests;
