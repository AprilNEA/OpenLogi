//! The swipe-gesture runtime machinery: travel thresholds, the
//! [`detect_swipe`] classifier, and the [`SwipeAccumulator`] state machine
//! shared by both gesture-capture paths. This is input processing, distinct
//! from the `Action` vocabulary the parent [`binding`](super) module defines.

use std::time::{Duration, Instant};

use crate::hid::Dpi;

use super::GestureDirection;

/// Minimum dominant-axis travel (raw-XY units) before a held gesture commits to
/// a direction. Tuned to match Logitech Options+'s responsiveness.
pub const GESTURE_SWIPE_THRESHOLD: i32 = 50;
/// Maximum cross-axis travel allowed at the threshold, so only a reasonably
/// straight swipe commits. Grows with the dominant axis (`max(deadzone, 35%)`).
pub const GESTURE_SWIPE_DEADZONE: i32 = 40;
/// Minimum time a gesture button must be held before its travel can commit to a
/// swipe. Distinguishes a deliberate hold-and-swipe from a quick click whose
/// cursor happened to be moving. Shared by both gesture paths (the HID++ thumb
/// pad and the OS-hook Middle/Back/Forward).
pub const GESTURE_HOLD_FOR_SWIPE: Duration = Duration::from_millis(160);
/// Physical click/drag deadzone for hold-mode travel. A side-button press
/// itself moves the mouse 0.5–2mm, so a raw-count threshold (12 counts is
/// 0.30mm at 1000 DPI) reads every click as a drag.
pub const HOLD_DRAG_DEADZONE_MM: f64 = 2.5;
/// How long a hold-mode stream may go without a raw-XY sample before it is
/// presumed to have lost its button-up. Matches the OS-hook stale bound so a
/// dropped HID++ release cannot leave a gesture session open indefinitely.
///
/// Measured from the last sample, not from the press: a hold-mode pan is
/// meant to run for as long as the user keeps panning.
pub const HOLD_STALE: Duration = Duration::from_secs(10);

/// Raw-XY counts equal to [`HOLD_DRAG_DEADZONE_MM`] at `dpi`.
///
/// This is the only hold-mode threshold. There is no count-based overload: a
/// caller that has a sensor reading cannot accidentally use a DPI-blind path.
#[must_use]
pub fn hold_drag_threshold_counts(dpi: Dpi) -> i32 {
    // 2.5mm / 25.4mm-per-inch * dpi counts-per-inch = 25 * dpi / 254.
    let counts = u32::from(u16::from(dpi)).saturating_mul(25) / 254;
    i32::try_from(counts).unwrap_or(i32::MAX)
}

/// How a hold-mode stream ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamRelease {
    /// No stream was open. A late end must not fire an action.
    Idle,
    /// The hold never cleared the physical deadzone — a click, not a drag.
    Click,
    /// Travel cleared the deadzone. Must not also report as a click.
    Drag,
}

/// Classify the *running* raw-XY travel of a held gesture button into a
/// directional swipe, the instant it commits — or `None` while it's still too
/// short or too diagonal.
///
/// The dominant axis must pass [`GESTURE_SWIPE_THRESHOLD`] while the cross axis
/// stays within `max(`[`GESTURE_SWIPE_DEADZONE`]`, 35% of dominant)`. Callers
/// fire the bound action the moment this returns `Some` — mid-swipe, like
/// Options+ — rather than waiting for the button release; a press that never
/// commits a direction is treated as [`GestureDirection::Click`] on release.
///
/// Coordinates follow the device's raw-XY convention (`+x` = right, `+y` =
/// down), so an upward swipe (negative `dy`) maps to [`GestureDirection::Up`].
#[must_use]
pub fn detect_swipe(dx: i32, dy: i32) -> Option<GestureDirection> {
    // Saturating throughout: a [`SwipeAccumulator`] hold that never commits (a
    // sustained diagonal) keeps summing travel, so `dx`/`dy` can reach the i32
    // bounds. `i32::MIN.abs()` would panic and a plain `dominant * 35` would
    // overflow — and a panic in the input-hook callback is exactly the freeze
    // hazard we must never hit. The clamp is inert in the normal range.
    let (abs_x, abs_y) = (dx.saturating_abs(), dy.saturating_abs());
    let dominant = abs_x.max(abs_y);
    if dominant < GESTURE_SWIPE_THRESHOLD {
        return None;
    }
    let cross_limit = GESTURE_SWIPE_DEADZONE.max(dominant.saturating_mul(35) / 100);
    if abs_x > abs_y {
        if abs_y > cross_limit {
            return None;
        }
        Some(if dx > 0 {
            GestureDirection::Right
        } else {
            GestureDirection::Left
        })
    } else {
        if abs_x > cross_limit {
            return None;
        }
        Some(if dy > 0 {
            GestureDirection::Down
        } else {
            GestureDirection::Up
        })
    }
}

/// The mid-swipe state machine shared by both gesture-capture paths: the HID++
/// dedicated gesture button (`openlogi-hid`'s `0x1b04` raw-XY divert) and the OS-hook
/// Middle/Back/Forward buttons (`openlogi-agent-core`'s CGEventTap). A gesture
/// button's hold accumulates travel; the instant the dominant axis commits a
/// direction — after the button has been held [`GESTURE_HOLD_FOR_SWIPE`], so a
/// quick click whose cursor drifted doesn't count — [`Self::accumulate`] returns
/// that direction exactly once, like Logitech Options+. A hold that never
/// commits is a plain click, reported by [`Self::end`].
///
/// The two paths differ only in *what identifies the held control* (a
/// [`ButtonId`](super::ButtonId) for the OS hook, a diverted CID for the HID++ gesture control), so each owns
/// that and embeds this for the shared travel logic. Keeping the logic in one
/// place is deliberate: the two copies it replaced had already drifted apart
/// (one resolved a swipe only on release), which mis-fired the click.
#[derive(Debug, Default)]
pub struct SwipeAccumulator {
    /// When the current hold began, or `None` when not holding. Gates a
    /// deliberate swipe against a quick click whose cursor happened to move.
    held_since: Option<Instant>,
    /// The most recent evidence this hold is alive: the press itself, then
    /// every sample fed into an open stream. Separate from `held_since`
    /// because the two answer different questions — how long the user has
    /// been holding, versus how long the device has been quiet.
    last_activity: Option<Instant>,
    /// Accumulated raw-XY travel since the hold began (saturating, so an
    /// arbitrarily long hold can never overflow).
    dx: i32,
    dy: i32,
    /// Set once a direction has committed this hold, so it fires exactly once
    /// and the release isn't then also read as a click. In stream mode, set
    /// once travel clears the physical deadzone.
    fired: bool,
    /// Squared count threshold while a hold-mode stream is open. Discrete
    /// swipe holds leave this `None` so [`Self::accumulate`] keeps classifying
    /// directions. Required at [`Self::begin_stream`] so a caller with DPI
    /// cannot start a stream on the count-blind swipe path.
    stream_threshold_sq: Option<u64>,
}

impl SwipeAccumulator {
    /// Begin a fresh hold, resetting the travel accumulator and commit state.
    pub fn begin(&mut self) {
        let now = Instant::now();
        self.held_since = Some(now);
        self.last_activity = Some(now);
        self.dx = 0;
        self.dy = 0;
        self.fired = false;
        self.stream_threshold_sq = None;
    }

    /// Begin a hold-mode stream whose click/drag threshold is
    /// [`HOLD_DRAG_DEADZONE_MM`] at `dpi`. This is the only way to start a
    /// streaming hold.
    pub fn begin_stream(&mut self, dpi: Dpi) {
        self.begin();
        let threshold = u64::from(hold_drag_threshold_counts(dpi).cast_unsigned());
        self.stream_threshold_sq = Some(threshold.saturating_mul(threshold));
    }

    /// Whether a hold is in progress (between [`Self::begin`] and [`Self::end`]),
    /// so callers can do rising/falling-edge detection without a second flag.
    #[must_use]
    pub fn is_holding(&self) -> bool {
        self.held_since.is_some()
    }

    /// Instant of the newest sample in the current hold, for the session's
    /// stale-hold bound. `None` when not holding.
    #[must_use]
    pub fn last_activity(&self) -> Option<Instant> {
        self.last_activity
    }

    /// Feed a pointer-move / raw-XY delta into the current hold. Returns
    /// `Some(direction)` exactly once per hold — the instant travel commits, and
    /// only after the hold passes [`GESTURE_HOLD_FOR_SWIPE`] — and `None` while
    /// still too short, already committed, not holding, or in stream mode.
    pub fn accumulate(&mut self, dx: i32, dy: i32) -> Option<GestureDirection> {
        if self.stream_threshold_sq.is_some() || self.fired || self.held_since.is_none() {
            return None;
        }
        self.dx = self.dx.saturating_add(dx);
        self.dy = self.dy.saturating_add(dy);
        let held_long_enough = self
            .held_since
            .is_some_and(|t| t.elapsed() >= GESTURE_HOLD_FOR_SWIPE);
        if held_long_enough && let Some(dir) = detect_swipe(self.dx, self.dy) {
            self.fired = true;
            return Some(dir);
        }
        None
    }

    /// Feed a raw-XY delta into a hold-mode stream. Returns the delta while
    /// the stream is live — including before the deadzone clears, so the
    /// injector can start on the first sample — and `None` when not streaming.
    /// A torn-down hold stays terminal: this never re-opens one.
    pub fn accumulate_stream(&mut self, dx: i32, dy: i32) -> Option<(i32, i32)> {
        let threshold_sq = self.stream_threshold_sq?;
        self.held_since?;
        self.last_activity = Some(Instant::now());
        self.dx = self.dx.saturating_add(dx);
        self.dy = self.dy.saturating_add(dy);
        let travel_sq = i64::from(self.dx)
            .saturating_mul(i64::from(self.dx))
            .saturating_add(i64::from(self.dy).saturating_mul(i64::from(self.dy)));
        if u64::try_from(travel_sq).is_ok_and(|sq| sq >= threshold_sq) {
            self.fired = true;
        }
        Some((dx, dy))
    }

    /// End the current hold. Returns `true` when an in-progress discrete hold
    /// ended without committing a swipe — the caller should fire the plain
    /// `Click` action — and `false` when a swipe already fired, a stream was
    /// open (use [`Self::end_stream`]), or there was no hold to end.
    pub fn end(&mut self) -> bool {
        if self.stream_threshold_sq.is_some() {
            let _ = self.end_stream();
            return false;
        }
        let was_click = self.held_since.is_some() && !self.fired;
        self.held_since = None;
        self.last_activity = None;
        was_click
    }

    /// End a hold-mode stream. A late call after teardown is [`StreamRelease::Idle`]
    /// and must not fire an action.
    pub fn end_stream(&mut self) -> StreamRelease {
        if self.stream_threshold_sq.is_none() || self.held_since.is_none() {
            self.held_since = None;
            self.last_activity = None;
            self.stream_threshold_sq = None;
            return StreamRelease::Idle;
        }
        let traveled = self.fired;
        self.held_since = None;
        self.last_activity = None;
        self.stream_threshold_sq = None;
        self.fired = false;
        self.dx = 0;
        self.dy = 0;
        if traveled {
            StreamRelease::Drag
        } else {
            StreamRelease::Click
        }
    }

    /// Test-only seam: backdate the current hold so its [`GESTURE_HOLD_FOR_SWIPE`]
    /// gate is already satisfied, letting a test exercise a committed swipe
    /// without sleeping. Real code never calls this — [`Self::begin`] records the
    /// true start instant. A no-op when not currently holding.
    #[doc(hidden)]
    pub fn backdate_hold_for_test(&mut self) {
        if self.held_since.is_some() {
            self.held_since = Instant::now().checked_sub(GESTURE_HOLD_FOR_SWIPE * 2);
        }
    }

    /// Test-only seam: age the current hold's last sample past
    /// [`HOLD_STALE`] — the hold has gone quiet.
    #[doc(hidden)]
    pub fn backdate_hold_past_stale_for_test(&mut self) {
        if self.held_since.is_some() {
            self.last_activity = Instant::now().checked_sub(HOLD_STALE * 2);
        }
    }

    /// Test-only seam: age only the press instant past [`HOLD_STALE`] — the
    /// hold has been down a long time but is still producing samples.
    #[doc(hidden)]
    pub fn backdate_press_past_stale_for_test(&mut self) {
        if self.held_since.is_some() {
            self.held_since = Instant::now().checked_sub(HOLD_STALE * 2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Gesture classification ────────────────────────────────────────────────

    #[test]
    fn detect_swipe_below_threshold_keeps_accumulating() {
        // Too little travel to commit — caller keeps summing raw-XY.
        assert_eq!(detect_swipe(40, 5), None);
        assert_eq!(detect_swipe(0, 0), None);
    }

    #[test]
    fn detect_swipe_commits_clean_direction() {
        assert_eq!(detect_swipe(120, 5), Some(GestureDirection::Right));
        assert_eq!(detect_swipe(-120, 5), Some(GestureDirection::Left));
        assert_eq!(detect_swipe(5, 120), Some(GestureDirection::Down));
        assert_eq!(detect_swipe(5, -120), Some(GestureDirection::Up));
    }

    #[test]
    fn detect_swipe_rejects_diagonal() {
        // Past the threshold but too diagonal (cross axis beyond the band).
        assert_eq!(detect_swipe(60, 60), None);
        assert_eq!(detect_swipe(-60, -60), None);
    }

    #[test]
    fn detect_swipe_threshold_and_cross_band_boundaries() {
        // The threshold bound is inclusive (`< THRESHOLD` rejects), so exactly at
        // it commits and one below does not.
        assert_eq!(
            detect_swipe(GESTURE_SWIPE_THRESHOLD, 0),
            Some(GestureDirection::Right)
        );
        assert_eq!(detect_swipe(GESTURE_SWIPE_THRESHOLD - 1, 0), None);

        // The cross-axis band is max(deadzone, 35% of dominant). For a large
        // dominant the 35% term wins (200 → 70): 69 commits, 71 is too diagonal.
        assert_eq!(detect_swipe(200, 69), Some(GestureDirection::Right));
        assert_eq!(detect_swipe(200, 71), None);
        // For a small dominant the 40-unit floor wins (100 → max(40, 35) = 40).
        assert_eq!(detect_swipe(100, 39), Some(GestureDirection::Right));
        assert_eq!(detect_swipe(100, 41), None);
    }

    #[test]
    fn detect_swipe_does_not_panic_on_extreme_values() {
        // Saturated accumulator travel can reach the i32 bounds. `i32::MIN.abs()`
        // panics and `dominant * 35` overflows — both must be clamped, not crash.
        assert_eq!(detect_swipe(i32::MAX, 0), Some(GestureDirection::Right));
        assert_eq!(detect_swipe(i32::MIN, 0), Some(GestureDirection::Left));
        assert_eq!(detect_swipe(0, i32::MAX), Some(GestureDirection::Down));
        assert_eq!(detect_swipe(0, i32::MIN), Some(GestureDirection::Up));
        // A diagonal at the extremes is still rejected, without panicking.
        assert_eq!(detect_swipe(i32::MIN, i32::MIN), None);
    }

    // ── SwipeAccumulator (the shared mid-swipe state machine) ─────────────────

    #[test]
    fn accumulator_commits_a_direction_once_after_the_hold_gate() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        // A clear rightward swipe commits exactly once, mid-motion.
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        // Further travel in the same hold must not re-fire.
        assert_eq!(acc.accumulate(50, 0), None);
    }

    #[test]
    fn accumulator_does_not_commit_before_the_hold_gate() {
        let mut acc = SwipeAccumulator::default();
        acc.begin(); // held_since = now, so the gate is not yet satisfied
        // A big delta arriving immediately (a quick click whose cursor drifted)
        // must not commit.
        assert_eq!(acc.accumulate(GESTURE_SWIPE_THRESHOLD + 100, 0), None);
        // Once held long enough, the next delta commits.
        acc.backdate_hold_for_test();
        assert!(acc.accumulate(GESTURE_SWIPE_THRESHOLD + 100, 0).is_some());
    }

    #[test]
    fn accumulator_end_reports_click_only_when_no_swipe_fired() {
        // A hold with only tiny drift never commits → end() is a click.
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(acc.accumulate(2, -1), None);
        assert!(acc.end(), "a hold that never swiped is a click");

        // A hold that committed a swipe → end() is not a click.
        acc.begin();
        acc.backdate_hold_for_test();
        assert!(acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0).is_some());
        assert!(!acc.end(), "a committed swipe must not also click");
    }

    #[test]
    fn accumulator_ignores_motion_when_not_holding() {
        let mut acc = SwipeAccumulator::default();
        assert!(!acc.is_holding());
        // Travel outside a hold is dropped, never committing a stray swipe.
        assert_eq!(acc.accumulate(GESTURE_SWIPE_THRESHOLD + 100, 0), None);
    }

    #[test]
    fn accumulator_sums_sub_threshold_deltas_until_they_commit() {
        // The whole reason for an accumulator (vs. detect_swipe on one delta):
        // several deltas each too small to commit on their own must sum across
        // the hold until the running total crosses the threshold, then commit.
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        // Just under half the threshold: one or two steps never reach it, three do.
        let step = GESTURE_SWIPE_THRESHOLD / 2 - 1;
        assert_eq!(acc.accumulate(step, 0), None, "one step is sub-threshold");
        assert_eq!(acc.accumulate(step, 0), None, "two steps still under");
        assert_eq!(
            acc.accumulate(step, 0),
            Some(GestureDirection::Right),
            "the running sum finally crosses the threshold"
        );
    }

    #[test]
    fn accumulator_saturates_instead_of_overflowing() {
        // The doc promises an arbitrarily long hold can't overflow. A perfect
        // diagonal never commits, so travel keeps summing; feed deltas that would
        // overflow both an i32 sum and a naive cross-band multiply — both must
        // saturate, not panic (debug builds panic on overflow).
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(i32::MAX, i32::MAX),
            None,
            "a diagonal never commits"
        );
        assert_eq!(
            acc.accumulate(i32::MAX, i32::MAX),
            None,
            "the saturating sum must not panic"
        );
        // A clean axis on a fresh hold still commits with a saturated magnitude.
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(acc.accumulate(i32::MAX, 0), Some(GestureDirection::Right));
    }

    #[test]
    fn accumulator_begin_recovers_a_stale_hold() {
        // A missed release (e.g. focus loss between press and release) can leave
        // a dangling hold that already fired with travel in some direction. A
        // fresh begin() must wipe both the `fired` latch and the travel, so the
        // next press isn't poisoned by the old one.
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        // Stale hold commits LEFT (negative dx) and latches `fired`.
        assert_eq!(
            acc.accumulate(-(GESTURE_SWIPE_THRESHOLD + 10), 0),
            Some(GestureDirection::Left)
        );
        // No end() — a dropped release, then a fresh press.
        acc.begin();
        acc.backdate_hold_for_test();
        // Had `fired` leaked this would be None; had the negative travel leaked it
        // would commit Left. Committing Right proves begin() reset both.
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
    }

    #[test]
    fn accumulator_end_without_a_hold_is_not_a_click() {
        // end() in isolation (no begin) must not claim a click — there was no
        // hold — so a stray release can't be read as a press.
        let mut acc = SwipeAccumulator::default();
        assert!(!acc.end(), "a release with no hold is not a click");
        // A redundant second release after a real hold already ended is inert too.
        acc.begin();
        assert!(acc.end(), "the held release is a click");
        assert!(!acc.end(), "the redundant second release is not a click");
    }

    // ── Hold-mode stream (physical deadzone, never a swipe direction) ─────────

    #[test]
    fn hold_drag_threshold_scales_with_dpi() {
        // 2.5mm at 400 DPI is ~39 counts; at 1600 DPI it is ~157. A DPI-blind
        // constant (the old 12-count click deadzone, or GESTURE_SWIPE_THRESHOLD)
        // would make these two equal and this test fail.
        let low = hold_drag_threshold_counts(Dpi::new(400));
        let high = hold_drag_threshold_counts(Dpi::new(1600));
        assert!(low > 0, "400 DPI must still have a physical deadzone");
        assert!(
            high > low * 3,
            "quadrupling DPI must roughly quadruple the count threshold, got {low} vs {high}"
        );
        assert_ne!(low, GESTURE_SWIPE_THRESHOLD);
        assert_ne!(high, GESTURE_SWIPE_THRESHOLD);
    }

    #[test]
    fn the_same_raw_counts_are_a_drag_at_low_dpi_and_a_click_at_high_dpi() {
        // 80 counts is past 2.5mm at 400 DPI and short of it at 1600 DPI. If
        // begin_stream were swapped for the DPI-blind swipe path, both holds
        // would classify the same way.
        let travel = 80;
        assert!(travel > hold_drag_threshold_counts(Dpi::new(400)));
        assert!(travel < hold_drag_threshold_counts(Dpi::new(1600)));

        let mut low = SwipeAccumulator::default();
        low.begin_stream(Dpi::new(400));
        assert_eq!(low.accumulate_stream(travel, 0), Some((travel, 0)));
        assert_eq!(low.end_stream(), StreamRelease::Drag);

        let mut high = SwipeAccumulator::default();
        high.begin_stream(Dpi::new(1600));
        assert_eq!(high.accumulate_stream(travel, 0), Some((travel, 0)));
        assert_eq!(high.end_stream(), StreamRelease::Click);
    }

    #[test]
    fn a_stream_that_cleared_the_deadzone_is_not_a_click() {
        let mut acc = SwipeAccumulator::default();
        acc.begin_stream(Dpi::new(1000));
        let step = hold_drag_threshold_counts(Dpi::new(1000)) + 1;
        assert_eq!(acc.accumulate_stream(step, 0), Some((step, 0)));
        assert_eq!(acc.end_stream(), StreamRelease::Drag);
        assert_eq!(
            acc.end_stream(),
            StreamRelease::Idle,
            "a late end after teardown must not fire"
        );
    }

    #[test]
    fn a_stream_that_never_moved_is_a_click() {
        let mut acc = SwipeAccumulator::default();
        acc.begin_stream(Dpi::new(1000));
        assert_eq!(acc.accumulate_stream(1, 0), Some((1, 0)));
        assert_eq!(acc.end_stream(), StreamRelease::Click);
    }

    #[test]
    fn stream_mode_never_commits_a_swipe_direction() {
        let mut acc = SwipeAccumulator::default();
        acc.begin_stream(Dpi::new(1000));
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 100, 0),
            None,
            "a caller with DPI must not fall onto the discrete swipe classifier"
        );
        assert!(
            acc.accumulate_stream(10, 0).is_some(),
            "the stream stays open under a rejected swipe classification"
        );
        assert!(!acc.end(), "ending a stream through end() is not a click");
    }

    #[test]
    fn accumulate_stream_does_not_reopen_a_torn_down_hold() {
        let mut acc = SwipeAccumulator::default();
        acc.begin_stream(Dpi::new(1000));
        let _ = acc.end_stream();
        assert_eq!(
            acc.accumulate_stream(200, 200),
            None,
            "late motion must not silently re-open a session"
        );
        assert_eq!(acc.end_stream(), StreamRelease::Idle);
    }

    #[test]
    fn stream_sums_sub_threshold_deltas_until_they_clear_the_deadzone() {
        let mut acc = SwipeAccumulator::default();
        acc.begin_stream(Dpi::new(400));
        let threshold = hold_drag_threshold_counts(Dpi::new(400));
        let step = threshold / 2;
        assert_eq!(acc.accumulate_stream(step, 0), Some((step, 0)));
        assert_eq!(acc.end_stream(), StreamRelease::Click);

        acc.begin_stream(Dpi::new(400));
        assert_eq!(acc.accumulate_stream(step, 0), Some((step, 0)));
        assert_eq!(acc.accumulate_stream(step, 0), Some((step, 0)));
        assert_eq!(acc.accumulate_stream(step, 0), Some((step, 0)));
        assert_eq!(acc.end_stream(), StreamRelease::Drag);
    }
}
