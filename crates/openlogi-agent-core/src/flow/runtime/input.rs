//! Flow state shared with the OS input hook.
//!
//! The hook callback runs on a freeze-sensitive thread, so everything it
//! touches here is a lock-free atomic; anything slower is handed to an async
//! task through a [`Notify`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

/// Long enough to cover the receiver lease and the switch writes.
const POINTER_HOLD: Duration = Duration::from_millis(1500);
/// How long after a handoff placed the pointer here its first movement is
/// checked for a burst.
const ARRIVAL_WINDOW: Duration = Duration::from_secs(2);
/// A single movement at least this long right after an arrival is not a hand
/// moving: it is the motion the mouse accumulated while it reconnected,
/// released in one report and amplified by pointer acceleration.
const BURST_PX: f64 = 150.0;
/// How long after the first movement of an arrival reports still count as the
/// burst. The accumulated motion comes in the first few reports; past this, a
/// long report is a hand moving fast and must pass, or the pointer lags it.
const BURST_SPAN: Duration = Duration::from_millis(40);
/// How long key presses are swallowed while a keyboard that stayed behind is
/// sent on, so they land on neither machine rather than on the wrong one.
const KEY_SUPPRESS: Duration = Duration::from_millis(400);
/// How many times one handoff may send a lingering keyboard on.
const KEYBOARD_FOLLOW_ATTEMPTS: u64 = 3;
/// Minimum spacing between two of those attempts.
const KEYBOARD_FOLLOW_SPACING: Duration = Duration::from_millis(600);
/// How long a burst is collected before it glides: a reconnecting mouse can
/// release what it accumulated over a few consecutive reports.
const BURST_COLLECT: Duration = Duration::from_millis(16);
/// The longest glide. The rest of a large burst is dropped, not flung.
const GLIDE_MAX_PX: f64 = 320.0;
/// Glide frames, and the time between them: about 120 ms in all.
const GLIDE_STEPS: u32 = 15;
const GLIDE_FRAME: Duration = Duration::from_millis(8);

/// Glide each arrival burst in, decelerating, instead of letting it jump.
pub(super) async fn run_arrival_glide(input: Arc<FlowInput>) {
    loop {
        input.burst.notified().await;
        tokio::time::sleep(BURST_COLLECT).await;
        let (delta_x, delta_y) = input.take_burst();
        #[expect(
            clippy::cast_precision_loss,
            reason = "a burst is a few thousand pixels, exactly representable in f64"
        )]
        glide(delta_x as f64, delta_y as f64).await;
    }
}

async fn glide(delta_x: f64, delta_y: f64) {
    let length = delta_x.hypot(delta_y);
    if length < 1.0 {
        return;
    }
    let scale = (GLIDE_MAX_PX / length).min(1.0);
    let (target_x, target_y) = (delta_x * scale, delta_y * scale);
    let (mut done_x, mut done_y) = (0.0, 0.0);
    for frame in 1..=GLIDE_STEPS {
        // Ease-out: fast first, as the hand was moving, then settling.
        let progress = 1.0 - (1.0 - f64::from(frame) / f64::from(GLIDE_STEPS)).powi(3);
        let (next_x, next_y) = (target_x * progress, target_y * progress);
        // Relative to wherever the pointer is now, so real motion during the
        // glide adds to it instead of being overwritten.
        let Some(position) = openlogi_hook::edge_cursor_position() else {
            return;
        };
        let (x, y) = (position.x + next_x - done_x, position.y + next_y - done_y);
        (done_x, done_y) = (next_x, next_y);
        if openlogi_inject::warp_cursor(x, y)
            && let Some(actual) = openlogi_hook::edge_cursor_position()
        {
            openlogi_hook::reseed_motion_baseline(actual);
        }
        tokio::time::sleep(GLIDE_FRAME).await;
    }
}

/// What the hook and the Flow runtime share about local input.
pub(crate) struct FlowInput {
    epoch: Instant,
    /// Milliseconds after `epoch` until which pointer movement is swallowed,
    /// keeping the pointer on the edge it crossed while the devices leave.
    /// Always expires, so a lost release cannot freeze the pointer.
    hold_until_ms: AtomicU64,
    /// A handoff placed the pointer on an armed edge from outside; the edge
    /// detector latches so that placement cannot read as a crossing.
    latch_pending: AtomicBool,
    arrival: Arrival,
    /// How many handoffs have placed the pointer here: work a departure left
    /// behind stops once this moves on, as the devices came back.
    arrivals: AtomicU64,
    keyboard: KeyboardFollow,
    /// Wakes the task that sends a lingering keyboard on.
    pub(super) keyboard_stayed: Notify,
    /// Wakes the task that glides a burst in.
    pub(super) burst: Notify,
}

/// The first input after the last arrival.
#[derive(Default)]
struct Arrival {
    /// When the pointer was placed, ms after `epoch`; 0 = never.
    at_ms: AtomicU64,
    /// The first real movement after that is still awaited.
    open: AtomicBool,
    first_move_ms: AtomicU64,
    first_key_ms: AtomicU64,
    burst_x: AtomicI64,
    burst_y: AtomicI64,
    burst_events: AtomicU64,
    burst_px: AtomicU64,
}

/// Whether keystrokes here mean the keyboard stayed behind.
#[derive(Default)]
struct KeyboardFollow {
    /// This machine sent the devices away and has seen neither local pointer
    /// movement nor an arrival since.
    handed_off: AtomicBool,
    attempts: AtomicU64,
    last_attempt_ms: AtomicU64,
    suppress_until_ms: AtomicU64,
}

/// What came in after one arrival, for the log.
pub(crate) struct ArrivalReport {
    pub(crate) first_move_ms: Option<u64>,
    pub(crate) first_key_ms: Option<u64>,
    pub(crate) burst_events: u64,
    pub(crate) burst_px: u64,
}

impl FlowInput {
    pub(crate) fn new() -> Self {
        Self {
            epoch: Instant::now(),
            hold_until_ms: AtomicU64::new(0),
            latch_pending: AtomicBool::new(false),
            arrival: Arrival::default(),
            arrivals: AtomicU64::new(0),
            keyboard: KeyboardFollow::default(),
            keyboard_stayed: Notify::new(),
            burst: Notify::new(),
        }
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn ms(duration: Duration) -> u64 {
        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
    }

    /// Hold the pointer on the edge it just crossed.
    pub(super) fn hold(&self) {
        self.hold_until_ms.store(
            self.now_ms().saturating_add(Self::ms(POINTER_HOLD)),
            Ordering::Release,
        );
    }

    /// Let the pointer move again: the handoff did not move the devices.
    pub(crate) fn release(&self) {
        self.hold_until_ms.store(0, Ordering::Release);
    }

    fn held(&self) -> bool {
        self.now_ms() < self.hold_until_ms.load(Ordering::Acquire)
    }

    /// This machine switched the devices to a peer. `keyboards_can_follow`:
    /// a keyboard configured for that peer could still be here, so a
    /// keystroke before local movement is taken as proof it stayed behind.
    pub(crate) fn departed(&self, keyboards_can_follow: bool) {
        self.arrival.open.store(false, Ordering::Release);
        self.keyboard.attempts.store(0, Ordering::Relaxed);
        self.keyboard.last_attempt_ms.store(0, Ordering::Relaxed);
        self.keyboard.suppress_until_ms.store(0, Ordering::Release);
        self.keyboard
            .handed_off
            .store(keyboards_can_follow, Ordering::Release);
    }

    /// Flow stopped: no keyboard is followed any more, so typing here must
    /// not be swallowed waiting for one.
    pub(crate) fn stopped(&self) {
        self.keyboard.handed_off.store(false, Ordering::Release);
        self.keyboard.suppress_until_ms.store(0, Ordering::Release);
    }

    /// A handoff just placed the pointer here.
    pub(crate) fn arrived(&self) {
        // Nothing of ours is leaving: a hold from this machine's own last
        // departure must not freeze the pointer that is arriving.
        self.release();
        self.arrivals.fetch_add(1, Ordering::AcqRel);
        self.keyboard.handed_off.store(false, Ordering::Release);
        self.keyboard.suppress_until_ms.store(0, Ordering::Release);
        let arrival = &self.arrival;
        arrival.first_move_ms.store(0, Ordering::Relaxed);
        arrival.first_key_ms.store(0, Ordering::Relaxed);
        arrival.burst_x.store(0, Ordering::Relaxed);
        arrival.burst_y.store(0, Ordering::Relaxed);
        arrival.burst_events.store(0, Ordering::Relaxed);
        arrival.burst_px.store(0, Ordering::Relaxed);
        arrival.at_ms.store(self.now_ms().max(1), Ordering::Release);
        arrival.open.store(true, Ordering::Release);
        self.latch_pending.store(true, Ordering::Release);
    }

    /// Whether the devices were sent away with no local movement or arrival
    /// since: a keystroke here then means the keyboard stayed behind.
    pub(super) fn keyboard_handed_off(&self) -> bool {
        self.keyboard.handed_off.load(Ordering::Acquire)
    }

    /// The arrival count; compare a later read to tell whether a handoff
    /// brought the devices back in between.
    pub(super) fn arrivals(&self) -> u64 {
        self.arrivals.load(Ordering::Acquire)
    }

    pub(super) fn take_latch(&self) -> bool {
        self.latch_pending.swap(false, Ordering::AcqRel)
    }

    /// Milliseconds since the last arrival, or `None` before any.
    fn since_arrival(&self) -> Option<u64> {
        let at = self.arrival.at_ms.load(Ordering::Acquire);
        (at != 0).then(|| self.now_ms().saturating_sub(at))
    }

    /// One real pointer movement from the hook; returns whether to swallow it.
    pub(super) fn pointer_moved(&self, delta_x: i32, delta_y: i32) -> bool {
        if self.held() {
            return true;
        }
        // Real local motion: the pointer lives here now, so keys typed here
        // are meant for here.
        self.keyboard.handed_off.store(false, Ordering::Release);
        self.smooth_arrival(delta_x, delta_y)
    }

    /// Swallow the burst a reconnecting mouse releases as its first report and
    /// hand it to the glide task instead, so the pointer slides in rather than
    /// jumping across the screen.
    fn smooth_arrival(&self, delta_x: i32, delta_y: i32) -> bool {
        if !self.arrival.open.load(Ordering::Acquire) {
            return false;
        }
        let Some(since) = self.since_arrival() else {
            return false;
        };
        if since > Self::ms(ARRIVAL_WINDOW) {
            self.arrival.open.store(false, Ordering::Release);
            return false;
        }
        let first_move = match self.arrival.first_move_ms.compare_exchange(
            0,
            since.max(1),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => since.max(1),
            Err(recorded) => recorded,
        };
        if since.saturating_sub(first_move) > Self::ms(BURST_SPAN) {
            self.arrival.open.store(false, Ordering::Release);
            return false;
        }
        let length = f64::from(delta_x).hypot(f64::from(delta_y));
        if length < BURST_PX {
            // Ordinary motion: the mouse is back and reporting normally.
            self.arrival.open.store(false, Ordering::Release);
            return false;
        }
        self.arrival
            .burst_x
            .fetch_add(i64::from(delta_x), Ordering::AcqRel);
        self.arrival
            .burst_y
            .fetch_add(i64::from(delta_y), Ordering::AcqRel);
        self.arrival.burst_events.fetch_add(1, Ordering::Relaxed);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a finite, non-negative pixel length far below u64::MAX"
        )]
        self.arrival
            .burst_px
            .fetch_add(length.round() as u64, Ordering::Relaxed);
        self.burst.notify_one();
        true
    }

    /// The burst collected since the last call, as `(dx, dy)`.
    pub(super) fn take_burst(&self) -> (i64, i64) {
        (
            self.arrival.burst_x.swap(0, Ordering::AcqRel),
            self.arrival.burst_y.swap(0, Ordering::AcqRel),
        )
    }

    /// One key press from the hook; returns whether to swallow it.
    ///
    /// Only presses: a release always passes, so a key can never be left down.
    pub(super) fn key_pressed(&self) -> bool {
        if let Some(since) = self.since_arrival() {
            let _ = self.arrival.first_key_ms.compare_exchange(
                0,
                since.max(1),
                Ordering::AcqRel,
                Ordering::Relaxed,
            );
        }
        let now = self.now_ms();
        if now < self.keyboard.suppress_until_ms.load(Ordering::Acquire) {
            return true;
        }
        if !self.keyboard.handed_off.load(Ordering::Acquire) {
            return false;
        }
        // Typed here after the devices were sent away, before any local
        // pointer movement: the keyboard did not leave with the mouse.
        let last = self.keyboard.last_attempt_ms.load(Ordering::Acquire);
        if last != 0 && now.saturating_sub(last) < Self::ms(KEYBOARD_FOLLOW_SPACING) {
            return false;
        }
        if self.keyboard.attempts.fetch_add(1, Ordering::AcqRel) >= KEYBOARD_FOLLOW_ATTEMPTS {
            return false;
        }
        self.keyboard.last_attempt_ms.store(now, Ordering::Release);
        self.keyboard.suppress_until_ms.store(
            now.saturating_add(Self::ms(KEY_SUPPRESS)),
            Ordering::Release,
        );
        self.keyboard_stayed.notify_one();
        true
    }

    /// What came in after the last arrival.
    pub(crate) fn arrival_report(&self) -> ArrivalReport {
        let read = |value: &AtomicU64| Some(value.load(Ordering::Acquire)).filter(|ms| *ms != 0);
        ArrivalReport {
            first_move_ms: read(&self.arrival.first_move_ms),
            first_key_ms: read(&self.arrival.first_key_ms),
            burst_events: self.arrival.burst_events.load(Ordering::Acquire),
            burst_px: self.arrival.burst_px.load(Ordering::Acquire),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{BURST_SPAN, FlowInput};

    #[test]
    fn past_the_burst_span_a_fast_hand_is_not_held_back() {
        let input = FlowInput::new();
        input.arrived();
        assert!(
            input.pointer_moved(-900, 0),
            "the reconnect burst glides in"
        );

        std::thread::sleep(BURST_SPAN + Duration::from_millis(20));

        assert!(
            !input.pointer_moved(-400, 0),
            "a long report after the burst is the hand moving fast"
        );
    }

    #[test]
    fn a_mouse_only_departure_never_swallows_typing() {
        let input = FlowInput::new();
        input.departed(false);

        assert!(
            !input.key_pressed(),
            "no keyboard can follow, so none is chased"
        );
    }

    #[test]
    fn stopping_flow_gives_typing_back() {
        let input = FlowInput::new();
        input.departed(true);
        assert!(
            input.key_pressed(),
            "a keystroke here sends the keyboard on"
        );

        input.stopped();

        assert!(!input.key_pressed(), "a stopped Flow follows no keyboard");
    }

    #[test]
    fn a_hold_swallows_movement_until_released() {
        let input = FlowInput::new();
        assert!(
            !input.pointer_moved(1, 0),
            "nothing is held before a crossing"
        );

        input.hold();
        assert!(input.pointer_moved(1, 0));

        input.release();
        assert!(
            !input.pointer_moved(1, 0),
            "a failed handoff gives the pointer back"
        );
    }

    #[test]
    fn an_arrival_releases_a_hold_left_from_the_last_departure() {
        let input = FlowInput::new();
        input.hold();

        input.arrived();

        assert!(!input.pointer_moved(3, 0));
    }

    #[test]
    fn only_the_first_movement_after_an_arrival_can_be_a_burst() {
        let input = FlowInput::new();
        input.arrived();

        assert!(input.pointer_moved(-900, 40), "a reconnect burst glides in");
        assert!(
            input.pointer_moved(-400, 0),
            "a burst split over reports too"
        );
        assert_eq!(input.take_burst(), (-1300, 40));
        assert!(
            !input.pointer_moved(-12, 1),
            "ordinary motion ends the window"
        );
        assert!(
            !input.pointer_moved(-900, 0),
            "later fast motion is the hand"
        );
        let report = input.arrival_report();
        assert_eq!(report.burst_events, 2);
        assert!(report.first_move_ms.is_some());
    }

    #[test]
    fn keys_after_a_departure_send_the_keyboard_on_and_are_swallowed_briefly() {
        let input = FlowInput::new();
        assert!(!input.key_pressed(), "before any handoff, keys are local");

        input.departed(true);
        assert!(
            input.key_pressed(),
            "the keyboard stayed: swallow and send it on"
        );
        assert!(input.key_pressed(), "still within the suppression window");
    }

    #[test]
    fn local_pointer_movement_makes_keys_local_again() {
        let input = FlowInput::new();
        input.departed(true);

        assert!(!input.pointer_moved(2, 2));

        assert!(!input.key_pressed());
    }

    #[test]
    fn an_arrival_ends_keyboard_following() {
        let input = FlowInput::new();
        input.departed(true);

        input.arrived();

        assert!(!input.key_pressed());
    }
}
