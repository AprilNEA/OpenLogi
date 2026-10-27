//! Worker ownership and non-blocking producer capability for wheel output.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use openlogi_core::config::VerticalScrollSensitivity;
use openlogi_core::scroll::ScrollDelta;
use tracing::{trace, warn};

use super::lo_model::{LoWheelModel, MiddlePressWindow};
use super::{ScrollEngine, ScrollFrame, ScrollSource, WheelDelta, WheelPixels};
use crate::runtime::HidppSessionId;

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
    /// Main-wheel output of the wheel model whatever the app-wide smoothing
    /// setting: a diverted main wheel, whose own per-device switch asked for
    /// smoothing by diverting it.
    MainWheel {
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
            (ScrollOutputMode::MainWheel { at }, _) => ScrollOutputMode::MainWheel { at },
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
    CancelSource(ScrollSource),
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
            && match &input.source {
                ScrollSource::OsHook(_) => true,
                ScrollSource::Hidpp(session) => self
                    .sessions
                    .get(session.device_key())
                    .is_none_or(|cancelled_epoch| session.epoch() > *cancelled_epoch),
            }
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
    middle_press: Arc<Mutex<MiddlePressWindow>>,
}

impl ScrollInputHandle {
    /// Tell the wheel model the middle button changed state.
    ///
    /// A press opens a short window in which wheel reports are dropped on the
    /// model paths (a diverted main wheel, and a hooked vertical wheel under
    /// smoothing), because the press itself disturbs the wheel. A release
    /// does not extend it. The window length is an assumption, see
    /// `MIDDLE_PRESS_WINDOW`.
    pub fn on_middle_button(&self, pressed: bool) {
        if pressed {
            self.middle_press
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .press(Instant::now());
        }
    }

    fn middle_press_disturbs(&self, at: Instant) -> bool {
        self.middle_press
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .disturbs(at)
    }

    /// Queue one ordinary wheel impulse from the current OS-hook thread.
    ///
    /// Pixel input, zero/non-finite distance, a full queue, or an unavailable
    /// worker are rejected so the callback fails open. With smoothing on, the
    /// raw ticks are queued unscaled: the wheel model applies the sensitivity
    /// as its speed setting, to the vertical part only. A diagonal event is
    /// accepted whole and the worker posts its two parts separately. The
    /// callback time-stamps the tick (the OS event's own time is not plumbed
    /// through the hook), so ticks delivered in one batch share the model's
    /// 8 ms period floor. With smoothing disabled, only a changed vertical
    /// distance is accepted; the worker emits that distance directly and the
    /// callback remains injection-free.
    #[must_use]
    pub fn try_hook_scroll(&self, delta: ScrollDelta) -> bool {
        if !self.accepting.load(Ordering::Acquire) {
            return false;
        }
        let Ok(impulse) = WheelDelta::try_from(delta) else {
            return false;
        };
        let preferences = self.preferences.load();
        if preferences.smooth_scroll {
            if impulse.x == 0.0 && self.middle_press_disturbs(Instant::now()) {
                // Swallowed: the press disturbed the wheel, not the user.
                return true;
            }
            return self.try_enqueue(
                ScrollSource::current_hook(),
                impulse,
                ScrollOutputMode::Smooth { at: Instant::now() },
            );
        }
        let Some(scaled) =
            impulse.with_vertical_scale(preferences.vertical_sensitivity.scroll_multiplier())
        else {
            return false;
        };
        if scaled == impulse {
            return false;
        }
        self.try_enqueue(
            ScrollSource::current_hook(),
            scaled,
            ScrollOutputMode::Direct,
        )
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
    /// Always run through the wheel model, whatever the app-wide smoothing
    /// setting says: the device's own smooth-wheel switch is what diverted the
    /// wheel. The queued distance is unscaled; the model applies the
    /// main-wheel sensitivity as its speed setting.
    ///
    /// # Errors
    ///
    /// The distance the already-diverted caller must inject directly when the
    /// runtime is stopped or its queue is full, scaled by the plain
    /// sensitivity multiplier: with no worker there is no model state to shape
    /// it. A delta the model cannot take (not finite wheel ticks, or zero)
    /// comes back as given.
    pub(crate) fn try_hidpp_wheel_scroll(
        &self,
        session: &HidppSessionId,
        delta: ScrollDelta,
    ) -> Result<(), ScrollDelta> {
        let Ok(impulse) = WheelDelta::try_from(delta) else {
            return Err(delta);
        };
        if self.accepting.load(Ordering::Acquire) && self.middle_press_disturbs(Instant::now()) {
            return Ok(());
        }
        let queued = self.accepting.load(Ordering::Acquire)
            && self.try_enqueue(
                ScrollSource::Hidpp(session.clone()),
                impulse,
                ScrollOutputMode::MainWheel { at: Instant::now() },
            );
        if queued {
            return Ok(());
        }
        let multiplier = self.preferences.vertical_sensitivity().scroll_multiplier();
        impulse
            .with_vertical_scale(multiplier)
            .map_or(Err(delta), |scaled| Err(scaled.into()))
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
        let source = ScrollSource::Hidpp(session.clone());
        match self
            .commands
            .try_send(ScrollCommand::CancelSource(source.clone()))
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
            WheelPixels::post,
        )
    }

    pub(super) fn spawn_with(
        preferences: Arc<ScrollPreferences>,
        mut emit_smooth: impl FnMut(ScrollFrame) + Send + 'static,
        mut emit_direct: impl FnMut(WheelDelta) + Send + 'static,
        mut emit_pixels: impl FnMut(WheelPixels) + Send + 'static,
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
            middle_press: Arc::default(),
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
                    &mut emit_pixels,
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

fn receive_command(
    commands: &mpsc::Receiver<ScrollCommand>,
    deadline: Option<Instant>,
) -> Result<ScrollCommand, mpsc::RecvTimeoutError> {
    deadline.map_or_else(
        || {
            commands
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        },
        |deadline| commands.recv_timeout(deadline.saturating_duration_since(Instant::now())),
    )
}

/// Run the vertical part of one main-wheel report through its source's model.
///
/// The report is fractional notches (a diverted HID++ report is
/// `steps / per_notch`), which is what the model takes: the device's wheel
/// multiplier cancels out of the physics, and a sub-step report contributes
/// its fraction instead of being rounded away. A hooked tick has no hi-res
/// steps and is fed as one whole report; it cannot be spread over the next
/// milliseconds without a timer, which the model has none of, so a hooked
/// wheel posts one larger event per tick. A report that moves less than a
/// pixel posts nothing; the remainder is carried.
fn model_pixels(
    models: &mut HashMap<ScrollSource, LoWheelModel>,
    source: &ScrollSource,
    notches: f64,
    at: Instant,
    speed_setting: f64,
) -> Option<WheelPixels> {
    let model = models
        .entry(source.clone())
        .or_insert_with(|| LoWheelModel::new(speed_setting));
    model.set_speed_setting(speed_setting);
    let out = model.process(notches, at);
    trace!(
        notches,
        pixels = out.pixels,
        gain = out.gain,
        ema_ips = out.ema_ips,
        "wheel model"
    );
    (out.pixels != 0).then_some(WheelPixels {
        pixels: out.pixels,
        line_delta: out.line_delta,
    })
}

/// The worker's three outputs: eased frames, direct distances, model pixels.
struct Emitters<'a, S, D, P> {
    smooth: &'a mut S,
    direct: &'a mut D,
    pixels: &'a mut P,
}

/// Send one accepted input to the output its mode selects.
fn route_input(
    input: ScrollInput,
    smoothing: bool,
    preferences: &ScrollPreferences,
    models: &mut HashMap<ScrollSource, LoWheelModel>,
    engine: &mut ScrollEngine,
    out: &mut Emitters<
        '_,
        impl FnMut(ScrollFrame),
        impl FnMut(WheelDelta),
        impl FnMut(WheelPixels),
    >,
) {
    match input.output_under(smoothing) {
        ScrollOutputMode::Smooth { at } | ScrollOutputMode::MainWheel { at } => {
            let is_main_wheel = matches!(
                (&input.source, input.output),
                (ScrollSource::OsHook(_), ScrollOutputMode::Smooth { .. })
                    | (ScrollSource::Hidpp(_), ScrollOutputMode::MainWheel { .. })
            );
            if is_main_wheel {
                // Only the vertical part is the main wheel's; a
                // hook's horizontal ticks keep the easing engine.
                if input.impulse.y != 0.0 {
                    let speed = preferences.vertical_sensitivity().lo_speed_setting();
                    if let Some(pixels) =
                        model_pixels(models, &input.source, input.impulse.y, at, speed)
                    {
                        (out.pixels)(pixels);
                    }
                }
                let horizontal = WheelDelta {
                    x: input.impulse.x,
                    y: 0.0,
                };
                if !horizontal.is_zero() {
                    engine.impulse(input.source, horizontal, at, out.smooth);
                }
            } else {
                engine.impulse(input.source, input.impulse, at, out.smooth);
            }
        }
        ScrollOutputMode::Phased { at } => {
            engine.phased_impulse(input.source, input.impulse, at, out.smooth);
        }
        ScrollOutputMode::Direct => {
            models.remove(&input.source);
            engine.cancel_source(&input.source, out.smooth);
            // A hook tick queued unscaled under smoothing (the
            // model applies the sensitivity itself) and
            // reclassified here by a toggle race still owes the
            // plain multiplier; every other direct input arrives
            // already scaled, or is never scaled.
            let unscaled_hook_tick = matches!(input.source, ScrollSource::OsHook(_))
                && matches!(input.output, ScrollOutputMode::Smooth { .. });
            let impulse = if unscaled_hook_tick {
                let multiplier = preferences.vertical_sensitivity().scroll_multiplier();
                input.impulse.with_vertical_scale(multiplier)
            } else {
                Some(input.impulse)
            };
            if let Some(impulse) = impulse {
                (out.direct)(impulse);
            }
        }
    }
}

fn run_worker(
    mut receive: impl FnMut(Option<Instant>) -> Result<ScrollCommand, mpsc::RecvTimeoutError>,
    controls: &mpsc::Receiver<ScrollControl>,
    shared_generation: &AtomicU64,
    preferences: &ScrollPreferences,
    emit_smooth: &mut impl FnMut(ScrollFrame),
    emit_direct: &mut impl FnMut(WheelDelta),
    emit_pixels: &mut impl FnMut(WheelPixels),
) {
    let mut engine = ScrollEngine::default();
    let mut models = HashMap::<ScrollSource, LoWheelModel>::new();
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
                        models.clear();
                        engine.cancel_all(emit_smooth);
                    }
                    if cancellations.cancel(&session, cancelled_generation) {
                        let source = ScrollSource::Hidpp(session);
                        models.remove(&source);
                        engine.cancel_source(&source, emit_smooth);
                    }
                }
                ScrollControl::Shutdown(request) => {
                    models.clear();
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
            models.clear();
            engine.cancel_all(emit_smooth);
        } else if toggled {
            models.clear();
            engine.cancel_all(emit_smooth);
        }

        match command {
            Ok(ScrollCommand::Input(input)) if cancellations.accepts(&input) => {
                route_input(
                    input,
                    smoothing,
                    preferences,
                    &mut models,
                    &mut engine,
                    &mut Emitters {
                        smooth: emit_smooth,
                        direct: emit_direct,
                        pixels: emit_pixels,
                    },
                );
            }
            Ok(ScrollCommand::CancelSource(source)) => {
                models.remove(&source);
                engine.cancel_source(&source, emit_smooth);
            }
            Ok(ScrollCommand::Input(_) | ScrollCommand::Wake) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                engine.advance_due(Instant::now(), emit_smooth);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                models.clear();
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
                middle_press: Arc::default(),
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
    fn hook_forwards_raw_ticks_to_the_wheel_model_when_smoothing() {
        let (input, receiver, _controls) = standalone_input(1, preferences(true, 7));
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(2.0, 2.0)));

        let queued = queued_input(&receiver);
        assert_eq!(
            queued.impulse,
            WheelDelta { x: 2.0, y: 2.0 },
            "the model applies the sensitivity as its speed setting; no pre-scaling"
        );
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

    /// The device's own switch diverted the wheel, so its input runs through
    /// the wheel model, unscaled, under either app-wide smoothing setting, at
    /// submission and after a toggle while queued.
    #[test]
    fn a_diverted_main_wheel_takes_the_model_whatever_the_app_setting() {
        for smoothing in [false, true] {
            let (input, receiver, _controls) = standalone_input(1, preferences(smoothing, 7));
            let session = HidppSessionId::with_epoch("mouse-a", 7);
            assert_eq!(
                input.try_hidpp_wheel_scroll(&session, ScrollDelta::wheel_ticks(0.0, 2.0)),
                Ok(())
            );
            let queued = queued_input(&receiver);
            assert_eq!(queued.impulse, WheelDelta { x: 0.0, y: 2.0 });
            for dequeued_under in [false, true] {
                assert!(
                    matches!(
                        queued.output_under(dequeued_under),
                        ScrollOutputMode::MainWheel { .. }
                    ),
                    "queued with smoothing {smoothing}, dequeued with {dequeued_under}"
                );
            }
        }
    }

    /// What the model cannot take comes back as given; what the runtime
    /// cannot queue comes back scaled by the plain multiplier, the best a
    /// caller's direct injection can do without model state.
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
            |_| {},
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
            |_| {},
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
            |_| {},
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
                &mut |_| {},
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
                &mut |_| {},
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
            &mut |_| {},
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
        assert_eq!(
            queued.impulse,
            WheelDelta { x: 0.0, y: 1.0 },
            "smoothing hands the raw tick to the model"
        );
        assert!(matches!(queued.output, ScrollOutputMode::Smooth { .. }));
    }

    #[test]
    fn hidpp_cancellation_targets_its_session() {
        let (input, commands, _controls) = standalone_input(1, preferences(true, 14));
        let session = HidppSessionId::with_epoch("mouse-a", 7);
        input.cancel_hidpp_session(&session);

        let ScrollCommand::CancelSource(ScrollSource::Hidpp(cancelled)) =
            commands.recv().expect("queued cancellation")
        else {
            panic!("expected HID++ source cancellation");
        };
        assert_eq!(cancelled, session);
        assert_eq!(input.generation.load(Ordering::Acquire), 0);
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
                &mut |_| {},
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
                &mut |_| {},
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
            &mut |_| {},
        );

        assert_eq!(
            emitted,
            [WheelDelta { x: 0.0, y: 1.0 }, WheelDelta { x: 0.0, y: 3.0 }],
            "the cancelled session must not emit, and other sources must"
        );
    }

    #[test]
    fn idle_worker_shutdown_wakes_after_publishing_its_request() {
        let mut runtime = ScrollRuntime::spawn_with(preferences(false, 14), |_| {}, |_| {}, |_| {})
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
            |_| {},
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
            |_| {},
        )
        .expect("spawn scroll worker");
        let input = runtime.input();
        // A horizontal hook tick is eased by the engine; a vertical one would
        // go to the wheel model, which has no animation to cancel.
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(1.0, 0.0)));
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

    /// Runs the worker over the commands already queued on `input` and returns
    /// what it emitted: eased frames, direct distances and model pixel events.
    fn drain(
        input: ScrollInputHandle,
        commands: &mpsc::Receiver<ScrollCommand>,
        controls: &mpsc::Receiver<ScrollControl>,
    ) -> (Vec<ScrollFrame>, Vec<WheelDelta>, Vec<WheelPixels>) {
        let generation = Arc::clone(&input.generation);
        let preferences = Arc::clone(&input.preferences);
        drop(input);
        let (mut frames, mut direct, mut pixels) = (Vec::new(), Vec::new(), Vec::new());
        run_worker(
            |deadline| receive_command(commands, deadline),
            controls,
            &generation,
            &preferences,
            &mut |frame| frames.push(frame),
            &mut |delta| direct.push(delta),
            &mut |event| pixels.push(event),
        );
        (frames, direct, pixels)
    }

    #[test]
    fn a_hook_tick_with_smoothing_posts_one_model_pixel_event_and_no_frames() {
        let (input, commands, controls) = standalone_input(4, preferences(true, 14));
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));
        let (frames, direct, pixels) = drain(input, &commands, &controls);
        assert!(
            frames.is_empty() && direct.is_empty(),
            "{frames:?} {direct:?}"
        );
        // One notch at gain 1 is 12.96 px, truncated; 12 px flush one line.
        assert_eq!(
            pixels,
            [WheelPixels {
                pixels: 12,
                line_delta: 1.2
            }]
        );
    }

    /// Receives everything a live worker emits, in order.
    enum Emitted {
        Frame(ScrollFrame),
        Pixels(WheelPixels),
    }

    fn spawn_recording(
        preferences: Arc<ScrollPreferences>,
    ) -> (ScrollRuntime, mpsc::Receiver<Emitted>) {
        let (sender, received) = mpsc::channel();
        let frames = sender.clone();
        let runtime = ScrollRuntime::spawn_with(
            preferences,
            move |frame| {
                frames
                    .send(Emitted::Frame(frame))
                    .expect("test receiver remains open");
            },
            |_| {},
            move |pixels| {
                sender
                    .send(Emitted::Pixels(pixels))
                    .expect("test receiver remains open");
            },
        )
        .expect("spawn scroll worker");
        (runtime, received)
    }

    /// Collects emissions until the eased stream reports its end.
    fn until_ended(received: &mpsc::Receiver<Emitted>) -> (Vec<ScrollFrame>, Vec<WheelPixels>) {
        let (mut frames, mut pixels) = (Vec::new(), Vec::new());
        loop {
            match received
                .recv_timeout(Duration::from_secs(1))
                .expect("emission before the stream ends")
            {
                Emitted::Frame(frame) => {
                    let ended = frame.phase == openlogi_inject::SmoothScrollPhase::Ended;
                    frames.push(frame);
                    if ended {
                        return (frames, pixels);
                    }
                }
                Emitted::Pixels(event) => pixels.push(event),
            }
        }
    }

    #[test]
    fn a_hook_tick_splits_into_model_pixels_and_an_eased_horizontal_part() {
        let (mut runtime, received) = spawn_recording(preferences(true, 14));
        assert!(
            runtime
                .input()
                .try_hook_scroll(ScrollDelta::wheel_ticks(2.0, -1.0))
        );
        let (frames, pixels) = until_ended(&received);
        runtime.shutdown();
        assert_eq!(
            pixels,
            [WheelPixels {
                pixels: -12,
                line_delta: -1.2
            }]
        );
        let horizontal: f64 = frames.iter().map(|frame| frame.delta.x).sum();
        assert!((horizontal - 2.0).abs() < 1e-9, "{frames:?}");
        assert!(frames.iter().all(|frame| frame.delta.y == 0.0));
    }

    #[test]
    fn a_diverted_main_wheel_posts_model_pixels_whatever_the_app_setting() {
        for smoothing in [false, true] {
            let (input, commands, controls) = standalone_input(4, preferences(smoothing, 14));
            let session = HidppSessionId::with_epoch("mouse-a", 1);
            assert_eq!(
                input.try_hidpp_wheel_scroll(&session, ScrollDelta::wheel_ticks(0.0, -1.0)),
                Ok(())
            );
            let (frames, direct, pixels) = drain(input, &commands, &controls);
            assert!(frames.is_empty() && direct.is_empty());
            assert_eq!(pixels.len(), 1, "smoothing {smoothing}");
            assert_eq!(pixels[0].pixels, -12);
        }
    }

    #[test]
    fn the_thumb_wheel_stays_on_the_easing_engine() {
        let (mut runtime, received) = spawn_recording(preferences(true, 14));
        let session = HidppSessionId::with_epoch("mouse-a", 1);
        assert!(
            runtime
                .input()
                .try_hidpp_scroll(&session, ScrollDelta::wheel_ticks(1.0, 0.0))
        );
        let (frames, pixels) = until_ended(&received);
        runtime.shutdown();
        assert!(pixels.is_empty());
        assert_eq!(
            frames.first().map(|frame| frame.phase),
            Some(openlogi_inject::SmoothScrollPhase::Began)
        );
        let horizontal: f64 = frames.iter().map(|frame| frame.delta.x).sum();
        assert!((horizontal - 1.0).abs() < 1e-9, "{frames:?}");
    }

    /// The sensitivity is the model's speed setting and is not applied on top:
    /// a lone notch is gain 1 (12 px) at every slider position.
    #[test]
    fn sensitivity_is_not_applied_on_top_of_the_model() {
        for raw in [1u8, 14, 28, 100] {
            let (input, commands, controls) = standalone_input(2, preferences(true, raw));
            assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));
            let (_, _, pixels) = drain(input, &commands, &controls);
            assert_eq!(pixels.len(), 1, "sensitivity {raw}");
            assert_eq!(pixels[0].pixels, 12, "sensitivity {raw}");
        }
    }

    /// The speed setting does reach the model: the same flick pushes harder at
    /// a higher slider position.
    #[test]
    fn a_higher_sensitivity_flick_posts_more_pixels() {
        let flick = |raw: u8| {
            let (input, commands, controls) = standalone_input(64, preferences(true, raw));
            let session = HidppSessionId::with_epoch("mouse-a", 1);
            for _ in 0..40 {
                assert_eq!(
                    input.try_hidpp_wheel_scroll(&session, ScrollDelta::wheel_ticks(0.0, 0.2)),
                    Ok(())
                );
                // Queued back to back: the callbacks' own timestamps are a
                // few microseconds apart, so the model sees its 8 ms floor.
            }
            let (_, _, pixels) = drain(input, &commands, &controls);
            pixels
                .iter()
                .map(|event| i64::from(event.pixels))
                .sum::<i64>()
        };
        assert!(flick(1) < flick(14) && flick(14) < flick(100));
    }

    /// A hook tick queued unscaled under smoothing and reclassified to direct
    /// output by a toggle race still owes the plain multiplier.
    #[test]
    fn an_unscaled_hook_tick_reclassified_to_direct_gets_the_multiplier() {
        let preferences = preferences(true, 28);
        let (input, commands, controls) = standalone_input(2, Arc::clone(&preferences));
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 4.0)));
        preferences.publish(false, sensitivity(28));
        let (frames, direct, pixels) = drain(input, &commands, &controls);
        assert!(frames.is_empty() && pixels.is_empty());
        assert_eq!(direct, [WheelDelta { x: 0.0, y: 8.0 }]);
    }

    /// The rescale belongs to the hook source only: a HID++ impulse that a
    /// toggle race reclassifies to direct output was never pre-scaled and must
    /// not be scaled now (the vertical part would double at sensitivity 28).
    #[test]
    fn a_reclassified_hidpp_impulse_is_not_rescaled() {
        let preferences = preferences(true, 28);
        let (input, commands, controls) = standalone_input(2, Arc::clone(&preferences));
        let session = HidppSessionId::with_epoch("mouse-a", 1);
        assert!(input.try_hidpp_scroll(&session, ScrollDelta::wheel_ticks(1.0, 2.0)));
        preferences.publish(false, sensitivity(28));
        let (frames, direct, pixels) = drain(input, &commands, &controls);
        assert!(frames.is_empty() && pixels.is_empty());
        assert_eq!(direct, [WheelDelta { x: 1.0, y: 2.0 }]);
    }

    /// Cancelling a session drops its model: the next report starts from an
    /// empty history, so it matches a first report (12 px, remainder gone).
    #[test]
    fn cancelling_a_session_resets_its_model() {
        let (input, commands, controls) = standalone_input(10, preferences(true, 14));
        let session = HidppSessionId::with_epoch("mouse-a", 1);
        for _ in 0..2 {
            assert_eq!(
                input.try_hidpp_wheel_scroll(&session, ScrollDelta::wheel_ticks(0.0, 1.0)),
                Ok(())
            );
        }
        input.cancel_hidpp_session(&session);
        assert_eq!(
            input.try_hidpp_wheel_scroll(&session, ScrollDelta::wheel_ticks(0.0, 1.0)),
            Ok(())
        );
        let (_, _, pixels) = drain(input, &commands, &controls);
        let pixels: Vec<i32> = pixels.iter().map(|event| event.pixels).collect();
        assert_eq!(pixels.len(), 3, "{pixels:?}");
        assert_eq!(pixels[2], 12);
    }

    /// Reversal hysteresis belongs to the model, which sees the report in
    /// hi-res steps; the handle queues a half-notch reversal whole.
    #[test]
    fn the_handle_leaves_reversal_hysteresis_to_the_model() {
        let (input, receiver, _controls) = standalone_input(10, preferences(true, 14));
        let session = HidppSessionId::with_epoch("mouse-a", 1);
        assert_eq!(
            input.try_hidpp_wheel_scroll(&session, ScrollDelta::wheel_ticks(0.0, 1.0)),
            Ok(())
        );
        assert!(queued_input(&receiver).impulse.y > 0.0);
        assert_eq!(
            input.try_hidpp_wheel_scroll(&session, ScrollDelta::wheel_ticks(0.0, -0.5)),
            Ok(())
        );
        assert!(queued_input(&receiver).impulse.y < 0.0);
    }

    /// A corrupt, absurdly large hook tick is dropped by the model instead of
    /// producing a runaway scroll.
    #[test]
    fn an_absurd_hook_tick_posts_nothing() {
        let (input, commands, controls) = standalone_input(2, preferences(true, 14));
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, f64::MAX)));
        let (frames, direct, pixels) = drain(input, &commands, &controls);
        assert!(frames.is_empty() && direct.is_empty() && pixels.is_empty());
    }

    #[test]
    fn a_middle_press_drops_model_reports_but_not_the_thumb_wheel_or_direct_output() {
        let (input, receiver, _controls) = standalone_input(8, preferences(true, 14));
        let session = HidppSessionId::with_epoch("mouse-a", 1);
        input.on_middle_button(true);
        assert_eq!(
            input.try_hidpp_wheel_scroll(&session, ScrollDelta::wheel_ticks(0.0, 1.0)),
            Ok(())
        );
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));
        assert!(
            receiver.try_recv().is_err(),
            "both vertical reports dropped"
        );
        assert!(input.try_hidpp_scroll(&session, ScrollDelta::wheel_ticks(1.0, 0.0)));
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(1.0, 0.0)));
        assert_eq!(
            std::iter::from_fn(|| receiver.try_recv().ok()).count(),
            2,
            "horizontal input is not the model's"
        );
    }

    #[test]
    fn without_a_press_or_after_a_release_alone_reports_flow() {
        let (input, receiver, _controls) = standalone_input(8, preferences(true, 14));
        let session = HidppSessionId::with_epoch("mouse-a", 1);
        input.on_middle_button(false);
        assert_eq!(
            input.try_hidpp_wheel_scroll(&session, ScrollDelta::wheel_ticks(0.0, 1.0)),
            Ok(())
        );
        assert!(
            receiver.try_recv().is_ok(),
            "a release alone opens no window"
        );
    }
}
