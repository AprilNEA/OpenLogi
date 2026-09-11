//! Worker ownership and non-blocking producer capability for wheel output.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
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
const SMOOTH_SCROLL_FLAG: u8 = 0x80;
const SENSITIVITY_MASK: u8 = !SMOOTH_SCROLL_FLAG;

/// Above this raw (pre-scale) vertical magnitude, `ScrollPreferences::resolution_scale`
/// is not applied — real-hardware measurement (MX Master 3S) showed the raw
/// magnitude macOS reports for a free-spinning/fast wheel saturates to the
/// same value regardless of `scroll_resolution` (Low and High both plateaued
/// at an identical `~21.49`), while only the first one or two isolated-detent
/// ticks of a gesture (raw magnitude well under this threshold) actually
/// differ between resolution modes. Boosting an already-saturated value would
/// make `Low`/Standard resolution scroll several times farther than `High`
/// for the same physical motion — see
/// `openspec/changes/normalize-scroll-sensitivity-by-resolution` design.md.
const RESOLUTION_SCALE_MAGNITUDE_THRESHOLD: f64 = 3.0;

/// Below this gap since the previous accepted OS-hook tick, a new tick is a
/// genuinely isolated, deliberate action rather than part of an ongoing
/// gesture. Mirrors `openlogi-inject`'s `GESTURE_IDLE_GAP` — both exist to
/// tell "a fresh deliberate tick" apart from "a continuing sequence", just at
/// different layers (this one gates `ISOLATED_TICK_BOOST`, the other gates
/// the quantizer's visibility floor).
const ISOLATED_TICK_GAP: Duration = Duration::from_millis(500);

/// Extra multiplier applied only to a genuinely isolated tick (see
/// `ISOLATED_TICK_GAP`) below `RESOLUTION_SCALE_MAGNITUDE_THRESHOLD`, on top
/// of the normal `resolution_scale * scroll_multiplier` scaling, when smooth
/// scrolling is on.
///
/// Real-hardware testing (multiple apps: VS Code, Safari, Finder) found a
/// single isolated Low-mode tick's smooth-scroll output (~18 points at
/// `POINTS_PER_WHEEL_TICK=20.0`) was not visible anywhere, while multi-tick/
/// fast gestures (hundreds of points) already felt correct. A single flat
/// increase to `POINTS_PER_WHEEL_TICK` can't fix this without also making
/// those already-correct fast gestures overshoot, since it scales every tick
/// uniformly — see `openspec/changes/normalize-scroll-sensitivity-by-resolution`
/// design.md. This boost instead targets only the specific case that was
/// measured invisible: the first, isolated tick of a fresh gesture.
const ISOLATED_TICK_BOOST: f64 = 8.0;

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
    /// Resolution-aware base scale for the current device's vertical wheel,
    /// applied ahead of `vertical_sensitivity` so the same sensitivity value
    /// feels consistent whether the wheel is in `Low`/Standard or `High`
    /// `scroll_resolution` mode. `0` and `1` are both neutral (no device-
    /// specific scale published yet, or the device is in `High` mode).
    resolution_scale: AtomicU8,
}

impl ScrollPreferences {
    /// Create a live settings cell from validated config values.
    #[must_use]
    pub fn new(smooth_scroll: bool, vertical_sensitivity: VerticalScrollSensitivity) -> Self {
        Self {
            encoded: AtomicU8::new(Self::encode(smooth_scroll, vertical_sensitivity)),
            resolution_scale: AtomicU8::new(0),
        }
    }

    /// Publish both settings as one snapshot.
    pub fn publish(&self, smooth_scroll: bool, vertical_sensitivity: VerticalScrollSensitivity) {
        self.encoded.store(
            Self::encode(smooth_scroll, vertical_sensitivity),
            Ordering::Relaxed,
        );
    }

    /// Publish the current device's resolution-aware base scale (its
    /// HiRes-wheel `multiplier` when in `Low`/Standard mode, or `1` when in
    /// `High` mode / unknown).
    pub fn publish_resolution_scale(&self, scale: u8) {
        self.resolution_scale.store(scale, Ordering::Relaxed);
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

    /// The resolution-aware base scale to apply ahead of vertical
    /// sensitivity. Neutral (`1.0`) until a device-specific scale has been
    /// published, or while the current device is in `High` resolution mode.
    #[must_use]
    pub fn resolution_scale(&self) -> f64 {
        let raw = self.resolution_scale.load(Ordering::Relaxed);
        if raw <= 1 { 1.0 } else { f64::from(raw) }
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
    /// Timestamp of the last accepted OS-hook tick, used to detect a fresh,
    /// isolated tick for `ISOLATED_TICK_BOOST`. Separate from the quantizer's
    /// own isolation tracking (a different crate, a different purpose).
    last_hook_tick_at: Arc<Mutex<Option<Instant>>>,
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
        let now = Instant::now();
        let is_isolated_tick = {
            let mut last = self.last_hook_tick_at.lock().unwrap_or_else(|poisoned| {
                self.last_hook_tick_at.clear_poison();
                poisoned.into_inner()
            });
            let isolated =
                last.is_none_or(|at| now.saturating_duration_since(at) >= ISOLATED_TICK_GAP);
            *last = Some(now);
            isolated
        };
        let resolution_scale = if impulse.y.abs() < RESOLUTION_SCALE_MAGNITUDE_THRESHOLD {
            self.preferences.resolution_scale()
        } else {
            1.0
        };
        let mut scale = resolution_scale * preferences.vertical_sensitivity.scroll_multiplier();
        if preferences.smooth_scroll
            && is_isolated_tick
            && impulse.y.abs() < RESOLUTION_SCALE_MAGNITUDE_THRESHOLD
        {
            scale *= ISOLATED_TICK_BOOST;
        }
        let Some(scaled) = impulse.with_vertical_scale(scale) else {
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
            last_hook_tick_at: Arc::new(Mutex::new(None)),
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
                    if cancellations.cancel(&session, cancelled_generation) {
                        engine.cancel_source(&ScrollSource::Hidpp(session), emit_smooth);
                    }
                }
                ScrollControl::Shutdown(request) => {
                    engine.cancel_all(emit_smooth);
                    let _ = request.done.send(());
                    return;
                }
            }
        }

        let current_generation = shared_generation.load(Ordering::Acquire);
        if cancellations.advance_to(current_generation) {
            engine.cancel_all(emit_smooth);
            // Every accepted command from before the transition carries the
            // old generation and is rejected below, so its source tombstone
            // can no longer protect anything. A delayed old-generation control
            // is also ignored above instead of recreating it.
        } else if !preferences.smooth_scroll_enabled() {
            engine.cancel_all(emit_smooth);
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
            Ok(ScrollCommand::Input(input)) if cancellations.accepts(&input) => {
                match input.output {
                    ScrollOutputMode::Smooth { at } if preferences.smooth_scroll_enabled() => {
                        engine.impulse(input.source, input.impulse, at, emit_smooth);
                    }
                    ScrollOutputMode::Direct => {
                        engine.cancel_source(&input.source, emit_smooth);
                        emit_direct(input.impulse);
                    }
                    ScrollOutputMode::Smooth { .. } => {}
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
                last_hook_tick_at: Arc::new(Mutex::new(None)),
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
    fn resolution_scale_multiplies_ahead_of_sensitivity() {
        let preferences = preferences(false, 7);
        let (input, receiver, _controls) = standalone_input(1, Arc::clone(&preferences));
        preferences.publish_resolution_scale(15);
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 2.0)));

        let queued = queued_input(&receiver);
        assert_eq!(queued.impulse, WheelDelta { x: 0.0, y: 15.0 });
    }

    #[test]
    fn resolution_scale_does_not_apply_above_the_magnitude_threshold() {
        let preferences = preferences(false, 7);
        let (input, receiver, _controls) = standalone_input(1, Arc::clone(&preferences));
        preferences.publish_resolution_scale(15);
        // Above RESOLUTION_SCALE_MAGNITUDE_THRESHOLD (3.0): falls back to a
        // neutral 1x resolution scale, only sensitivity's own 0.5x applies.
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 21.49)));

        let queued = queued_input(&receiver);
        assert_eq!(queued.impulse, WheelDelta { x: 0.0, y: 10.745 });
    }

    /// Characterizes a High-resolution single physical click: the device's
    /// own `HiResWheel` capability reports `multiplier=15` ("a single
    /// ratchet distance will produce this amount of wheel movement reports
    /// in hi-res mode"), and `resolution_scale` stays neutral (`1`) in High
    /// mode regardless of magnitude, so this burst is completely unaffected
    /// by the resolution-normalization fix. Verifies the worker's per-event
    /// scaling stays linear across a rapid burst of many small reports (no
    /// clipping, no accumulation bug), which is what "feels like in-between
    /// movements rather than one atomic jump" reduces to architecturally —
    /// a real regression here would show up as a wrong cumulative total.
    #[test]
    fn high_resolution_burst_of_small_reports_scales_linearly() {
        const SUB_REPORT_MAGNITUDE: f64 = 0.1; // below the resolution-scale threshold too
        const SUB_REPORTS_PER_RATCHET: usize = 15; // device-reported `multiplier`
        let preferences = preferences(false, 8); // real calibrated sensitivity
        let (input, receiver, _controls) = standalone_input(15, Arc::clone(&preferences));

        for _ in 0..SUB_REPORTS_PER_RATCHET {
            assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, SUB_REPORT_MAGNITUDE)));
        }

        let sensitivity_multiplier = sensitivity(8).scroll_multiplier();
        let mut total = 0.0;
        for _ in 0..SUB_REPORTS_PER_RATCHET {
            total += queued_input(&receiver).impulse.y;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "SUB_REPORTS_PER_RATCHET is a small constant (15), exactly representable as f64"
        )]
        let expected =
            SUB_REPORT_MAGNITUDE * sensitivity_multiplier * SUB_REPORTS_PER_RATCHET as f64;
        assert!(
            (total - expected).abs() < 1.0e-9,
            "burst total {total} != expected {expected} (per-event scaling must stay linear)"
        );
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "resolution_scale is an exact integer-to-f64 conversion, no rounding possible"
    )]
    fn resolution_scale_of_zero_or_one_is_neutral() {
        assert_eq!(
            ScrollPreferences::new(false, sensitivity(7)).resolution_scale(),
            1.0
        );
        let preferences = ScrollPreferences::new(false, sensitivity(7));
        preferences.publish_resolution_scale(1);
        assert_eq!(preferences.resolution_scale(), 1.0);
        preferences.publish_resolution_scale(15);
        assert_eq!(preferences.resolution_scale(), 15.0);
    }

    #[test]
    fn hook_scales_vertical_distance_before_smoothing() {
        let (input, receiver, _controls) = standalone_input(2, preferences(true, 7));
        // Prime isolation state first so the assertion below reflects plain
        // sensitivity scaling, not the isolated-tick boost (see the dedicated
        // isolated-tick tests further down).
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 2.0)));
        let _ = queued_input(&receiver);

        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(2.0, 2.0)));
        let queued = queued_input(&receiver);
        assert_eq!(queued.impulse, WheelDelta { x: 2.0, y: 1.0 });
        assert!(matches!(queued.output, ScrollOutputMode::Smooth { .. }));
    }

    #[test]
    fn isolated_small_tick_is_boosted_only_when_smoothing() {
        let (input, receiver, _controls) = standalone_input(1, preferences(true, 7));
        // The very first tick on a fresh handle is isolated by definition
        // (no prior tick to compare against).
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 2.0)));
        let queued = queued_input(&receiver);
        // Base scale (sensitivity 7 -> 0.5) times ISOLATED_TICK_BOOST (8.0).
        assert_eq!(queued.impulse, WheelDelta { x: 0.0, y: 8.0 });
    }

    #[test]
    fn a_quickly_following_tick_is_not_treated_as_isolated() {
        let (input, receiver, _controls) = standalone_input(2, preferences(true, 7));
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 2.0)));
        let _ = queued_input(&receiver);

        // Well within ISOLATED_TICK_GAP of the first tick.
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 2.0)));
        let queued = queued_input(&receiver);
        assert_eq!(
            queued.impulse,
            WheelDelta { x: 0.0, y: 1.0 },
            "a tick that is part of an ongoing gesture must not receive the isolated-tick boost"
        );
    }

    #[test]
    fn isolated_tick_boost_does_not_apply_without_smoothing() {
        let (input, receiver, _controls) = standalone_input(1, preferences(false, 7));
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 2.0)));
        let queued = queued_input(&receiver);
        assert_eq!(
            queued.impulse,
            WheelDelta { x: 0.0, y: 1.0 },
            "the isolated-tick boost only targets the smooth-scroll path"
        );
        assert!(matches!(queued.output, ScrollOutputMode::Direct));
    }

    #[test]
    fn isolated_tick_boost_does_not_apply_above_the_magnitude_threshold() {
        let (input, receiver, _controls) = standalone_input(1, preferences(true, 7));
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 21.49)));
        let queued = queued_input(&receiver);
        assert_eq!(
            queued.impulse,
            WheelDelta { x: 0.0, y: 10.745 },
            "a fast/large isolated tick must not also receive the isolated-tick boost"
        );
    }

    /// Regression guard using real hardware calibration (MX Master 3S,
    /// `resolution_scale=15`, `sensitivity=8`): an accelerating tick's raw
    /// magnitude (observed in real traces, well above
    /// `RESOLUTION_SCALE_MAGNITUDE_THRESHOLD`) must fall back to neutral
    /// `resolution_scale` and must not additionally receive
    /// `ISOLATED_TICK_BOOST`, even though it is the first (isolated) tick on
    /// a fresh handle - confirming the boost's scope stays narrow with real
    /// numbers, not just the smaller fixture ones used above.
    #[test]
    fn a_fast_isolated_tick_at_real_calibration_is_not_boosted() {
        const REAL_FAST_RAW: f64 = 5.67;
        let preferences_cell = preferences(true, 8);
        preferences_cell.publish_resolution_scale(15);
        let (input, receiver, _controls) = standalone_input(1, Arc::clone(&preferences_cell));

        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, REAL_FAST_RAW)));
        let queued = queued_input(&receiver);

        let expected = REAL_FAST_RAW * sensitivity(8).scroll_multiplier();
        assert!(
            (queued.impulse.y - expected).abs() < 1.0e-9,
            "a fast isolated tick above the magnitude threshold must not receive \
             resolution_scale or ISOLATED_TICK_BOOST: got {}, expected {expected}",
            queued.impulse.y
        );
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
                &command_rx,
                &control_rx,
                &worker_generation,
                &preferences,
                &mut |frame| emitted.send(frame).expect("frame receiver remains open"),
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
