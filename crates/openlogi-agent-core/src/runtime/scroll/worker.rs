//! Worker ownership and non-blocking producer capability for wheel output.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use openlogi_core::config::VerticalScrollSensitivity;
use openlogi_core::scroll::{ScrollDelivery, ScrollDelta};
use openlogi_hook::PointerTarget;
use tracing::warn;

use super::{NotchAccumulator, ScrollEngine, ScrollFrame, ScrollSource, WheelDelta};
use crate::runtime::{HidppSessionId, receive_command};

/// OS-hook callbacks must fail open rather than wait for the worker.
const INPUT_QUEUE_CAPACITY: usize = 128;
/// Bounds graceful process shutdown if platform injection stops returning.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
const SMOOTH_SCROLL_FLAG: u8 = 0x80;
const SENSITIVITY_MASK: u8 = !SMOOTH_SCROLL_FLAG;

#[derive(Clone, Copy)]
struct ScrollPreferenceSnapshot {
    smooth_scroll: bool,
    vertical_sensitivity: VerticalScrollSensitivity,
}

/// Atomically published settings read from input callbacks and the scroll
/// worker without taking the orchestrator's config lock.
///
/// The smooth-scroll flag occupies the high bit and the validated `1..=100`
/// sensitivity occupies the low seven bits. One byte therefore publishes a
/// consistent settings snapshot instead of two independently changing values.
pub struct ScrollPreferences {
    encoded: AtomicU8,
}

impl ScrollPreferences {
    /// Create a live settings cell from validated config values.
    #[must_use]
    pub fn new(smooth_scroll: bool, vertical_sensitivity: VerticalScrollSensitivity) -> Self {
        Self {
            encoded: AtomicU8::new(Self::encode(smooth_scroll, vertical_sensitivity)),
        }
    }

    /// Publish both settings as one snapshot.
    pub fn publish(&self, smooth_scroll: bool, vertical_sensitivity: VerticalScrollSensitivity) {
        self.encoded.store(
            Self::encode(smooth_scroll, vertical_sensitivity),
            Ordering::Relaxed,
        );
    }

    /// Whether finite smooth scrolling is currently enabled.
    #[must_use]
    pub fn smooth_scroll_enabled(&self) -> bool {
        self.load().smooth_scroll
    }

    /// The current traditional vertical wheel sensitivity.
    #[must_use]
    pub fn vertical_sensitivity(&self) -> VerticalScrollSensitivity {
        self.load().vertical_sensitivity
    }

    fn encode(smooth_scroll: bool, vertical_sensitivity: VerticalScrollSensitivity) -> u8 {
        let sensitivity = u8::from(vertical_sensitivity);
        debug_assert_eq!(sensitivity & SMOOTH_SCROLL_FLAG, 0);
        sensitivity | if smooth_scroll { SMOOTH_SCROLL_FLAG } else { 0 }
    }

    fn load(&self) -> ScrollPreferenceSnapshot {
        let encoded = self.encoded.load(Ordering::Relaxed);
        let raw_sensitivity = encoded & SENSITIVITY_MASK;
        let Ok(vertical_sensitivity) = VerticalScrollSensitivity::try_new(raw_sensitivity) else {
            unreachable!("ScrollPreferences is initialized and published from validated values");
        };
        ScrollPreferenceSnapshot {
            smooth_scroll: encoded & SMOOTH_SCROLL_FLAG != 0,
            vertical_sensitivity,
        }
    }
}

#[derive(Clone, Copy)]
enum ScrollOutputMode {
    Smooth {
        at: Instant,
    },
    /// Smoothing is off and the impulse is a diverted horizontal wheel tick,
    /// emitted at once inside a phased gesture.
    Phased {
        at: Instant,
    },
    /// Eased whatever the app-wide smoothing setting: a diverted main wheel,
    /// whose own per-device switch asked for smoothing by diverting it.
    Eased {
        at: Instant,
    },
    /// A horizontal tick for an application's delivery lane, emitted as whole
    /// notches and never smoothed: an animated Ctrl+wheel lane would zoom once
    /// per frame. `window` is the root window the lane was resolved for, or
    /// `None` when it was resolved for the focused application instead.
    Deliver {
        via: ScrollDelivery,
        window: Option<u64>,
        at: Instant,
    },
    Direct,
}

impl ScrollOutputMode {
    /// Output for a diverted HID++ impulse under the given smoothing setting.
    /// `None` leaves the distance to direct, unphased wheel output.
    fn hidpp(smoothing: bool, impulse: WheelDelta, at: Instant) -> Option<Self> {
        if smoothing {
            Some(Self::Smooth { at })
        } else if impulse.y == 0.0 {
            Some(Self::Phased { at })
        } else {
            None
        }
    }
}

struct ScrollInput {
    generation: u64,
    source: ScrollSource,
    impulse: WheelDelta,
    output: ScrollOutputMode,
}

impl ScrollInput {
    /// The output this input gets under the current smoothing setting.
    ///
    /// A toggle can land between enqueue and dequeue. The physical event was
    /// already consumed by then, so input queued under the other setting is
    /// reclassified as if it were submitted now rather than dropped; a
    /// horizontal HID++ tick thereby keeps the scroll phases AppKit's swipe
    /// recognizers need.
    fn output_under(&self, smoothing: bool) -> ScrollOutputMode {
        match (self.output, &self.source) {
            (ScrollOutputMode::Eased { at }, _) => ScrollOutputMode::Eased { at },
            (ScrollOutputMode::Deliver { .. }, _) => self.output,
            (
                ScrollOutputMode::Smooth { at } | ScrollOutputMode::Phased { at },
                ScrollSource::Hidpp(_),
            ) => ScrollOutputMode::hidpp(smoothing, self.impulse, at)
                .unwrap_or(ScrollOutputMode::Direct),
            (
                ScrollOutputMode::Smooth { at } | ScrollOutputMode::Phased { at },
                ScrollSource::OsHook(_),
            ) if smoothing => ScrollOutputMode::Smooth { at },
            _ => ScrollOutputMode::Direct,
        }
    }
}

enum ScrollCommand {
    Input(ScrollInput),
    CancelSession(HidppSessionId),
    Wake,
}

struct ShutdownRequest {
    done: mpsc::SyncSender<()>,
}

/// Lossless fallback for overflow cancellation and graceful shutdown.
enum ScrollControl {
    CancelOverflowSession {
        session: HidppSessionId,
        generation: u64,
    },
    Shutdown(ShutdownRequest),
}

/// Highest overflow-cancelled HID++ session epoch per device in this worker
/// generation. A successor may emit while late input from its predecessor
/// remains rejected, and repeated session restarts replace one watermark.
struct OverflowCancellations {
    generation: u64,
    sessions: HashMap<String, u64>,
}

impl OverflowCancellations {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            sessions: HashMap::new(),
        }
    }

    fn cancel(&mut self, session: &HidppSessionId, generation: u64) -> bool {
        if generation != self.generation {
            return false;
        }
        self.sessions
            .entry(session.device_key().to_owned())
            .and_modify(|epoch| *epoch = (*epoch).max(session.epoch()))
            .or_insert_with(|| session.epoch());
        true
    }

    fn advance_to(&mut self, generation: u64) -> bool {
        if generation == self.generation {
            return false;
        }
        self.generation = generation;
        self.sessions.clear();
        true
    }

    fn accepts(&self, input: &ScrollInput) -> bool {
        input.generation == self.generation
            && input.source.hidpp_session().is_none_or(|session| {
                self.sessions
                    .get(session.device_key())
                    .is_none_or(|cancelled_epoch| session.epoch() > *cancelled_epoch)
            })
    }
}

/// Cloneable, non-owning capability for physical input producers.
///
/// Submission is always non-blocking. `false` asks each producer to use its
/// source-appropriate direct-output fallback.
#[derive(Clone)]
pub struct ScrollInputHandle {
    commands: mpsc::SyncSender<ScrollCommand>,
    controls: mpsc::Sender<ScrollControl>,
    generation: Arc<AtomicU64>,
    accepting: Arc<AtomicBool>,
    preferences: Arc<ScrollPreferences>,
}

impl ScrollInputHandle {
    /// Queue one ordinary wheel impulse from the current OS-hook thread.
    ///
    /// Pixel input, zero/non-finite distance, a full queue, or an unavailable
    /// worker are rejected so the callback fails open. With smoothing disabled,
    /// only a changed vertical distance is accepted; the worker emits that
    /// distance directly and the callback remains injection-free.
    #[must_use]
    pub fn try_hook_scroll(&self, delta: ScrollDelta) -> bool {
        if !self.accepting.load(Ordering::Acquire) {
            return false;
        }
        let Ok(impulse) = WheelDelta::try_from(delta) else {
            return false;
        };
        let preferences = self.preferences.load();
        let Some(scaled) =
            impulse.with_vertical_scale(preferences.vertical_sensitivity.scroll_multiplier())
        else {
            return false;
        };
        let output = if preferences.smooth_scroll {
            ScrollOutputMode::Smooth { at: Instant::now() }
        } else if scaled != impulse {
            ScrollOutputMode::Direct
        } else {
            return false;
        };
        self.try_enqueue(ScrollSource::current_hook(), scaled, output)
    }

    /// Queue one diverted thumb-wheel impulse from an active HID++ session.
    ///
    /// With smoothing on the impulse is eased. With it off, a horizontal-only
    /// impulse becomes a phased gesture, because AppKit's swipe recognizers
    /// (e.g. Messages revealing timestamps) ignore horizontal scrolls that
    /// carry no scroll phase. Anything else is rejected, which tells the
    /// already-diverted caller to inject the distance directly; unlike an OS
    /// hook, there is no physical event to pass through.
    #[must_use]
    pub(crate) fn try_hidpp_scroll(&self, session: &HidppSessionId, delta: ScrollDelta) -> bool {
        if !self.accepting.load(Ordering::Acquire) {
            return false;
        }
        let Ok(impulse) = WheelDelta::try_from(delta) else {
            return false;
        };
        let Some(output) = ScrollOutputMode::hidpp(
            self.preferences.smooth_scroll_enabled(),
            impulse,
            Instant::now(),
        ) else {
            return false;
        };
        self.try_enqueue(ScrollSource::Hidpp(session.clone()), impulse, output)
    }

    /// Queue one diverted main-wheel impulse from an active HID++ session.
    ///
    /// Always eased, whatever the app-wide smoothing setting says: the
    /// device's own smooth-wheel switch is what diverted the wheel. The
    /// vertical distance is scaled by the main-wheel sensitivity, as the hook
    /// scales a native wheel.
    ///
    /// # Errors
    ///
    /// The distance the already-diverted caller must inject directly when the
    /// runtime is stopped or its queue is full. It carries the same
    /// sensitivity scaling, so a refused report scrolls as far as a queued
    /// one. A delta that cannot be eased (not finite wheel ticks, or zero)
    /// comes back as given.
    pub(crate) fn try_hidpp_wheel_scroll(
        &self,
        session: &HidppSessionId,
        delta: ScrollDelta,
    ) -> Result<(), ScrollDelta> {
        let Ok(impulse) = WheelDelta::try_from(delta) else {
            return Err(delta);
        };
        let multiplier = self.preferences.vertical_sensitivity().scroll_multiplier();
        let Some(scaled) = impulse.with_vertical_scale(multiplier) else {
            return Err(delta);
        };
        let queued = self.accepting.load(Ordering::Acquire)
            && self.try_enqueue(
                ScrollSource::HidppMainWheel(session.clone()),
                scaled,
                ScrollOutputMode::Eased { at: Instant::now() },
            );
        if queued { Ok(()) } else { Err(scaled.into()) }
    }

    /// Queue one native horizontal wheel tick from the current OS-hook thread
    /// for delivery through `via`.
    ///
    /// `target` is the pointer window `via` was resolved for; once the pointer
    /// has left it or its application is unfocused, the worker emits the tick
    /// as a plain horizontal wheel instead. With no target (the focused policy)
    /// or an unidentified one, `via` is the focused application's, so the
    /// pointer must be over that application instead. Vertical distance, pixel
    /// input, a desktop target, a full queue, or an unavailable worker are
    /// rejected so the callback fails open.
    #[must_use]
    pub fn try_hook_deliver(
        &self,
        delta: ScrollDelta,
        via: ScrollDelivery,
        target: Option<PointerTarget>,
    ) -> bool {
        self.try_deliver(ScrollSource::current_hook(), delta, via, target)
    }

    /// Queue one diverted horizontal thumb-wheel impulse for delivery through
    /// `via`, as [`Self::try_hook_deliver`] does for the OS hook. A rejection
    /// leaves the already-diverted caller to its ordinary scroll output.
    #[must_use]
    pub(crate) fn try_hidpp_deliver(
        &self,
        session: &HidppSessionId,
        delta: ScrollDelta,
        via: ScrollDelivery,
        target: Option<PointerTarget>,
    ) -> bool {
        self.try_deliver(ScrollSource::Hidpp(session.clone()), delta, via, target)
    }

    fn try_deliver(
        &self,
        source: ScrollSource,
        delta: ScrollDelta,
        via: ScrollDelivery,
        target: Option<PointerTarget>,
    ) -> bool {
        if !self.accepting.load(Ordering::Acquire) {
            return false;
        }
        let Ok(impulse) = WheelDelta::try_from(delta) else {
            return false;
        };
        if impulse.y != 0.0 {
            return false;
        }
        let window = match target {
            Some(PointerTarget::Window { window_id, .. }) => Some(window_id),
            None | Some(PointerTarget::Unavailable) => None,
            Some(PointerTarget::Desktop | PointerTarget::Unsupported) => return false,
        };
        let output = ScrollOutputMode::Deliver {
            via,
            window,
            at: Instant::now(),
        };
        self.try_enqueue(source, impulse, output)
    }

    fn try_enqueue(
        &self,
        source: ScrollSource,
        impulse: WheelDelta,
        output: ScrollOutputMode,
    ) -> bool {
        let input = ScrollInput {
            generation: self.generation.load(Ordering::Acquire),
            source,
            impulse,
            output,
        };
        match self.commands.try_send(ScrollCommand::Input(input)) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(_)) => {
                warn!("scroll output queue full — input rejected");
                false
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                warn!("scroll output worker unavailable — input rejected");
                false
            }
        }
    }

    /// Invalidate every accepted OS-hook animation without blocking.
    pub fn cancel_hooks(&self) {
        self.cancel_all();
    }

    /// Cancel output belonging to one HID++ capture-session incarnation.
    pub(crate) fn cancel_hidpp_session(&self, session: &HidppSessionId) {
        let generation = self.generation.load(Ordering::Acquire);
        match self
            .commands
            .try_send(ScrollCommand::CancelSession(session.clone()))
        {
            Ok(()) | Err(mpsc::TrySendError::Disconnected(_)) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                if self
                    .controls
                    .send(ScrollControl::CancelOverflowSession {
                        session: session.clone(),
                        generation,
                    })
                    .is_ok()
                {
                    self.wake();
                }
            }
        }
    }

    fn cancel_all(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.wake();
    }

    fn wake(&self) {
        let _ = self.commands.try_send(ScrollCommand::Wake);
    }

    fn stop_accepting(&self) {
        self.accepting.store(false, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
}

/// Output for [`ScrollOutputMode::Deliver`] input.
pub(super) struct DeliveryOutput<W, F, E> {
    /// The root window now under the pointer, in [`PointerTarget::Window`]'s
    /// `window_id` terms.
    pub(super) pointer_root_window: W,
    /// Whether the pointer is now over the focused application. Keyboard
    /// modifiers go to the foreground thread, even under the pointer policy.
    pub(super) pointer_over_focused_app: F,
    /// Inject whole horizontal notches, positive scrolling right, through a
    /// delivery lane.
    pub(super) emit: E,
}

/// Unique owner of the scroll output worker and its graceful shutdown.
pub struct ScrollRuntime {
    input: ScrollInputHandle,
    controls: mpsc::Sender<ScrollControl>,
    worker: Option<JoinHandle<()>>,
}

impl ScrollRuntime {
    /// Start the dedicated output worker using the live scroll settings.
    pub fn spawn(preferences: Arc<ScrollPreferences>) -> io::Result<Self> {
        Self::spawn_with(
            preferences,
            ScrollFrame::post,
            WheelDelta::post,
            DeliveryOutput {
                pointer_root_window: openlogi_hook::pointer_root_window,
                pointer_over_focused_app: openlogi_hook::pointer_over_focused_process,
                emit: openlogi_inject::post_scroll_via,
            },
        )
    }

    pub(super) fn spawn_with(
        preferences: Arc<ScrollPreferences>,
        mut emit_smooth: impl FnMut(ScrollFrame) + Send + 'static,
        mut emit_direct: impl FnMut(WheelDelta) + Send + 'static,
        mut delivery: DeliveryOutput<
            impl FnMut() -> Option<u64> + Send + 'static,
            impl FnMut() -> bool + Send + 'static,
            impl FnMut(ScrollDelivery, i32) + Send + 'static,
        >,
    ) -> io::Result<Self> {
        let (commands, command_rx) = mpsc::sync_channel(INPUT_QUEUE_CAPACITY);
        let (controls, control_rx) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let input = ScrollInputHandle {
            commands,
            controls: controls.clone(),
            generation: Arc::clone(&generation),
            accepting: Arc::new(AtomicBool::new(true)),
            preferences: Arc::clone(&preferences),
        };
        let worker = thread::Builder::new()
            .name("openlogi-scroll".into())
            .spawn(move || {
                run_worker(
                    |deadline| receive_command(&command_rx, deadline),
                    &control_rx,
                    &generation,
                    &preferences,
                    &mut emit_smooth,
                    &mut emit_direct,
                    &mut delivery,
                );
            })?;
        Ok(Self {
            input,
            controls,
            worker: Some(worker),
        })
    }

    /// Clone the non-owning input capability for physical input producers.
    #[must_use]
    pub fn input(&self) -> ScrollInputHandle {
        self.input.clone()
    }

    /// Reject new input, cancel active output, and join the worker.
    pub fn shutdown(&mut self) {
        let _ = self.shutdown_with_timeout(SHUTDOWN_TIMEOUT);
    }

    fn shutdown_with_timeout(&mut self, timeout: Duration) -> bool {
        let Some(worker) = self.worker.take() else {
            return true;
        };
        self.input.stop_accepting();
        let (done, wait) = mpsc::sync_channel(0);
        if self
            .controls
            .send(ScrollControl::Shutdown(ShutdownRequest { done }))
            .is_err()
        {
            let _ = worker.join();
            return false;
        }
        // Send the wake only after the shutdown request is visible. Otherwise
        // an idle worker could consume it first and block again on `commands`.
        self.input.wake();
        if wait.recv_timeout(timeout).is_err() {
            warn!("smooth-scroll worker did not shut down before the deadline");
            return false;
        }
        if worker.join().is_err() {
            warn!("smooth-scroll worker panicked during shutdown");
            return false;
        }
        true
    }
}

impl Drop for ScrollRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_worker(
    mut receive: impl FnMut(Option<Instant>) -> Result<ScrollCommand, mpsc::RecvTimeoutError>,
    controls: &mpsc::Receiver<ScrollControl>,
    shared_generation: &AtomicU64,
    preferences: &ScrollPreferences,
    emit_smooth: &mut impl FnMut(ScrollFrame),
    emit_direct: &mut impl FnMut(WheelDelta),
    delivery: &mut DeliveryOutput<
        impl FnMut() -> Option<u64>,
        impl FnMut() -> bool,
        impl FnMut(ScrollDelivery, i32),
    >,
) {
    let mut engine = ScrollEngine::default();
    let mut notches = NotchAccumulator::default();
    let mut observed_smoothing = preferences.smooth_scroll_enabled();
    // An overflow-cancelled incarnation stays tombstoned so accepted input that
    // was already queued when control overtook the saturated queue is ignored.
    let mut cancellations = OverflowCancellations::new(shared_generation.load(Ordering::Acquire));
    loop {
        while let Ok(control) = controls.try_recv() {
            match control {
                ScrollControl::CancelOverflowSession {
                    session,
                    generation: cancelled_generation,
                } => {
                    // A control from a generation this worker has not observed
                    // yet is not stale: catch up first, or it is dropped and
                    // the cancelled session's queued input is accepted below.
                    if cancelled_generation > cancellations.generation
                        && cancellations.advance_to(shared_generation.load(Ordering::Acquire))
                    {
                        engine.cancel_all(emit_smooth);
                        notches.reset();
                    }
                    if cancellations.cancel(&session, cancelled_generation) {
                        engine.cancel_session(&session, emit_smooth);
                        notches.reset();
                    }
                }
                ScrollControl::Shutdown(request) => {
                    engine.cancel_all(emit_smooth);
                    let _ = request.done.send(());
                    return;
                }
            }
        }

        let command = receive(engine.next_deadline());

        // Settings can change while an idle worker waits for input. Observe the
        // new setting after receiving, so a toggle cancels the previous gesture
        // before the first input under the new setting starts another one.
        let smoothing = preferences.smooth_scroll_enabled();
        let toggled = smoothing != observed_smoothing;
        observed_smoothing = smoothing;
        if cancellations.advance_to(shared_generation.load(Ordering::Acquire)) {
            // Every accepted command from before the transition carries the
            // old generation and is rejected below, so its source tombstone
            // can no longer protect anything. A delayed old-generation control
            // is also ignored above instead of recreating it.
            engine.cancel_all(emit_smooth);
            notches.reset();
        } else if toggled {
            engine.cancel_all(emit_smooth);
            notches.reset();
        }

        match command {
            Ok(ScrollCommand::Input(input)) if cancellations.accepts(&input) => {
                match input.output_under(smoothing) {
                    ScrollOutputMode::Smooth { at } | ScrollOutputMode::Eased { at } => {
                        engine.impulse(input.source, input.impulse, at, emit_smooth);
                    }
                    ScrollOutputMode::Phased { at } => {
                        engine.phased_impulse(input.source, input.impulse, at, emit_smooth);
                    }
                    ScrollOutputMode::Deliver { via, window, at } => {
                        // A lane closes the HID++ thumb wheel's previous
                        // horizontal motion. Its main wheel has a separate
                        // source, so easing continues, as it does for the OS
                        // hook's main wheel.
                        if matches!(input.source, ScrollSource::Hidpp(_)) {
                            engine.cancel_source(&input.source, emit_smooth);
                        }
                        let fresh = (delivery.pointer_over_focused_app)()
                            && window.is_none_or(|window| {
                                (delivery.pointer_root_window)() == Some(window)
                            });
                        if fresh {
                            let due = notches.push(via, input.impulse.x, at);
                            if due != 0 {
                                (delivery.emit)(via, due);
                            }
                        } else {
                            // The pointer is not over the application this
                            // lane belongs to; that application's modifiers
                            // would misfire there, so it gets a plain wheel.
                            notches.reset();
                            emit_direct(input.impulse);
                        }
                    }
                    ScrollOutputMode::Direct => {
                        engine.cancel_source(&input.source, emit_smooth);
                        emit_direct(input.impulse);
                    }
                }
            }
            Ok(ScrollCommand::CancelSession(session)) => {
                engine.cancel_session(&session, emit_smooth);
                notches.reset();
            }
            Ok(ScrollCommand::Input(_) | ScrollCommand::Wake) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                engine.advance_due(Instant::now(), emit_smooth);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                engine.cancel_all(emit_smooth);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::PHASED_IDLE;
    use super::*;

    fn sensitivity(raw: u8) -> VerticalScrollSensitivity {
        VerticalScrollSensitivity::try_new(raw).expect("test sensitivity is valid")
    }

    fn preferences(smooth_scroll: bool, sensitivity: u8) -> Arc<ScrollPreferences> {
        Arc::new(ScrollPreferences::new(
            smooth_scroll,
            self::sensitivity(sensitivity),
        ))
    }

    type NoDelivery = DeliveryOutput<fn() -> Option<u64>, fn() -> bool, fn(ScrollDelivery, i32)>;

    fn no_delivery() -> NoDelivery {
        DeliveryOutput {
            pointer_root_window: || None,
            pointer_over_focused_app: || false,
            emit: |_, _| panic!("no delivery-lane input was submitted"),
        }
    }

    fn standalone_input(
        capacity: usize,
        preferences: Arc<ScrollPreferences>,
    ) -> (
        ScrollInputHandle,
        mpsc::Receiver<ScrollCommand>,
        mpsc::Receiver<ScrollControl>,
    ) {
        let (commands, receiver) = mpsc::sync_channel(capacity);
        let (controls, control_rx) = mpsc::channel();
        (
            ScrollInputHandle {
                commands,
                controls,
                generation: Arc::new(AtomicU64::new(0)),
                accepting: Arc::new(AtomicBool::new(true)),
                preferences,
            },
            receiver,
            control_rx,
        )
    }

    fn queued_input(receiver: &mpsc::Receiver<ScrollCommand>) -> ScrollInput {
        let ScrollCommand::Input(input) = receiver.recv().expect("queued input") else {
            panic!("expected scroll input");
        };
        input
    }

    #[test]
    fn callback_submission_rejects_ineligible_input_and_fails_open_when_full() {
        let (disabled, _commands, _controls) = standalone_input(
            1,
            preferences(false, u8::from(VerticalScrollSensitivity::DEFAULT)),
        );
        assert!(!disabled.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));

        let (input, _commands, _controls) = standalone_input(
            1,
            preferences(true, u8::from(VerticalScrollSensitivity::DEFAULT)),
        );
        assert!(!input.try_hook_scroll(ScrollDelta::pixels(0.0, 1.0)));
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));
        assert!(!input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));
    }

    #[test]
    fn hook_scales_only_vertical_wheel_distance_and_selects_direct_output() {
        let (input, receiver, _controls) = standalone_input(2, preferences(false, 7));
        assert!(!input.try_hook_scroll(ScrollDelta::wheel_ticks(2.0, 0.0)));
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(2.0, 2.0)));

        let queued = queued_input(&receiver);
        assert_eq!(queued.impulse, WheelDelta { x: 2.0, y: 1.0 });
        assert!(matches!(queued.output, ScrollOutputMode::Direct));
    }

    #[test]
    fn hook_scales_vertical_distance_before_smoothing() {
        let (input, receiver, _controls) = standalone_input(1, preferences(true, 7));
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(2.0, 2.0)));

        let queued = queued_input(&receiver);
        assert_eq!(queued.impulse, WheelDelta { x: 2.0, y: 1.0 });
        assert!(matches!(queued.output, ScrollOutputMode::Smooth { .. }));
    }

    #[test]
    fn hidpp_smoothing_does_not_apply_main_wheel_sensitivity() {
        let (input, receiver, _controls) = standalone_input(1, preferences(true, 7));
        let session = HidppSessionId::with_epoch("mouse-a", 7);
        assert!(input.try_hidpp_scroll(&session, ScrollDelta::wheel_ticks(0.0, 2.0)));

        let queued = queued_input(&receiver);
        assert_eq!(queued.impulse, WheelDelta { x: 0.0, y: 2.0 });
        assert!(matches!(queued.output, ScrollOutputMode::Smooth { .. }));
    }

    #[test]
    fn hidpp_without_smoothing_phases_only_horizontal_ticks() {
        let (input, receiver, _controls) = standalone_input(3, preferences(false, 14));
        let session = HidppSessionId::with_epoch("mouse-a", 7);
        assert!(input.try_hidpp_scroll(&session, ScrollDelta::wheel_ticks(2.0, 0.0)));
        assert!(
            !input.try_hidpp_scroll(&session, ScrollDelta::wheel_ticks(0.0, 2.0)),
            "a vertical tick keeps the caller's direct, unphased wheel output"
        );
        assert!(
            !input.try_hidpp_scroll(&session, ScrollDelta::wheel_ticks(2.0, 1.0)),
            "a mixed tick is not a horizontal wheel"
        );

        let queued = queued_input(&receiver);
        assert_eq!(queued.impulse, WheelDelta { x: 2.0, y: 0.0 });
        assert!(matches!(queued.output, ScrollOutputMode::Phased { .. }));
    }

    /// The device's own switch diverted the wheel to ease it, so its input
    /// eases and takes the main-wheel sensitivity under either app-wide
    /// smoothing setting, at submission and after a toggle while queued.
    #[test]
    fn a_diverted_main_wheel_is_eased_and_scaled_whatever_the_app_setting() {
        for smoothing in [false, true] {
            let (input, receiver, _controls) = standalone_input(1, preferences(smoothing, 7));
            let session = HidppSessionId::with_epoch("mouse-a", 7);
            assert_eq!(
                input.try_hidpp_wheel_scroll(&session, ScrollDelta::wheel_ticks(0.0, 2.0)),
                Ok(())
            );
            let queued = queued_input(&receiver);
            assert_eq!(queued.impulse, WheelDelta { x: 0.0, y: 1.0 });
            for dequeued_under in [false, true] {
                assert!(
                    matches!(
                        queued.output_under(dequeued_under),
                        ScrollOutputMode::Eased { .. }
                    ),
                    "queued with smoothing {smoothing}, dequeued with {dequeued_under}"
                );
            }
        }
    }

    /// What the runtime cannot ease comes back as given; what it cannot
    /// queue comes back scaled, so the caller's direct injection scrolls as
    /// far as the eased output would have.
    #[test]
    fn a_diverted_main_wheel_hands_back_what_it_cannot_take() {
        let (input, receiver, _controls) = standalone_input(1, preferences(true, 7));
        let session = HidppSessionId::with_epoch("mouse-a", 7);
        let pixels = ScrollDelta::pixels(0.0, 1.0);
        assert_eq!(input.try_hidpp_wheel_scroll(&session, pixels), Err(pixels));
        let zero = ScrollDelta::wheel_ticks(0.0, 0.0);
        assert_eq!(input.try_hidpp_wheel_scroll(&session, zero), Err(zero));

        let tick = ScrollDelta::wheel_ticks(0.0, 2.0);
        let scaled = ScrollDelta::wheel_ticks(0.0, 1.0);
        assert_eq!(input.try_hidpp_wheel_scroll(&session, tick), Ok(()));
        assert_eq!(
            input.try_hidpp_wheel_scroll(&session, tick),
            Err(scaled),
            "a full queue hands back the scaled distance"
        );
        queued_input(&receiver);
        input.accepting.store(false, Ordering::Release);
        assert_eq!(
            input.try_hidpp_wheel_scroll(&session, tick),
            Err(scaled),
            "a stopped runtime hands back the scaled distance"
        );
    }

    #[test]
    fn phased_horizontal_ticks_begin_and_end_through_the_worker() {
        let (frames, received) = mpsc::channel();
        let mut runtime = ScrollRuntime::spawn_with(
            preferences(false, 14),
            move |frame| {
                frames
                    .send(frame)
                    .expect("test frame receiver remains open");
            },
            |_| {},
            no_delivery(),
        )
        .expect("spawn scroll worker");
        let input = runtime.input();
        let session = HidppSessionId::with_epoch("mouse-a", 1);
        assert!(input.try_hidpp_scroll(&session, ScrollDelta::wheel_ticks(1.0, 0.0)));
        assert!(input.try_hidpp_scroll(&session, ScrollDelta::wheel_ticks(1.0, 0.0)));

        let wait = PHASED_IDLE * 20;
        let first = received.recv_timeout(wait).expect("first tick");
        let second = received.recv_timeout(wait).expect("second tick");
        assert_eq!(first.phase, openlogi_inject::SmoothScrollPhase::Began);
        assert_eq!(second.phase, openlogi_inject::SmoothScrollPhase::Changed);
        assert_eq!(first.delta, WheelDelta { x: 1.0, y: 0.0 });

        let end = received.recv_timeout(wait).expect("idle end");
        assert_eq!(end.phase, openlogi_inject::SmoothScrollPhase::Ended);
        assert!(end.delta.is_zero());
        assert!(
            received.recv_timeout(PHASED_IDLE * 2).is_err(),
            "gesture ends once"
        );
        runtime.shutdown();
    }

    #[test]
    fn shutdown_cancels_an_open_phased_gesture() {
        let (frames, received) = mpsc::channel();
        let mut runtime = ScrollRuntime::spawn_with(
            preferences(false, 14),
            move |frame| {
                frames
                    .send(frame)
                    .expect("test frame receiver remains open");
            },
            |_| {},
            no_delivery(),
        )
        .expect("spawn scroll worker");
        let session = HidppSessionId::with_epoch("mouse-a", 1);
        assert!(
            runtime
                .input()
                .try_hidpp_scroll(&session, ScrollDelta::wheel_ticks(1.0, 0.0))
        );
        assert_eq!(
            received
                .recv_timeout(Duration::from_secs(1))
                .expect("gesture begins")
                .phase,
            openlogi_inject::SmoothScrollPhase::Began
        );

        runtime.shutdown();
        assert_eq!(
            received
                .recv_timeout(Duration::from_secs(1))
                .expect("shutdown terminates the gesture")
                .phase,
            openlogi_inject::SmoothScrollPhase::Cancelled
        );
    }

    #[test]
    fn toggling_smoothing_cancels_an_open_phased_gesture() {
        let preferences = preferences(false, 14);
        let (frames, received) = mpsc::channel();
        let mut runtime = ScrollRuntime::spawn_with(
            Arc::clone(&preferences),
            move |frame| {
                frames
                    .send(frame)
                    .expect("test frame receiver remains open");
            },
            |_| {},
            no_delivery(),
        )
        .expect("spawn scroll worker");
        let input = runtime.input();
        let session = HidppSessionId::with_epoch("mouse-a", 1);
        assert!(input.try_hidpp_scroll(&session, ScrollDelta::wheel_ticks(1.0, 0.0)));
        assert_eq!(
            received
                .recv_timeout(Duration::from_secs(1))
                .expect("gesture begins")
                .phase,
            openlogi_inject::SmoothScrollPhase::Began
        );

        preferences.publish(true, sensitivity(14));
        input.wake();
        assert_eq!(
            received
                .recv_timeout(Duration::from_secs(1))
                .expect("toggle terminates the gesture")
                .phase,
            openlogi_inject::SmoothScrollPhase::Cancelled
        );
        runtime.shutdown();
    }

    fn first_horizontal_tick_after_idle_toggle(initial_smoothing: bool) {
        let preferences = preferences(initial_smoothing, 14);
        let (input, commands, controls) = standalone_input(1, Arc::clone(&preferences));
        let generation = Arc::clone(&input.generation);
        let (idle, waiting) = mpsc::sync_channel(0);
        let (resume, resumed) = mpsc::sync_channel(0);
        let (frames, received) = mpsc::channel();
        let worker_preferences = Arc::clone(&preferences);
        let worker = thread::spawn(move || {
            let mut first_receive = true;
            run_worker(
                |deadline| {
                    if first_receive {
                        first_receive = false;
                        assert_eq!(deadline, None, "the worker starts idle");
                        idle.send(()).expect("test waits for the idle worker");
                        resumed.recv().expect("test releases the idle worker");
                    }
                    receive_command(&commands, deadline)
                },
                &controls,
                &generation,
                &worker_preferences,
                &mut |frame| {
                    frames
                        .send(frame)
                        .expect("test frame receiver remains open");
                },
                &mut |_| panic!("horizontal HID++ input must carry scroll phases"),
                &mut no_delivery(),
            );
        });

        waiting
            .recv_timeout(Duration::from_secs(1))
            .expect("worker reaches its idle receive before the setting changes");
        preferences.publish(!initial_smoothing, sensitivity(14));
        resume.send(()).expect("worker resumes its receive");
        let session = HidppSessionId::with_epoch("mouse-a", 1);
        assert!(input.try_hidpp_scroll(&session, ScrollDelta::wheel_ticks(1.0, 0.0)));

        let mut output = Vec::new();
        while let Ok(frame) = received.recv_timeout(Duration::from_secs(1)) {
            let terminal = matches!(
                frame.phase,
                openlogi_inject::SmoothScrollPhase::Ended
                    | openlogi_inject::SmoothScrollPhase::Cancelled
            );
            output.push(frame);
            if terminal {
                break;
            }
        }
        drop(input);
        worker.join().expect("worker exits after input disconnects");

        assert_eq!(
            output.first().map(|frame| frame.phase),
            Some(openlogi_inject::SmoothScrollPhase::Began),
            "the first tick after a smoothing toggle must start a gesture"
        );
        assert_eq!(
            output.last().map(|frame| frame.phase),
            Some(openlogi_inject::SmoothScrollPhase::Ended),
            "the new gesture must finish without a stale toggle cancellation"
        );
        let distance = output
            .iter()
            .fold(WheelDelta::ZERO, |sum, frame| sum.plus(frame.delta));
        assert!(
            (distance.x - 1.0).abs() < 1.0e-12 && distance.y.abs() < 1.0e-12,
            "the first tick must retain its full distance: {distance:?}"
        );
    }

    #[test]
    fn idle_worker_keeps_first_tick_when_smoothing_is_enabled_without_wake() {
        first_horizontal_tick_after_idle_toggle(false);
    }

    #[test]
    fn idle_worker_keeps_first_tick_when_smoothing_is_disabled_without_wake() {
        first_horizontal_tick_after_idle_toggle(true);
    }

    fn horizontal_tick_queued_before_a_smoothing_toggle(initial_smoothing: bool) {
        let preferences = preferences(initial_smoothing, 14);
        let (input, commands, controls) = standalone_input(1, Arc::clone(&preferences));
        let generation = Arc::clone(&input.generation);
        let session = HidppSessionId::with_epoch("mouse-a", 1);
        assert!(input.try_hidpp_scroll(&session, ScrollDelta::wheel_ticks(1.0, 0.0)));
        preferences.publish(!initial_smoothing, sensitivity(14));

        let (frames, received) = mpsc::channel();
        let worker_preferences = Arc::clone(&preferences);
        let worker = thread::spawn(move || {
            run_worker(
                |deadline| receive_command(&commands, deadline),
                &controls,
                &generation,
                &worker_preferences,
                &mut |frame| {
                    frames
                        .send(frame)
                        .expect("test frame receiver remains open");
                },
                &mut |_| panic!("a horizontal HID++ tick must keep its scroll phases"),
                &mut no_delivery(),
            );
        });

        let mut output = Vec::new();
        while let Ok(frame) = received.recv_timeout(Duration::from_secs(1)) {
            let terminal = matches!(
                frame.phase,
                openlogi_inject::SmoothScrollPhase::Ended
                    | openlogi_inject::SmoothScrollPhase::Cancelled
            );
            output.push(frame);
            if terminal {
                break;
            }
        }
        drop(input);
        worker.join().expect("worker exits after input disconnects");

        assert_eq!(
            output.first().map(|frame| frame.phase),
            Some(openlogi_inject::SmoothScrollPhase::Began),
            "a tick queued under the old setting must start a gesture"
        );
        assert_eq!(
            output.last().map(|frame| frame.phase),
            Some(openlogi_inject::SmoothScrollPhase::Ended),
            "the gesture must finish rather than be dropped or cancelled"
        );
        let distance = output
            .iter()
            .fold(WheelDelta::ZERO, |sum, frame| sum.plus(frame.delta));
        assert!(
            (distance.x - 1.0).abs() < 1.0e-12 && distance.y.abs() < 1.0e-12,
            "the tick must keep its full distance: {distance:?}"
        );
    }

    #[test]
    fn horizontal_tick_queued_before_smoothing_is_disabled_becomes_a_phased_gesture() {
        horizontal_tick_queued_before_a_smoothing_toggle(true);
    }

    #[test]
    fn horizontal_tick_queued_before_smoothing_is_enabled_is_smoothed() {
        horizontal_tick_queued_before_a_smoothing_toggle(false);
    }

    #[test]
    fn stale_input_is_reclassified_as_if_submitted_now() {
        let at = Instant::now();
        let hidpp = ScrollSource::Hidpp(HidppSessionId::with_epoch("mouse-a", 1));
        let hook = ScrollSource::current_hook();
        let input = |source: &ScrollSource, x, y, output| ScrollInput {
            generation: 0,
            source: source.clone(),
            impulse: WheelDelta { x, y },
            output,
        };

        // Queued with smoothing on, dequeued with it off.
        assert!(matches!(
            input(&hidpp, 1.0, 0.0, ScrollOutputMode::Smooth { at }).output_under(false),
            ScrollOutputMode::Phased { at: kept } if kept == at
        ));
        assert!(matches!(
            input(&hidpp, 0.0, 1.0, ScrollOutputMode::Smooth { at }).output_under(false),
            ScrollOutputMode::Direct
        ));
        assert!(matches!(
            input(&hook, 0.0, 1.0, ScrollOutputMode::Smooth { at }).output_under(false),
            ScrollOutputMode::Direct
        ));
        // Queued with smoothing off, dequeued with it on.
        assert!(matches!(
            input(&hidpp, 1.0, 0.0, ScrollOutputMode::Phased { at }).output_under(true),
            ScrollOutputMode::Smooth { at: kept } if kept == at
        ));
        assert!(matches!(
            input(&hook, 0.0, 1.0, ScrollOutputMode::Direct).output_under(true),
            ScrollOutputMode::Direct
        ));
        // Input that still fits the setting is unchanged.
        assert!(matches!(
            input(&hidpp, 1.0, 0.0, ScrollOutputMode::Phased { at }).output_under(false),
            ScrollOutputMode::Phased { at: kept } if kept == at
        ));
        assert!(matches!(
            input(&hidpp, 0.0, 1.0, ScrollOutputMode::Smooth { at }).output_under(true),
            ScrollOutputMode::Smooth { at: kept } if kept == at
        ));
        assert!(matches!(
            input(&hook, 0.0, 1.0, ScrollOutputMode::Smooth { at }).output_under(true),
            ScrollOutputMode::Smooth { at: kept } if kept == at
        ));
        assert!(matches!(
            input(&hook, 0.0, 1.0, ScrollOutputMode::Direct).output_under(false),
            ScrollOutputMode::Direct
        ));
    }

    #[test]
    fn unphased_ticks_queued_before_smoothing_is_disabled_are_emitted_directly() {
        let preferences = preferences(true, 14);
        let (input, commands, controls) = standalone_input(2, Arc::clone(&preferences));
        let generation = Arc::clone(&input.generation);
        let session = HidppSessionId::with_epoch("mouse-a", 1);
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));
        assert!(input.try_hidpp_scroll(&session, ScrollDelta::wheel_ticks(0.0, 2.0)));
        preferences.publish(false, sensitivity(14));
        drop(input);

        let mut frames = Vec::new();
        let mut direct = Vec::new();
        run_worker(
            |deadline| receive_command(&commands, deadline),
            &controls,
            &generation,
            &preferences,
            &mut |frame| frames.push(frame),
            &mut |delta| direct.push(delta),
            &mut no_delivery(),
        );

        assert!(
            frames.is_empty(),
            "no gesture for unphased output: {frames:?}"
        );
        assert_eq!(
            direct,
            [WheelDelta { x: 0.0, y: 1.0 }, WheelDelta { x: 0.0, y: 2.0 }],
            "hook and vertical HID++ ticks keep their distance as direct output"
        );
    }

    #[test]
    fn live_preferences_change_hook_admission_and_output_mode() {
        let preferences = preferences(false, u8::from(VerticalScrollSensitivity::DEFAULT));
        let (input, receiver, _controls) = standalone_input(2, Arc::clone(&preferences));
        assert!(!input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));

        preferences.publish(false, sensitivity(7));
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 2.0)));
        assert!(matches!(
            queued_input(&receiver).output,
            ScrollOutputMode::Direct
        ));

        preferences.publish(true, sensitivity(28));
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));
        let queued = queued_input(&receiver);
        assert_eq!(queued.impulse, WheelDelta { x: 0.0, y: 2.0 });
        assert!(matches!(queued.output, ScrollOutputMode::Smooth { .. }));
    }

    #[test]
    fn hidpp_cancellation_targets_its_session() {
        let (input, commands, _controls) = standalone_input(1, preferences(true, 14));
        let session = HidppSessionId::with_epoch("mouse-a", 7);
        input.cancel_hidpp_session(&session);

        let ScrollCommand::CancelSession(cancelled) = commands.recv().expect("queued cancellation")
        else {
            panic!("expected HID++ source cancellation");
        };
        assert_eq!(cancelled, session);
        assert_eq!(input.generation.load(Ordering::Acquire), 0);
    }

    #[test]
    fn overflow_cancellation_rejects_both_wheels_but_accepts_the_successor() {
        let (input, receiver, _controls) = standalone_input(3, preferences(true, 14));
        let old = HidppSessionId::with_epoch("mouse-a", 7);
        let successor = HidppSessionId::with_epoch("mouse-a", 8);
        assert!(input.try_hidpp_scroll(&old, ScrollDelta::wheel_ticks(1.0, 0.0)));
        assert_eq!(
            input.try_hidpp_wheel_scroll(&old, ScrollDelta::wheel_ticks(0.0, 1.0)),
            Ok(())
        );
        assert_eq!(
            input.try_hidpp_wheel_scroll(&successor, ScrollDelta::wheel_ticks(0.0, 1.0)),
            Ok(())
        );

        let mut cancellations = OverflowCancellations::new(0);
        assert!(cancellations.cancel(&old, 0));
        assert!(!cancellations.accepts(&queued_input(&receiver)));
        assert!(!cancellations.accepts(&queued_input(&receiver)));
        assert!(cancellations.accepts(&queued_input(&receiver)));
    }

    #[test]
    fn cancellation_overtakes_a_full_queue_without_discarding_another_source() {
        let preferences = preferences(true, 14);
        let (input, commands, controls) = standalone_input(2, Arc::clone(&preferences));
        let cancelled = HidppSessionId::with_epoch("mouse-a", 7);
        let survivor = HidppSessionId::with_epoch("mouse-b", 3);
        assert!(input.try_hidpp_scroll(&cancelled, ScrollDelta::wheel_ticks(1.0, 0.0)));
        assert!(input.try_hidpp_scroll(&survivor, ScrollDelta::wheel_ticks(0.0, 1.0)));

        input.cancel_hidpp_session(&cancelled);
        assert_eq!(
            input.generation.load(Ordering::Acquire),
            0,
            "source-local cancellation must not invalidate unrelated accepted input"
        );

        let generation = Arc::clone(&input.generation);
        let (emitted, frames) = mpsc::channel();
        let worker = thread::spawn(move || {
            run_worker(
                |deadline| receive_command(&commands, deadline),
                &controls,
                &generation,
                &preferences,
                &mut |frame| {
                    emitted.send(frame).expect("frame receiver remains open");
                },
                &mut |_| {},
                &mut no_delivery(),
            );
        });

        let mut output = Vec::new();
        loop {
            let frame = frames
                .recv_timeout(Duration::from_secs(1))
                .expect("surviving source completes");
            output.push(frame);
            if frame.phase == openlogi_inject::SmoothScrollPhase::Ended {
                break;
            }
        }

        let (done, wait) = mpsc::sync_channel(0);
        input
            .controls
            .send(ScrollControl::Shutdown(ShutdownRequest { done }))
            .expect("worker control channel remains open");
        input.wake();
        wait.recv_timeout(Duration::from_secs(1))
            .expect("worker acknowledges shutdown");
        worker.join().expect("worker exits cleanly");

        let total = output
            .iter()
            .fold(WheelDelta::ZERO, |sum, frame| sum.plus(frame.delta));
        assert!(total.x.abs() < f64::EPSILON, "cancelled source emitted");
        assert!((total.y - 1.0).abs() < f64::EPSILON);
        assert!(
            output
                .iter()
                .all(|frame| frame.phase != openlogi_inject::SmoothScrollPhase::Cancelled)
        );
    }

    #[test]
    fn generation_transition_retires_overflow_tombstone_without_reviving_late_input() {
        let cancelled = HidppSessionId::with_epoch("mouse-a", 7);
        let successor = HidppSessionId::with_epoch("mouse-a", 8);
        let input = |session: &HidppSessionId, generation| ScrollInput {
            generation,
            source: ScrollSource::Hidpp(session.clone()),
            impulse: WheelDelta { x: 0.0, y: 1.0 },
            output: ScrollOutputMode::Direct,
        };
        let mut cancellations = OverflowCancellations::new(0);
        cancellations.cancel(&cancelled, 0);
        assert!(!cancellations.accepts(&input(&cancelled, 0)));
        assert!(cancellations.accepts(&input(&successor, 0)));
        assert!(
            !cancellations.accepts(&input(&cancelled, 0)),
            "a newer session must not resurrect late accepted input from its predecessor"
        );

        cancellations.cancel(&successor, 0);
        assert_eq!(
            cancellations.sessions.len(),
            1,
            "new session epochs replace rather than accumulate tombstones"
        );

        assert!(cancellations.advance_to(1));
        assert!(cancellations.sessions.is_empty());
        assert!(!cancellations.accepts(&input(&successor, 0)));
        assert!(cancellations.accepts(&input(&successor, 1)));

        assert!(!cancellations.cancel(&successor, 0));
        assert!(
            cancellations.accepts(&input(&successor, 1)),
            "a delayed old-generation control must not recreate its tombstone"
        );
    }

    #[test]
    fn stale_overflow_control_does_not_cancel_current_generation_motion() {
        let preferences = preferences(true, 14);
        let (commands, command_rx) = mpsc::sync_channel(2);
        let (controls, control_rx) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(1));
        let session = HidppSessionId::with_epoch("mouse-a", 7);
        let (emitted, frames) = mpsc::channel();
        let worker_generation = Arc::clone(&generation);
        let worker = thread::spawn(move || {
            run_worker(
                |deadline| receive_command(&command_rx, deadline),
                &control_rx,
                &worker_generation,
                &preferences,
                &mut |frame| emitted.send(frame).expect("frame receiver remains open"),
                &mut |_| {},
                &mut no_delivery(),
            );
        });

        commands
            .send(ScrollCommand::Input(ScrollInput {
                generation: 1,
                source: ScrollSource::Hidpp(session.clone()),
                impulse: WheelDelta { x: 0.0, y: 1.0 },
                output: ScrollOutputMode::Smooth { at: Instant::now() },
            }))
            .expect("worker command channel remains open");
        assert_eq!(
            frames
                .recv_timeout(Duration::from_secs(1))
                .expect("current-generation motion begins")
                .phase,
            openlogi_inject::SmoothScrollPhase::Began
        );

        controls
            .send(ScrollControl::CancelOverflowSession {
                session,
                generation: 0,
            })
            .expect("worker control channel remains open");
        commands
            .send(ScrollCommand::Wake)
            .expect("wake reaches the worker");

        let terminal = loop {
            let phase = frames
                .recv_timeout(Duration::from_secs(1))
                .expect("current-generation motion reaches a terminal phase")
                .phase;
            if matches!(
                phase,
                openlogi_inject::SmoothScrollPhase::Ended
                    | openlogi_inject::SmoothScrollPhase::Cancelled
            ) {
                break phase;
            }
        };
        assert_eq!(
            terminal,
            openlogi_inject::SmoothScrollPhase::Ended,
            "a stale control must not cancel current-generation engine state"
        );

        drop(commands);
        drop(controls);
        worker.join().expect("worker exits cleanly");
    }

    #[test]
    fn overflow_control_from_an_unobserved_generation_still_cancels_its_session() {
        let cancelled = HidppSessionId::with_epoch("mouse-a", 7);
        let survivor = HidppSessionId::with_epoch("mouse-b", 3);
        let direct = |session: &HidppSessionId, generation, y| {
            Ok(ScrollCommand::Input(ScrollInput {
                generation,
                source: ScrollSource::Hidpp(session.clone()),
                impulse: WheelDelta { x: 0.0, y },
                output: ScrollOutputMode::Direct,
            }))
        };
        let mut script = [
            direct(&survivor, 0, 1.0),
            direct(&cancelled, 1, 2.0),
            direct(&survivor, 1, 3.0),
        ]
        .into_iter();
        let (controls, control_rx) = mpsc::channel();
        let generation = AtomicU64::new(0);
        let mut emitted = Vec::new();

        run_worker(
            |_| {
                script
                    .next()
                    .unwrap_or(Err(mpsc::RecvTimeoutError::Disconnected))
            },
            &control_rx,
            &generation,
            &preferences(false, 14),
            &mut |_| {},
            &mut |delta| {
                if emitted.is_empty() {
                    // While the worker is still inside a generation-0 output
                    // callback, hooks are cancelled and new-generation input
                    // from `cancelled` saturates the queue.
                    generation.fetch_add(1, Ordering::AcqRel);
                    controls
                        .send(ScrollControl::CancelOverflowSession {
                            session: cancelled.clone(),
                            generation: 1,
                        })
                        .expect("worker control channel remains open");
                }
                emitted.push(delta);
            },
            &mut no_delivery(),
        );

        assert_eq!(
            emitted,
            [WheelDelta { x: 0.0, y: 1.0 }, WheelDelta { x: 0.0, y: 3.0 }],
            "the cancelled session must not emit, and other sources must"
        );
    }

    #[test]
    fn idle_worker_shutdown_wakes_after_publishing_its_request() {
        let mut runtime =
            ScrollRuntime::spawn_with(preferences(false, 14), |_| {}, |_| {}, no_delivery())
                .expect("spawn scroll worker");
        assert!(runtime.shutdown_with_timeout(Duration::from_millis(100)));
    }

    #[test]
    fn direct_scaled_output_is_emitted_by_the_worker() {
        let (outputs, received) = mpsc::channel();
        let smooth_outputs = outputs.clone();
        let mut runtime = ScrollRuntime::spawn_with(
            preferences(false, 7),
            move |frame| {
                smooth_outputs
                    .send(Err(frame))
                    .expect("test output receiver remains open");
            },
            move |delta| {
                outputs
                    .send(Ok(delta))
                    .expect("test output receiver remains open");
            },
            no_delivery(),
        )
        .expect("spawn scroll worker");
        assert!(
            runtime
                .input()
                .try_hook_scroll(ScrollDelta::wheel_ticks(3.0, 2.0))
        );

        assert_eq!(
            received
                .recv_timeout(Duration::from_secs(1))
                .expect("direct worker output")
                .expect("output must be direct"),
            WheelDelta { x: 3.0, y: 1.0 }
        );
        runtime.shutdown();
    }

    #[test]
    fn generation_invalidation_cancels_started_output_out_of_band() {
        let (frames, received) = mpsc::channel();
        let mut runtime = ScrollRuntime::spawn_with(
            preferences(true, 14),
            move |frame| {
                frames
                    .send(frame)
                    .expect("test frame receiver remains open");
            },
            |_| {},
            no_delivery(),
        )
        .expect("spawn scroll worker");
        let input = runtime.input();
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));
        assert_eq!(
            received
                .recv_timeout(Duration::from_secs(1))
                .expect("first animation frame")
                .phase,
            openlogi_inject::SmoothScrollPhase::Began
        );

        input.cancel_hooks();
        loop {
            let phase = received
                .recv_timeout(Duration::from_secs(1))
                .expect("cancellation frame")
                .phase;
            if phase == openlogi_inject::SmoothScrollPhase::Cancelled {
                break;
            }
            assert_eq!(phase, openlogi_inject::SmoothScrollPhase::Changed);
        }
        runtime.shutdown();
    }

    fn window(window_id: u64) -> PointerTarget {
        PointerTarget::Window {
            process_id: 41,
            window_id,
        }
    }

    #[test]
    fn delivery_submission_accepts_only_horizontal_wheel_ticks_and_fails_open_when_full() {
        let (input, receiver, _controls) = standalone_input(1, preferences(true, 14));
        for delta in [
            ScrollDelta::wheel_ticks(0.0, 1.0),
            ScrollDelta::wheel_ticks(1.0, 1.0),
            ScrollDelta::wheel_ticks(0.0, 0.0),
            ScrollDelta::pixels(1.0, 0.0),
        ] {
            assert!(
                !input.try_hook_deliver(delta, ScrollDelivery::ShiftWheel, Some(window(7))),
                "{delta:?}"
            );
        }
        let tick = ScrollDelta::wheel_ticks(1.0, 0.0);
        assert!(input.try_hook_deliver(tick, ScrollDelivery::ShiftWheel, Some(window(7))));
        assert!(
            !input.try_hook_deliver(tick, ScrollDelivery::ShiftWheel, Some(window(7))),
            "a full queue fails open"
        );

        let queued = queued_input(&receiver);
        for smoothing in [true, false] {
            assert!(
                matches!(
                    queued.output_under(smoothing),
                    ScrollOutputMode::Deliver {
                        via: ScrollDelivery::ShiftWheel,
                        window: Some(7),
                        ..
                    }
                ),
                "smoothing never claims a delivery-lane tick"
            );
        }
    }

    #[test]
    fn hidpp_delivery_keeps_its_session_and_rechecks_unidentified_targets_against_focus() {
        let (input, receiver, _controls) = standalone_input(1, preferences(false, 14));
        let session = HidppSessionId::with_epoch("mouse-a", 3);
        let tick = ScrollDelta::wheel_ticks(-0.5, 0.0);
        assert!(
            !input.try_hidpp_deliver(
                &session,
                tick,
                ScrollDelivery::CtrlWheel,
                Some(PointerTarget::Desktop),
            ),
            "no lane is resolved for the desktop"
        );
        assert!(input.try_hidpp_deliver(
            &session,
            tick,
            ScrollDelivery::CtrlWheel,
            Some(PointerTarget::Unavailable),
        ));

        let queued = queued_input(&receiver);
        assert_eq!(queued.source, ScrollSource::Hidpp(session));
        assert!(
            matches!(
                queued.output,
                ScrollOutputMode::Deliver {
                    via: ScrollDelivery::CtrlWheel,
                    window: None,
                    ..
                }
            ),
            "an unidentified pointer selected the focused application's lane"
        );
    }

    #[derive(Debug, PartialEq)]
    enum Emitted {
        Smooth(ScrollFrame),
        Direct(WheelDelta),
        Lane(ScrollDelivery, i32),
    }

    #[test]
    fn canceled_session_does_not_lend_its_partial_notch_to_its_successor() {
        for overflow in [false, true] {
            let old = HidppSessionId::with_epoch("mouse-a", 7);
            let successor = HidppSessionId::with_epoch("mouse-a", 8);
            let at = Instant::now();
            let tick = |session, x| {
                ScrollCommand::Input(ScrollInput {
                    generation: 0,
                    source: ScrollSource::Hidpp(session),
                    impulse: WheelDelta { x, y: 0.0 },
                    output: ScrollOutputMode::Deliver {
                        via: ScrollDelivery::CtrlWheel,
                        window: Some(7),
                        at,
                    },
                })
            };
            let boundary = if overflow {
                ScrollCommand::Wake
            } else {
                ScrollCommand::CancelSession(old.clone())
            };
            let mut script = [tick(old.clone(), 0.6), boundary, tick(successor, 0.4)]
                .into_iter()
                .enumerate();
            let (controls, control_rx) = mpsc::channel();
            let mut emitted = Vec::new();

            run_worker(
                |_| {
                    let Some((index, command)) = script.next() else {
                        return Err(mpsc::RecvTimeoutError::Disconnected);
                    };
                    if overflow && index == 1 {
                        controls
                            .send(ScrollControl::CancelOverflowSession {
                                session: old.clone(),
                                generation: 0,
                            })
                            .expect("control receiver remains open");
                    }
                    Ok(command)
                },
                &control_rx,
                &AtomicU64::new(0),
                &preferences(false, 14),
                &mut |_| panic!("delivery must not be smoothed"),
                &mut |_| panic!("pointer stays over the delivery window"),
                &mut DeliveryOutput {
                    pointer_root_window: || Some(7),
                    pointer_over_focused_app: || true,
                    emit: |via, notches| emitted.push((via, notches)),
                },
            );

            assert!(
                emitted.is_empty(),
                "a canceled 0.6 notch must not combine with its successor's 0.4: overflow={overflow}, {emitted:?}"
            );
        }
    }

    #[test]
    fn delivery_residue_obeys_global_resets_and_ignores_stale_controls() {
        #[derive(Clone, Copy, Debug)]
        enum Boundary {
            Unchanged,
            Hooks,
            Smoothing,
            StaleOverflow,
        }

        for boundary in [
            Boundary::Unchanged,
            Boundary::Hooks,
            Boundary::Smoothing,
            Boundary::StaleOverflow,
        ] {
            let session = HidppSessionId::with_epoch("mouse-a", 8);
            let at = Instant::now();
            let generation = AtomicU64::new(1);
            let preferences = preferences(false, 14);
            let (controls, control_rx) = mpsc::channel();
            let mut script = [Some(0.6), None, Some(0.4)].into_iter();
            let mut emitted = Vec::new();

            run_worker(
                |_| {
                    let Some(ticks) = script.next() else {
                        return Err(mpsc::RecvTimeoutError::Disconnected);
                    };
                    let Some(x) = ticks else {
                        match boundary {
                            Boundary::Unchanged => {}
                            Boundary::Hooks => {
                                generation.fetch_add(1, Ordering::AcqRel);
                            }
                            Boundary::Smoothing => {
                                preferences.publish(true, sensitivity(14));
                            }
                            Boundary::StaleOverflow => controls
                                .send(ScrollControl::CancelOverflowSession {
                                    session: session.clone(),
                                    generation: 0,
                                })
                                .expect("control receiver remains open"),
                        }
                        return Ok(ScrollCommand::Wake);
                    };
                    Ok(ScrollCommand::Input(ScrollInput {
                        generation: generation.load(Ordering::Acquire),
                        source: ScrollSource::Hidpp(session.clone()),
                        impulse: WheelDelta { x, y: 0.0 },
                        output: ScrollOutputMode::Deliver {
                            via: ScrollDelivery::CtrlWheel,
                            window: Some(7),
                            at,
                        },
                    }))
                },
                &control_rx,
                &generation,
                &preferences,
                &mut |_| panic!("delivery must not be smoothed"),
                &mut |_| panic!("pointer stays over the delivery window"),
                &mut DeliveryOutput {
                    pointer_root_window: || Some(7),
                    pointer_over_focused_app: || true,
                    emit: |via, notches| emitted.push((via, notches)),
                },
            );

            if matches!(boundary, Boundary::Unchanged | Boundary::StaleOverflow) {
                assert_eq!(emitted, [(ScrollDelivery::CtrlWheel, 1)], "{boundary:?}");
            } else {
                assert!(emitted.is_empty(), "{boundary:?}: {emitted:?}");
            }
        }
    }

    /// A worker whose every output lands on one channel, with the pointer
    /// fixed over `root_window`, inside or outside the focused application.
    fn recording_runtime(
        smooth_scroll: bool,
        root_window: Option<u64>,
        over_focused_app: bool,
    ) -> (ScrollRuntime, mpsc::Receiver<Emitted>) {
        let (outputs, received) = mpsc::channel();
        let smooth = outputs.clone();
        let direct = outputs.clone();
        let runtime = ScrollRuntime::spawn_with(
            preferences(smooth_scroll, 14),
            move |frame| {
                smooth
                    .send(Emitted::Smooth(frame))
                    .expect("test output receiver remains open");
            },
            move |delta| {
                direct
                    .send(Emitted::Direct(delta))
                    .expect("test output receiver remains open");
            },
            DeliveryOutput {
                pointer_root_window: move || root_window,
                pointer_over_focused_app: move || over_focused_app,
                emit: move |via, notches| {
                    outputs
                        .send(Emitted::Lane(via, notches))
                        .expect("test output receiver remains open");
                },
            },
        )
        .expect("spawn scroll worker");
        (runtime, received)
    }

    fn next(received: &mpsc::Receiver<Emitted>) -> Emitted {
        received
            .recv_timeout(Duration::from_secs(1))
            .expect("worker output")
    }

    #[test]
    fn review_pointer_lanes_are_plain_over_an_unfocused_application() {
        for via in [
            ScrollDelivery::ShiftWheel,
            ScrollDelivery::CtrlShiftWheel,
            ScrollDelivery::AltWheel,
            ScrollDelivery::CtrlWheel,
            ScrollDelivery::VerticalWheel,
        ] {
            let (mut runtime, received) = recording_runtime(false, Some(7), false);
            assert!(runtime.input().try_hook_deliver(
                ScrollDelta::wheel_ticks(1.0, 0.0),
                via,
                Some(window(7)),
            ));
            assert_eq!(
                next(&received),
                Emitted::Direct(WheelDelta { x: 1.0, y: 0.0 }),
                "an unfocused pointer application must keep the plain wheel: {via:?}"
            );
            runtime.shutdown();
        }
    }

    #[test]
    fn delivery_ticks_bypass_smoothing_while_the_pointer_stays() {
        let (mut runtime, received) = recording_runtime(true, Some(7), true);
        let input = runtime.input();
        assert!(input.try_hook_deliver(
            ScrollDelta::wheel_ticks(1.0, 0.0),
            ScrollDelivery::ShiftWheel,
            Some(window(7)),
        ));
        assert!(input.try_hook_deliver(
            ScrollDelta::wheel_ticks(-2.0, 0.0),
            ScrollDelivery::ShiftWheel,
            None,
        ));

        assert_eq!(
            next(&received),
            Emitted::Lane(ScrollDelivery::ShiftWheel, 1)
        );
        assert_eq!(
            next(&received),
            Emitted::Lane(ScrollDelivery::ShiftWheel, -2)
        );
        runtime.shutdown();
        assert!(received.try_recv().is_err(), "nothing was smoothed");
    }

    #[test]
    fn delivery_ticks_become_a_plain_wheel_once_the_pointer_leaves() {
        let (mut runtime, received) = recording_runtime(false, Some(8), true);
        let input = runtime.input();
        let half = ScrollDelta::wheel_ticks(0.5, 0.0);
        assert!(input.try_hook_deliver(half, ScrollDelivery::CtrlShiftWheel, Some(window(7))));
        assert!(input.try_hook_deliver(half, ScrollDelivery::CtrlShiftWheel, Some(window(7))));
        // A focused-policy lane follows the focused application instead.
        assert!(input.try_hook_deliver(
            ScrollDelta::wheel_ticks(1.0, 0.0),
            ScrollDelivery::CtrlShiftWheel,
            None,
        ));

        let plain = Emitted::Direct(WheelDelta { x: 0.5, y: 0.0 });
        assert_eq!(next(&received), plain);
        assert_eq!(next(&received), plain);
        assert_eq!(
            next(&received),
            Emitted::Lane(ScrollDelivery::CtrlShiftWheel, 1),
            "ticks sent as a plain wheel never count toward a lane notch"
        );
        runtime.shutdown();

        let (mut runtime, received) = recording_runtime(false, None, true);
        let input = runtime.input();
        assert!(input.try_hook_deliver(half, ScrollDelivery::CtrlShiftWheel, Some(window(7))));
        assert_eq!(
            next(&received),
            plain,
            "an unreadable pointer is not the lane's window"
        );
        runtime.shutdown();
    }

    #[test]
    fn a_lane_resolved_for_the_focused_app_is_plain_elsewhere() {
        let (mut runtime, received) = recording_runtime(false, Some(7), false);
        let input = runtime.input();
        let tick = ScrollDelta::wheel_ticks(1.0, 0.0);
        for target in [None, Some(PointerTarget::Unavailable)] {
            assert!(input.try_hook_deliver(tick, ScrollDelivery::CtrlWheel, target));
            assert_eq!(
                next(&received),
                Emitted::Direct(WheelDelta { x: 1.0, y: 0.0 }),
                "the pointer is over another application: {target:?}"
            );
        }
        runtime.shutdown();
    }

    #[test]
    fn a_lane_tick_leaves_the_hooks_smooth_vertical_motion_running() {
        let (mut runtime, received) = recording_runtime(true, Some(7), true);
        let input = runtime.input();
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));
        assert!(input.try_hook_deliver(
            ScrollDelta::wheel_ticks(1.0, 0.0),
            ScrollDelivery::ShiftWheel,
            Some(window(7)),
        ));

        let mut vertical = 0.0;
        let mut lanes = Vec::new();
        loop {
            match next(&received) {
                Emitted::Smooth(frame) => {
                    assert_ne!(
                        frame.phase,
                        openlogi_inject::SmoothScrollPhase::Cancelled,
                        "the lane tick cut the vertical motion short"
                    );
                    vertical += frame.delta.y;
                    if frame.phase == openlogi_inject::SmoothScrollPhase::Ended {
                        break;
                    }
                }
                Emitted::Lane(via, notches) => lanes.push((via, notches)),
                Emitted::Direct(delta) => panic!("unexpected plain wheel {delta:?}"),
            }
        }
        runtime.shutdown();

        assert_eq!(lanes, [(ScrollDelivery::ShiftWheel, 1)]);
        let target = sensitivity(14).scroll_multiplier();
        assert!(
            (vertical - target).abs() < 1.0e-9,
            "the vertical motion reached {vertical}, not {target}"
        );
    }

    #[test]
    fn a_lane_tick_leaves_the_same_sessions_diverted_main_wheel_running() {
        let (mut runtime, received) = recording_runtime(false, Some(7), true);
        let input = runtime.input();
        let session = HidppSessionId::with_epoch("mouse-a", 3);
        assert_eq!(
            input.try_hidpp_wheel_scroll(&session, ScrollDelta::wheel_ticks(0.0, 1.0)),
            Ok(())
        );
        let Emitted::Smooth(first) = next(&received) else {
            panic!("the diverted main wheel must begin before the lane tick");
        };
        assert_eq!(first.phase, openlogi_inject::SmoothScrollPhase::Began);
        assert!(input.try_hidpp_deliver(
            &session,
            ScrollDelta::wheel_ticks(1.0, 0.0),
            ScrollDelivery::CtrlWheel,
            Some(window(7)),
        ));

        let mut vertical = first.delta.y;
        let mut lanes = Vec::new();
        loop {
            match next(&received) {
                Emitted::Smooth(frame) => {
                    assert_ne!(
                        frame.phase,
                        openlogi_inject::SmoothScrollPhase::Cancelled,
                        "the thumb-wheel lane canceled the same session's main wheel"
                    );
                    vertical += frame.delta.y;
                    if frame.phase == openlogi_inject::SmoothScrollPhase::Ended {
                        break;
                    }
                }
                Emitted::Lane(via, notches) => lanes.push((via, notches)),
                Emitted::Direct(delta) => panic!("unexpected plain wheel {delta:?}"),
            }
        }
        runtime.shutdown();

        assert_eq!(lanes, [(ScrollDelivery::CtrlWheel, 1)]);
        let target = sensitivity(14).scroll_multiplier();
        assert!(
            (vertical - target).abs() < 1.0e-9,
            "the diverted main wheel reached {vertical}, not {target}"
        );
    }
}
