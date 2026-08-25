//! Worker ownership and non-blocking producer capability for wheel output.

use std::collections::HashSet;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use openlogi_core::config::VerticalScrollSensitivity;
use openlogi_core::scroll::ScrollDelta;
use tracing::warn;

use super::{ScrollEngine, ScrollFrame, ScrollSource, WheelDelta};
use crate::runtime::HidppSessionId;

/// OS-hook callbacks must fail open rather than wait for the worker.
const INPUT_QUEUE_CAPACITY: usize = 128;
/// Bounds graceful process shutdown if platform injection stops returning.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollPreferenceSnapshot {
    pub smooth_scroll: bool,
    pub vertical_sensitivity: VerticalScrollSensitivity,
    pub vertical_acceleration_enabled: bool,
    pub vertical_acceleration: f64,
    pub vertical_max_gain: f64,
    pub horizontal_acceleration_enabled: bool,
    pub horizontal_acceleration: f64,
    pub horizontal_max_gain: f64,
}

impl From<&openlogi_core::config::AppSettings> for ScrollPreferenceSnapshot {
    fn from(settings: &openlogi_core::config::AppSettings) -> Self {
        Self {
            smooth_scroll: settings.smooth_scroll,
            vertical_sensitivity: settings.vertical_scroll_sensitivity,
            vertical_acceleration_enabled: settings.vertical_acceleration_enabled,
            vertical_acceleration: settings.vertical_acceleration,
            vertical_max_gain: settings.vertical_max_gain,
            horizontal_acceleration_enabled: settings.horizontal_acceleration_enabled,
            horizontal_acceleration: settings.horizontal_acceleration,
            horizontal_max_gain: settings.horizontal_max_gain,
        }
    }
}

/// Atomically published settings read from input callbacks and the scroll
/// worker without taking the orchestrator's config lock.
pub struct ScrollPreferences {
    snapshot: std::sync::RwLock<ScrollPreferenceSnapshot>,
}

impl ScrollPreferences {
    /// Create live scroll settings from full app settings.
    #[must_use]
    pub fn from_app_settings(settings: &openlogi_core::config::AppSettings) -> Self {
        Self {
            snapshot: std::sync::RwLock::new(ScrollPreferenceSnapshot::from(settings)),
        }
    }

    /// Create a live settings cell from validated config values.
    #[must_use]
    pub fn new(smooth_scroll: bool, vertical_sensitivity: VerticalScrollSensitivity) -> Self {
        let mut snapshot =
            ScrollPreferenceSnapshot::from(&openlogi_core::config::AppSettings::default());
        snapshot.smooth_scroll = smooth_scroll;
        snapshot.vertical_sensitivity = vertical_sensitivity;
        Self {
            snapshot: std::sync::RwLock::new(snapshot),
        }
    }

    /// Publish updated settings snapshot from AppSettings.
    pub fn publish_app_settings(&self, settings: &openlogi_core::config::AppSettings) {
        if let Ok(mut lock) = self.snapshot.write() {
            *lock = ScrollPreferenceSnapshot::from(settings);
        }
    }

    /// Publish smooth_scroll and vertical_sensitivity.
    pub fn publish(&self, smooth_scroll: bool, vertical_sensitivity: VerticalScrollSensitivity) {
        if let Ok(mut lock) = self.snapshot.write() {
            lock.smooth_scroll = smooth_scroll;
            lock.vertical_sensitivity = vertical_sensitivity;
        }
    }

    /// Load a snapshot of current scroll settings.
    #[must_use]
    pub fn load(&self) -> ScrollPreferenceSnapshot {
        *self
            .snapshot
            .read()
            .expect("scroll preferences lock poisoned")
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
}

#[derive(Clone, Copy)]
enum ScrollOutputMode {
    Smooth { at: Instant },
    Direct,
}

struct ScrollInput {
    generation: u64,
    source: ScrollSource,
    impulse: WheelDelta,
    output: ScrollOutputMode,
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
    CancelOverflowSource(ScrollSource),
    Shutdown(ShutdownRequest),
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
        let vertical_scaled = impulse.y != 0.0
            && (preferences.vertical_sensitivity.scroll_multiplier() != 1.0
                || preferences.vertical_acceleration_enabled);
        let horizontal_scaled =
            impulse.x != 0.0 && preferences.horizontal_acceleration_enabled;

        let output = if preferences.smooth_scroll {
            ScrollOutputMode::Smooth { at: Instant::now() }
        } else if vertical_scaled || horizontal_scaled {
            ScrollOutputMode::Direct
        } else {
            return false;
        };
        self.try_enqueue(ScrollSource::current_hook(), impulse, output)
    }

    /// Queue one diverted thumb-wheel impulse from an active HID++ session.
    ///
    /// Rejection tells the already-diverted caller to inject the distance
    /// directly; unlike an OS hook, there is no physical event to pass through.
    #[must_use]
    pub(crate) fn try_hidpp_scroll(&self, session: &HidppSessionId, delta: ScrollDelta) -> bool {
        if !self.accepting.load(Ordering::Acquire) || !self.preferences.smooth_scroll_enabled() {
            return false;
        }
        let Ok(impulse) = WheelDelta::try_from(delta) else {
            return false;
        };
        self.try_enqueue(
            ScrollSource::Hidpp(session.clone()),
            impulse,
            ScrollOutputMode::Smooth { at: Instant::now() },
        )
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
        let source = ScrollSource::Hidpp(session.clone());
        match self
            .commands
            .try_send(ScrollCommand::CancelSource(source.clone()))
        {
            Ok(()) | Err(mpsc::TrySendError::Disconnected(_)) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                if self
                    .controls
                    .send(ScrollControl::CancelOverflowSource(source))
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
        Self::spawn_with(preferences, ScrollFrame::post, WheelDelta::post)
    }

    pub(super) fn spawn_with(
        preferences: Arc<ScrollPreferences>,
        mut emit_smooth: impl FnMut(ScrollFrame) + Send + 'static,
        mut emit_direct: impl FnMut(WheelDelta) + Send + 'static,
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
                    &command_rx,
                    &control_rx,
                    &generation,
                    &preferences,
                    &mut emit_smooth,
                    &mut emit_direct,
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
    commands: &mpsc::Receiver<ScrollCommand>,
    controls: &mpsc::Receiver<ScrollControl>,
    shared_generation: &AtomicU64,
    preferences: &ScrollPreferences,
    emit_smooth: &mut impl FnMut(ScrollFrame),
    emit_direct: &mut impl FnMut(WheelDelta),
) {
    let mut engine = ScrollEngine::default();
    let mut accel_engine = super::ScrollAccelerationEngine::default();
    // An overflow-cancelled incarnation stays tombstoned so accepted input that
    // was already queued when control overtook the saturated queue is ignored.
    let mut overflow_cancelled_sources = HashSet::new();
    let mut generation = shared_generation.load(Ordering::Acquire);
    loop {
        while let Ok(control) = controls.try_recv() {
            match control {
                ScrollControl::CancelOverflowSource(source) => {
                    engine.cancel_source(&source, emit_smooth);
                    overflow_cancelled_sources.insert(source);
                }
                ScrollControl::Shutdown(request) => {
                    engine.cancel_all(emit_smooth);
                    let _ = request.done.send(());
                    return;
                }
            }
        }

        let current_generation = shared_generation.load(Ordering::Acquire);
        if current_generation != generation || !preferences.smooth_scroll_enabled() {
            engine.cancel_all(emit_smooth);
            generation = current_generation;
        }

        let command = engine.next_deadline().map_or_else(
            || {
                commands
                    .recv()
                    .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
            },
            |deadline| commands.recv_timeout(deadline.saturating_duration_since(Instant::now())),
        );
        match command {
            Ok(ScrollCommand::Input(input))
                if input.generation == generation
                    && !overflow_cancelled_sources.contains(&input.source) =>
            {
                let prefs = preferences.load();
                let at = match input.output {
                    ScrollOutputMode::Smooth { at } => at,
                    ScrollOutputMode::Direct => Instant::now(),
                };
                let (gain_x, gain_y) = accel_engine.compute_gains(
                    input.impulse.x,
                    input.impulse.y,
                    at,
                    prefs.vertical_acceleration_enabled,
                    prefs.vertical_acceleration,
                    prefs.vertical_max_gain,
                    prefs.horizontal_acceleration_enabled,
                    prefs.horizontal_acceleration,
                    prefs.horizontal_max_gain,
                );
                let scaled_impulse = WheelDelta {
                    x: input.impulse.x * gain_x,
                    y: input.impulse.y * prefs.vertical_sensitivity.scroll_multiplier() * gain_y,
                };

                if scaled_impulse.x.is_finite()
                    && scaled_impulse.y.is_finite()
                    && !scaled_impulse.is_zero()
                {
                    match input.output {
                        ScrollOutputMode::Smooth { .. } if prefs.smooth_scroll => {
                            engine.impulse(input.source, scaled_impulse, at, emit_smooth);
                        }
                        ScrollOutputMode::Direct => {
                            engine.cancel_source(&input.source, emit_smooth);
                            emit_direct(scaled_impulse);
                        }
                        ScrollOutputMode::Smooth { .. } => {}
                    }
                }
            }
            Ok(ScrollCommand::CancelSource(source)) => {
                engine.cancel_source(&source, emit_smooth);
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
    use super::*;

    fn sensitivity(raw: u8) -> VerticalScrollSensitivity {
        VerticalScrollSensitivity::try_new(raw).expect("test sensitivity is valid")
    }

    fn preferences(smooth_scroll: bool, sensitivity: u8) -> Arc<ScrollPreferences> {
        Arc::new(ScrollPreferences::from_app_settings(
            &openlogi_core::config::AppSettings {
                smooth_scroll,
                vertical_scroll_sensitivity: self::sensitivity(sensitivity),
                vertical_acceleration_enabled: false,
                horizontal_acceleration_enabled: false,
                ..Default::default()
            },
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
        assert_eq!(queued.impulse, WheelDelta { x: 2.0, y: 2.0 });
        assert!(matches!(queued.output, ScrollOutputMode::Direct));
    }

    #[test]
    fn hook_scales_vertical_distance_before_smoothing() {
        let (input, receiver, _controls) = standalone_input(1, preferences(true, 7));
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(2.0, 2.0)));

        let queued = queued_input(&receiver);
        assert_eq!(queued.impulse, WheelDelta { x: 2.0, y: 2.0 });
        assert!(matches!(queued.output, ScrollOutputMode::Smooth { .. }));
    }

    #[test]
    fn hidpp_smoothing_does_not_apply_main_wheel_sensitivity() {
        let (input, receiver, _controls) = standalone_input(1, preferences(true, 7));
        let session = HidppSessionId::new("mouse-a", 7);
        assert!(input.try_hidpp_scroll(&session, ScrollDelta::wheel_ticks(0.0, 2.0)));

        let queued = queued_input(&receiver);
        assert_eq!(queued.impulse, WheelDelta { x: 0.0, y: 2.0 });
        assert!(matches!(queued.output, ScrollOutputMode::Smooth { .. }));
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
        assert_eq!(queued.impulse, WheelDelta { x: 0.0, y: 1.0 });
        assert!(matches!(queued.output, ScrollOutputMode::Smooth { .. }));
    }

    #[test]
    fn hidpp_cancellation_targets_its_session() {
        let (input, commands, _controls) = standalone_input(1, preferences(true, 14));
        let session = HidppSessionId::new("mouse-a", 7);
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
        let cancelled = HidppSessionId::new("mouse-a", 7);
        let survivor = HidppSessionId::new("mouse-b", 3);
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
                &commands,
                &controls,
                &generation,
                &preferences,
                &mut |frame| {
                    emitted.send(frame).expect("frame receiver remains open");
                },
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
    fn idle_worker_shutdown_wakes_after_publishing_its_request() {
        let mut runtime = ScrollRuntime::spawn_with(preferences(false, 14), |_| {}, |_| {})
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
}
