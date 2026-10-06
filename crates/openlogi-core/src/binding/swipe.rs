//! The swipe-gesture runtime machinery: travel thresholds, the
//! [`detect_swipe`] classifier, and the [`SwipeAccumulator`] state machine
//! shared by both gesture-capture paths. This is input processing, distinct
//! from the `Action` vocabulary the parent [`binding`](super) module defines.

use std::time::Instant;

use super::GestureDirection;

/// Minimum dominant-axis travel (raw-XY units) before a held gesture commits to
/// a direction. Tuned to match Logitech Options+'s responsiveness.
pub const GESTURE_SWIPE_THRESHOLD: i32 = 50;
/// Maximum cross-axis travel allowed at the threshold, so only a reasonably
/// straight swipe commits. Grows with the dominant axis (`max(deadzone, 35%)`).
pub const GESTURE_SWIPE_DEADZONE: i32 = 40;
/// Net travel that keeps going the committed direction before that direction
/// fires again, so one continuous motion steps through several desktops like
/// Options+. Larger than [`GESTURE_SWIPE_THRESHOLD`] so the overshoot of a
/// single swipe doesn't double-fire; backtracking subtracts from it, so a
/// wobble never accumulates into a repeat.
// ponytail: fixed constant, make it a user setting if people want to tune it.
pub const GESTURE_REPEAT_DISTANCE: i32 = 3 * GESTURE_SWIPE_THRESHOLD;
/// How long the pointer must rest mid-hold to end a stroke: the next swipe then
/// starts fresh and may repeat the last direction at the normal threshold.
pub const GESTURE_REARM_PAUSE: std::time::Duration = std::time::Duration::from_millis(250);
/// Minimum time a gesture button must be held before its travel can commit to a
/// swipe. Distinguishes a deliberate hold-and-swipe from a quick click whose
/// cursor happened to be moving. Shared by both gesture paths (the HID++ thumb
/// pad and the OS-hook Back/Forward buttons).
pub const GESTURE_HOLD_FOR_SWIPE: std::time::Duration = std::time::Duration::from_millis(160);

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
/// HID++ gesture sources (`openlogi-hid`'s `0x1b04` raw-XY divert) and the
/// OS-hook Back/Forward buttons (`openlogi-agent-core`'s CGEventTap). A gesture
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
    /// Accumulated raw-XY travel since the hold began (saturating, so an
    /// arbitrarily long hold can never overflow).
    dx: i32,
    dy: i32,
    /// Set once a direction has committed this hold, so the release isn't
    /// then also read as a click.
    fired: bool,
    /// The direction committed by the current stroke. Travel restarts from
    /// zero after each commit, so one hold can chain swipes (left, right,
    /// left...) the way Options+ does.
    last: Option<GestureDirection>,
    /// Net travel continuing `last` since it committed; fires `last` again at
    /// [`GESTURE_REPEAT_DISTANCE`].
    repeat: i32,
    /// When the last motion arrived, to detect the pause that ends a stroke.
    moved_at: Option<Instant>,
}

impl SwipeAccumulator {
    /// Begin a fresh hold, resetting the travel accumulator and commit state.
    pub fn begin(&mut self) {
        self.held_since = Some(Instant::now());
        self.dx = 0;
        self.dy = 0;
        self.fired = false;
        self.last = None;
        self.repeat = 0;
        self.moved_at = None;
    }

    /// Whether a hold is in progress (between [`Self::begin`] and [`Self::end`]),
    /// so callers can do rising/falling-edge detection without a second flag.
    #[must_use]
    pub fn is_holding(&self) -> bool {
        self.held_since.is_some()
    }

    /// Feed a pointer-move / raw-XY delta into the current hold. Returns
    /// `Some(direction)` the instant travel commits (only after the hold passes
    /// [`GESTURE_HOLD_FOR_SWIPE`]), then restarts travel so the same hold can
    /// commit again; `None` while still too short or not holding.
    pub fn accumulate(&mut self, dx: i32, dy: i32) -> Option<GestureDirection> {
        self.held_since?;
        let now = Instant::now();
        if self
            .moved_at
            .is_some_and(|t| now.saturating_duration_since(t) >= GESTURE_REARM_PAUSE)
        {
            // A rest ends the stroke: the next swipe may repeat the last
            // direction. Summed travel the new report heads against is an
            // abandoned partial swipe and is dropped; travel it continues is
            // kept, so a reversal paused halfway still completes.
            if i64::from(self.dx) * i64::from(dx) + i64::from(self.dy) * i64::from(dy) < 0 {
                self.dx = 0;
                self.dy = 0;
            }
            self.last = None;
            self.repeat = 0;
        }
        self.moved_at = Some(now);
        if let Some(dir) = self.last {
            // Motion still heading the committed way before any reversal is the
            // same stroke: only its net along-axis travel counts (drift is
            // dropped), and it fires the direction again every
            // GESTURE_REPEAT_DISTANCE so a continuous motion keeps stepping.
            if continues(dir, dx, dy) && reversal(dir, self.dx, self.dy) == 0 {
                self.repeat = self.repeat.saturating_add(along(dir, dx, dy));
                if self.repeat >= GESTURE_REPEAT_DISTANCE {
                    self.repeat = 0;
                    return Some(dir);
                }
                return None;
            }
            // Backtracking takes back repeat progress, so a wobble never repeats.
            self.repeat = self.repeat.saturating_add(along(dir, dx, dy)).max(0);
        }
        self.dx = self.dx.saturating_add(dx);
        self.dy = self.dy.saturating_add(dy);
        // Once reversing, backtracking subtracts, but travel never banks past
        // the turning point: a reversal counts from there at the normal
        // threshold, and a net-zero wobble never fires.
        match self.last {
            Some(GestureDirection::Right) => self.dx = self.dx.min(0),
            Some(GestureDirection::Left) => self.dx = self.dx.max(0),
            Some(GestureDirection::Down) => self.dy = self.dy.min(0),
            Some(GestureDirection::Up) => self.dy = self.dy.max(0),
            Some(GestureDirection::Click) | None => {}
        }
        let held_long_enough = self
            .held_since
            .is_some_and(|t| t.elapsed() >= GESTURE_HOLD_FOR_SWIPE);
        if held_long_enough && let Some(dir) = detect_swipe(self.dx, self.dy) {
            self.fired = true;
            self.last = Some(dir);
            self.repeat = 0;
            self.dx = 0;
            self.dy = 0;
            return Some(dir);
        }
        None
    }

    /// End the current hold. Returns `true` when an in-progress hold ended
    /// without committing a swipe — the caller should fire the plain `Click`
    /// action — and `false` when a swipe already fired mid-motion, or when there
    /// was no hold to end (a stray release reports no click).
    pub fn end(&mut self) -> bool {
        let was_click = self.held_since.is_some() && !self.fired;
        self.held_since = None;
        was_click
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

    /// Test-only seam: age the last motion past [`GESTURE_REARM_PAUSE`], as if
    /// the pointer had rested, without sleeping.
    #[doc(hidden)]
    pub fn rest_for_test(&mut self) {
        if let Some(moved_at) = self.moved_at {
            self.moved_at = moved_at.checked_sub(GESTURE_REARM_PAUSE * 2);
        }
    }
}

/// The signed component of one report along `direction` (positive continues it).
fn along(direction: GestureDirection, dx: i32, dy: i32) -> i32 {
    match direction {
        GestureDirection::Left => dx.saturating_neg(),
        GestureDirection::Right => dx,
        GestureDirection::Up => dy.saturating_neg(),
        GestureDirection::Down => dy,
        GestureDirection::Click => 0,
    }
}

/// How far summed travel has moved back against the committed `direction`.
fn reversal(direction: GestureDirection, dx: i32, dy: i32) -> i32 {
    match direction {
        GestureDirection::Left => dx,
        GestureDirection::Right => dx.saturating_neg(),
        GestureDirection::Up => dy,
        GestureDirection::Down => dy.saturating_neg(),
        GestureDirection::Click => 0,
    }
    .max(0)
}

/// Whether one motion report keeps heading in the already-committed direction
/// (dominant along it), rather than reversing or turning.
fn continues(direction: GestureDirection, dx: i32, dy: i32) -> bool {
    let (ax, ay) = (dx.saturating_abs(), dy.saturating_abs());
    match direction {
        GestureDirection::Left => dx < 0 && ax >= ay,
        GestureDirection::Right => dx > 0 && ax >= ay,
        GestureDirection::Up => dy < 0 && ay >= ax,
        GestureDirection::Down => dy > 0 && ay >= ax,
        GestureDirection::Click => false,
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
        // Continuing the same swipe must not re-fire.
        assert_eq!(acc.accumulate(50, 0), None);
    }

    #[test]
    fn accumulator_chains_reversals_within_one_hold() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        let t = GESTURE_SWIPE_THRESHOLD + 10;
        // Each reversal fires at the normal threshold (issue #1634).
        assert_eq!(acc.accumulate(-t, 0), Some(GestureDirection::Left));
        assert_eq!(acc.accumulate(t, 0), Some(GestureDirection::Right));
        assert_eq!(acc.accumulate(-t, 0), Some(GestureDirection::Left));
        assert!(!acc.end(), "a hold that swiped is not a click");
    }

    #[test]
    fn accumulator_continuous_swipe_keeps_stepping() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        // Keep going right without stopping, drifting a little upward: one more
        // Right per GESTURE_REPEAT_DISTANCE, and the drift never fires Up.
        let mut fired = Vec::new();
        for _ in 0..(2 * GESTURE_REPEAT_DISTANCE / 10) {
            fired.extend(acc.accumulate(10, -3));
        }
        assert_eq!(fired, [GestureDirection::Right, GestureDirection::Right]);
    }

    #[test]
    fn accumulator_overshoot_alone_does_not_repeat() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        assert_eq!(acc.accumulate(GESTURE_REPEAT_DISTANCE - 1, 0), None);
    }

    #[test]
    fn accumulator_reverses_from_the_turning_point_after_an_overshoot() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(-(GESTURE_SWIPE_THRESHOLD + 10), 0),
            Some(GestureDirection::Left)
        );
        // Overshoot 100 units left, then turn: the normal threshold suffices.
        for _ in 0..10 {
            assert_eq!(acc.accumulate(-10, 0), None);
        }
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
    }

    #[test]
    fn accumulator_reversal_survives_jitter_back_toward_the_last_swipe() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        // Left 30, a one-unit rightward jitter packet, then left 30 more.
        assert_eq!(acc.accumulate(-30, 0), None);
        assert_eq!(acc.accumulate(1, 0), None);
        assert_eq!(acc.accumulate(-30, 0), Some(GestureDirection::Left));
    }

    #[test]
    fn accumulator_reversal_survives_a_rest_halfway() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        assert_eq!(acc.accumulate(-30, 0), None);
        acc.rest_for_test();
        assert_eq!(acc.accumulate(-30, 0), Some(GestureDirection::Left));
    }

    #[test]
    fn accumulator_repeats_a_direction_after_a_rest() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        let t = GESTURE_SWIPE_THRESHOLD + 10;
        assert_eq!(acc.accumulate(t, 0), Some(GestureDirection::Right));
        assert_eq!(acc.accumulate(t, 0), None, "still the same stroke");
        acc.rest_for_test();
        assert_eq!(acc.accumulate(t, 0), Some(GestureDirection::Right));
    }

    #[test]
    fn accumulator_repeat_after_a_rest_ignores_an_abandoned_reversal() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        let t = GESTURE_SWIPE_THRESHOLD + 10;
        assert_eq!(acc.accumulate(t, 0), Some(GestureDirection::Right));
        assert_eq!(acc.accumulate(-30, 0), None);
        acc.rest_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD, 0),
            Some(GestureDirection::Right)
        );
    }

    #[test]
    fn accumulator_backtracking_counts_against_a_reversal() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        let t = GESTURE_SWIPE_THRESHOLD + 10;
        assert_eq!(acc.accumulate(t, 0), Some(GestureDirection::Right));
        // Net Left travel is only 40.
        assert_eq!(acc.accumulate(-30, 0), None);
        assert_eq!(acc.accumulate(25, 0), None);
        assert_eq!(acc.accumulate(-35, 0), None);
    }

    #[test]
    fn accumulator_net_zero_wobble_after_a_swipe_never_fires() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        let t = GESTURE_SWIPE_THRESHOLD + 10;
        assert_eq!(acc.accumulate(t, 0), Some(GestureDirection::Right));
        for _ in 0..20 {
            assert_eq!(acc.accumulate(-15, 0), None);
            assert_eq!(acc.accumulate(15, 0), None);
        }
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
}
