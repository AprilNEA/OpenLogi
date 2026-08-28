//! Source-independent button and key lifecycle state.
//!
//! Capture backends report different raw shapes: OS hooks carry discrete
//! edges, while HID++ diverted-control reports carry complete held-control
//! snapshots. Producers normalise both into typed inputs for one worker. The
//! worker is the sole owner of active presses and emits balanced lifecycle
//! events carrying a unique [`PressToken`].

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use openlogi_core::binding::{Action, Binding, ButtonId, LONG_PRESS_THRESHOLD};
use tracing::warn;

use super::ActionDispatchTarget;

/// OS-hook callbacks must fail open rather than block.
const EVENT_QUEUE_CAPACITY: usize = 128;
/// Bounds how long graceful process exit waits for terminal handlers.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
/// Lets the worker observe the out-of-band shutdown channel even while idle.
const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Process-unique identity of one HID++ hardware capture incarnation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct CaptureEpoch(u64);

/// A capture epoch bound to the config namespace its actions currently use.
/// The namespace can hot-swap while the epoch remains the task's stable input
/// identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct HidppSessionId {
    device_key: Arc<str>,
    epoch: CaptureEpoch,
}

/// Feeds [`HidppSessionId::new`] — one counter for the whole process.
static NEXT_SESSION_EPOCH: AtomicU64 = AtomicU64::new(1);

impl HidppSessionId {
    /// Mint the identity for a fresh capture-session incarnation. The epoch
    /// is allocated here rather than by the calling manager, so uniqueness
    /// holds by construction: the gesture and keyboard managers both run a
    /// session for an online bound keyboard, and manager-local counters would
    /// deterministically collide on its first sessions.
    pub(crate) fn new(device_key: &str) -> Self {
        Self {
            device_key: Arc::from(device_key),
            epoch: CaptureEpoch(NEXT_SESSION_EPOCH.fetch_add(1, Ordering::Relaxed)),
        }
    }

    /// Test-only: an id with a chosen epoch, so stale-vs-current cases are
    /// constructible regardless of minting order.
    #[cfg(test)]
    pub(crate) fn with_epoch(device_key: &str, epoch: u64) -> Self {
        Self {
            device_key: Arc::from(device_key),
            epoch: CaptureEpoch(epoch),
        }
    }

    pub(crate) fn device_key(&self) -> &str {
        &self.device_key
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.0
    }

    /// Whether two IDs name the same hardware capture incarnation even when
    /// its hot-swappable config namespace has changed.
    pub(crate) fn same_epoch(&self, other: &Self) -> bool {
        self.epoch == other.epoch
    }

    /// Move future dispatch from this hardware epoch to a different config
    /// namespace. Managers cancel the old action lifecycle before calling it.
    pub(crate) fn rekey(&mut self, device_key: &str) {
        self.device_key = Arc::from(device_key);
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

/// Physical control carried through a press lifecycle.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PressControl {
    /// A mouse or HID++ control represented in the shared binding schema.
    Button(ButtonId),
    /// A function key represented by its platform-neutral macOS keycode.
    Key(u16),
}

/// Correlation key shared by consecutive edges from one physical control.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PressKey {
    source: ButtonSource,
    control: PressControl,
}

impl PressKey {
    fn new(source: ButtonSource, button: ButtonId) -> Self {
        Self {
            source,
            control: PressControl::Button(button),
        }
    }

    fn for_key(source: ButtonSource, keycode: u16) -> Self {
        Self {
            source,
            control: PressControl::Key(keycode),
        }
    }
}

/// Unique identity of one accepted press, including a restart of the same key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PressId(u64);

/// Capability used to run a gesture action only while its originating press
/// remains active. Future timers and repeat workers can use the same token to
/// reject work scheduled by a superseded press.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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
    behavior: PressBehavior,
    target: ActionDispatchTarget,
}

/// Runtime-only state of the action semantics attached to one active press.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PressBehavior {
    /// Gesture lifecycles dispatch their semantic action separately.
    LifecycleOnly,
    /// Existing behavior: fire once when the press starts.
    Immediate(Action),
    /// Wait for either release or the threshold, whichever occurs first.
    LongPressPending {
        short: Action,
        long: Action,
        deadline: Instant,
    },
    /// The long action fired; release must not also fire the short action.
    LongPressFired,
}

impl PressBehavior {
    fn new(binding: Option<&Binding>, pressed_at: Instant) -> Self {
        match binding {
            None => Self::LifecycleOnly,
            Some(Binding::Single(action)) => Self::Immediate(action.clone()),
            Some(binding @ Binding::Gesture(_)) => Self::Immediate(binding.click_action()),
            Some(Binding::LongPress(binding)) => Self::LongPressPending {
                short: binding.short().clone(),
                long: binding.long().clone(),
                deadline: pressed_at + LONG_PRESS_THRESHOLD,
            },
        }
    }

    fn deadline(&self) -> Option<Instant> {
        match self {
            Self::LongPressPending { deadline, .. } => Some(*deadline),
            Self::LifecycleOnly | Self::Immediate(_) | Self::LongPressFired => None,
        }
    }

    fn start_action(&self) -> Option<&Action> {
        match self {
            Self::Immediate(action) => Some(action),
            Self::LifecycleOnly | Self::LongPressPending { .. } | Self::LongPressFired => None,
        }
    }

    fn release_action(&self) -> Option<&Action> {
        match self {
            Self::LongPressPending { short, .. } => Some(short),
            Self::LifecycleOnly | Self::Immediate(_) | Self::LongPressFired => None,
        }
    }

    fn fire_long(&mut self, now: Instant) -> Option<Action> {
        let Self::LongPressPending { long, deadline, .. } = self else {
            return None;
        };
        if now < *deadline {
            return None;
        }
        let action = long.clone();
        *self = Self::LongPressFired;
        Some(action)
    }
}

impl ActivePress {
    pub(crate) fn token(&self) -> &PressToken {
        &self.token
    }

    pub(crate) fn control(&self) -> &PressControl {
        &self.token.key.control
    }

    pub(crate) fn device_key(&self) -> Option<&str> {
        self.token.key.source.device_key()
    }

    pub(crate) fn start_action(&self) -> Option<&Action> {
        self.behavior.start_action()
    }

    pub(crate) fn target(&self) -> ActionDispatchTarget {
        self.target
    }

    fn release_action(&self) -> Option<&Action> {
        self.behavior.release_action()
    }

    fn fire_long(&mut self, now: Instant) -> Option<Action> {
        self.behavior.fire_long(now)
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
    /// An action admitted by an active press: a gesture, a long-press
    /// threshold, or a short release.
    Triggered {
        press: ActivePress,
        action: Action,
    },
}

/// Inputs cannot represent `Up + action` or a source-authored `Cancel`.
enum ButtonInput {
    Down(ActivePress),
    Up { key: PressKey, released_at: Instant },
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

    fn fire_selected_long_presses(
        &mut self,
        tokens: &[PressToken],
        now: Instant,
    ) -> Vec<(ActivePress, Action)> {
        tokens
            .iter()
            .filter_map(|token| {
                let press = self.active.get_mut(&token.key)?;
                if press.token != *token {
                    return None;
                }
                press.fire_long(now).map(|action| (press.clone(), action))
            })
            .collect()
    }

    fn has_due_long_press(&self, now: Instant) -> bool {
        self.active
            .values()
            .filter_map(|press| press.behavior.deadline())
            .any(|deadline| deadline <= now)
    }

    fn due_long_presses(&self, now: Instant) -> Vec<PressToken> {
        self.active
            .values()
            .filter(|press| {
                press
                    .behavior
                    .deadline()
                    .is_some_and(|deadline| deadline <= now)
            })
            .map(|press| press.token.clone())
            .collect()
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
    #[cfg(test)]
    pub(crate) fn try_hook_down(
        &self,
        button: ButtonId,
        binding: Option<&Binding>,
    ) -> Option<PressToken> {
        self.try_hook_down_with_target(button, binding, ActionDispatchTarget::capture())
    }

    pub(crate) fn try_hook_down_with_target(
        &self,
        button: ButtonId,
        binding: Option<&Binding>,
        target: ActionDispatchTarget,
    ) -> Option<PressToken> {
        self.try_down(ButtonSource::current_hook(), button, binding, target)
    }

    pub(crate) fn try_hook_up(&self, button: ButtonId) -> bool {
        self.try_up(ButtonSource::current_hook(), button)
    }

    pub(crate) fn try_hook_key_down(&self, keycode: u16, action: &Action) -> Option<PressToken> {
        let generation = self.generation.load(Ordering::Acquire);
        let press = self.new_press(
            PressKey::for_key(ButtonSource::current_hook(), keycode),
            PressBehavior::Immediate(action.clone()),
            generation,
            ActionDispatchTarget::capture(),
        );
        let token = press.token.clone();
        self.try_input(generation, ButtonInput::Down(press))
            .then_some(token)
    }

    pub(crate) fn try_hook_key_up(&self, keycode: u16) -> bool {
        let generation = self.generation.load(Ordering::Acquire);
        self.try_input(
            generation,
            ButtonInput::Up {
                key: PressKey::for_key(ButtonSource::current_hook(), keycode),
                released_at: Instant::now(),
            },
        )
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
        binding: Option<&Binding>,
        target: ActionDispatchTarget,
    ) -> Option<PressToken> {
        self.try_down(
            ButtonSource::Hidpp(session.clone()),
            button,
            binding,
            target,
        )
    }

    pub(crate) fn try_hidpp_up(&self, session: &HidppSessionId, button: ButtonId) -> bool {
        self.try_up(ButtonSource::Hidpp(session.clone()), button)
    }

    pub(crate) fn try_hidpp_pulse(
        &self,
        session: &HidppSessionId,
        button: ButtonId,
        binding: Option<&Binding>,
        target: ActionDispatchTarget,
    ) -> bool {
        let generation = self.generation.load(Ordering::Acquire);
        let press = self.new_press(
            PressKey::new(ButtonSource::Hidpp(session.clone()), button),
            PressBehavior::new(binding, Instant::now()),
            generation,
            target,
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
        binding: Option<&Binding>,
        target: ActionDispatchTarget,
    ) -> Option<PressToken> {
        let generation = self.generation.load(Ordering::Acquire);
        let press = self.new_press(
            PressKey::new(source, button),
            PressBehavior::new(binding, Instant::now()),
            generation,
            target,
        );
        let token = press.token.clone();
        self.try_input(generation, ButtonInput::Down(press))
            .then_some(token)
    }

    fn try_up(&self, source: ButtonSource, button: ButtonId) -> bool {
        let generation = self.generation.load(Ordering::Acquire);
        self.try_input(
            generation,
            ButtonInput::Up {
                key: PressKey::new(source, button),
                released_at: Instant::now(),
            },
        )
    }

    fn new_press(
        &self,
        key: PressKey,
        behavior: PressBehavior,
        generation: u64,
        target: ActionDispatchTarget,
    ) -> ActivePress {
        let id = PressId(self.next_press.fetch_add(1, Ordering::Relaxed));
        ActivePress {
            token: PressToken {
                id,
                key,
                generation,
            },
            behavior,
            target,
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
        mut on_event: impl FnMut(ButtonRuntimeEvent) + Send + 'static,
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
            .spawn(move || run_worker(&event_rx, &shutdown_rx, &generation, &mut on_event))?;
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
    emit: &mut impl FnMut(ButtonRuntimeEvent),
) {
    let mut state = ButtonState::default();
    let mut generation = shared_generation.load(Ordering::Acquire);
    loop {
        if finish_shutdown_if_requested(shutdown, &mut state, emit) {
            return;
        }
        let command = match events.recv_timeout(SHUTDOWN_POLL_INTERVAL) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if settle_due_long_presses(
                    events,
                    shutdown,
                    shared_generation,
                    &mut generation,
                    &mut state,
                    None,
                    emit,
                ) {
                    return;
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                emit_canceled(state.cancel_all(), CancelReason::SourceEnded, emit);
                return;
            }
        };
        synchronize_generation(&mut state, shared_generation, &mut generation, emit);
        if state.has_due_long_press(Instant::now()) {
            if settle_due_long_presses(
                events,
                shutdown,
                shared_generation,
                &mut generation,
                &mut state,
                Some(command),
                emit,
            ) {
                return;
            }
            continue;
        }
        process_command(&mut state, command, generation, emit);
        if settle_due_long_presses(
            events,
            shutdown,
            shared_generation,
            &mut generation,
            &mut state,
            None,
            emit,
        ) {
            return;
        }
    }
}

fn process_command(
    state: &mut ButtonState,
    command: ButtonCommand,
    generation: u64,
    emit: &mut impl FnMut(ButtonRuntimeEvent),
) {
    match command {
        ButtonCommand::Input {
            generation: input_generation,
            input,
        } if input_generation == generation => process_input(state, input, emit),
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

fn settle_due_long_presses(
    events: &mpsc::Receiver<ButtonCommand>,
    shutdown: &mpsc::Receiver<ShutdownRequest>,
    shared_generation: &AtomicU64,
    generation: &mut u64,
    state: &mut ButtonState,
    first_command: Option<ButtonCommand>,
    emit: &mut impl FnMut(ButtonRuntimeEvent),
) -> bool {
    if finish_shutdown_if_requested(shutdown, state, emit) {
        return true;
    }
    // An event handler may have blocked while another thread invalidated
    // this generation. Observe that transition before any overdue timer.
    synchronize_generation(state, shared_generation, generation, emit);
    let due_presses = state.due_long_presses(Instant::now());
    if due_presses.is_empty() {
        if let Some(command) = first_command {
            process_command(state, command, *generation, emit);
        }
        return false;
    }

    // Before firing an overdue timer, settle one channel-capacity snapshot.
    // FIFO ordering guarantees that this includes every command that was
    // already queued at the checkpoint. State transitions are separated from
    // synchronous handlers so unrelated actions cannot delay the deadline.
    let mut deferred = Vec::new();
    let queued_limit = if let Some(command) = first_command {
        process_command(state, command, *generation, &mut |event| {
            deferred.push(event);
        });
        if finish_deferred_shutdown_if_requested(shutdown, state, &due_presses, &mut deferred, emit)
        {
            return true;
        }
        synchronize_generation(state, shared_generation, generation, &mut |event| {
            deferred.push(event);
        });
        EVENT_QUEUE_CAPACITY - 1
    } else {
        EVENT_QUEUE_CAPACITY
    };
    for _ in 0..queued_limit {
        if finish_deferred_shutdown_if_requested(shutdown, state, &due_presses, &mut deferred, emit)
        {
            return true;
        }
        synchronize_generation(state, shared_generation, generation, &mut |event| {
            deferred.push(event);
        });
        let command = match events.try_recv() {
            Ok(command) => command,
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                emit_canceled(
                    state.cancel_all(),
                    CancelReason::SourceEnded,
                    &mut |event| deferred.push(event),
                );
                emit_settled_events(deferred, &due_presses, emit);
                return true;
            }
        };
        process_command(state, command, *generation, &mut |event| {
            deferred.push(event);
        });
        if finish_deferred_shutdown_if_requested(shutdown, state, &due_presses, &mut deferred, emit)
        {
            return true;
        }
        synchronize_generation(state, shared_generation, generation, &mut |event| {
            deferred.push(event);
        });
    }

    emit_selected_long_presses(state, &due_presses, Instant::now(), &mut |event| {
        deferred.push(event);
    });
    emit_settled_events(deferred, &due_presses, emit);
    false
}

fn finish_deferred_shutdown_if_requested(
    shutdown: &mpsc::Receiver<ShutdownRequest>,
    state: &mut ButtonState,
    due_presses: &[PressToken],
    deferred: &mut Vec<ButtonRuntimeEvent>,
    emit: &mut impl FnMut(ButtonRuntimeEvent),
) -> bool {
    let Ok(request) = shutdown.try_recv() else {
        return false;
    };
    emit_canceled(state.cancel_all(), CancelReason::Shutdown, &mut |event| {
        deferred.push(event);
    });
    emit_settled_events(std::mem::take(deferred), due_presses, emit);
    let _ = request.done.send(());
    true
}

fn emit_settled_events(
    events: Vec<ButtonRuntimeEvent>,
    due_presses: &[PressToken],
    emit: &mut impl FnMut(ButtonRuntimeEvent),
) {
    let (deadline_events, ordered_events): (Vec<_>, Vec<_>) =
        events.into_iter().partition(|event| match event {
            ButtonRuntimeEvent::Triggered { press, .. }
            | ButtonRuntimeEvent::Ended { press, .. } => {
                matches!(press.behavior, PressBehavior::LongPressFired)
                    && due_presses.contains(press.token())
            }
            ButtonRuntimeEvent::Started(_) => false,
        });
    for event in deadline_events.into_iter().chain(ordered_events) {
        emit(event);
    }
}

fn finish_shutdown_if_requested(
    shutdown: &mpsc::Receiver<ShutdownRequest>,
    state: &mut ButtonState,
    emit: &mut impl FnMut(ButtonRuntimeEvent),
) -> bool {
    let Ok(request) = shutdown.try_recv() else {
        return false;
    };
    emit_canceled(state.cancel_all(), CancelReason::Shutdown, emit);
    let _ = request.done.send(());
    true
}

fn synchronize_generation(
    state: &mut ButtonState,
    shared_generation: &AtomicU64,
    generation: &mut u64,
    emit: &mut impl FnMut(ButtonRuntimeEvent),
) {
    let current = shared_generation.load(Ordering::Acquire);
    if current != *generation {
        emit_canceled(state.cancel_all(), CancelReason::Invalidated, emit);
        *generation = current;
    }
}

fn process_input(
    state: &mut ButtonState,
    input: ButtonInput,
    emit: &mut impl FnMut(ButtonRuntimeEvent),
) {
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
        ButtonInput::Up { key, released_at } => {
            if let Some(mut press) = state.release(&key) {
                if let Some(action) = press.fire_long(released_at) {
                    emit(ButtonRuntimeEvent::Triggered {
                        press: press.clone(),
                        action,
                    });
                }
                emit_released(press, emit);
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
                emit_released(press, emit);
            }
        }
        ButtonInput::TriggerWhilePressed { token, action } => {
            if let Some(press) = state.active(&token).cloned() {
                emit(ButtonRuntimeEvent::Triggered { press, action });
            }
        }
    }
}

fn emit_selected_long_presses(
    state: &mut ButtonState,
    tokens: &[PressToken],
    now: Instant,
    emit: &mut impl FnMut(ButtonRuntimeEvent),
) {
    for (press, action) in state.fire_selected_long_presses(tokens, now) {
        emit(ButtonRuntimeEvent::Triggered { press, action });
    }
}

fn emit_released(press: ActivePress, emit: &mut impl FnMut(ButtonRuntimeEvent)) {
    if let Some(action) = press.release_action().cloned() {
        emit(ButtonRuntimeEvent::Triggered {
            press: press.clone(),
            action,
        });
    }
    emit(ButtonRuntimeEvent::Ended {
        press,
        reason: EndReason::Released,
    });
}

fn emit_canceled(
    presses: Vec<ActivePress>,
    reason: CancelReason,
    emit: &mut impl FnMut(ButtonRuntimeEvent),
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
