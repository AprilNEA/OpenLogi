//! The swipe-gesture runtime machinery: travel thresholds, the
//! [`detect_swipe`] classifier, and the [`SwipeAccumulator`] state machine
//! shared by both gesture-capture paths. This is input processing, distinct
//! from the `Action` vocabulary the parent [`binding`](super) module defines.

use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::GestureDirection;
use crate::config::{GestureAxisBias, GestureSensitivity};

/// A single raw $(dx, dy)$ hardware motion sample recorded during a gesture hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawMotionSample {
    /// Milliseconds elapsed since the previous sample or hold begin.
    pub dt_ms: u32,
    /// Horizontal raw displacement delta.
    pub dx: i32,
    /// Vertical raw displacement delta.
    pub dy: i32,
}

/// A complete captured trace of a gesture hold from press to commit or release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GestureTrace {
    /// Unique identifier for this recorded gesture trace.
    pub id: String,
    /// Unix timestamp in milliseconds when the trace completed.
    pub timestamp_ms: u64,
    /// Optional hardware device config key.
    pub device_key: Option<String>,
    /// Physical source button that initiated the gesture.
    pub button: super::ButtonId,
    /// Classified gesture direction or click commit.
    pub detected: super::GestureDirection,
    /// Whether this gesture committed via the high-velocity flick bypass.
    pub fast_flick: bool,
    /// Total duration of the button hold in milliseconds.
    pub duration_ms: u64,
    /// Accumulated total horizontal travel.
    pub total_dx: i32,
    /// Accumulated total vertical travel.
    pub total_dy: i32,
    /// The active gesture sensitivity level during capture.
    pub sensitivity: i32,
    /// The active gesture axis bias during capture.
    pub axis_bias: i32,
    /// Ordered sequence of raw $(dx, dy)$ hardware motion samples.
    pub samples: Vec<RawMotionSample>,
    /// User-labeled intended direction when feedback is submitted.
    pub intended: Option<super::GestureDirection>,
    /// User feedback status (`"unlabeled"`, `"confirmed_correct"`, `"misinterpreted"`).
    pub feedback_status: String,
}

/// Minimum dominant-axis travel (raw-XY units) before a held gesture commits to
/// a direction. Tuned to match Logitech Options+'s responsiveness.
pub const GESTURE_SWIPE_THRESHOLD: i32 = 50;
/// Maximum cross-axis travel allowed at the threshold, so only a reasonably
/// straight swipe commits. Grows with the dominant axis (`max(deadzone, 35%)`).
pub const GESTURE_SWIPE_DEADZONE: i32 = 25;
/// Minimum time after button-down before any swipe may commit. Suppresses the
/// mechanical thumb-press contact kick (typically the first 10–25 ms packet)
/// from locking in the wrong direction.
pub const GESTURE_CONTACT_SETTLE: std::time::Duration = std::time::Duration::from_millis(40);
/// Net travel that keeps going the committed direction before that direction
/// fires again, so one continuous motion steps through several desktops like
/// Options+. Larger than [`GESTURE_SWIPE_THRESHOLD`] so the overshoot of a
/// single swipe doesn't double-fire; backtracking subtracts from it, so a
/// wobble never accumulates into a repeat.
///
/// A fast hand-width sweep on an MX Master 3 over Bluetooth is about 2000
/// raw-XY units. At 150 (3x the threshold) such a sweep repeated every
/// 30-40 ms and ran past four desktops; 600 gives about three repeats per hand
/// width (#1652).
// ponytail: fixed constant, make it a user setting if people want to tune it.
pub const GESTURE_REPEAT_DISTANCE: i32 = 12 * GESTURE_SWIPE_THRESHOLD;
/// How long the pointer must rest mid-hold to end a stroke: the next swipe then
/// starts fresh and may repeat the last direction at the normal threshold.
pub const GESTURE_REARM_PAUSE: std::time::Duration = std::time::Duration::from_millis(250);
/// Minimum time a gesture button must be held before its travel can commit to a
/// swipe. Distinguishes a deliberate hold-and-swipe from a quick click whose
/// cursor happened to be moving. Shared by both gesture paths (the HID++ thumb
/// pad and the OS-hook Back/Forward buttons).
pub const GESTURE_HOLD_FOR_SWIPE: std::time::Duration = std::time::Duration::from_millis(160);

/// Classify the *running* raw-XY travel of a held gesture button into a
/// directional swipe using default thresholds, the instant it commits — or
/// `None` while it's still too short or too diagonal.
#[must_use]
pub fn detect_swipe(dx: i32, dy: i32) -> Option<GestureDirection> {
    detect_swipe_with_thresholds(
        dx,
        dy,
        GESTURE_SWIPE_THRESHOLD,
        GESTURE_SWIPE_DEADZONE,
        GestureAxisBias::DEFAULT,
    )
}

/// Classify the *running* raw-XY travel of a held gesture button into a
/// directional swipe using custom distance threshold, deadzone, and axis bias,
/// the instant it commits — or `None` while it's still too short or too diagonal.
///
/// Swipes prioritize the intended axis using biomechanical scaling, clear-winner
/// cone boundaries, and directional axis bias:
/// - Horizontal swipes (Left/Right) allow natural wrist arcing while requiring dominant
///   horizontal travel.
/// - Vertical swipes (Up/Down) scale with axis bias to prevent initial thumb-button
///   click pressure from mis-firing as Up/Down.
///
/// Coordinates follow the device's raw-XY convention (`+x` = right, `+y` =
/// down), so an upward swipe (negative `dy`) maps to [`GestureDirection::Up`].
#[must_use]
pub fn detect_swipe_with_thresholds(
    dx: i32,
    dy: i32,
    threshold: i32,
    deadzone: i32,
    bias: GestureAxisBias,
) -> Option<GestureDirection> {
    let (abs_x, abs_y) = (dx.saturating_abs(), dy.saturating_abs());
    let (h_thresh, v_thresh, h_cone_pct, v_cone_pct) = bias.scale_thresholds(threshold, deadzone);

    let bias_val = i32::from(bias.into_inner());
    let weight_x = (100 - bias_val).max(50);
    let weight_y = (100 + bias_val).max(50);

    let effective_x = abs_x.saturating_mul(weight_x);
    let effective_y = abs_y.saturating_mul(weight_y);

    if effective_x > effective_y {
        if abs_x < h_thresh {
            return None;
        }
        let cross_limit_x = deadzone.max(abs_x.saturating_mul(h_cone_pct) / 100);
        if abs_y > cross_limit_x {
            return None;
        }
        Some(if dx > 0 {
            GestureDirection::Right
        } else {
            GestureDirection::Left
        })
    } else {
        if abs_y < v_thresh {
            return None;
        }
        let cross_limit_y = deadzone.max(abs_y.saturating_mul(v_cone_pct) / 100);
        if abs_x > cross_limit_y {
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
/// OS-hook Middle/Back/Forward buttons (`openlogi-agent-core`'s CGEventTap). A gesture
/// button's hold accumulates travel; the instant the dominant axis commits a
/// direction — after the button has been held [`GestureSensitivity::hold_duration`]
/// (or immediately if travel exceeds the high-velocity flick threshold) —
/// [`Self::accumulate`] returns that direction exactly once, like Logitech Options+.
/// A hold that never commits is a plain click, reported by [`Self::end`].
///
/// The two paths differ only in *what identifies the held control* (a
/// [`ButtonId`](super::ButtonId) for the OS hook, a diverted CID for the HID++ gesture control), so each owns
/// that and embeds this for the shared travel logic. Keeping the logic in one
/// place is deliberate: the two copies it replaced had already drifted apart
/// (one resolved a swipe only on release), which mis-fired the click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwipeAccumulator {
    /// When the current hold began, or `None` when not holding. Gates a
    /// deliberate swipe against a quick click whose cursor happened to move.
    held_since: Option<Instant>,
    /// Last sample arrival instant, for per-sample relative delta timing.
    last_sample_at: Option<Instant>,
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
    /// Signed sideways drift accompanying `repeat`, held to the same
    /// straightness rule as a fresh swipe so a diagonal can't repeat.
    repeat_cross: i32,
    /// When the last motion arrived, to detect the pause that ends a stroke.
    moved_at: Option<Instant>,
    /// Count of samples accumulated this hold.
    kept_samples: u32,
    /// The gesture sensitivity governing this accumulator's thresholds.
    sensitivity: GestureSensitivity,
    /// The gesture axis bias balancing horizontal vs vertical recognition.
    axis_bias: GestureAxisBias,
    /// Whether the most recent commit went through the high-velocity flick
    /// bypass. Recorded at commit because travel restarts from zero after it.
    fast_flick: bool,
    /// Sequence of raw motion samples recorded during this hold.
    samples: Vec<RawMotionSample>,
}

impl Default for SwipeAccumulator {
    fn default() -> Self {
        Self::new(GestureSensitivity::DEFAULT, GestureAxisBias::DEFAULT)
    }
}

impl SwipeAccumulator {
    /// Create a new swipe accumulator configured with `sensitivity` and `axis_bias`.
    #[must_use]
    pub const fn new(sensitivity: GestureSensitivity, axis_bias: GestureAxisBias) -> Self {
        Self {
            held_since: None,
            last_sample_at: None,
            dx: 0,
            dy: 0,
            fired: false,
            last: None,
            repeat: 0,
            repeat_cross: 0,
            moved_at: None,
            kept_samples: 0,
            fast_flick: false,
            sensitivity,
            axis_bias,
            samples: Vec::new(),
        }
    }

    /// Set the gesture sensitivity for this accumulator.
    pub fn set_sensitivity(&mut self, sensitivity: GestureSensitivity) {
        self.sensitivity = sensitivity;
    }

    /// The active gesture sensitivity.
    #[must_use]
    pub const fn sensitivity(&self) -> GestureSensitivity {
        self.sensitivity
    }

    /// Set the gesture axis bias for this accumulator.
    pub fn set_axis_bias(&mut self, axis_bias: GestureAxisBias) {
        self.axis_bias = axis_bias;
    }

    /// The active gesture axis bias.
    #[must_use]
    pub const fn axis_bias(&self) -> GestureAxisBias {
        self.axis_bias
    }

    /// The recorded motion samples in the current hold.
    #[must_use]
    pub fn samples(&self) -> &[RawMotionSample] {
        &self.samples
    }

    /// Total accumulated horizontal travel ($dx$).
    #[must_use]
    pub const fn total_dx(&self) -> i32 {
        self.dx
    }

    /// Total accumulated vertical travel ($dy$).
    #[must_use]
    pub const fn total_dy(&self) -> i32 {
        self.dy
    }

    /// Create a completed gesture trace representing this hold.
    #[must_use]
    pub fn create_trace(
        &self,
        button: super::ButtonId,
        detected: super::GestureDirection,
        fast_flick: bool,
    ) -> GestureTrace {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let duration_ms = self.held_since.map_or(0, |t| {
            u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX)
        });
        GestureTrace {
            id: format!("gt_{now_ms}_{}", self.samples.len()),
            timestamp_ms: now_ms,
            device_key: None,
            button,
            detected,
            fast_flick,
            duration_ms,
            total_dx: self
                .samples
                .iter()
                .fold(0, |sum, s| sum.saturating_add(s.dx)),
            total_dy: self
                .samples
                .iter()
                .fold(0, |sum, s| sum.saturating_add(s.dy)),
            sensitivity: i32::from(self.sensitivity),
            axis_bias: i32::from(self.axis_bias),
            samples: self.samples.clone(),
            intended: None,
            feedback_status: "unlabeled".to_string(),
        }
    }

    /// Whether the most recent commit went through the high-velocity flick bypass.
    #[must_use]
    pub const fn is_fast_flick(&self) -> bool {
        self.fast_flick
    }

    /// Begin a fresh hold, resetting the travel accumulator and commit state.
    pub fn begin(&mut self) {
        let now = Instant::now();
        self.held_since = Some(now);
        self.last_sample_at = Some(now);
        self.dx = 0;
        self.dy = 0;
        self.fired = false;
        self.last = None;
        self.repeat = 0;
        self.repeat_cross = 0;
        self.moved_at = None;
        self.kept_samples = 0;
        self.fast_flick = false;
        self.samples.clear();
    }

    /// Begin a fresh hold with updated `sensitivity` and `axis_bias`.
    pub fn begin_with_config(
        &mut self,
        sensitivity: GestureSensitivity,
        axis_bias: GestureAxisBias,
    ) {
        self.sensitivity = sensitivity;
        self.axis_bias = axis_bias;
        self.begin();
    }

    /// Begin a fresh hold with an updated `sensitivity`.
    pub fn begin_with_sensitivity(&mut self, sensitivity: GestureSensitivity) {
        self.sensitivity = sensitivity;
        self.begin();
    }

    /// Whether a hold is in progress (between [`Self::begin`] and [`Self::end`]),
    /// so callers can do rising/falling-edge detection without a second flag.
    #[must_use]
    pub fn is_holding(&self) -> bool {
        self.held_since.is_some()
    }

    /// Feed a pointer-move / raw-XY delta into the current hold. Returns
    /// `Some(direction)` the instant travel commits, then restarts travel so the
    /// same hold can commit again; `None` while still too short or not holding.
    ///
    /// Commits only after [`GESTURE_CONTACT_SETTLE`], and only once travel is
    /// confirmed by enough samples, the hold duration, or a high-velocity flick.
    pub fn accumulate(&mut self, dx: i32, dy: i32) -> Option<GestureDirection> {
        self.held_since?;
        let now = Instant::now();
        let dt_ms = self.last_sample_at.map_or(0, |t| {
            u32::try_from(now.duration_since(t).as_millis()).unwrap_or(u32::MAX)
        });
        self.last_sample_at = Some(now);
        self.samples.push(RawMotionSample { dt_ms, dx, dy });
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
            self.repeat_cross = 0;
        }
        self.moved_at = Some(now);
        if let Some(dir) = self.last {
            // Net travel continuing the committed way fires it again every
            // GESTURE_REPEAT_DISTANCE, so a continuous motion keeps stepping.
            // Backtracking takes back that progress and its drift alike, so a
            // wobble never repeats and a corrected stroke isn't held to drift
            // it has undone.
            let (a, c) = (along(dir, dx, dy), across(dir, dx, dy));
            let earlier = self.repeat;
            let total = earlier.saturating_add(a);
            // Only backtracking to the start clears drift: a purely
            // sideways report at zero progress is still part of the stroke.
            self.repeat_cross = if a < 0 && total <= 0 {
                0
            } else {
                self.repeat_cross.saturating_add(c)
            };
            self.repeat = total.max(0);
            if continues(dir, dx, dy) {
                // The same straightness rule as a fresh swipe, seen in the
                // committed direction's frame (`Right` = along it).
                let straight =
                    detect_swipe(self.repeat, self.repeat_cross) == Some(GestureDirection::Right);
                if self.repeat >= GESTURE_REPEAT_DISTANCE && straight {
                    // Carry the excess, so a report spanning two repeat
                    // distances fires again on the next report. The excess is
                    // the stroke's tail, so it keeps the tail's drift: this
                    // report's share first, then earlier travel's.
                    let excess = self.repeat - GESTURE_REPEAT_DISTANCE;
                    let tail = excess.min(a);
                    let mut carried = i64::from(c) * i64::from(tail) / i64::from(a);
                    if excess > a && earlier > 0 {
                        carried += i64::from(self.repeat_cross.saturating_sub(c))
                            * i64::from(excess - a)
                            / i64::from(earlier);
                    }
                    self.repeat = excess;
                    self.repeat_cross = i32::try_from(carried).unwrap_or(if carried < 0 {
                        i32::MIN
                    } else {
                        i32::MAX
                    });
                    // The stroke went back past any partial reversal.
                    self.dx = 0;
                    self.dy = 0;
                    self.fast_flick = false;
                    return Some(dir);
                }
                // Before any reversal this is the same stroke: drop it from
                // the reversal travel below.
                if reversal(dir, self.dx, self.dy) == 0 {
                    return None;
                }
            }
        }
        self.dx = self.dx.saturating_add(dx);
        self.dy = self.dy.saturating_add(dy);
        self.kept_samples = self.kept_samples.saturating_add(1);
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
        let elapsed = self
            .held_since
            .map_or(std::time::Duration::ZERO, |t| t.elapsed());
        if elapsed < GESTURE_CONTACT_SETTLE {
            return None;
        }

        let dominant = self.dx.saturating_abs().max(self.dy.saturating_abs());
        let fast_flick = dominant >= self.sensitivity.velocity_bypass_threshold();
        let held_long_enough = elapsed >= self.sensitivity.hold_duration();
        // Two+ samples confirm direction without waiting the full hold gate.
        // A single post-settle sample that already meets the travel threshold also
        // confirms — OS hooks often coalesce a whole swipe into one move event.
        let enough_travel = dominant >= self.sensitivity.travel_threshold();
        let direction_confirmed =
            self.kept_samples >= 2 || held_long_enough || fast_flick || enough_travel;

        if direction_confirmed
            && let Some(dir) = detect_swipe_with_thresholds(
                self.dx,
                self.dy,
                self.sensitivity.travel_threshold(),
                self.sensitivity.deadzone(),
                self.axis_bias,
            )
        {
            self.fired = true;
            self.fast_flick = fast_flick;
            self.last = Some(dir);
            self.repeat = 0;
            self.repeat_cross = 0;
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

    /// Test-only seam: backdate the current hold past the contact-settle window.
    #[doc(hidden)]
    pub fn backdate_settle_for_test(&mut self) {
        if self.held_since.is_some() {
            self.held_since = Instant::now().checked_sub(GESTURE_CONTACT_SETTLE * 2);
        }
    }

    /// Test-only seam: backdate the current hold so its hold duration
    /// gate is already satisfied, letting a test exercise a committed swipe
    /// without sleeping. Real code never calls this — [`Self::begin`] records the
    /// true start instant. A no-op when not currently holding.
    #[doc(hidden)]
    pub fn backdate_hold_for_test(&mut self) {
        if self.held_since.is_some() {
            self.held_since = Instant::now().checked_sub(self.sensitivity.hold_duration() * 2);
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

/// The signed component of one report across `direction` (its sideways drift).
fn across(direction: GestureDirection, dx: i32, dy: i32) -> i32 {
    match direction {
        GestureDirection::Left | GestureDirection::Right => dy,
        GestureDirection::Up | GestureDirection::Down => dx,
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

        assert_eq!(detect_swipe(0, GESTURE_SWIPE_THRESHOLD - 1), None);
        assert_eq!(
            detect_swipe(0, GESTURE_SWIPE_THRESHOLD),
            Some(GestureDirection::Down)
        );

        // At default neutral bias, cross-axis is max(deadzone, 45% of dominant).
        // For dominant 200 (200 * 45% = 90): 89 commits, 91 is too diagonal.
        assert_eq!(detect_swipe(200, 89), Some(GestureDirection::Right));
        assert_eq!(detect_swipe(200, 91), None);
        // For dominant 50, the 25-unit deadzone floor wins (50 * 45% = 22 < 25).
        assert_eq!(detect_swipe(50, 24), Some(GestureDirection::Right));
        assert_eq!(detect_swipe(50, 26), None);
    }

    #[test]
    fn detect_swipe_with_axis_bias() {
        let favor_h = GestureAxisBias::MIN; // -50 (h_thresh = 37, v_thresh = 75)
        assert_eq!(
            detect_swipe_with_thresholds(
                40,
                0,
                GESTURE_SWIPE_THRESHOLD,
                GESTURE_SWIPE_DEADZONE,
                favor_h
            ),
            Some(GestureDirection::Right)
        );
        // 30 horizontal is under h_thresh (37).
        assert_eq!(
            detect_swipe_with_thresholds(
                30,
                0,
                GESTURE_SWIPE_THRESHOLD,
                GESTURE_SWIPE_DEADZONE,
                favor_h
            ),
            None
        );

        // A diagonal that has equal raw travel (dx = 45, dy = 45) resolves as Right
        // under horizontal bias because weight_x (150) > weight_y (50).
        assert_eq!(
            detect_swipe_with_thresholds(
                45,
                27,
                GESTURE_SWIPE_THRESHOLD,
                GESTURE_SWIPE_DEADZONE,
                favor_h
            ),
            Some(GestureDirection::Right)
        );

        let favor_v = GestureAxisBias::MAX; // +50 (h_thresh = 75, v_thresh = 37)
        assert_eq!(
            detect_swipe_with_thresholds(
                0,
                40,
                GESTURE_SWIPE_THRESHOLD,
                GESTURE_SWIPE_DEADZONE,
                favor_v
            ),
            Some(GestureDirection::Down)
        );
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
    fn single_post_settle_move_above_travel_commits() {
        let mut acc = SwipeAccumulator::default();
        acc.begin_with_config(GestureSensitivity::DEFAULT, GestureAxisBias::DEFAULT);
        acc.backdate_settle_for_test();
        let travel = GestureSensitivity::DEFAULT.travel_threshold();
        let bypass = GestureSensitivity::DEFAULT.velocity_bypass_threshold();
        assert!(travel < bypass);
        assert_eq!(
            acc.accumulate(travel, 0),
            Some(GestureDirection::Right),
            "one post-settle sample at travel threshold must commit a swipe"
        );
    }

    #[test]
    fn accumulator_commits_a_direction_once_after_the_hold_gate() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
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
    fn accumulator_diagonal_motion_does_not_repeat() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        // Each report leans right, but together they are too diagonal to be
        // a swipe, so they must not repeat Right.
        for _ in 0..(GESTURE_REPEAT_DISTANCE / 10 + 5) {
            assert_eq!(acc.accumulate(10, 9), None);
        }
    }

    #[test]
    fn accumulator_large_report_carries_into_the_next_repeat() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        // One report spanning two repeat distances: one Right now, and the
        // carried excess fires the second on the next report.
        assert_eq!(
            acc.accumulate(2 * GESTURE_REPEAT_DISTANCE, 0),
            Some(GestureDirection::Right)
        );
        assert_eq!(acc.accumulate(1, 0), Some(GestureDirection::Right));
    }

    #[test]
    fn accumulator_carried_travel_keeps_its_drift() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        assert_eq!(acc.accumulate(GESTURE_REPEAT_DISTANCE - 10, 0), None);
        // Straight enough overall to repeat, but the carried tail is this
        // diagonal report, so it must not fire again on a nudge.
        assert_eq!(
            acc.accumulate(GESTURE_REPEAT_DISTANCE + 10, GESTURE_REPEAT_DISTANCE / 2),
            Some(GestureDirection::Right)
        );
        assert_eq!(acc.accumulate(1, 0), None);
    }

    #[test]
    fn accumulator_backtracking_takes_back_drift() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        // A repeat distance along with 50% of it as drift: too diagonal to
        // repeat yet.
        for _ in 0..(GESTURE_REPEAT_DISTANCE / 10) {
            assert_eq!(acc.accumulate(10, -5), None);
        }
        // Backing up undoes drift too, so the corrected stroke repeats.
        assert_eq!(acc.accumulate(-20, 40), None);
        assert_eq!(acc.accumulate(20, 0), Some(GestureDirection::Right));
    }

    #[test]
    fn accumulator_keeps_sideways_drift_at_zero_progress() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
        // 1.2 repeat distances along: the last report's drift alone is
        // straight enough to repeat, but with the sideways report at zero
        // progress the stroke is too diagonal. The sideways report stays
        // under the threshold so it commits no Up of its own.
        let half = GESTURE_REPEAT_DISTANCE * 3 / 5;
        assert_eq!(acc.accumulate(0, -40), None);
        assert_eq!(acc.accumulate(half, 0), None);
        assert_eq!(acc.accumulate(half, -(GESTURE_REPEAT_DISTANCE / 2)), None);
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
    fn accumulator_does_not_commit_before_settle() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        assert_eq!(acc.accumulate(GESTURE_SWIPE_THRESHOLD + 20, 0), None);
        assert_eq!(acc.accumulate(GESTURE_SWIPE_THRESHOLD + 20, 0), None);
        acc.backdate_settle_for_test();
        assert_eq!(acc.accumulate(10, 0), Some(GestureDirection::Right));
    }

    #[test]
    fn accumulator_end_reports_click_only_when_no_swipe_fired() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(acc.accumulate(2, -1), None);
        assert!(acc.end(), "a hold that never swiped is a click");

        acc.begin();
        acc.backdate_hold_for_test();
        assert!(acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0).is_some());
        assert!(!acc.end(), "a committed swipe must not also click");
    }

    #[test]
    fn accumulator_ignores_motion_when_not_holding() {
        let mut acc = SwipeAccumulator::default();
        assert!(!acc.is_holding());
        assert_eq!(acc.accumulate(GESTURE_SWIPE_THRESHOLD + 100, 0), None);
    }

    #[test]
    fn accumulator_sums_sub_threshold_deltas_until_they_commit() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
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
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(acc.accumulate(i32::MAX, 0), Some(GestureDirection::Right));
    }

    #[test]
    fn accumulator_begin_recovers_a_stale_hold() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(-(GESTURE_SWIPE_THRESHOLD + 10), 0),
            Some(GestureDirection::Left)
        );
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some(GestureDirection::Right)
        );
    }

    #[test]
    fn accumulator_end_without_a_hold_is_not_a_click() {
        let mut acc = SwipeAccumulator::default();
        assert!(!acc.end(), "a release with no hold is not a click");
        acc.begin();
        assert!(acc.end(), "the held release is a click");
        assert!(!acc.end(), "the redundant second release is not a click");
    }

    #[test]
    fn accumulator_commits_after_settle_with_confirmed_direction() {
        let mut acc = SwipeAccumulator::default();
        acc.begin();
        let bypass = GestureSensitivity::DEFAULT.velocity_bypass_threshold();
        assert_eq!(
            acc.accumulate(bypass + 5, 0),
            None,
            "cannot commit before settle"
        );
        acc.backdate_settle_for_test();
        assert_eq!(
            acc.accumulate(0, 0),
            Some(GestureDirection::Right),
            "commits once settle passes with confirmed travel"
        );
    }

    #[test]
    fn accumulator_scales_with_custom_sensitivity() {
        let max_sens = GestureSensitivity::MAX;
        let mut acc = SwipeAccumulator::new(max_sens, GestureAxisBias::DEFAULT);
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(
            acc.accumulate(max_sens.travel_threshold(), 0),
            Some(GestureDirection::Right)
        );

        let min_sens = GestureSensitivity::MIN;
        let mut min_acc = SwipeAccumulator::new(min_sens, GestureAxisBias::DEFAULT);
        min_acc.begin();
        min_acc.backdate_hold_for_test();
        assert_eq!(min_acc.accumulate(50, 0), None);
        assert_eq!(min_acc.accumulate(35, 0), Some(GestureDirection::Right));
    }

    #[test]
    fn accumulator_scales_with_axis_bias() {
        let mut acc = SwipeAccumulator::new(GestureSensitivity::DEFAULT, GestureAxisBias::MIN);
        acc.begin();
        acc.backdate_hold_for_test();
        assert_eq!(acc.accumulate(45, 0), Some(GestureDirection::Right));
    }
}
