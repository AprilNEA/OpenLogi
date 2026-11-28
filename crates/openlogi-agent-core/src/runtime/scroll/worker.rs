//! Worker ownership and non-blocking producer capability for wheel output.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use openlogi_core::config::{
    MomentumParams, ThumbwheelSensitivity, VerticalScrollSensitivity, WheelScrollOverrides,
};
use openlogi_core::scroll::ScrollDelta;
use tracing::warn;

use super::{
    HidppWheel, ScrollEngine, ScrollFrame, ScrollSource, SoftwareWheel, ThumbDispatch, WheelDelta,
    WheelStream,
};
use crate::runtime::{HidppSessionId, receive_command};

/// OS-hook callbacks must fail open rather than wait for the worker.
const INPUT_QUEUE_CAPACITY: usize = 128;
/// Bounds graceful process shutdown if platform injection stops returning.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
// One byte per scope: the high bit enables smoothing and bits 0..7 hold
// its validated 1..=100 sensitivity. Both wheel epochs occupy equal fields above
// them along with the profile-presence flag, so callbacks see settings and
// ownership in one load.
const SMOOTH_SCROLL_FLAG: u8 = 0x80;
const MAIN_EPOCH_SHIFT: u32 = u16::BITS;
const PROFILE_PRESENT_FLAG: u64 = 1 << (u64::BITS - 1);
const EPOCH_BITS: u32 = (u64::BITS - MAIN_EPOCH_SHIFT - 1) / 2;
const HORIZONTAL_EPOCH_SHIFT: u32 = MAIN_EPOCH_SHIFT + EPOCH_BITS;
const EPOCH_MASK: u64 = (1 << EPOCH_BITS) - 1;

/// Identifies the app policy under which wheel input was observed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WheelProfileEpoch(u64);

impl WheelProfileEpoch {
    fn advance(&mut self) {
        self.0 = self.0.wrapping_add(1) & EPOCH_MASK;
    }
}

/// One coherent set of live software scroll preferences.
#[derive(Clone, Copy)]
pub struct ScrollSettings {
    /// Whether any software wheel profile exists, even while no app is identifiable.
    pub has_profiles: bool,
    /// Legacy global smoothing, also used by wheel-hold bindings.
    pub smooth_scroll: bool,
    /// Effective software smoothing for ordinary wheels and diverted thumb scroll.
    pub main_wheel_smooth_scroll: bool,
    /// Global vertical distance setting used by the diverted HID++ main wheel.
    pub vertical_sensitivity: VerticalScrollSensitivity,
    /// Effective application setting for the ordinary vertical wheel distance.
    pub main_wheel_vertical_sensitivity: VerticalScrollSensitivity,
}

#[derive(Clone, Copy)]
struct ScrollPreferenceSnapshot {
    settings: ScrollSettings,
    main_wheel_epoch: WheelProfileEpoch,
    horizontal_epoch: WheelProfileEpoch,
}

impl ScrollPreferenceSnapshot {
    fn epoch_for(self, source: &ScrollSource) -> Option<WheelProfileEpoch> {
        if matches!(source, ScrollSource::Hidpp(_, HidppWheel::Held)) {
            return Some(self.horizontal_epoch);
        }
        source.software_wheel().map(|wheel| match wheel {
            SoftwareWheel::Primary => self.main_wheel_epoch,
            SoftwareWheel::Horizontal => self.horizontal_epoch,
        })
    }

    fn swipe_stream(self) -> WheelStream {
        if self.settings.has_profiles {
            WheelStream::DestinationSwipe
        } else {
            WheelStream::Legacy
        }
    }

    fn apply_transition(
        self,
        previous: Self,
        generation_changed: bool,
        engine: &mut ScrollEngine,
        emit: &mut impl FnMut(ScrollFrame),
    ) {
        let horizontal_changed = previous.horizontal_epoch != self.horizontal_epoch;
        if horizontal_changed {
            engine.cancel_destination_swipe(previous.swipe_stream(), emit);
        }
        if generation_changed || previous.settings.smooth_scroll != self.settings.smooth_scroll {
            engine.cancel_all(emit);
        } else if horizontal_changed || previous.main_wheel_epoch != self.main_wheel_epoch {
            engine.cancel_where(
                |source| {
                    let globally_eased_hold = self.settings.smooth_scroll
                        && matches!(source, ScrollSource::Hidpp(_, HidppWheel::Held));
                    !globally_eased_hold && previous.epoch_for(source) != self.epoch_for(source)
                },
                emit,
            );
        }
    }

    fn smooths_input(self, input: &ScrollInput) -> bool {
        if input.source.software_wheel().is_some() {
            self.settings.main_wheel_smooth_scroll
        } else {
            self.settings.smooth_scroll
        }
    }
}

#[derive(Default)]
struct PublishedProfile {
    app: Option<String>,
    overrides: WheelScrollOverrides,
    thumb_override: Option<ThumbwheelSensitivity>,
}

/// Atomically published settings read from input callbacks and the scroll
/// worker without taking the orchestrator's config lock.
///
/// The smoothing flags, sensitivity and both wheel application epochs share
/// one atomic value, so accepted input belongs to the policy that admitted it.
pub struct ScrollPreferences {
    // Only publishers touch the app ID. The hook and worker read its epoch
    // in the atomic snapshot and never lock or clone an identifier.
    published_profile: Mutex<PublishedProfile>,
    encoded: AtomicU64,
}

impl ScrollPreferences {
    /// Create a live settings cell from validated config values.
    #[must_use]
    pub fn new(smooth_scroll: bool, vertical_sensitivity: VerticalScrollSensitivity) -> Self {
        Self {
            published_profile: Mutex::default(),
            encoded: AtomicU64::new(Self::encode(ScrollSettings {
                has_profiles: false,
                smooth_scroll,
                main_wheel_smooth_scroll: smooth_scroll,
                vertical_sensitivity,
                main_wheel_vertical_sensitivity: vertical_sensitivity,
            })),
        }
    }

    /// Publish both axes' destination and software settings in one snapshot.
    pub(crate) fn publish(
        &self,
        app: Option<&str>,
        overrides: WheelScrollOverrides,
        thumb_override: Option<ThumbwheelSensitivity>,
        settings: ScrollSettings,
    ) {
        let mut profile = self
            .published_profile
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let previous = self.load();
        let app_changed = profile.app.as_deref() != app
            || settings.has_profiles != previous.settings.has_profiles;
        if app_changed {
            profile.app = app.map(str::to_owned);
        }
        let smoothing_changed = (profile.overrides.smooth_scroll || overrides.smooth_scroll)
            && settings.main_wheel_smooth_scroll != previous.settings.main_wheel_smooth_scroll;
        let vertical_changed = (profile.overrides.vertical_sensitivity
            || overrides.vertical_sensitivity)
            && settings.main_wheel_vertical_sensitivity
                != previous.settings.main_wheel_vertical_sensitivity;
        let mut main_epoch = previous.main_wheel_epoch;
        let mut horizontal_epoch = previous.horizontal_epoch;
        if app_changed || smoothing_changed || vertical_changed {
            main_epoch.advance();
        }
        if app_changed || smoothing_changed || thumb_override != profile.thumb_override {
            horizontal_epoch.advance();
        }
        profile.overrides = overrides;
        profile.thumb_override = thumb_override;
        self.encoded.store(
            Self::encode(settings)
                | (main_epoch.0 << MAIN_EPOCH_SHIFT)
                | (horizontal_epoch.0 << HORIZONTAL_EPOCH_SHIFT),
            Ordering::Release,
        );
    }

    /// Destination of raw thumb rotations observed by a HID++ capture session.
    pub(crate) fn wheel_profile_epoch(&self) -> WheelProfileEpoch {
        self.load().horizontal_epoch
    }

    /// Whether global smoothing, used by wheel-hold button scrolls, is enabled.
    #[must_use]
    pub fn smooth_scroll_enabled(&self) -> bool {
        self.load().settings.smooth_scroll
    }

    /// Whether this application eases ordinary wheel ticks and diverted thumb scrolls.
    #[must_use]
    pub fn main_wheel_smooth_scroll_enabled(&self) -> bool {
        self.load().settings.main_wheel_smooth_scroll
    }

    /// The global vertical sensitivity for the diverted main wheel.
    #[must_use]
    pub fn vertical_sensitivity(&self) -> VerticalScrollSensitivity {
        self.load().settings.vertical_sensitivity
    }

    fn encode(settings: ScrollSettings) -> u64 {
        let byte = |smooth, sensitivity: VerticalScrollSensitivity| {
            u8::from(sensitivity) | if smooth { SMOOTH_SCROLL_FLAG } else { 0 }
        };
        u64::from(u16::from_le_bytes([
            byte(settings.smooth_scroll, settings.vertical_sensitivity),
            byte(
                settings.main_wheel_smooth_scroll,
                settings.main_wheel_vertical_sensitivity,
            ),
        ])) | if settings.has_profiles {
            PROFILE_PRESENT_FLAG
        } else {
            0
        }
    }

    fn load(&self) -> ScrollPreferenceSnapshot {
        Self::decode(self.encoded.load(Ordering::Acquire))
    }

    fn decode(encoded: u64) -> ScrollPreferenceSnapshot {
        let [global, main_wheel, ..] = encoded.to_le_bytes();
        let sensitivity = |byte: u8| {
            VerticalScrollSensitivity::try_new(byte & !SMOOTH_SCROLL_FLAG).unwrap_or_else(|_| {
                unreachable!("scroll sensitivity was validated before encoding")
            })
        };
        ScrollPreferenceSnapshot {
            settings: ScrollSettings {
                has_profiles: encoded & PROFILE_PRESENT_FLAG != 0,
                smooth_scroll: global & SMOOTH_SCROLL_FLAG != 0,
                main_wheel_smooth_scroll: main_wheel & SMOOTH_SCROLL_FLAG != 0,
                vertical_sensitivity: sensitivity(global),
                main_wheel_vertical_sensitivity: sensitivity(main_wheel),
            },
            main_wheel_epoch: WheelProfileEpoch((encoded >> MAIN_EPOCH_SHIFT) & EPOCH_MASK),
            horizontal_epoch: WheelProfileEpoch((encoded >> HORIZONTAL_EPOCH_SHIFT) & EPOCH_MASK),
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
        /// The raw signed wheel notches before sensitivity scaling, which
        /// momentum scrolling's free-spin detector measures speed in.
        notches: f64,
        /// The momentum-scrolling settings of the wheel's device, resolved;
        /// `None` while momentum scrolling is off for it.
        momentum: Option<MomentumParams>,
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
    profile_epoch: Option<WheelProfileEpoch>,
    source: ScrollSource,
    impulse: WheelDelta,
    output: ScrollOutputMode,
}

impl ScrollInput {
    /// The output this input gets under the current smoothing setting.
    ///
    /// A global toggle can land between enqueue and dequeue. Its accepted
    /// input is reclassified rather than dropped, so a horizontal HID++ tick
    /// keeps the scroll phases AppKit's swipe recognizers need. Wheel input
    /// from an expired application epoch is rejected before this step.
    fn output_under(&self, smoothing: bool) -> ScrollOutputMode {
        match (self.output, &self.source) {
            (
                ScrollOutputMode::Eased {
                    at,
                    notches,
                    momentum,
                },
                _,
            ) => ScrollOutputMode::Eased {
                at,
                notches,
                momentum,
            },
            (
                ScrollOutputMode::Smooth { at } | ScrollOutputMode::Phased { at },
                ScrollSource::Hidpp(..),
            ) => ScrollOutputMode::hidpp(smoothing, self.impulse, at)
                .unwrap_or(ScrollOutputMode::Direct),
            (
                ScrollOutputMode::Smooth { at } | ScrollOutputMode::Phased { at },
                ScrollSource::OsHook(..),
            ) if smoothing => ScrollOutputMode::Smooth { at },
            _ => ScrollOutputMode::Direct,
        }
    }
}

enum ScrollCommand {
    Input(ScrollInput),
    /// Cancel both wheels of a HID++ capture session.
    CancelSession(HidppSessionId),
    Wake,
}

struct ShutdownRequest {
    done: mpsc::SyncSender<()>,
}

/// Lossless fallback for overflow cancellation and graceful shutdown.
enum ScrollControl {
    CancelThumb(ScrollSource),
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
                ScrollSource::OsHook(..) => true,
                ScrollSource::Hidpp(session, _) => self
                    .sessions
                    .get(session.device_key())
                    .is_none_or(|cancelled_epoch| session.epoch() > *cancelled_epoch),
            }
    }
}

/// Cloneable, non-owning capability for physical input producers.
///
/// The OS hook never locks or waits for queue capacity. Diverted thumb
/// producers also retain their current dispatch identity until reconfiguration.
/// A rejected submission asks the producer to use its direct-output fallback.
#[derive(Clone)]
pub struct ScrollInputHandle {
    commands: mpsc::SyncSender<ScrollCommand>,
    controls: mpsc::Sender<ScrollControl>,
    generation: Arc<AtomicU64>,
    accepting: Arc<AtomicBool>,
    preferences: Arc<ScrollPreferences>,
    thumb_dispatches: Arc<Mutex<HashMap<HidppSessionId, ThumbDispatch>>>,
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
        let Some(scaled) = impulse.with_vertical_scale(
            preferences
                .settings
                .main_wheel_vertical_sensitivity
                .scroll_multiplier(),
        ) else {
            return false;
        };
        let wheel = SoftwareWheel::for_impulse(impulse);
        let output = if preferences.settings.main_wheel_smooth_scroll {
            ScrollOutputMode::Smooth { at: Instant::now() }
        } else if scaled != impulse {
            ScrollOutputMode::Direct
        } else {
            return false;
        };
        let source = ScrollSource::current_hook(wheel);
        let epoch = preferences.epoch_for(&source);
        self.try_enqueue(source, scaled, output, epoch)
    }

    pub(crate) fn wheel_profile_epoch(&self) -> WheelProfileEpoch {
        self.preferences.wheel_profile_epoch()
    }

    /// Queue a diverted thumb-wheel distance already scaled by its device's
    /// dispatch plan. A stale report was consumed and must not be reinjected.
    pub(crate) fn try_hidpp_scroll(
        &self,
        session: &HidppSessionId,
        delta: ScrollDelta,
        epoch: WheelProfileEpoch,
    ) -> bool {
        let preferences = self.preferences.load();
        if epoch != preferences.horizontal_epoch {
            return true;
        }
        let mut dispatches = self
            .thumb_dispatches
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let dispatch = dispatches.entry(session.clone()).or_default().clone();
        self.try_diverted_scroll(
            ScrollSource::Hidpp(session.clone(), HidppWheel::Thumb(dispatch)),
            delta,
            preferences.settings.main_wheel_smooth_scroll,
            Some(epoch),
        )
    }

    /// A wheel-hold button's horizontal scroll retains its binding's global
    /// smoothing policy, independent of a physical thumb-wheel profile.
    pub(crate) fn try_hidpp_held_scroll(
        &self,
        session: &HidppSessionId,
        delta: ScrollDelta,
    ) -> bool {
        let preferences = self.preferences.load();
        // A button's phased horizontal scroll shares the physical thumb's
        // destination gesture. Eased wheel-hold motion stays global.
        self.try_diverted_scroll(
            ScrollSource::Hidpp(session.clone(), HidppWheel::Held),
            delta,
            preferences.settings.smooth_scroll,
            (!preferences.settings.smooth_scroll).then_some(preferences.horizontal_epoch),
        )
    }

    fn try_diverted_scroll(
        &self,
        source: ScrollSource,
        delta: ScrollDelta,
        smoothing: bool,
        epoch: Option<WheelProfileEpoch>,
    ) -> bool {
        if !self.accepting.load(Ordering::Acquire) {
            return false;
        }
        let Ok(impulse) = WheelDelta::try_from(delta) else {
            return false;
        };
        let Some(output) = ScrollOutputMode::hidpp(smoothing, impulse, Instant::now()) else {
            return false;
        };
        self.try_enqueue(source, impulse, output, epoch)
    }

    /// Queue one diverted main-wheel impulse from an active HID++ session.
    ///
    /// Always eased, whatever the app-wide smoothing setting says: the
    /// device's own smooth-wheel switch is what diverted the wheel. The
    /// vertical distance uses the global sensitivity across application profiles.
    /// `momentum` is the device's resolved setting, `None` while it is off.
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
        momentum: Option<MomentumParams>,
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
                ScrollSource::Hidpp(session.clone(), HidppWheel::Main),
                scaled,
                ScrollOutputMode::Eased {
                    at: Instant::now(),
                    notches: impulse.y,
                    momentum,
                },
                None,
            );
        if queued { Ok(()) } else { Err(scaled.into()) }
    }

    fn try_enqueue(
        &self,
        source: ScrollSource,
        impulse: WheelDelta,
        output: ScrollOutputMode,
        profile_epoch: Option<WheelProfileEpoch>,
    ) -> bool {
        let input = ScrollInput {
            generation: self.generation.load(Ordering::Acquire),
            profile_epoch,
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

    /// Retire only this session's physical thumb rotation after a dispatch
    /// change. The unbounded control can overtake a full input queue.
    pub(crate) fn cancel_hidpp_thumb(&self, session: &HidppSessionId) {
        let retired = self
            .thumb_dispatches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(session);
        if let Some(dispatch) = retired {
            dispatch.retire();
            let source = ScrollSource::Hidpp(session.clone(), HidppWheel::Thumb(dispatch));
            if self
                .controls
                .send(ScrollControl::CancelThumb(source))
                .is_ok()
            {
                self.wake();
            }
        }
    }

    /// Cancel output belonging to one HID++ capture-session incarnation.
    pub(crate) fn cancel_hidpp_session(&self, session: &HidppSessionId) {
        self.thumb_dispatches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(session);
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

    /// Observe the production scroll worker's frames without injecting any OS events.
    #[cfg(test)]
    pub(crate) fn spawn_observed(
        preferences: Arc<ScrollPreferences>,
        mut emit: impl FnMut(ScrollDelta, openlogi_inject::SmoothScrollPhase) + Send + 'static,
        mut direct: impl FnMut(ScrollDelta) + Send + 'static,
    ) -> io::Result<Self> {
        Self::spawn_with(
            preferences,
            move |frame| {
                let observe = |delta, phase| (delta, phase);
                let (delta, phase) = frame.dispatch(observe, observe);
                emit(delta, phase);
            },
            move |delta| direct(delta.into()),
        )
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
            thumb_dispatches: Arc::default(),
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
) {
    let mut engine = ScrollEngine::default();
    let mut observed_preferences = preferences.load();
    // An overflow-cancelled incarnation stays tombstoned so accepted input that
    // was already queued when control overtook the saturated queue is ignored.
    let mut cancellations = OverflowCancellations::new(shared_generation.load(Ordering::Acquire));
    loop {
        let command = receive(engine.next_deadline());
        // Apply cancellations that arrived while waiting before emitting a
        // due frame, including when an overflowing queue swallowed the wake.
        while let Ok(control) = controls.try_recv() {
            match control {
                ScrollControl::CancelThumb(source) => engine.cancel_source(&source, emit_smooth),
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
                    }
                    if cancellations.cancel(&session, cancelled_generation) {
                        engine.cancel_session(&session, emit_smooth);
                    }
                }
                ScrollControl::Shutdown(request) => {
                    engine.cancel_all(emit_smooth);
                    let _ = request.done.send(());
                    return;
                }
            }
        }

        // Settings can change while an idle worker waits for input. Observe the
        // new setting after receiving, so a toggle cancels the previous gesture
        // before the first input under the new setting starts another one.
        let current_preferences = preferences.load();
        current_preferences.apply_transition(
            observed_preferences,
            cancellations.advance_to(shared_generation.load(Ordering::Acquire)),
            &mut engine,
            emit_smooth,
        );
        observed_preferences = current_preferences;

        match command {
            Ok(ScrollCommand::Input(input))
                if cancellations.accepts(&input)
                    && !matches!(&input.source, ScrollSource::Hidpp(_, HidppWheel::Thumb(dispatch)) if dispatch.retired())
                    && input.profile_epoch.is_none_or(|epoch| {
                        Some(epoch) == current_preferences.epoch_for(&input.source)
                    }) =>
            {
                match input.output_under(current_preferences.smooths_input(&input)) {
                    ScrollOutputMode::Smooth { at } => {
                        engine.impulse(input.source, input.impulse, at, emit_smooth);
                    }
                    ScrollOutputMode::Eased {
                        at,
                        notches,
                        momentum,
                    } => {
                        engine.wheel_impulse(
                            input.source,
                            input.impulse,
                            notches,
                            momentum,
                            at,
                            emit_smooth,
                        );
                    }
                    ScrollOutputMode::Phased { at } => {
                        engine.phased_impulse_on(
                            input.source,
                            input.impulse,
                            at,
                            current_preferences.swipe_stream(),
                            emit_smooth,
                        );
                    }
                    ScrollOutputMode::Direct => {
                        engine.cancel_source(&input.source, emit_smooth);
                        emit_direct(input.impulse);
                    }
                }
            }
            Ok(ScrollCommand::CancelSession(session)) => {
                engine.cancel_session(&session, emit_smooth);
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

    impl ScrollInputHandle {
        fn submit_thumb(&self, session: &HidppSessionId, delta: ScrollDelta) -> bool {
            self.try_hidpp_scroll(session, delta, self.wheel_profile_epoch())
        }
    }

    fn sensitivity(raw: u8) -> VerticalScrollSensitivity {
        VerticalScrollSensitivity::try_new(raw).expect("test sensitivity is valid")
    }

    fn preferences(smooth_scroll: bool, sensitivity: u8) -> Arc<ScrollPreferences> {
        Arc::new(ScrollPreferences::new(
            smooth_scroll,
            self::sensitivity(sensitivity),
        ))
    }

    const APP_SMOOTHING: WheelScrollOverrides = WheelScrollOverrides {
        smooth_scroll: true,
        vertical_sensitivity: false,
    };

    fn publish_global(preferences: &ScrollPreferences, smooth_scroll: bool, sensitivity: u8) {
        preferences.publish(
            None,
            WheelScrollOverrides::default(),
            None,
            ScrollSettings {
                has_profiles: false,
                smooth_scroll,
                main_wheel_smooth_scroll: smooth_scroll,
                vertical_sensitivity: self::sensitivity(sensitivity),
                main_wheel_vertical_sensitivity: self::sensitivity(sensitivity),
            },
        );
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
                thumb_dispatches: Arc::default(),
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
        assert!(input.submit_thumb(&session, ScrollDelta::wheel_ticks(0.0, 2.0)));

        let queued = queued_input(&receiver);
        assert_eq!(queued.impulse, WheelDelta { x: 0.0, y: 2.0 });
        assert!(matches!(queued.output, ScrollOutputMode::Smooth { .. }));
    }

    #[test]
    fn hidpp_without_smoothing_phases_only_horizontal_ticks() {
        let (input, receiver, _controls) = standalone_input(3, preferences(false, 14));
        let session = HidppSessionId::with_epoch("mouse-a", 7);
        assert!(input.submit_thumb(&session, ScrollDelta::wheel_ticks(2.0, 0.0)));
        assert!(
            !input.submit_thumb(&session, ScrollDelta::wheel_ticks(0.0, 2.0)),
            "a vertical tick keeps the caller's direct, unphased wheel output"
        );
        assert!(
            !input.submit_thumb(&session, ScrollDelta::wheel_ticks(2.0, 1.0)),
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
                input.try_hidpp_wheel_scroll(&session, ScrollDelta::wheel_ticks(0.0, 2.0), None),
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

    /// A diverted main wheel's input carries its device's momentum settings
    /// to the worker, so each mouse glides by its own.
    #[test]
    fn a_diverted_main_wheel_carries_its_devices_momentum_settings() {
        let (input, receiver, _controls) = standalone_input(1, preferences(true, 14));
        let session = HidppSessionId::with_epoch("mouse-a", 7);
        let on = openlogi_core::config::MomentumScrolling {
            enabled: true,
            ..Default::default()
        }
        .resolve();
        assert!(on.is_some(), "sanity: an enabled setting resolves");
        for momentum in [on, None] {
            input
                .try_hidpp_wheel_scroll(&session, ScrollDelta::wheel_ticks(0.0, 1.0), momentum)
                .expect("the main wheel's tick is queued");
            let queued = queued_input(&receiver);
            assert!(matches!(
                queued.output_under(true),
                ScrollOutputMode::Eased { momentum: carried, .. } if carried == momentum
            ));
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
        assert_eq!(
            input.try_hidpp_wheel_scroll(&session, pixels, None),
            Err(pixels)
        );
        let zero = ScrollDelta::wheel_ticks(0.0, 0.0);
        assert_eq!(
            input.try_hidpp_wheel_scroll(&session, zero, None),
            Err(zero)
        );

        let tick = ScrollDelta::wheel_ticks(0.0, 2.0);
        let scaled = ScrollDelta::wheel_ticks(0.0, 1.0);
        assert_eq!(input.try_hidpp_wheel_scroll(&session, tick, None), Ok(()));
        assert_eq!(
            input.try_hidpp_wheel_scroll(&session, tick, None),
            Err(scaled),
            "a full queue hands back the scaled distance"
        );
        queued_input(&receiver);
        input.accepting.store(false, Ordering::Release);
        assert_eq!(
            input.try_hidpp_wheel_scroll(&session, tick, None),
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
        )
        .expect("spawn scroll worker");
        let input = runtime.input();
        let session = HidppSessionId::with_epoch("mouse-a", 1);
        assert!(input.submit_thumb(&session, ScrollDelta::wheel_ticks(1.0, 0.0)));
        assert!(input.submit_thumb(&session, ScrollDelta::wheel_ticks(1.0, 0.0)));

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
        )
        .expect("spawn scroll worker");
        let session = HidppSessionId::with_epoch("mouse-a", 1);
        assert!(
            runtime
                .input()
                .submit_thumb(&session, ScrollDelta::wheel_ticks(1.0, 0.0))
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
        )
        .expect("spawn scroll worker");
        let input = runtime.input();
        let session = HidppSessionId::with_epoch("mouse-a", 1);
        assert!(input.submit_thumb(&session, ScrollDelta::wheel_ticks(1.0, 0.0)));
        assert_eq!(
            received
                .recv_timeout(Duration::from_secs(1))
                .expect("gesture begins")
                .phase,
            openlogi_inject::SmoothScrollPhase::Began
        );

        publish_global(&preferences, true, 14);
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
            );
        });

        waiting
            .recv_timeout(Duration::from_secs(1))
            .expect("worker reaches its idle receive before the setting changes");
        publish_global(&preferences, !initial_smoothing, 14);
        resume.send(()).expect("worker resumes its receive");
        let session = HidppSessionId::with_epoch("mouse-a", 1);
        assert!(input.submit_thumb(&session, ScrollDelta::wheel_ticks(1.0, 0.0)));

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
        assert!(input.submit_thumb(&session, ScrollDelta::wheel_ticks(1.0, 0.0)));
        publish_global(&preferences, !initial_smoothing, 14);

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
        let hidpp = ScrollSource::Hidpp(
            HidppSessionId::with_epoch("mouse-a", 1),
            HidppWheel::Thumb(ThumbDispatch::default()),
        );
        let hook = ScrollSource::current_hook(SoftwareWheel::Primary);
        let input = |source: &ScrollSource, x, y, output| ScrollInput {
            generation: 0,
            profile_epoch: None,
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
        assert!(input.submit_thumb(&session, ScrollDelta::wheel_ticks(0.0, 2.0)));
        publish_global(&preferences, false, 14);
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

        publish_global(&preferences, false, 7);
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 2.0)));
        assert!(matches!(
            queued_input(&receiver).output,
            ScrollOutputMode::Direct
        ));

        publish_global(&preferences, true, 28);
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));
        let queued = queued_input(&receiver);
        assert_eq!(queued.impulse, WheelDelta { x: 0.0, y: 2.0 });
        assert!(matches!(queued.output, ScrollOutputMode::Smooth { .. }));
    }

    #[test]
    fn native_game_profile_passes_both_axes_and_preserves_diverted_main_wheel_distance() {
        use crate::observable::ObservableState;
        use crate::orchestrator::Orchestrator;
        use openlogi_core::app::ForegroundApp;
        use openlogi_core::config::Config;

        use crate::runtime::hook::handle_scroll;
        use openlogi_hook::{EventDevice, EventDisposition, LOGITECH_VENDOR_ID};

        let mouse = EventDevice {
            vendor_id: Some(LOGITECH_VENDOR_ID),
            product_name: Some("Logitech MX Master 3S".into()),
            ..EventDevice::default()
        };
        let game = "com.example.game";
        let mut config = Config::default();
        config.app_settings.smooth_scroll = true;
        config.app_settings.vertical_scroll_sensitivity = sensitivity(18);
        config.app_settings.thumbwheel_sensitivity = ThumbwheelSensitivity::MAX;
        config.set_app_thumbwheel_sensitivity(game, ThumbwheelSensitivity::DEFAULT);
        config.set_app_smooth_scroll(game, false);
        config.set_app_vertical_scroll_sensitivity(game, VerticalScrollSensitivity::DEFAULT);
        let mut orch = Orchestrator::new(config, Arc::new(ObservableState::new("test".into())));
        let (input, commands, _controls) = standalone_input(1, orch.shared().scroll_preferences);
        let tick = ScrollDelta::wheel_ticks(0.0, 14.0);

        orch.set_current_app(Some(ForegroundApp::unnamed(game.into())));
        assert_eq!(
            handle_scroll(tick, false, Some(&mouse), &input),
            EventDisposition::PassThrough,
            "neutral wheel distance and smoothing off pass through"
        );
        assert!(!input.try_hook_scroll(ScrollDelta::pixels(0.0, 14.0)));
        assert!(matches!(
            commands.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        let horizontal = ScrollDelta::wheel_ticks(2.0, 0.0);
        for delta in [horizontal, ScrollDelta::wheel_ticks(2.0, 14.0)] {
            assert_eq!(
                handle_scroll(delta, false, Some(&mouse), &input),
                EventDisposition::PassThrough
            );
        }
        assert!(matches!(
            commands.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        let session = HidppSessionId::with_epoch("mouse-a", 1);
        input
            .try_hidpp_wheel_scroll(&session, tick, None)
            .expect("diverted main wheel is queued");
        let diverted = queued_input(&commands);
        assert_eq!(diverted.impulse, WheelDelta { x: 0.0, y: 18.0 });
        assert!(matches!(diverted.output, ScrollOutputMode::Eased { .. }));

        orch.set_current_app(Some(ForegroundApp::unnamed("com.apple.Safari".into())));
        assert_eq!(
            handle_scroll(tick, false, Some(&mouse), &input),
            EventDisposition::Suppress
        );
        let safari = queued_input(&commands);
        assert_eq!(safari.impulse, WheelDelta { x: 0.0, y: 18.0 });
        assert!(matches!(safari.output, ScrollOutputMode::Smooth { .. }));
        assert_eq!(
            handle_scroll(horizontal, false, Some(&mouse), &input),
            EventDisposition::Suppress,
        );
        let horizontal_frame = queued_input(&commands);
        assert_eq!(
            horizontal_frame.impulse,
            WheelDelta { x: 2.0, y: 0.0 },
            "an ordinary OS horizontal event has no thumb-device scale"
        );
        assert!(matches!(
            horizontal_frame.output,
            ScrollOutputMode::Smooth { .. }
        ));
        let trackpad = EventDevice {
            product_name: Some("Magic Trackpad".into()),
            ..EventDevice::default()
        };
        assert_eq!(
            handle_scroll(
                ScrollDelta::pixels(0.0, 14.0),
                true,
                Some(&trackpad),
                &input
            ),
            EventDisposition::PassThrough
        );
        orch.set_current_app(Some(ForegroundApp::unnamed(game.into())));
        assert_eq!(
            handle_scroll(tick, false, Some(&mouse), &input),
            EventDisposition::PassThrough
        );
        assert!(matches!(
            commands.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn editing_a_profile_sensitivity_expires_scaled_ticks_while_global_edits_retain_them() {
        use crate::observable::ObservableState;
        use crate::orchestrator::Orchestrator;
        use openlogi_core::app::ForegroundApp;
        use openlogi_core::config::Config;

        for scoped in [false, true] {
            let mut config = Config::default();
            config.app_settings.vertical_scroll_sensitivity = sensitivity(7);
            if scoped {
                for app in ["editor", "browser"] {
                    config.set_app_vertical_scroll_sensitivity(app, sensitivity(7));
                }
            }
            let mut orch = Orchestrator::new(
                config.clone(),
                Arc::new(ObservableState::new("test".into())),
            );
            let prefs = orch.shared().scroll_preferences;
            let (input, commands, controls) = standalone_input(3, Arc::clone(&prefs));
            orch.set_current_app(Some(ForegroundApp::unnamed("editor".into())));
            let editor = prefs.load().main_wheel_epoch;
            assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 2.0)));
            orch.set_current_app(Some(ForegroundApp::unnamed("browser".into())));
            assert_eq!(
                prefs.load().main_wheel_epoch != editor,
                scoped,
                "only scoped input changes owner between apps with identical settings"
            );
            assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 4.0)));

            if scoped {
                config.set_app_vertical_scroll_sensitivity("browser", sensitivity(28));
            } else {
                config.app_settings.vertical_scroll_sensitivity = sensitivity(28);
            }
            orch.reload_config(config);
            assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 6.0)));
            let generation = Arc::clone(&input.generation);
            drop(input);
            let mut distances = Vec::new();
            run_worker(
                |deadline| receive_command(&commands, deadline),
                &controls,
                &generation,
                &prefs,
                &mut |_| panic!("software smoothing stays off"),
                &mut |delta| distances.push(delta.y),
            );
            if scoped {
                assert_eq!(
                    distances,
                    [12.0],
                    "only the current profile distance survives"
                );
            } else {
                assert_eq!(
                    distances,
                    [1.0, 2.0, 12.0],
                    "accepted legacy ticks keep their distance"
                );
            }
        }
    }

    #[test]
    fn application_smoothing_controls_os_wheels_and_thumb_but_preserves_wheel_hold_policy() {
        let preferences = preferences(false, 14);
        let (input, receiver, _controls) = standalone_input(1, Arc::clone(&preferences));
        let session = HidppSessionId::with_epoch("mouse-a", 1);
        let side = ScrollDelta::wheel_ticks(1.0, 0.0);
        for smooth in [true, false] {
            preferences.publish(
                Some("editor"),
                APP_SMOOTHING,
                None,
                ScrollSettings {
                    has_profiles: true,
                    smooth_scroll: !smooth,
                    main_wheel_smooth_scroll: smooth,
                    vertical_sensitivity: sensitivity(14),
                    main_wheel_vertical_sensitivity: sensitivity(14),
                },
            );
            for tick in [
                side,
                ScrollDelta::wheel_ticks(0.0, 1.0),
                ScrollDelta::wheel_ticks(1.0, 1.0),
            ] {
                assert_eq!(input.try_hook_scroll(tick), smooth);
                if smooth {
                    let queued = queued_input(&receiver);
                    assert_eq!(ScrollDelta::from(queued.impulse), tick);
                    assert!(matches!(queued.output, ScrollOutputMode::Smooth { .. }));
                }
            }
            assert!(!input.try_hook_scroll(ScrollDelta::pixels(1.0, 1.0)));
            assert!(input.submit_thumb(&session, side));
            let thumb = queued_input(&receiver);
            assert_eq!(
                matches!(thumb.output, ScrollOutputMode::Smooth { .. }),
                smooth
            );
            assert_eq!(
                matches!(thumb.output, ScrollOutputMode::Phased { .. }),
                !smooth
            );
            assert!(input.try_hidpp_held_scroll(&session, side));
            let held = queued_input(&receiver);
            assert_eq!(
                matches!(held.output, ScrollOutputMode::Smooth { .. }),
                !smooth
            );
            assert_eq!(
                matches!(held.output, ScrollOutputMode::Phased { .. }),
                smooth
            );
        }
    }

    #[test]
    fn application_switch_closes_hook_motion_and_the_wheel_hold_swipe() {
        for with_hidpp in [false, true] {
            let preferences = preferences(false, 14);
            preferences.publish(
                Some("editor"),
                APP_SMOOTHING,
                None,
                ScrollSettings {
                    has_profiles: true,
                    smooth_scroll: false,
                    main_wheel_smooth_scroll: true,
                    vertical_sensitivity: sensitivity(14),
                    main_wheel_vertical_sensitivity: sensitivity(14),
                },
            );
            let (input, commands, controls) = standalone_input(2, Arc::clone(&preferences));
            assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));
            let mut first_hook = queued_input(&commands);
            assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));
            let mut second_hook = queued_input(&commands);
            // Retargeting after one logical frame starts the stream without
            // relying on real-time scheduling or sleeping in the test.
            let at = Instant::now();
            first_hook.output = ScrollOutputMode::Smooth { at };
            second_hook.output = ScrollOutputMode::Smooth {
                at: at + super::super::FRAME_PERIOD,
            };
            let mut hook = [first_hook, second_hook].into_iter();
            let session = HidppSessionId::with_epoch("mouse-a", 1);
            if with_hidpp {
                assert!(input.try_hidpp_held_scroll(&session, ScrollDelta::wheel_ticks(1.0, 0.0)));
            }
            let mut step = 0;
            let switch_step = if with_hidpp { 4 } else { 3 };
            let mut frames = Vec::new();
            run_worker(
                |deadline| {
                    step += 1;
                    if step <= 2 {
                        Ok(ScrollCommand::Input(
                            hook.next().expect("first two inputs are the hook ticks"),
                        ))
                    } else if step < switch_step {
                        commands
                            .recv()
                            .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
                    } else if step == switch_step {
                        assert!(deadline.is_some(), "the first wheel is still active");
                        publish_global(&preferences, false, 14);
                        Ok(ScrollCommand::Wake)
                    } else if step == switch_step + 1 {
                        assert_eq!(
                            deadline, None,
                            "the old destination has no remaining swipe or hook animation"
                        );
                        Ok(ScrollCommand::CancelSession(session.clone()))
                    } else {
                        assert_eq!(deadline, None, "the old hook animation must not survive");
                        Err(mpsc::RecvTimeoutError::Disconnected)
                    }
                },
                &controls,
                &input.generation,
                &preferences,
                &mut |frame| frames.push(frame),
                &mut |_| panic!("accepted smooth input must stay in the gesture stream"),
            );
            assert_eq!(
                frames.last().map(|frame| frame.phase),
                Some(openlogi_inject::SmoothScrollPhase::Cancelled)
            );
        }
    }

    #[test]
    fn application_switch_discards_queued_hook_ticks_and_wheel_hold_swipes() {
        for return_to_enabled_app in [false, true] {
            let preferences = preferences(false, 14);
            let enabled = ScrollSettings {
                has_profiles: true,
                smooth_scroll: false,
                main_wheel_smooth_scroll: true,
                vertical_sensitivity: sensitivity(14),
                main_wheel_vertical_sensitivity: sensitivity(14),
            };
            preferences.publish(Some("editor"), APP_SMOOTHING, None, enabled);
            let (input, commands, controls) = standalone_input(2, Arc::clone(&preferences));
            assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));
            let session = HidppSessionId::with_epoch("mouse-a", 1);
            assert!(input.try_hidpp_held_scroll(&session, ScrollDelta::wheel_ticks(1.0, 0.0)));
            publish_global(&preferences, false, 14);
            if return_to_enabled_app {
                preferences.publish(Some("editor"), APP_SMOOTHING, None, enabled);
            }
            let generation = Arc::clone(&input.generation);
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
            );
            assert!(
                direct.is_empty(),
                "an expired hook tick must not reach the next application"
            );
            assert_eq!(
                frames
                    .iter()
                    .fold(WheelDelta::ZERO, |sum, frame| sum.plus(frame.delta)),
                WheelDelta::ZERO,
                "neither wheel can post an old distance into the next destination"
            );
        }
    }

    #[test]
    fn primary_motion_tracks_only_the_resolved_mouse_application() {
        use crate::observable::ObservableState;
        use crate::orchestrator::Orchestrator;
        use openlogi_core::app::ForegroundApp;
        use openlogi_core::config::{Config, MouseProfileTarget};
        use openlogi_hook::{PointerContext, PointerTarget};

        let window = |app: &str, window_id| PointerContext {
            app: Some(ForegroundApp::unnamed(app.to_owned())),
            target: PointerTarget::Window {
                process_id: 41,
                window_id,
            },
        };
        let mut config = Config::default();
        config.app_settings.mouse_profile_target = MouseProfileTarget::Pointer;
        config.app_settings.smooth_scroll = true;
        let mut orch = Orchestrator::new(
            config.clone(),
            Arc::new(ObservableState::new("test".into())),
        );
        let preferences = orch.shared().scroll_preferences;
        let epoch = || preferences.load().main_wheel_epoch;
        let inherited = epoch();
        orch.set_pointer_context(window("browser", 1));
        assert_eq!(
            epoch(),
            inherited,
            "legacy global motion survives app switches"
        );

        config.set_app_smooth_scroll("editor", true);
        orch.reload_config(config.clone());
        let browser = epoch();
        orch.set_pointer_context(window("terminal", 2));
        assert_ne!(
            epoch(),
            browser,
            "inherited apps own separate motion once profiles exist"
        );
        orch.set_pointer_context(window("editor", 3));
        let editor = epoch();
        assert_ne!(editor, inherited, "the editor owns its wheel motion");

        orch.set_current_app(Some(ForegroundApp::unnamed("browser".into())));
        orch.set_pointer_context(window("editor", 4));
        assert_eq!(
            epoch(),
            editor,
            "focus and same-app windows preserve the pointer profile"
        );
        config.app_settings.mouse_profile_target = MouseProfileTarget::Focused;
        orch.reload_config(config);
        assert_ne!(epoch(), editor, "the mouse now follows the focused browser");
    }

    #[test]
    fn editing_global_settings_preserves_queued_inherited_input_in_the_same_app() {
        use crate::observable::ObservableState;
        use crate::orchestrator::Orchestrator;
        use openlogi_core::app::ForegroundApp;
        use openlogi_core::config::Config;

        for sensitivity_override in [false, true] {
            let mut config = Config::default();
            config.app_settings.smooth_scroll = true;
            config.app_settings.vertical_scroll_sensitivity = sensitivity(7);
            config.set_app_smooth_scroll("game", false);
            if sensitivity_override {
                config.set_app_vertical_scroll_sensitivity("browser", sensitivity(7));
            }
            let mut orch = Orchestrator::new(
                config.clone(),
                Arc::new(ObservableState::new("test".into())),
            );
            orch.set_current_app(Some(ForegroundApp::unnamed("browser".into())));
            let preferences = orch.shared().scroll_preferences;
            let (input, commands, controls) = standalone_input(1, Arc::clone(&preferences));
            assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 2.0)));
            config.app_settings.smooth_scroll = false;
            config.app_settings.vertical_scroll_sensitivity = sensitivity(28);
            orch.reload_config(config);
            let generation = Arc::clone(&input.generation);
            drop(input);
            let mut distances = Vec::new();
            run_worker(
                |deadline| receive_command(&commands, deadline),
                &controls,
                &generation,
                &preferences,
                &mut |_| panic!("global smoothing was disabled before dequeue"),
                &mut |delta| distances.push(delta.y),
            );
            assert_eq!(
                distances,
                [1.0],
                "accepted tick keeps its original distance"
            );
        }
    }

    #[test]
    fn switching_between_enabled_apps_retires_active_and_queued_os_and_thumb_motion() {
        use crate::observable::ObservableState;
        use crate::orchestrator::Orchestrator;
        use openlogi_core::app::ForegroundApp;
        use openlogi_core::config::Config;

        let focus = |orch: &mut Orchestrator, app: &str| {
            orch.set_current_app(Some(ForegroundApp::unnamed(app.to_owned())));
        };
        for (inherited, switch_back) in [(false, false), (false, true), (true, false), (true, true)]
        {
            for (tick, thumb) in [
                (ScrollDelta::wheel_ticks(0.0, 1.0), false),
                (ScrollDelta::wheel_ticks(1.0, 0.0), false),
                (ScrollDelta::wheel_ticks(1.0, 0.0), true),
            ] {
                let mut config = Config::default();
                if inherited {
                    config.app_settings.smooth_scroll = true;
                    config.set_app_smooth_scroll("game", false);
                } else {
                    config.set_app_smooth_scroll("editor", true);
                    config.set_app_smooth_scroll("browser", true);
                }
                let mut orch =
                    Orchestrator::new(config, Arc::new(ObservableState::new("test".into())));
                focus(&mut orch, "editor");
                let preferences = orch.shared().scroll_preferences;
                let (input, commands, controls) = standalone_input(3, Arc::clone(&preferences));
                let session = HidppSessionId::with_epoch("mouse", 1);
                let submit = || {
                    if thumb {
                        input.submit_thumb(&session, tick)
                    } else {
                        input.try_hook_scroll(tick)
                    }
                };
                let old_epoch = input.wheel_profile_epoch();
                for _ in 0..3 {
                    assert!(submit());
                }

                let at = Instant::now();
                let mut step = 0;
                run_worker(
                    |deadline| {
                        step += 1;
                        match step {
                            1 | 2 => {
                                let mut tick = queued_input(&commands);
                                tick.output = ScrollOutputMode::Smooth {
                                    at: if step == 1 {
                                        at
                                    } else {
                                        at + super::super::FRAME_PERIOD
                                    },
                                };
                                Ok(ScrollCommand::Input(tick))
                            }
                            3 => {
                                assert!(deadline.is_some(), "the first app has active motion");
                                focus(&mut orch, "browser");
                                if switch_back {
                                    focus(&mut orch, "editor");
                                }
                                assert!(preferences.main_wheel_smooth_scroll_enabled());
                                Ok(ScrollCommand::Wake)
                            }
                            4 => {
                                assert_eq!(deadline, None, "old animation must end at the switch");
                                Ok(ScrollCommand::Input(queued_input(&commands)))
                            }
                            5 => {
                                assert_eq!(deadline, None, "old queued tick must be discarded");
                                if thumb {
                                    assert!(
                                        input.try_hidpp_scroll(&session, tick, old_epoch),
                                        "consumed stale reports must not fall back to injection"
                                    );
                                    assert!(matches!(
                                        commands.try_recv(),
                                        Err(mpsc::TryRecvError::Empty)
                                    ));
                                }
                                assert!(submit());
                                Ok(ScrollCommand::Input(queued_input(&commands)))
                            }
                            _ => {
                                assert!(deadline.is_some(), "new app's wheel tick starts fresh");
                                Err(mpsc::RecvTimeoutError::Disconnected)
                            }
                        }
                    },
                    &controls,
                    &input.generation,
                    &preferences,
                    &mut |_| {},
                    &mut |_| panic!("both profiles smooth their primary wheel"),
                );
            }
        }
    }

    #[test]
    fn legacy_global_smoothing_preserves_active_and_queued_horizontal_motion_across_apps() {
        use crate::observable::ObservableState;
        use crate::orchestrator::Orchestrator;
        use openlogi_core::app::ForegroundApp;
        use openlogi_core::config::Config;
        let mut config = Config::default();
        config.app_settings.smooth_scroll = true;
        let mut orch = Orchestrator::new(config, Arc::new(ObservableState::new("test".into())));
        orch.set_current_app(Some(ForegroundApp::unnamed("browser".into())));
        let preferences = orch.shared().scroll_preferences;
        let (input, commands, controls) = standalone_input(3, Arc::clone(&preferences));
        for _ in 0..3 {
            assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(1.0, 0.0)));
        }
        let mut ticks = [
            queued_input(&commands),
            queued_input(&commands),
            queued_input(&commands),
        ];
        let at = Instant::now();
        // Two ticks start a gesture. The queued third arrives after the old
        // segment ends, proving the first two retain their complete distance.
        for (tick, offset) in ticks.iter_mut().zip([
            Duration::ZERO,
            super::super::FRAME_PERIOD,
            super::super::ANIMATION_DURATION * 2,
        ]) {
            tick.output = ScrollOutputMode::Smooth { at: at + offset };
        }
        let mut ticks = ticks.into_iter();
        let mut step = 0;
        let mut frames = Vec::new();
        run_worker(
            |deadline| {
                step += 1;
                if step == 3 {
                    assert!(
                        deadline.is_some(),
                        "horizontal motion is active before the switch"
                    );
                    orch.set_current_app(Some(ForegroundApp::unnamed("editor".into())));
                    Ok(ScrollCommand::Wake)
                } else if step <= 4 {
                    if step == 4 {
                        assert!(
                            deadline.is_some(),
                            "legacy global smoothing keeps horizontal motion across application switches"
                        );
                    }
                    Ok(ScrollCommand::Input(
                        ticks.next().expect("three horizontal ticks"),
                    ))
                } else {
                    assert!(
                        deadline.is_some(),
                        "the horizontal tick queued before the switch is accepted afterwards"
                    );
                    Err(mpsc::RecvTimeoutError::Disconnected)
                }
            },
            &controls,
            &input.generation,
            &preferences,
            &mut |frame| frames.push(frame),
            &mut |_| panic!("legacy global smoothing stays enabled"),
        );
        let distance = frames
            .iter()
            .fold(WheelDelta::ZERO, |sum, frame| sum.plus(frame.delta));
        assert_eq!(
            distance,
            WheelDelta { x: 2.0, y: 0.0 },
            "active horizontal motion keeps its full distance"
        );
    }

    #[test]
    fn changing_one_thumb_dispatch_retires_its_full_queue_without_stopping_other_devices() {
        use openlogi_inject::SmoothScrollPhase::{Began, Cancelled, Changed};

        let preferences = preferences(false, 14);
        let (input, commands, controls) = standalone_input(2, Arc::clone(&preferences));
        let first = HidppSessionId::with_epoch("mouse-a", 7);
        let other = HidppSessionId::with_epoch("mouse-b", 8);
        let tick = |n| ScrollDelta::wheel_ticks(n, 0.0);
        assert!(input.submit_thumb(&first, tick(1.0)));
        let mut output = Vec::new();
        let mut step = 0;
        run_worker(
            |deadline| {
                step += 1;
                match step {
                    1 | 4 => {}
                    2 => {
                        assert!(deadline.is_some(), "first device has an open thumb gesture");
                        assert!(input.submit_thumb(&first, tick(2.0)));
                        assert!(input.submit_thumb(&other, tick(4.0)));
                        input.cancel_hidpp_thumb(&first);
                    }
                    3 => assert!(input.submit_thumb(&first, tick(8.0))),
                    _ => return Err(mpsc::RecvTimeoutError::Disconnected),
                }
                Ok(ScrollCommand::Input(queued_input(&commands)))
            },
            &controls,
            &input.generation,
            &preferences,
            &mut |frame| output.push((frame.delta.x, frame.phase)),
            &mut |_| panic!("synthetic thumb ticks are phased"),
        );
        assert_eq!(
            output,
            [
                (1.0, Began),
                (0.0, Cancelled),
                (4.0, Began),
                (8.0, Changed),
                (0.0, Cancelled)
            ]
        );
    }

    #[derive(Clone, Copy)]
    enum TestWheel {
        Thumb,
        Main,
        Held,
    }

    /// The production input handle and queue driven with a logical clock.
    struct WheelTrace<'a> {
        input: &'a ScrollInputHandle,
        commands: &'a mpsc::Receiver<ScrollCommand>,
        session: HidppSessionId,
        start: Instant,
    }

    impl<'a> WheelTrace<'a> {
        fn new(input: &'a ScrollInputHandle, commands: &'a mpsc::Receiver<ScrollCommand>) -> Self {
            Self {
                input,
                commands,
                session: HidppSessionId::with_epoch("mouse", 1),
                start: Instant::now(),
            }
        }

        fn tick(&self, wheel: TestWheel, distance: f64, offset: Duration) -> ScrollCommand {
            let side = ScrollDelta::wheel_ticks(distance, 0.0);
            match wheel {
                TestWheel::Main => self
                    .input
                    .try_hidpp_wheel_scroll(
                        &self.session,
                        ScrollDelta::wheel_ticks(0.0, distance),
                        None,
                    )
                    .expect("diverted device wheel is eased without momentum"),
                TestWheel::Thumb => assert!(self.input.submit_thumb(&self.session, side)),
                TestWheel::Held => assert!(self.input.try_hidpp_held_scroll(&self.session, side)),
            }
            let mut tick = queued_input(self.commands);
            match &mut tick.output {
                ScrollOutputMode::Smooth { at }
                | ScrollOutputMode::Phased { at }
                | ScrollOutputMode::Eased { at, .. } => *at = self.start + offset,
                ScrollOutputMode::Direct => panic!("trace expects the animation or gesture worker"),
            }
            ScrollCommand::Input(tick)
        }
    }

    #[test]
    fn app_and_desktop_swipes_cancel_in_order_without_stopping_the_device_main_wheel() {
        use super::super::FrameKind::{Eased, Phased};
        use crate::observable::ObservableState;
        use crate::orchestrator::Orchestrator;
        use TestWheel::{Held, Main, Thumb};
        use openlogi_core::app::ForegroundApp;
        use openlogi_core::config::Config;
        use openlogi_inject::SmoothScrollPhase::{Began, Cancelled, Changed, Ended};

        let focus = |orch: &mut Orchestrator, app: Option<&str>| {
            orch.set_current_app(app.map(|app| ForegroundApp::unnamed(app.to_owned())));
        };
        let mut config = Config::default();
        let thumb_sensitivity = ThumbwheelSensitivity::try_new(28).expect("valid test sensitivity");
        for app in ["editor", "browser"] {
            config.set_app_thumbwheel_sensitivity(app, thumb_sensitivity);
        }
        let mut orch = Orchestrator::new(
            config.clone(),
            Arc::new(ObservableState::new("test".into())),
        );
        focus(&mut orch, Some("editor"));
        let prefs = orch.shared().scroll_preferences;
        let (input, commands, controls) = standalone_input(1, Arc::clone(&prefs));
        let trace = WheelTrace::new(&input, &commands);

        let mut frames = Vec::new();
        let mut step = 0;
        run_worker(
            |_| {
                step += 1;
                let time = super::super::FRAME_PERIOD * step;
                Ok(match step {
                    1 => trace.tick(Main, 4.0, Duration::ZERO),
                    2 => trace.tick(Main, 4.0, super::super::ANIMATION_DURATION / 2),
                    3 => trace.tick(Thumb, 2.0, time),
                    4 => trace.tick(Held, -1.0, time),
                    5 => {
                        let stale = trace.tick(Thumb, 100.0, time);
                        focus(&mut orch, None);
                        stale
                    }
                    6 => trace.tick(Held, 3.0, time),
                    7 => {
                        let stale = trace.tick(Held, 100.0, time);
                        focus(&mut orch, Some("browser"));
                        stale
                    }
                    8 => trace.tick(Thumb, 4.0, time),
                    9 => trace.tick(Held, -2.0, time),
                    10 => {
                        focus(&mut orch, Some("editor"));
                        trace.tick(Thumb, -5.0, time)
                    }
                    11 => trace.tick(Main, 4.0, super::super::ANIMATION_DURATION * 2),
                    12 => {
                        config.remove_wheel_scroll_profile("editor");
                        config.remove_wheel_scroll_profile("browser");
                        orch.reload_config(config.clone());
                        ScrollCommand::Wake
                    }
                    _ => return Err(mpsc::RecvTimeoutError::Disconnected),
                })
            },
            &controls,
            &input.generation,
            &prefs,
            &mut |frame| frames.push(frame.dispatch(|d, p| (Eased, p, d), |d, p| (Phased, p, d))),
            &mut |_| panic!("only the physical main wheel and two phased producers are active"),
        );
        let x = |distance| ScrollDelta::wheel_ticks(distance, 0.0);
        let y = |distance| ScrollDelta::wheel_ticks(0.0, distance);
        assert_eq!(
            frames,
            [
                (Eased, Began, y(2.0)),
                (Phased, Began, x(2.0)),
                (Phased, Changed, x(-1.0)),
                (Phased, Cancelled, x(0.0)),
                (Phased, Began, x(3.0)),
                (Phased, Cancelled, x(0.0)),
                (Phased, Began, x(4.0)),
                (Phased, Changed, x(-2.0)),
                (Phased, Cancelled, x(0.0)),
                (Phased, Began, x(-5.0)),
                (Eased, Ended, y(6.0)),
                (Phased, Cancelled, x(0.0)),
            ]
        );
        assert!(!prefs.load().settings.has_profiles);
    }

    #[test]
    fn profiled_thumb_swipe_stays_separate_from_held_and_device_easing() {
        use super::super::FrameKind::{Eased, Phased};
        use openlogi_inject::SmoothScrollPhase::{Began, Cancelled, Changed, Ended};

        let preferences = preferences(true, 14);
        preferences.publish(
            Some("game"),
            APP_SMOOTHING,
            None,
            ScrollSettings {
                has_profiles: true,
                smooth_scroll: true,
                main_wheel_smooth_scroll: false,
                vertical_sensitivity: sensitivity(14),
                main_wheel_vertical_sensitivity: sensitivity(14),
            },
        );
        for held in [false, true] {
            for thumb_first in [false, true] {
                let (input, commands, controls) = standalone_input(1, Arc::clone(&preferences));
                let trace = WheelTrace::new(&input, &commands);
                let thumb = |at| trace.tick(TestWheel::Thumb, 1.0, at);
                let other = |at| {
                    trace.tick(
                        if held {
                            TestWheel::Held
                        } else {
                            TestWheel::Main
                        },
                        2.0,
                        at,
                    )
                };
                let at = Duration::ZERO;
                let mut script = Vec::new();
                if thumb_first {
                    script.push(thumb(at));
                }
                script.push(other(at));
                script.push(other(at + super::super::ANIMATION_DURATION / 2));
                if !thumb_first {
                    script.push(thumb(at + super::super::ANIMATION_DURATION / 2));
                }
                script.push(thumb(at + super::super::ANIMATION_DURATION * 3 / 4));
                script.push(other(at + super::super::ANIMATION_DURATION * 2));
                let mut script = script.into_iter();
                let mut frames = Vec::new();
                run_worker(
                    |_| script.next().ok_or(mpsc::RecvTimeoutError::Disconnected),
                    &controls,
                    &input.generation,
                    &preferences,
                    &mut |frame| frames.push((frame.kind, frame.phase, frame.delta)),
                    &mut |_| panic!("the other wheel uses smoothing"),
                );
                let swipe: Vec<_> = frames
                    .iter()
                    .filter(|(kind, ..)| *kind == Phased)
                    .map(|(_, phase, d)| (*phase, d.x, d.y))
                    .collect();
                assert_eq!(
                    swipe,
                    [
                        (Began, 1.0, 0.0),
                        (Changed, 1.0, 0.0),
                        (Cancelled, 0.0, 0.0)
                    ]
                );
                let motion: Vec<_> = frames
                    .iter()
                    .filter(|(kind, ..)| *kind == Eased)
                    .map(|(_, phase, d)| (*phase, if held { d.x } else { d.y }))
                    .collect();
                assert_eq!(motion, [(Began, 1.0), (Ended, 3.0)]);
            }
        }
    }

    /// The thumb wheel and the diverted main wheel of one session are
    /// separate sources, so one's momentum and easing never reach the
    /// other's motion — and both belong to the session its cancellation
    /// ends.
    #[test]
    fn the_main_and_thumb_wheels_are_separate_sources_of_one_session() {
        let (input, commands, _controls) = standalone_input(2, preferences(true, 14));
        let session = HidppSessionId::with_epoch("mouse-a", 7);
        assert!(input.submit_thumb(&session, ScrollDelta::wheel_ticks(1.0, 0.0)));
        input
            .try_hidpp_wheel_scroll(&session, ScrollDelta::wheel_ticks(0.0, 1.0), None)
            .expect("the main wheel's tick is queued");
        let source = |command| match command {
            Ok(ScrollCommand::Input(input)) => input.source,
            _ => panic!("expected queued input"),
        };
        let thumb = source(commands.recv());
        let main = source(commands.recv());
        assert_ne!(thumb, main);
        assert!(thumb.in_session(&session) && main.in_session(&session));
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
    fn cancellation_overtakes_a_full_queue_without_discarding_another_source() {
        let preferences = preferences(true, 14);
        let (input, commands, controls) = standalone_input(2, Arc::clone(&preferences));
        let cancelled = HidppSessionId::with_epoch("mouse-a", 7);
        let survivor = HidppSessionId::with_epoch("mouse-b", 3);
        assert!(input.submit_thumb(&cancelled, ScrollDelta::wheel_ticks(1.0, 0.0)));
        assert!(input.submit_thumb(&survivor, ScrollDelta::wheel_ticks(0.0, 1.0)));

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
            profile_epoch: None,
            source: ScrollSource::Hidpp(
                session.clone(),
                HidppWheel::Thumb(ThumbDispatch::default()),
            ),
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
            );
        });

        commands
            .send(ScrollCommand::Input(ScrollInput {
                generation: 1,
                profile_epoch: None,
                source: ScrollSource::Hidpp(
                    session.clone(),
                    HidppWheel::Thumb(ThumbDispatch::default()),
                ),
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
                profile_epoch: None,
                source: ScrollSource::Hidpp(
                    session.clone(),
                    HidppWheel::Thumb(ThumbDispatch::default()),
                ),
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
        );

        assert_eq!(
            emitted,
            [WheelDelta { x: 0.0, y: 1.0 }, WheelDelta { x: 0.0, y: 3.0 }],
            "the cancelled session must not emit, and other sources must"
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
