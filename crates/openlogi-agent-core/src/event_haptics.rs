//! Event haptics: a short waveform on the haptic mouse for a button press or
//! a main-wheel detent, in every application.
//!
//! Input reaches here from two places. The OS hook reports native presses and
//! wheel movement on its callback thread, which must never block, so
//! [`EventHaptics::observe`] only records a cue under a `try_lock` and wakes
//! the player. A control diverted over HID++ never reaches the hook: a
//! diverted button (the gesture button, or Back and Forward over Bluetooth)
//! arrives through [`EventHaptics::observe_button`], and a diverted main
//! wheel (`0x2121`, the smooth wheel) through
//! [`EventHaptics::observe_main_wheel`], both from the gesture dispatcher.
//!
//! The agent's single-flight haptic player drains the cues through
//! [`EventHaptics::next_play`], so a cue costs nothing until the player is free.
//! The cue queue decides what plays next: a press before a wheel detent, wheel
//! detents no closer together than [`WHEEL_INTERVAL`], and nothing that waited
//! past [`STALE_AFTER`].

use std::cell::Cell;
use std::num::{NonZeroU8, NonZeroU16};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use openlogi_core::config::{EventHapticFeedback, EventHapticInput, EventHapticsConfig};
use openlogi_core::scroll::ScrollDelta;
use openlogi_hid::{DeviceRoute, HapticWaveform};
use openlogi_hook::{ButtonId, EventDevice, KeyEvent, MouseEvent};
use tokio::sync::{Notify, watch};

/// The shortest gap between two wheel-detent plays.
///
/// One play holds the device's transport for about 20 ms over Bluetooth, and
/// a fast roll reports far more detents than that admits. Thrum Haptics
/// settled on 35 ms by feel on the same mouse, and so does this.
pub const WHEEL_INTERVAL: Duration = Duration::from_millis(35);

/// How long a cue may wait for the player before it is dropped.
///
/// Haptic feedback that arrives this late no longer reads as belonging to the
/// input that caused it. The player waits this long behind an Actions Ring
/// buzz or an arming at most.
pub const STALE_AFTER: Duration = Duration::from_millis(150);

/// The shortest gap between two thumb-wheel steps.
///
/// Wider than [`WHEEL_INTERVAL`]: the thumb wheel has no detents, so its
/// haptic is the only thing marking a step, and an uneven run of them reads
/// as a rattle rather than a scale. Thrum Haptics' default on this mouse.
pub const THUMBWHEEL_INTERVAL: Duration = Duration::from_millis(50);

/// How long after a Command-key press a clipboard change counts as the copy
/// that press made. Apps answer a copy in milliseconds; a change much later is
/// something else writing the clipboard.
pub const COPY_WINDOW: Duration = Duration::from_millis(500);

/// How long after one wheel-mode cue a second is taken for the same switch.
pub const WHEEL_MODE_ECHO: Duration = Duration::from_millis(400);

/// The gap between the plays of one repeated signal, such as the taps that
/// count a DPI preset: wide enough to count, short enough to read as one.
pub const REPEAT_GAP: Duration = Duration::from_millis(140);

/// Which input a cue answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventCue {
    /// A press of this button.
    Button(ButtonId),
    /// One detent of the main wheel.
    WheelDetent,
    /// One step of the thumb wheel.
    ThumbwheelStep,
    /// A gesture was recognized and its action ran.
    Gesture,
    /// The device changed mode. Plays `count` times, so a DPI step can tell
    /// which preset it landed on.
    ModeChange {
        /// How many times to play.
        count: NonZeroU8,
    },
    /// The wheel switched between ratchet and free spin (SmartShift),
    /// reported by the device or by OpenLogi's own toggle. One switch can
    /// arrive both ways, so a second within [`WHEEL_MODE_ECHO`] is dropped.
    WheelMode,
    /// The output volume moved one step.
    LevelStep,
    /// The output volume reached 0 or 100 %.
    LevelLimit,
    /// The pointer moved onto another display.
    DisplayCrossing,
    /// A drag picked something up, or dropped it.
    Drag,
    /// A copy or cut put something on the clipboard.
    Copy,
}

impl EventCue {
    /// The setting that decides what this cue plays.
    #[must_use]
    pub fn input(self) -> EventHapticInput {
        match self {
            Self::Button(button) => EventHapticInput::Button(button),
            Self::WheelDetent => EventHapticInput::Wheel,
            Self::ThumbwheelStep => EventHapticInput::Thumbwheel,
            Self::Gesture => EventHapticInput::Gestures,
            Self::ModeChange { .. } | Self::WheelMode => EventHapticInput::ModeChanges,
            Self::LevelStep => EventHapticInput::LevelSteps,
            Self::LevelLimit => EventHapticInput::LevelLimits,
            Self::DisplayCrossing => EventHapticInput::DisplayCrossing,
            Self::Drag => EventHapticInput::Drag,
            Self::Copy => EventHapticInput::Copy,
        }
    }

    /// The name the player logs this cue under.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Button(_) => "button",
            Self::WheelDetent => "wheel",
            Self::ThumbwheelStep => "thumb wheel",
            Self::Gesture => "gesture",
            Self::ModeChange { .. } => "mode change",
            Self::WheelMode => "wheel mode",
            Self::LevelStep => "level step",
            Self::LevelLimit => "level limit",
            Self::DisplayCrossing => "display crossing",
            Self::Drag => "drag",
            Self::Copy => "copy",
        }
    }
}

/// The device event haptics play on, and what each input plays there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventHapticsTarget {
    /// The device's physical config key: the diverted controls' session key.
    pub config_key: String,
    /// Where the player sends the plays.
    pub route: DeviceRoute,
    /// The device's settings.
    pub config: EventHapticsConfig,
}

/// The device the player holds the haptic engine on for, and which arming
/// of it is current.
///
/// The generation changes whenever the engine there must be armed again: the
/// target moved, or the device may have lost the engine's enable flag in a
/// power transition the route does not show ([`EventHaptics::rearm`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineHold {
    /// The target's route, or `None` while event haptics play nowhere.
    pub route: Option<DeviceRoute>,
    /// Which arming is current; only ever grows.
    pub generation: u64,
}

/// One play the player owes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventPlay {
    /// Where to play.
    pub route: DeviceRoute,
    /// What to play.
    pub waveform: HapticWaveform,
    /// Which input it answers.
    pub cue: EventCue,
}

/// What the player does next about event haptics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NextPlay {
    /// Play this now.
    Play(EventPlay),
    /// A wheel detent waits for [`WHEEL_INTERVAL`] to pass: look again then.
    At(Instant),
    /// Nothing is owed.
    Idle,
}

/// What [`CueQueue::next`] decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Due {
    Now(EventCue),
    At(Instant),
    Idle,
}

/// A signal waiting to play, possibly several times.
#[derive(Clone, Copy, Debug)]
struct Pending {
    cue: EventCue,
    /// Plays still owed, this one included.
    remaining: u8,
    /// When the next play is due.
    at: Instant,
}

/// A paced stream of steps: a wheel, whose steps may come faster than the
/// device plays them.
#[derive(Clone, Copy, Debug)]
struct Paced {
    interval: Duration,
    queued: Option<Instant>,
    last_play: Option<Instant>,
}

impl Paced {
    const fn new(interval: Duration) -> Self {
        Self {
            interval,
            queued: None,
            last_play: None,
        }
    }

    /// `Ok(true)` when a step plays at `now`, `Err(at)` when one waits for
    /// the interval, `Ok(false)` when none is queued.
    fn take(&mut self, now: Instant) -> Result<bool, Instant> {
        if self.queued.is_none() {
            return Ok(false);
        }
        match self.last_play.map(|last| last + self.interval) {
            Some(free_at) if free_at > now => Err(free_at),
            _ => {
                self.queued = None;
                self.last_play = Some(now);
                Ok(true)
            }
        }
    }
}

/// The cues not yet played.
///
/// Four slots, in the order they play: a press, a signal (a gesture, a mode
/// or level change, a crossing, a drag, a copy), a main-wheel detent, a
/// thumb-wheel step. A press goes first because it is the deliberate act the
/// user feels for. Each slot keeps only its newest cue: the player is
/// single-flight, and two queued detents would only buzz twice for a moment
/// that has passed.
#[derive(Debug)]
struct CueQueue {
    press: Option<(Instant, ButtonId)>,
    signal: Option<Pending>,
    wheel: Paced,
    thumbwheel: Paced,
    /// When the last wheel-mode cue arrived, to drop its echo.
    last_wheel_mode: Option<Instant>,
}

impl Default for CueQueue {
    fn default() -> Self {
        Self {
            press: None,
            signal: None,
            last_wheel_mode: None,
            wheel: Paced::new(WHEEL_INTERVAL),
            thumbwheel: Paced::new(THUMBWHEEL_INTERVAL),
        }
    }
}

impl CueQueue {
    fn push(&mut self, cue: EventCue, now: Instant) {
        match cue {
            EventCue::Button(button) => self.press = Some((now, button)),
            EventCue::WheelDetent => self.wheel.queued = Some(now),
            EventCue::ThumbwheelStep => self.thumbwheel.queued = Some(now),
            EventCue::ModeChange { count } => {
                self.signal = Some(Pending {
                    cue,
                    remaining: count.get(),
                    at: now,
                });
            }
            EventCue::WheelMode => {
                let echo = self
                    .last_wheel_mode
                    .is_some_and(|last| now.saturating_duration_since(last) < WHEEL_MODE_ECHO);
                self.last_wheel_mode = Some(now);
                if !echo {
                    self.signal = Some(Pending {
                        cue,
                        remaining: 1,
                        at: now,
                    });
                }
            }
            EventCue::Gesture
            | EventCue::LevelStep
            | EventCue::LevelLimit
            | EventCue::DisplayCrossing
            | EventCue::Drag
            | EventCue::Copy => {
                self.signal = Some(Pending {
                    cue,
                    remaining: 1,
                    at: now,
                });
            }
        }
    }

    fn clear(&mut self) {
        self.press = None;
        self.signal = None;
        self.wheel.queued = None;
        self.thumbwheel.queued = None;
    }

    /// What plays at `now`, taking it off the queue.
    ///
    /// A detent or step plays only once its wheel's interval has passed since
    /// the last one; a repeated signal plays [`REPEAT_GAP`] apart. Anything
    /// that waited past [`STALE_AFTER`] is dropped.
    fn next(&mut self, now: Instant) -> Due {
        let fresh = |at: &Instant| now.saturating_duration_since(*at) < STALE_AFTER;
        self.press = self.press.filter(|(at, _)| fresh(at));
        self.signal = self.signal.filter(|pending| fresh(&pending.at));
        self.wheel.queued = self.wheel.queued.filter(fresh);
        self.thumbwheel.queued = self.thumbwheel.queued.filter(fresh);
        if let Some((_, button)) = self.press.take() {
            return Due::Now(EventCue::Button(button));
        }
        let mut wait: Option<Instant> = None;
        if let Some(pending) = self.signal {
            if pending.at <= now {
                self.signal = (pending.remaining > 1).then(|| Pending {
                    remaining: pending.remaining - 1,
                    at: now + REPEAT_GAP,
                    ..pending
                });
                return Due::Now(pending.cue);
            }
            wait = Some(pending.at);
        }
        for (paced, cue) in [
            (&mut self.wheel, EventCue::WheelDetent),
            (&mut self.thumbwheel, EventCue::ThumbwheelStep),
        ] {
            match paced.take(now) {
                Ok(true) => return Due::Now(cue),
                Ok(false) => {}
                Err(at) => wait = Some(wait.map_or(at, |earlier| earlier.min(at))),
            }
        }
        wait.map_or(Due::Idle, Due::At)
    }
}

/// How far, in pointer points, the pointer travels with the left button held
/// before the press counts as a drag rather than a click.
const DRAG_THRESHOLD: i64 = 10;

/// How far, in pixels, a native thumb wheel scrolls per step. The thumb wheel
/// has no detents, so this distance stands in for one: Thrum Haptics'
/// default on this mouse.
const THUMBWHEEL_STEP_PIXELS: f64 = 12.0;

/// Whether the left button is carrying something: a press from the target
/// that moved past [`DRAG_THRESHOLD`] before its release.
#[derive(Debug, Default)]
struct DragTracker {
    /// The left button is down, pressed on the target device.
    held: bool,
    /// How far the pointer moved since the press, while not yet a drag.
    travelled: i64,
    dragging: bool,
}

impl DragTracker {
    /// The left button went down or up. `ours` says whether the press came
    /// from the target device: another pointer's press is never a drag here.
    fn button(&mut self, pressed: bool, ours: bool) -> Option<EventCue> {
        let dropped = self.dragging;
        *self = Self {
            held: pressed && ours,
            ..Self::default()
        };
        (!pressed && dropped).then_some(EventCue::Drag)
    }

    /// The pointer moved by `(dx, dy)`: the pick-up once the press travelled
    /// far enough.
    fn moved(&mut self, dx: i32, dy: i32) -> Option<EventCue> {
        if !self.held || self.dragging {
            return None;
        }
        self.travelled += i64::from(dx.unsigned_abs()) + i64::from(dy.unsigned_abs());
        self.dragging = self.travelled >= DRAG_THRESHOLD;
        self.dragging.then_some(EventCue::Drag)
    }

    /// The hook lost events: a release may have gone unseen.
    fn cancel(&mut self) {
        *self = Self::default();
    }
}

thread_local! {
    /// Native wheel distance, in detents, that has not made a whole detent
    /// yet: one per hook callback thread, like the hook's own hold state, so
    /// the hot path takes no lock.
    static PARTIAL_DETENT: Cell<f64> = const { Cell::new(0.0) };
    /// Native thumb-wheel distance short of a step, in the event's own unit.
    static PARTIAL_THUMB_STEP: Cell<f64> = const { Cell::new(0.0) };
    /// The display the pointer was on at the last move this thread saw.
    static LAST_DISPLAY: Cell<Option<u32>> = const { Cell::new(None) };
    /// The left button's drag, per hook callback thread.
    static DRAG: std::cell::RefCell<DragTracker> = std::cell::RefCell::new(DragTracker::default());
}

/// Shared between the hook callback, the gesture dispatcher, the
/// orchestrator, and the haptic player. Cheap to clone.
#[derive(Clone)]
pub struct EventHaptics {
    inner: Arc<Inner>,
}

struct Inner {
    /// The target, as the hook callback reads it.
    target: RwLock<Option<EventHapticsTarget>>,
    /// The target's route, for the player's arming lifecycle.
    hold: watch::Sender<EngineHold>,
    queue: Mutex<CueQueue>,
    /// Diverted-wheel distance, in hi-res units, short of a whole detent.
    partial_units: Mutex<u32>,
    /// Diverted thumb-wheel increments short of a whole step.
    partial_thumb_increments: Mutex<u32>,
    wake: Notify,
    /// When a Command-key press last armed the copy watch.
    copy_armed: Mutex<Option<Instant>>,
    copy_wake: Notify,
}

impl Default for EventHaptics {
    fn default() -> Self {
        Self {
            inner: Arc::new(Inner {
                target: RwLock::new(None),
                hold: watch::Sender::new(EngineHold::default()),
                queue: Mutex::new(CueQueue::default()),
                partial_units: Mutex::new(0),
                partial_thumb_increments: Mutex::new(0),
                wake: Notify::new(),
                copy_armed: Mutex::new(None),
                copy_wake: Notify::new(),
            }),
        }
    }
}

impl EventHaptics {
    /// Publish the device event haptics play on, or `None` to stop them. Only
    /// the orchestrator calls this.
    pub fn publish(&self, target: Option<EventHapticsTarget>) {
        let route = target.as_ref().map(|target| target.route.clone());
        let tracks_display = target
            .as_ref()
            .is_some_and(|target| target.config.display_crossing.plays());
        {
            let mut current = self
                .inner
                .target
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            if *current == target {
                return;
            }
            let retargeted = current.as_ref().map(|target| &target.route) != route.as_ref();
            *current = target;
            // A cue belongs to the device it was felt on: one left over from
            // the previous target must not play on the next one.
            if retargeted {
                self.queue().clear();
            }
        }
        // The hook notes the pointer's display only while a crossing plays:
        // it costs a lookup on every pointer move.
        openlogi_hook::track_pointer_display(tracks_display);
        self.inner.hold.send_if_modified(|hold| {
            if hold.route == route {
                return false;
            }
            hold.route = route;
            hold.generation += 1;
            true
        });
    }

    /// Ask the player to arm the engine on `route` again, if event haptics
    /// play there: the device came back from a power transition — a system
    /// wake, its own reconnect broadcast — that may have turned the engine
    /// off while its route stayed the same. Only the orchestrator calls this,
    /// as it re-applies the device's other volatile settings.
    pub fn rearm(&self, route: &DeviceRoute) {
        self.inner.hold.send_if_modified(|hold| {
            if hold.route.as_ref() != Some(route) {
                return false;
            }
            hold.generation += 1;
            true
        });
    }

    /// Record the cue an OS-hook mouse event makes, if any. Runs on the hook
    /// callback thread, so it never blocks: a contended lock drops the cue.
    ///
    /// `source_admitted` is the hook's own policy for which physical sources
    /// it acts on, so a trackpad or another vendor's mouse never plays.
    pub fn observe(
        &self,
        event: &MouseEvent,
        source_admitted: impl Fn(Option<&EventDevice>) -> bool,
    ) {
        let Ok(published) = self.inner.target.try_read() else {
            return;
        };
        let Some(target) = published.as_ref() else {
            return;
        };
        let config = &target.config;
        let from_target = |device: &Option<EventDevice>| {
            source_admitted(device.as_ref()) && is_target(&target.route, device.as_ref())
        };
        // A press and the drop it ends can both play: at most two cues.
        let mut cues = [None, None];
        match event {
            MouseEvent::Button {
                id,
                pressed,
                device,
            } => {
                let ours = from_target(device);
                if *pressed && ours && config.button(*id).plays() {
                    cues[0] = Some(EventCue::Button(*id));
                }
                if *id == ButtonId::LeftClick && config.drag.plays() {
                    cues[1] = DRAG.with_borrow_mut(|drag| drag.button(*pressed, ours));
                }
            }
            MouseEvent::Moved { delta_x, delta_y } => {
                if config.drag.plays() {
                    cues[0] = DRAG.with_borrow_mut(|drag| drag.moved(*delta_x, *delta_y));
                }
                if config.display_crossing.plays() {
                    cues[1] = crossed_display(openlogi_hook::pointer_display())
                        .then_some(EventCue::DisplayCrossing);
                }
            }
            MouseEvent::CaptureInterrupted => DRAG.with_borrow_mut(DragTracker::cancel),
            MouseEvent::Scroll {
                delta,
                from_trackpad: false,
                device,
            } if from_target(device) => {
                if config.wheel.plays() && native_detent(*delta) {
                    cues[0] = Some(EventCue::WheelDetent);
                }
                if config.thumbwheel.plays() && native_thumbwheel_step(*delta) {
                    cues[1] = Some(EventCue::ThumbwheelStep);
                }
            }
            MouseEvent::Scroll { .. } => {}
        }
        drop(published);
        for cue in cues.into_iter().flatten() {
            self.cue(cue);
        }
    }

    /// Note a key event from the OS hook: a Command-key press may be a copy
    /// or cut, which the clipboard watch confirms ([`Self::copy_armed`]).
    /// Runs on the hook callback thread, so it never blocks.
    pub fn observe_key(&self, event: &KeyEvent) {
        if !(event.pressed && event.modifiers.command) {
            return;
        }
        let copy_plays = self.inner.target.try_read().is_ok_and(|target| {
            target
                .as_ref()
                .is_some_and(|target| target.config.copy.plays())
        });
        if copy_plays && let Ok(mut armed) = self.inner.copy_armed.try_lock() {
            *armed = Some(Instant::now());
            drop(armed);
            self.inner.copy_wake.notify_one();
        }
    }

    /// Resolve once a Command-key press armed the copy watch.
    pub async fn copy_keyed(&self) {
        self.inner.copy_wake.notified().await;
    }

    /// Whether a clipboard change at `now` is a copy the user made: it comes
    /// within [`COPY_WINDOW`] of a Command-key press.
    #[must_use]
    pub fn copy_armed(&self, now: Instant) -> bool {
        self.inner
            .copy_armed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some_and(|at| now.saturating_duration_since(at) < COPY_WINDOW)
    }

    /// Record a press of `button` diverted over HID++ from the device with
    /// config key `device_key`.
    pub fn observe_button(&self, device_key: &str, button: ButtonId) {
        self.observe_on(device_key, EventCue::Button(button));
    }

    /// Record `cue` for something that happened on the device with config
    /// key `device_key`: it plays only when that device is the target.
    pub fn observe_on(&self, device_key: &str, cue: EventCue) {
        if self.plays_on(device_key, |config| config.feedback(cue.input())) {
            self.cue(cue);
        }
    }

    /// Record `cue` for something that happened on the host rather than on a
    /// device: a volume step, a copy. It plays on the target, if any.
    pub fn observe_host(&self, cue: EventCue) {
        if self.plays_host(cue.input()) {
            self.cue(cue);
        }
    }

    /// Record `cue` for the device `device_key` names, or for the host when
    /// no device is named ([`Self::observe_on`], [`Self::observe_host`]).
    pub fn observe_for(&self, device_key: Option<&str>, cue: EventCue) {
        match device_key {
            Some(key) => self.observe_on(key, cue),
            None => self.observe_host(cue),
        }
    }

    /// Whether `input` plays something on the target, if any: lets a caller
    /// skip the work of finding out which cue an event makes.
    #[must_use]
    pub fn plays_host(&self, input: EventHapticInput) -> bool {
        self.inner.target.try_read().is_ok_and(|target| {
            target
                .as_ref()
                .is_some_and(|target| target.config.feedback(input).plays())
        })
    }

    /// Record the detents a diverted main wheel's report completes. `delta`
    /// is in hi-res units, `per_notch` of them to a detent.
    pub fn observe_main_wheel(&self, device_key: &str, delta: i16, per_notch: NonZeroU8) {
        if !self.plays_on(device_key, |config| config.wheel) {
            return;
        }
        let completed = {
            let Ok(mut partial) = self.inner.partial_units.try_lock() else {
                return;
            };
            let total = *partial + u32::from(delta.unsigned_abs());
            let per_notch = u32::from(per_notch.get());
            *partial = total % per_notch;
            total >= per_notch
        };
        if completed {
            self.cue(EventCue::WheelDetent);
        }
    }

    /// Record a diverted thumb wheel's rotation on the device `device_key`:
    /// a step each time `per_step` increments add up.
    pub fn observe_thumbwheel(&self, device_key: &str, increments: i16, per_step: NonZeroU16) {
        if !self.plays_on(device_key, |config| config.thumbwheel) {
            return;
        }
        let completed = {
            let Ok(mut partial) = self.inner.partial_thumb_increments.try_lock() else {
                return;
            };
            let total = *partial + u32::from(increments.unsigned_abs());
            let per_step = u32::from(per_step.get());
            *partial = total % per_step;
            total >= per_step
        };
        if completed {
            self.cue(EventCue::ThumbwheelStep);
        }
    }

    /// Whether the target is the device with config key `device_key` and
    /// `feedback` picks something to play there.
    fn plays_on(
        &self,
        device_key: &str,
        feedback: impl FnOnce(&EventHapticsConfig) -> EventHapticFeedback,
    ) -> bool {
        self.inner.target.try_read().is_ok_and(|target| {
            target.as_ref().is_some_and(|target| {
                target.config_key == device_key && feedback(&target.config).plays()
            })
        })
    }

    fn cue(&self, cue: EventCue) {
        if let Ok(mut queue) = self.inner.queue.try_lock() {
            queue.push(cue, Instant::now());
        } else {
            return;
        }
        self.inner.wake.notify_one();
    }

    /// The device to hold the engine on for, for the player to arm it there
    /// and hand it back.
    #[must_use]
    pub fn holds(&self) -> watch::Receiver<EngineHold> {
        self.inner.hold.subscribe()
    }

    /// Resolve once a cue was recorded since the last call.
    pub async fn cued(&self) {
        self.inner.wake.notified().await;
    }

    /// What the player plays next at `now`. A play is taken off the queue.
    #[must_use]
    pub fn next_play(&self, now: Instant) -> NextPlay {
        let due = self.queue().next(now);
        match due {
            Due::Idle => NextPlay::Idle,
            Due::At(at) => NextPlay::At(at),
            Due::Now(cue) => {
                let published = self
                    .inner
                    .target
                    .read()
                    .unwrap_or_else(PoisonError::into_inner);
                let Some(target) = published.as_ref() else {
                    return NextPlay::Idle;
                };
                let feedback = target.config.feedback(cue.input());
                // The setting may have been turned off since the cue.
                let Some(waveform) = hid_waveform(feedback) else {
                    return NextPlay::Idle;
                };
                NextPlay::Play(EventPlay {
                    route: target.route.clone(),
                    waveform,
                    cue,
                })
            }
        }
    }

    fn queue(&self) -> std::sync::MutexGuard<'_, CueQueue> {
        self.inner
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// Whether native wheel movement `delta` completes a detent.
///
/// Fractional wheel ticks add up to whole detents. Pixel deltas have no
/// detent scale: macOS reports a continuous wheel in accelerated pixels, so
/// one detent may be a few pixels or a hundred. Each such event is a detent
/// candidate instead, and [`WHEEL_INTERVAL`] paces them.
fn native_detent(delta: ScrollDelta) -> bool {
    match delta {
        ScrollDelta::WheelTicks { y, .. } => PARTIAL_DETENT.with(|partial| {
            let total = partial.get() + y.abs();
            partial.set(total.fract());
            total >= 1.0
        }),
        ScrollDelta::Pixels { y, .. } => y != 0.0,
    }
}

/// Whether the pointer, now on `display`, moved there from another display.
/// The first display seen is where it starts, not a crossing.
fn crossed_display(display: Option<u32>) -> bool {
    let Some(display) = display else {
        return false;
    };
    LAST_DISPLAY
        .replace(Some(display))
        .is_some_and(|last| last != display)
}

/// Whether native horizontal movement `delta` completes a thumb-wheel step:
/// one wheel tick, or [`THUMBWHEEL_STEP_PIXELS`] of pixel distance. Vertical
/// movement is the main wheel's.
fn native_thumbwheel_step(delta: ScrollDelta) -> bool {
    let (x, per_step) = match delta {
        ScrollDelta::WheelTicks { x, .. } => (x, 1.0),
        ScrollDelta::Pixels { x, .. } => (x, THUMBWHEEL_STEP_PIXELS),
    };
    if x == 0.0 {
        return false;
    }
    PARTIAL_THUMB_STEP.with(|partial| {
        let total = partial.get() + x.abs();
        partial.set(total % per_step);
        total >= per_step
    })
}

/// Whether an event from `device` comes from the device on `route`.
///
/// Only a direct route can be told apart: the hook sees a receiver, not the
/// device behind it. An event without a product ID cannot be ruled out.
fn is_target(route: &DeviceRoute, device: Option<&EventDevice>) -> bool {
    let DeviceRoute::Direct { product_id, .. } = route else {
        return true;
    };
    device
        .and_then(|device| device.product_id)
        .is_none_or(|seen| seen == u32::from(*product_id))
}

/// The HID++ waveform a configured feedback plays, `None` for off.
#[must_use]
pub fn hid_waveform(feedback: EventHapticFeedback) -> Option<HapticWaveform> {
    match feedback {
        EventHapticFeedback::Off => None,
        EventHapticFeedback::WhisperCollision => Some(HapticWaveform::WhisperCollision),
        EventHapticFeedback::SubtleCollision => Some(HapticWaveform::SubtleCollision),
        EventHapticFeedback::DampCollision => Some(HapticWaveform::DampCollision),
        EventHapticFeedback::SharpCollision => Some(HapticWaveform::SharpCollision),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: EventCue = EventCue::Button(ButtonId::LeftClick);

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn an_empty_queue_is_idle() {
        let mut queue = CueQueue::default();
        assert_eq!(queue.next(Instant::now()), Due::Idle);
    }

    #[test]
    fn a_press_plays_before_a_waiting_detent() {
        let start = Instant::now();
        let mut queue = CueQueue::default();
        queue.push(EventCue::WheelDetent, start);
        queue.push(LEFT, at(start, 1));
        assert_eq!(queue.next(at(start, 2)), Due::Now(LEFT));
        assert_eq!(queue.next(at(start, 3)), Due::Now(EventCue::WheelDetent));
        assert_eq!(queue.next(at(start, 4)), Due::Idle);
    }

    #[test]
    fn the_newest_press_wins() {
        let start = Instant::now();
        let mut queue = CueQueue::default();
        queue.push(LEFT, start);
        queue.push(EventCue::Button(ButtonId::Back), at(start, 1));
        assert_eq!(
            queue.next(at(start, 2)),
            Due::Now(EventCue::Button(ButtonId::Back))
        );
        assert_eq!(queue.next(at(start, 3)), Due::Idle);
    }

    #[test]
    fn detents_are_paced_by_the_wheel_interval() {
        let start = Instant::now();
        let mut queue = CueQueue::default();
        queue.push(EventCue::WheelDetent, start);
        assert_eq!(queue.next(start), Due::Now(EventCue::WheelDetent));
        queue.push(EventCue::WheelDetent, at(start, 10));
        assert_eq!(queue.next(at(start, 10)), Due::At(start + WHEEL_INTERVAL));
        assert_eq!(
            queue.next(start + WHEEL_INTERVAL),
            Due::Now(EventCue::WheelDetent)
        );
    }

    #[test]
    fn a_press_is_never_held_back_by_the_wheel_interval() {
        let start = Instant::now();
        let mut queue = CueQueue::default();
        queue.push(EventCue::WheelDetent, start);
        assert_eq!(queue.next(start), Due::Now(EventCue::WheelDetent));
        queue.push(LEFT, at(start, 5));
        assert_eq!(queue.next(at(start, 5)), Due::Now(LEFT));
    }

    #[test]
    fn detents_in_a_burst_collapse_into_one() {
        let start = Instant::now();
        let mut queue = CueQueue::default();
        for ms in 0..5 {
            queue.push(EventCue::WheelDetent, at(start, ms));
        }
        assert_eq!(queue.next(at(start, 5)), Due::Now(EventCue::WheelDetent));
        assert_eq!(queue.next(at(start, 6)), Due::Idle);
    }

    #[test]
    fn a_mode_change_plays_its_count_spaced_by_the_repeat_gap() {
        let start = Instant::now();
        let mut queue = CueQueue::default();
        let cue = EventCue::ModeChange {
            count: NonZeroU8::new(3).expect("non-zero"),
        };
        queue.push(cue, start);
        assert_eq!(queue.next(start), Due::Now(cue));
        assert_eq!(queue.next(at(start, 1)), Due::At(start + REPEAT_GAP));
        assert_eq!(queue.next(start + REPEAT_GAP), Due::Now(cue));
        let third = start + REPEAT_GAP + REPEAT_GAP;
        assert_eq!(queue.next(third), Due::Now(cue));
        assert_eq!(queue.next(third), Due::Idle);
    }

    #[test]
    fn a_press_cuts_into_a_repeated_signal_without_ending_it() {
        let start = Instant::now();
        let mut queue = CueQueue::default();
        let cue = EventCue::ModeChange {
            count: NonZeroU8::new(2).expect("non-zero"),
        };
        queue.push(cue, start);
        assert_eq!(queue.next(start), Due::Now(cue));
        queue.push(LEFT, at(start, 10));
        assert_eq!(queue.next(at(start, 10)), Due::Now(LEFT));
        assert_eq!(queue.next(start + REPEAT_GAP), Due::Now(cue));
    }

    #[test]
    fn a_newer_signal_replaces_one_still_repeating() {
        let start = Instant::now();
        let mut queue = CueQueue::default();
        queue.push(
            EventCue::ModeChange {
                count: NonZeroU8::new(3).expect("non-zero"),
            },
            start,
        );
        let _ = queue.next(start);
        queue.push(EventCue::Copy, at(start, 5));
        assert_eq!(queue.next(at(start, 5)), Due::Now(EventCue::Copy));
        assert_eq!(queue.next(at(start, 6)), Due::Idle);
    }

    #[test]
    fn the_wheels_are_paced_apart_and_a_signal_goes_before_them() {
        let start = Instant::now();
        let mut queue = CueQueue::default();
        queue.push(EventCue::ThumbwheelStep, start);
        assert_eq!(queue.next(start), Due::Now(EventCue::ThumbwheelStep));
        queue.push(EventCue::ThumbwheelStep, at(start, 10));
        queue.push(EventCue::WheelDetent, at(start, 10));
        queue.push(EventCue::Gesture, at(start, 10));
        assert_eq!(queue.next(at(start, 10)), Due::Now(EventCue::Gesture));
        assert_eq!(queue.next(at(start, 11)), Due::Now(EventCue::WheelDetent));
        assert_eq!(
            queue.next(at(start, 12)),
            Due::At(start + THUMBWHEEL_INTERVAL)
        );
        assert_eq!(
            queue.next(start + THUMBWHEEL_INTERVAL),
            Due::Now(EventCue::ThumbwheelStep)
        );
    }

    #[test]
    fn a_wheel_mode_switch_reported_twice_plays_once() {
        let start = Instant::now();
        let mut queue = CueQueue::default();
        queue.push(EventCue::WheelMode, start);
        assert_eq!(queue.next(start), Due::Now(EventCue::WheelMode));
        queue.push(EventCue::WheelMode, at(start, 30));
        assert_eq!(queue.next(at(start, 30)), Due::Idle, "the echo");
        queue.push(
            EventCue::WheelMode,
            start + WHEEL_MODE_ECHO + WHEEL_MODE_ECHO,
        );
        assert_eq!(
            queue.next(start + WHEEL_MODE_ECHO + WHEEL_MODE_ECHO),
            Due::Now(EventCue::WheelMode)
        );
    }

    #[test]
    fn a_stale_cue_is_dropped() {
        let start = Instant::now();
        let mut queue = CueQueue::default();
        queue.push(LEFT, start);
        queue.push(EventCue::WheelDetent, start);
        assert_eq!(queue.next(start + STALE_AFTER), Due::Idle);
    }

    #[test]
    fn clearing_drops_waiting_cues_but_keeps_the_pace() {
        let start = Instant::now();
        let mut queue = CueQueue::default();
        queue.push(EventCue::WheelDetent, start);
        assert_eq!(queue.next(start), Due::Now(EventCue::WheelDetent));
        queue.push(LEFT, at(start, 1));
        queue.push(EventCue::WheelDetent, at(start, 1));
        queue.clear();
        assert_eq!(queue.next(at(start, 2)), Due::Idle);
        queue.push(EventCue::WheelDetent, at(start, 3));
        assert_eq!(queue.next(at(start, 3)), Due::At(start + WHEEL_INTERVAL));
    }

    fn target(route: DeviceRoute) -> EventHapticsTarget {
        EventHapticsTarget {
            config_key: "mouse".to_owned(),
            route,
            config: EventHapticsConfig {
                enabled: true,
                ..EventHapticsConfig::default()
            },
        }
    }

    fn direct(product_id: u16) -> DeviceRoute {
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id,
        }
    }

    fn logitech(product_id: Option<u32>) -> EventDevice {
        EventDevice {
            vendor_id: Some(0x046d),
            product_id,
            product_name: None,
        }
    }

    fn press(id: ButtonId, device: Option<EventDevice>) -> MouseEvent {
        MouseEvent::Button {
            id,
            pressed: true,
            device,
        }
    }

    fn played(haptics: &EventHaptics) -> Option<(HapticWaveform, EventCue)> {
        match haptics.next_play(Instant::now()) {
            NextPlay::Play(play) => Some((play.waveform, play.cue)),
            NextPlay::At(_) | NextPlay::Idle => None,
        }
    }

    #[test]
    fn nothing_is_cued_without_a_target() {
        let haptics = EventHaptics::default();
        haptics.observe(&press(ButtonId::LeftClick, None), |_| true);
        assert_eq!(played(&haptics), None);
    }

    #[test]
    fn primary_presses_play_by_default() {
        let haptics = EventHaptics::default();
        haptics.publish(Some(target(direct(0xb042))));
        for id in [ButtonId::LeftClick, ButtonId::RightClick] {
            haptics.observe(&press(id, Some(logitech(Some(0xb042)))), |_| true);
            assert_eq!(
                played(&haptics),
                Some((HapticWaveform::WhisperCollision, EventCue::Button(id))),
                "{id:?}"
            );
        }
    }

    #[test]
    fn each_button_plays_what_it_is_set_to() {
        let haptics = EventHaptics::default();
        let mut target = target(direct(0xb042));
        target
            .config
            .buttons
            .insert(ButtonId::Back, EventHapticFeedback::SharpCollision);
        target
            .config
            .buttons
            .insert(ButtonId::LeftClick, EventHapticFeedback::Off);
        haptics.publish(Some(target));
        haptics.observe(&press(ButtonId::LeftClick, None), |_| true);
        assert_eq!(played(&haptics), None, "turned off");
        haptics.observe(&press(ButtonId::Back, None), |_| true);
        assert_eq!(
            played(&haptics),
            Some((
                HapticWaveform::SharpCollision,
                EventCue::Button(ButtonId::Back)
            ))
        );
    }

    #[test]
    fn a_diverted_press_plays_only_for_its_own_device() {
        let haptics = EventHaptics::default();
        let mut target = target(direct(0xb042));
        target.config.buttons.insert(
            ButtonId::GestureButton,
            EventHapticFeedback::SubtleCollision,
        );
        haptics.publish(Some(target));
        haptics.observe_button("another", ButtonId::GestureButton);
        assert_eq!(played(&haptics), None);
        haptics.observe_button("mouse", ButtonId::MiddleClick);
        assert_eq!(played(&haptics), None, "not set");
        haptics.observe_button("mouse", ButtonId::GestureButton);
        assert_eq!(
            played(&haptics),
            Some((
                HapticWaveform::SubtleCollision,
                EventCue::Button(ButtonId::GestureButton)
            ))
        );
    }

    #[test]
    fn releases_unset_buttons_and_refused_sources_play_nothing() {
        let haptics = EventHaptics::default();
        haptics.publish(Some(target(direct(0xb042))));
        let release = MouseEvent::Button {
            id: ButtonId::LeftClick,
            pressed: false,
            device: None,
        };
        haptics.observe(&release, |_| true);
        haptics.observe(&press(ButtonId::MiddleClick, None), |_| true);
        haptics.observe(&press(ButtonId::LeftClick, None), |_| false);
        assert_eq!(played(&haptics), None);
    }

    #[test]
    fn another_direct_device_plays_nothing() {
        let haptics = EventHaptics::default();
        haptics.publish(Some(target(direct(0xb042))));
        haptics.observe(
            &press(ButtonId::LeftClick, Some(logitech(Some(0xb034)))),
            |_| true,
        );
        assert_eq!(played(&haptics), None);
        haptics.observe(&press(ButtonId::LeftClick, Some(logitech(None))), |_| true);
        assert_eq!(
            played(&haptics),
            Some((HapticWaveform::WhisperCollision, LEFT)),
            "an event without a product ID cannot be ruled out"
        );
    }

    #[test]
    fn a_cue_does_not_follow_the_target_to_another_mouse() {
        let haptics = EventHaptics::default();
        haptics.publish(Some(target(direct(0xb042))));
        haptics.observe(&press(ButtonId::LeftClick, None), |_| true);
        haptics.publish(Some(target(direct(0xb034))));
        assert_eq!(played(&haptics), None);

        let mut retuned = target(direct(0xb034));
        haptics.observe(&press(ButtonId::LeftClick, None), |_| true);
        retuned.config.wheel = EventHapticFeedback::Off;
        haptics.publish(Some(retuned));
        assert_eq!(
            played(&haptics),
            Some((HapticWaveform::WhisperCollision, LEFT)),
            "new settings on the same mouse keep its cues"
        );
    }

    #[test]
    fn fractional_wheel_ticks_add_up_to_detents() {
        let haptics = EventHaptics::default();
        haptics.publish(Some(target(direct(0xb042))));
        let scroll = |y| MouseEvent::Scroll {
            delta: ScrollDelta::wheel_ticks(0.0, y),
            from_trackpad: false,
            device: None,
        };
        haptics.observe(&scroll(0.5), |_| true);
        assert_eq!(played(&haptics), None);
        haptics.observe(&scroll(-0.5), |_| true);
        assert_eq!(
            played(&haptics),
            Some((HapticWaveform::DampCollision, EventCue::WheelDetent))
        );
    }

    fn moved(dx: i32, dy: i32) -> MouseEvent {
        MouseEvent::Moved {
            delta_x: dx,
            delta_y: dy,
        }
    }

    fn release(id: ButtonId) -> MouseEvent {
        MouseEvent::Button {
            id,
            pressed: false,
            device: None,
        }
    }

    #[test]
    fn a_drag_plays_at_the_pick_up_and_at_the_drop() {
        let haptics = EventHaptics::default();
        let mut target = target(direct(0xb042));
        target.config.drag = EventHapticFeedback::DampCollision;
        target.config.buttons.clear();
        haptics.publish(Some(target));
        haptics.observe(&press(ButtonId::LeftClick, None), |_| true);
        haptics.observe(&moved(4, 3), |_| true);
        assert_eq!(played(&haptics), None, "still a click");
        haptics.observe(&moved(2, -2), |_| true);
        assert_eq!(
            played(&haptics),
            Some((HapticWaveform::DampCollision, EventCue::Drag))
        );
        haptics.observe(&moved(50, 0), |_| true);
        assert_eq!(played(&haptics), None, "one pick-up per drag");
        haptics.observe(&release(ButtonId::LeftClick), |_| true);
        assert_eq!(
            played(&haptics),
            Some((HapticWaveform::DampCollision, EventCue::Drag))
        );
    }

    #[test]
    fn a_click_or_another_pointers_press_is_no_drag() {
        let haptics = EventHaptics::default();
        let mut target = target(direct(0xb042));
        target.config.drag = EventHapticFeedback::DampCollision;
        target.config.buttons.clear();
        haptics.publish(Some(target));
        haptics.observe(&press(ButtonId::LeftClick, None), |_| true);
        haptics.observe(&release(ButtonId::LeftClick), |_| true);
        assert_eq!(played(&haptics), None);
        haptics.observe(&press(ButtonId::LeftClick, None), |_| false);
        haptics.observe(&moved(40, 40), |_| true);
        haptics.observe(&release(ButtonId::LeftClick), |_| true);
        assert_eq!(played(&haptics), None);
    }

    #[test]
    fn moving_onto_another_display_crosses_but_the_first_display_does_not() {
        assert!(!crossed_display(None));
        assert!(!crossed_display(Some(1)), "where the pointer starts");
        assert!(!crossed_display(Some(1)));
        assert!(crossed_display(Some(2)));
        assert!(!crossed_display(None), "an unknown display changes nothing");
        assert!(crossed_display(Some(1)));
    }

    #[test]
    fn native_horizontal_scrolling_steps_the_thumb_wheel() {
        let haptics = EventHaptics::default();
        let mut target = target(direct(0xb042));
        target.config.wheel = EventHapticFeedback::Off;
        haptics.publish(Some(target));
        let scroll = |x| MouseEvent::Scroll {
            delta: ScrollDelta::pixels(x, 0.0),
            from_trackpad: false,
            device: None,
        };
        haptics.observe(&scroll(8.0), |_| true);
        assert_eq!(played(&haptics), None);
        haptics.observe(&scroll(-5.0), |_| true);
        assert_eq!(
            played(&haptics),
            Some((HapticWaveform::SubtleCollision, EventCue::ThumbwheelStep))
        );
    }

    #[test]
    fn trackpad_scrolling_plays_nothing() {
        let haptics = EventHaptics::default();
        haptics.publish(Some(target(direct(0xb042))));
        haptics.observe(
            &MouseEvent::Scroll {
                delta: ScrollDelta::pixels(0.0, 12.0),
                from_trackpad: true,
                device: None,
            },
            |_| true,
        );
        assert_eq!(played(&haptics), None);
    }

    #[test]
    fn a_diverted_wheel_plays_per_completed_detent_of_its_own_device() {
        let haptics = EventHaptics::default();
        haptics.publish(Some(target(direct(0xb042))));
        let per_notch = NonZeroU8::new(8).expect("non-zero");
        haptics.observe_main_wheel("another", 8, per_notch);
        assert_eq!(played(&haptics), None);
        haptics.observe_main_wheel("mouse", 5, per_notch);
        assert_eq!(played(&haptics), None);
        haptics.observe_main_wheel("mouse", -3, per_notch);
        assert_eq!(
            played(&haptics),
            Some((HapticWaveform::DampCollision, EventCue::WheelDetent))
        );
    }

    #[test]
    fn a_diverted_thumb_wheel_steps_every_few_increments() {
        let haptics = EventHaptics::default();
        haptics.publish(Some(target(direct(0xb042))));
        let per_step = NonZeroU16::new(6).expect("non-zero");
        haptics.observe_thumbwheel("mouse", 4, per_step);
        assert_eq!(played(&haptics), None);
        haptics.observe_thumbwheel("mouse", -3, per_step);
        assert_eq!(
            played(&haptics),
            Some((HapticWaveform::SubtleCollision, EventCue::ThumbwheelStep))
        );
        haptics.observe_thumbwheel("another", 12, per_step);
        assert_eq!(played(&haptics), None);
    }

    #[test]
    fn a_wheel_set_to_off_plays_nothing() {
        let haptics = EventHaptics::default();
        let mut target = target(direct(0xb042));
        target.config.wheel = EventHapticFeedback::Off;
        haptics.publish(Some(target));
        haptics.observe_main_wheel("mouse", 8, NonZeroU8::new(8).expect("non-zero"));
        haptics.observe(
            &MouseEvent::Scroll {
                delta: ScrollDelta::wheel_ticks(0.0, 1.0),
                from_trackpad: false,
                device: None,
            },
            |_| true,
        );
        assert_eq!(played(&haptics), None);
    }

    #[test]
    fn host_and_device_signals_play_only_where_they_are_set() {
        let haptics = EventHaptics::default();
        let mut target = target(direct(0xb042));
        target.config.copy = EventHapticFeedback::SubtleCollision;
        haptics.publish(Some(target));
        haptics.observe_host(EventCue::DisplayCrossing);
        assert_eq!(played(&haptics), None, "off by default");
        haptics.observe_host(EventCue::Copy);
        assert_eq!(
            played(&haptics),
            Some((HapticWaveform::SubtleCollision, EventCue::Copy))
        );
        haptics.observe_on("another", EventCue::Gesture);
        assert_eq!(played(&haptics), None);
        haptics.observe_on("mouse", EventCue::Gesture);
        assert_eq!(
            played(&haptics),
            Some((HapticWaveform::SubtleCollision, EventCue::Gesture))
        );
    }

    #[test]
    fn a_command_key_press_arms_the_copy_watch_only_while_copy_plays() {
        let haptics = EventHaptics::default();
        let command_c = KeyEvent {
            keycode: 8,
            pressed: true,
            modifiers: openlogi_hook::KeyModifiers {
                command: true,
                ..openlogi_hook::KeyModifiers::default()
            },
        };
        let mut target = target(direct(0xb042));
        haptics.publish(Some(target.clone()));
        haptics.observe_key(&command_c);
        assert!(
            !haptics.copy_armed(Instant::now()),
            "copy is off by default"
        );
        target.config.copy = EventHapticFeedback::WhisperCollision;
        haptics.publish(Some(target));
        haptics.observe_key(&KeyEvent {
            modifiers: openlogi_hook::KeyModifiers::default(),
            ..command_c
        });
        assert!(!haptics.copy_armed(Instant::now()), "no Command key");
        haptics.observe_key(&command_c);
        assert!(haptics.copy_armed(Instant::now()));
        assert!(!haptics.copy_armed(Instant::now() + COPY_WINDOW));
    }

    #[test]
    fn withdrawing_the_target_drops_queued_cues_and_its_route() {
        let haptics = EventHaptics::default();
        let holds = haptics.holds();
        haptics.publish(Some(target(direct(0xb042))));
        assert_eq!(holds.borrow().route, Some(direct(0xb042)));
        haptics.observe(&press(ButtonId::LeftClick, None), |_| true);
        haptics.publish(None);
        assert_eq!(played(&haptics), None);
        assert_eq!(holds.borrow().route, None);
    }

    #[test]
    fn every_retarget_and_rearm_asks_for_a_fresh_arming() {
        let haptics = EventHaptics::default();
        let mut holds = haptics.holds();
        let mut generation = || {
            assert!(holds.has_changed().expect("sender alive"), "no new hold");
            holds.borrow_and_update().generation
        };
        haptics.publish(Some(target(direct(0xb042))));
        let first = generation();
        haptics.rearm(&direct(0xb034));
        haptics.rearm(&direct(0xb042));
        let rearmed = generation();
        assert!(rearmed > first);
        haptics.publish(None);
        haptics.publish(Some(target(direct(0xb042))));
        assert!(generation() > rearmed, "back on the same mouse");
        let mut retuned = target(direct(0xb042));
        retuned.config.wheel = EventHapticFeedback::Off;
        haptics.publish(Some(retuned));
        assert!(
            !holds.has_changed().expect("sender alive"),
            "same mouse, new settings"
        );
    }

    #[test]
    fn every_configured_feedback_maps_to_its_hidpp_id() {
        let ids: Vec<Option<u8>> = EventHapticFeedback::ALL
            .into_iter()
            .map(|feedback| hid_waveform(feedback).map(u8::from))
            .collect();
        assert_eq!(ids, [None, Some(27), Some(4), Some(3), Some(2)]);
    }
}
