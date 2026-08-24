//! Source-independent button lifecycle state.
//!
//! Capture backends report different raw shapes: OS hooks carry a pressed
//! boolean, while HID++ diverted-control reports carry a snapshot of every
//! held control. Both are normalised to [`ButtonEvent`] before a bound action
//! runs, so hold-based consumers have one place to observe terminal `Up` and
//! `Cancel` phases.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::{self, ThreadId};
use std::time::Duration;

use openlogi_core::binding::{Action, ButtonId};
use tracing::warn;

/// The lifecycle queue is bounded because OS-hook callbacks must fail open
/// rather than block. An overflow also advances the generation, causing the
/// worker to cancel every accepted press before it handles another event.
const EVENT_QUEUE_CAPACITY: usize = 128;

/// The phase of one physical button lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ButtonPhase {
    /// The button became physically held.
    Down,
    /// The button was physically released.
    Up,
    /// Capture ended before a matching release could be guaranteed.
    Cancel,
}

/// The capture source that owns one button lifecycle.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ButtonOrigin {
    /// One OS-hook callback thread. Linux runs one per grabbed device; macOS
    /// and Windows use one callback thread for their global hook.
    OsHook(ThreadId),
    /// One HID++ capture-session epoch for a stable device config key.
    Hidpp { device_key: Arc<str>, epoch: u64 },
}

impl ButtonOrigin {
    /// Build the origin for the current OS-hook callback thread.
    pub(crate) fn current_os_hook() -> Self {
        Self::OsHook(std::thread::current().id())
    }

    /// Build the origin for one HID++ capture session.
    pub(crate) fn hidpp(device_key: &str, epoch: u64) -> Self {
        Self::Hidpp {
            device_key: Arc::from(device_key),
            epoch,
        }
    }

    /// Device config key for a HID++ source; OS hooks cannot reliably provide
    /// one on every platform.
    pub(crate) fn device_key(&self) -> Option<&str> {
        match self {
            Self::OsHook(_) => None,
            Self::Hidpp { device_key, .. } => Some(device_key),
        }
    }
}

/// One normalised button lifecycle event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ButtonEvent {
    pub(crate) origin: ButtonOrigin,
    pub(crate) button: ButtonId,
    pub(crate) phase: ButtonPhase,
}

impl ButtonEvent {
    /// Build one event from its correlation key and phase.
    pub(crate) fn new(origin: ButtonOrigin, button: ButtonId, phase: ButtonPhase) -> Self {
        Self {
            origin,
            button,
            phase,
        }
    }

    fn key(&self) -> ButtonKey {
        ButtonKey {
            origin: self.origin.clone(),
            button: self.button,
        }
    }

    fn with_phase(&self, phase: ButtonPhase) -> Self {
        Self::new(self.origin.clone(), self.button, phase)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ButtonKey {
    origin: ButtonOrigin,
    button: ButtonId,
}

/// Accepted events for one raw transition. A repeated `Down` first cancels
/// the stale lifecycle and then begins a fresh one, so every accepted press
/// still has exactly one terminal phase.
pub(crate) type ButtonTransitions = [Option<ButtonEvent>; 2];

/// Tracks active physical presses across capture sources.
#[derive(Default)]
pub(crate) struct ButtonRuntime {
    active: HashSet<ButtonKey>,
}

impl ButtonRuntime {
    /// Accept one raw event and return the lifecycle transitions consumers
    /// should observe. Stray terminal events are ignored.
    pub(crate) fn update(&mut self, event: ButtonEvent) -> ButtonTransitions {
        let key = event.key();
        match event.phase {
            ButtonPhase::Down => {
                if self.active.insert(key) {
                    [Some(event), None]
                } else {
                    [Some(event.with_phase(ButtonPhase::Cancel)), Some(event)]
                }
            }
            ButtonPhase::Up | ButtonPhase::Cancel if self.active.remove(&key) => {
                [Some(event), None]
            }
            ButtonPhase::Up | ButtonPhase::Cancel => [None, None],
        }
    }

    /// Cancel every active press owned by one capture source.
    pub(crate) fn cancel_origin(&mut self, origin: &ButtonOrigin) -> Vec<ButtonEvent> {
        self.cancel_where(|key| key.origin == *origin)
    }

    /// Cancel every active press, used when bindings change or the agent exits.
    pub(crate) fn cancel_all(&mut self) -> Vec<ButtonEvent> {
        self.cancel_where(|_| true)
    }

    fn cancel_where(&mut self, matches: impl Fn(&ButtonKey) -> bool) -> Vec<ButtonEvent> {
        let canceled: Vec<ButtonKey> = self
            .active
            .iter()
            .filter(|key| matches(key))
            .cloned()
            .collect();
        for key in &canceled {
            self.active.remove(key);
        }
        canceled
            .into_iter()
            .map(|key| ButtonEvent::new(key.origin, key.button, ButtonPhase::Cancel))
            .collect()
    }
}

enum ButtonCommand {
    Event {
        generation: u64,
        event: ButtonEvent,
        action: Option<Action>,
    },
    CancelOrigin(ButtonOrigin),
    Wake,
    Barrier(mpsc::SyncSender<()>),
}

/// Non-blocking producer handle for the single button lifecycle worker.
#[derive(Clone)]
pub(crate) struct ButtonRuntimeHandle {
    tx: mpsc::SyncSender<ButtonCommand>,
    generation: Arc<AtomicU64>,
    accepting: Arc<AtomicBool>,
}

impl ButtonRuntimeHandle {
    /// Start one lifecycle worker and call `on_event` for every accepted
    /// `Down`, `Up`, or `Cancel` transition.
    pub(crate) fn spawn(on_event: impl Fn(ButtonEvent, Option<Action>) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
        let generation = Arc::new(AtomicU64::new(0));
        let accepting = Arc::new(AtomicBool::new(true));
        let worker_generation = Arc::clone(&generation);
        let _ = thread::Builder::new()
            .name("openlogi-buttons".into())
            .spawn(move || run_worker(&rx, &worker_generation, on_event));
        Self {
            tx,
            generation,
            accepting,
        }
    }

    /// Queue one OS-hook phase without blocking its callback.
    pub(crate) fn try_hook_event(
        &self,
        button: ButtonId,
        phase: ButtonPhase,
        action: Option<&Action>,
    ) -> bool {
        self.try_event(ButtonOrigin::current_os_hook(), button, phase, action)
    }

    /// Cancel active presses owned by the current OS-hook callback thread.
    pub(crate) fn cancel_hook_thread(&self) {
        self.try_command(ButtonCommand::CancelOrigin(ButtonOrigin::current_os_hook()));
    }

    /// Queue one HID++ phase for a capture-session epoch.
    pub(crate) fn hidpp_event(
        &self,
        device_key: &str,
        epoch: u64,
        button: ButtonId,
        phase: ButtonPhase,
        action: Option<&Action>,
    ) {
        self.try_event(
            ButtonOrigin::hidpp(device_key, epoch),
            button,
            phase,
            action,
        );
    }

    /// Deliver one firmware-reported tap with no observable hold duration.
    pub(crate) fn hidpp_pulse(
        &self,
        device_key: &str,
        epoch: u64,
        button: ButtonId,
        action: Option<&Action>,
    ) {
        let origin = ButtonOrigin::hidpp(device_key, epoch);
        if self.try_event(origin.clone(), button, ButtonPhase::Down, action) {
            self.try_event(origin, button, ButtonPhase::Up, None);
        }
    }

    /// Cancel active presses owned by one HID++ session.
    pub(crate) fn cancel_hidpp_session(&self, device_key: &str, epoch: u64) {
        self.try_command(ButtonCommand::CancelOrigin(ButtonOrigin::hidpp(
            device_key, epoch,
        )));
    }

    /// Invalidate every active lifecycle. Queued events carrying the previous
    /// generation are ignored even if producers race this call.
    pub(crate) fn cancel_all(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        let _ = self.tx.try_send(ButtonCommand::Wake);
    }

    /// Snapshot the current binding/profile generation. Gesture accumulators
    /// use this to reject a semantic click or swipe whose physical hold began
    /// before a global lifecycle cancellation.
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Cancel every active lifecycle and wait briefly for the worker to run
    /// terminal handlers before process shutdown.
    pub(crate) fn cancel_all_and_wait(&self) {
        self.accepting.store(false, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        let (done, wait) = mpsc::sync_channel(0);
        if self.tx.send(ButtonCommand::Barrier(done)).is_ok() {
            let _ = wait.recv_timeout(Duration::from_secs(1));
        }
    }

    fn try_event(
        &self,
        origin: ButtonOrigin,
        button: ButtonId,
        phase: ButtonPhase,
        action: Option<&Action>,
    ) -> bool {
        let generation = self.generation.load(Ordering::Acquire);
        if !self.accepting.load(Ordering::Acquire) {
            return false;
        }
        self.try_command(ButtonCommand::Event {
            generation,
            event: ButtonEvent::new(origin, button, phase),
            action: action.cloned(),
        })
    }

    fn try_command(&self, command: ButtonCommand) -> bool {
        match self.tx.try_send(command) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(_)) => {
                // A missing terminal event is more dangerous than dropping an
                // entire lifecycle. The generation change makes the worker
                // cancel everything it accepted before processing more input.
                self.generation.fetch_add(1, Ordering::AcqRel);
                warn!("button lifecycle queue full — canceling active presses");
                false
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                warn!("button lifecycle worker unavailable — event ignored");
                false
            }
        }
    }
}

fn run_worker(
    rx: &mpsc::Receiver<ButtonCommand>,
    shared_generation: &AtomicU64,
    on_event: impl Fn(ButtonEvent, Option<Action>),
) {
    let mut runtime = ButtonRuntime::default();
    let mut generation = shared_generation.load(Ordering::Acquire);
    while let Ok(command) = rx.recv() {
        let current = shared_generation.load(Ordering::Acquire);
        if current != generation {
            emit_canceled(runtime.cancel_all(), &on_event);
            generation = current;
        }
        match command {
            ButtonCommand::Event {
                generation: event_generation,
                event,
                action,
            } if event_generation == generation => {
                for event in runtime.update(event).into_iter().flatten() {
                    let event_action = (event.phase == ButtonPhase::Down)
                        .then(|| action.clone())
                        .flatten();
                    on_event(event, event_action);
                }
            }
            ButtonCommand::Event { .. } | ButtonCommand::Wake => {}
            ButtonCommand::CancelOrigin(origin) => {
                emit_canceled(runtime.cancel_origin(&origin), &on_event);
            }
            ButtonCommand::Barrier(done) => {
                emit_canceled(runtime.cancel_all(), &on_event);
                let _ = done.send(());
            }
        }
    }
}

fn emit_canceled(events: Vec<ButtonEvent>, on_event: &impl Fn(ButtonEvent, Option<Action>)) {
    for event in events {
        on_event(event, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(button: ButtonId, phase: ButtonPhase) -> ButtonEvent {
        ButtonEvent::new(ButtonOrigin::current_os_hook(), button, phase)
    }

    #[test]
    fn down_terminates_once_with_up() {
        let mut runtime = ButtonRuntime::default();
        assert_eq!(
            runtime.update(hook(ButtonId::Back, ButtonPhase::Down)),
            [Some(hook(ButtonId::Back, ButtonPhase::Down)), None]
        );
        assert_eq!(
            runtime.update(hook(ButtonId::Back, ButtonPhase::Up)),
            [Some(hook(ButtonId::Back, ButtonPhase::Up)), None]
        );
        assert_eq!(
            runtime.update(hook(ButtonId::Back, ButtonPhase::Up)),
            [None, None],
            "a duplicate release must not produce a second terminal event"
        );
    }

    #[test]
    fn a_repress_cancels_the_stale_lifecycle_before_restarting() {
        let mut runtime = ButtonRuntime::default();
        runtime.update(hook(ButtonId::Back, ButtonPhase::Down));

        assert_eq!(
            runtime.update(hook(ButtonId::Back, ButtonPhase::Down)),
            [
                Some(hook(ButtonId::Back, ButtonPhase::Cancel)),
                Some(hook(ButtonId::Back, ButtonPhase::Down)),
            ]
        );
        assert_eq!(
            runtime.update(hook(ButtonId::Back, ButtonPhase::Up)),
            [Some(hook(ButtonId::Back, ButtonPhase::Up)), None]
        );
    }

    #[test]
    fn cancellation_is_scoped_to_one_session() {
        let mut runtime = ButtonRuntime::default();
        let first = ButtonOrigin::hidpp("mouse-a", 7);
        let second = ButtonOrigin::hidpp("mouse-b", 3);
        runtime.update(ButtonEvent::new(
            first.clone(),
            ButtonId::Back,
            ButtonPhase::Down,
        ));
        runtime.update(ButtonEvent::new(
            second.clone(),
            ButtonId::Back,
            ButtonPhase::Down,
        ));

        assert_eq!(
            runtime.cancel_origin(&first),
            vec![ButtonEvent::new(first, ButtonId::Back, ButtonPhase::Cancel,)]
        );
        assert_eq!(
            runtime.update(ButtonEvent::new(
                second.clone(),
                ButtonId::Back,
                ButtonPhase::Up,
            )),
            [
                Some(ButtonEvent::new(second, ButtonId::Back, ButtonPhase::Up,)),
                None,
            ],
            "canceling one device must not terminate another device's hold"
        );
    }

    #[test]
    fn cancel_all_terminates_overlapping_buttons() {
        let mut runtime = ButtonRuntime::default();
        runtime.update(hook(ButtonId::Back, ButtonPhase::Down));
        runtime.update(hook(ButtonId::Forward, ButtonPhase::Down));

        let canceled = runtime.cancel_all();
        assert_eq!(canceled.len(), 2);
        assert!(
            canceled
                .iter()
                .all(|event| event.phase == ButtonPhase::Cancel)
        );
        assert!(
            runtime.cancel_all().is_empty(),
            "each active press is canceled at most once"
        );
    }

    #[test]
    fn worker_delivers_down_and_cancel_to_one_consumer() {
        let (events, received) = mpsc::channel();
        let handle = ButtonRuntimeHandle::spawn(move |event, action| {
            events.send((event, action)).unwrap();
        });

        assert!(handle.try_hook_event(ButtonId::Back, ButtonPhase::Down, Some(&Action::Copy),));
        let (down, action) = received.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(down.button, ButtonId::Back);
        assert_eq!(down.phase, ButtonPhase::Down);
        assert_eq!(action, Some(Action::Copy));

        handle.cancel_all_and_wait();
        let (cancel, action) = received.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(cancel.button, ButtonId::Back);
        assert_eq!(cancel.phase, ButtonPhase::Cancel);
        assert_eq!(action, None, "terminal events never replay the down action");
        assert!(
            !handle.try_hook_event(ButtonId::Forward, ButtonPhase::Down, Some(&Action::Paste)),
            "shutdown must reject a new press racing process exit"
        );
    }

    #[test]
    fn worker_ignores_an_event_queued_before_generation_invalidation() {
        let (commands, queued) = mpsc::sync_channel(2);
        commands
            .send(ButtonCommand::Event {
                generation: 0,
                event: hook(ButtonId::Back, ButtonPhase::Down),
                action: Some(Action::Copy),
            })
            .unwrap();
        let generation = AtomicU64::new(1);
        drop(commands);

        let (events, received) = mpsc::channel();
        run_worker(&queued, &generation, move |event, action| {
            events.send((event, action)).unwrap();
        });

        assert!(
            received.try_recv().is_err(),
            "an old profile's queued down must not activate a new lifecycle"
        );
    }
}
