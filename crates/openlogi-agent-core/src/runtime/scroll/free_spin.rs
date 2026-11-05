//! Free-spin and stop detection for a diverted main wheel.
//!
//! Fed the raw HID++ `0x2121` movement reports of one wheel, the detector
//! tells ratchet scrolling from a SmartShift free spin, and a natural
//! spin-down from a finger stopping the wheel. Thresholds come from labelled
//! recordings of an MX Master 4 (low-resolution reports on a ~7.5 ms poll
//! grid, 1–3 notches each):
//!
//! - ratchet gestures peak at ≤ 80 notches/s over 100 ms; free spins reach
//!   100–300/s within ~135 ms, gentle ones 50–120/s that keep going;
//! - a free spin decays linearly, by friction: about −33 notches/s²;
//! - a finger stop keeps the wheel near full speed until ~50 ms before its
//!   last report, so it shows as a silence, a collapse of the short-window
//!   speed, or a deceleration far beyond friction.
//!
//! On the recordings these rules have no false trigger and catch every
//! finger stop a median ~20 ms after the wheel's last report.
//!
//! The detector is pure: it reads the clock only through the instants it is
//! given, so it is deterministic under test.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Speed over the last 100 ms that alone marks a free spin.
const ONSET_FAST: f64 = 100.0;
/// Lower speed that marks a free spin once motion has lasted
/// [`ONSET_SLOW_AFTER`] and stayed continuous: a free wheel leaves no gap
/// over ~30 ms between reports, while ratchet detents and finger strokes
/// leave 60–200 ms gaps even in a sustained scroll.
const ONSET_SLOW: f64 = 50.0;
const ONSET_SLOW_AFTER: Duration = Duration::from_millis(350);
const CONTINUITY_WINDOW: Duration = Duration::from_millis(250);
const CONTINUITY_MAX_GAP: Duration = Duration::from_millis(35);
const ONSET_WINDOW: Duration = Duration::from_millis(100);
/// Quiet time that ends a gesture outright, whatever the speed.
const GESTURE_END: Duration = Duration::from_millis(400);

/// Stop rules only apply while the wheel turns at least this fast.
const STOP_MIN_SPEED: f64 = 60.0;
/// Speed reference window for the silence and brake rules.
const LONG_WINDOW: Duration = Duration::from_millis(150);
/// Silence longer than this many notches at the current speed is a stop...
const SILENCE_NOTCHES: f64 = 5.0;
/// ...but never shorter than this, so one dropped radio report is not.
const SILENCE_FLOOR: Duration = Duration::from_millis(35);
/// Below [`STOP_MIN_SPEED`], down to this speed, a finger stop shows only as
/// silence: the wheel is reporting so sparsely that the notch-count rules
/// cannot see it in time. There a silence of [`SLOW_SILENCE_GAPS`] times the
/// wheel's own recent report gaps is a stop. Friction lengthens a coasting
/// wheel's gaps by under 10% a report above this speed, so a natural
/// spin-down never trips it; below it the glide has already faded to
/// nothing (`momentum_output`), so there is nothing left to stop.
const SLOW_STOP_MIN_SPEED: f64 = 20.0;
const SLOW_SILENCE_GAPS: f64 = 2.5;
/// Recent report gaps the slow-silence rule takes its rhythm from.
const SLOW_SILENCE_SAMPLES: usize = 4;

/// The last 45 ms running below this fraction of the 150 ms speed is a brake.
/// Strict, so a dropped radio report at full speed does not trip it; the
/// deceleration rule catches the gentler brakes.
const BRAKE_WINDOW: Duration = Duration::from_millis(45);
const BRAKE_RATIO: f64 = 0.2;
/// The last 60 ms losing more than this fraction of the 150 ms before it is
/// a deceleration friction cannot produce.
const DECEL_RECENT: Duration = Duration::from_millis(60);
const DECEL_FRACTION: f64 = 0.6;
const DECEL_MIN_SPEED: f64 = 80.0;

/// A free wheel only slows down. In free spin, the last [`DRIVEN_RECENT`]
/// carrying at least [`DRIVEN_RATIO`]× the notches the [`LONG_WINDOW`] before
/// it predicts, and [`DRIVEN_EXTRA_NOTCHES`] more, means a finger is turning
/// the wheel: the gesture is ratchet scrolling again (a fresh flick re-enters
/// free spin through the onset rules within ~100 ms). Counted in notches, not
/// rates, so one quantization step at low speed is never mistaken for a push.
const DRIVEN_RECENT: Duration = Duration::from_millis(100);
const DRIVEN_RATIO: f64 = 1.4;
const DRIVEN_EXTRA_NOTCHES: f64 = 2.5;
/// The slow onset also needs the wheel coasting: the last 100 ms no faster
/// than this factor of the 100 ms before (plus quantization slack).
const COAST_RATIO: f64 = 1.1;
/// The slow onset also needs the gesture to have started as a flick: a peak
/// of at least this speed over [`ONSET_WINDOW`]. Gentle free spins peak at
/// 80–120 notches/s; slow finger-driven scrolling stays near 30–70.
const SLOW_ONSET_PEAK: f64 = 80.0;
/// How recent that peak must be: a gesture made of slow strokes with short
/// pauses can run for seconds, and a burst at its start says nothing about
/// a coast long after it.
const SLOW_ONSET_PEAK_AGE: Duration = Duration::from_millis(400);
const COAST_SLACK: f64 = 10.0;

/// After a stop, reports this soon are the wheel settling; later ones, or one
/// turning the other way, start a new gesture.
const STOP_SETTLE: Duration = Duration::from_millis(40);

/// Window of the speed that drives momentum output: long enough to smooth
/// the 1–3 notch quantization of low-resolution reports.
const SPEED_WINDOW: Duration = Duration::from_millis(120);

/// History kept: enough for every window above.
const HISTORY: Duration = Duration::from_millis(400);

/// Why a free spin was judged stopped by hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// No report for longer than the current speed allows.
    Silence,
    /// The short-window speed collapsed.
    Brake,
    /// The speed fell faster than friction can slow the wheel.
    Decel,
}

/// What changed on this update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelPhase {
    /// Ratchet scrolling, or not yet enough evidence of a free spin.
    Ratchet,
    /// The wheel spins freely.
    FreeSpin,
    /// The free spin was stopped by hand; reports until the gesture ends are
    /// the wheel settling and must not scroll.
    Stopped(StopReason),
}

#[derive(Clone, Copy)]
struct Report {
    at: Instant,
    notches: f64,
}

/// One wheel's detector.
pub struct FreeSpinDetector {
    reports: VecDeque<Report>,
    gesture_start: Option<Instant>,
    /// Direction of the gesture: the sign of its first report.
    direction: f64,
    free_spin_since: Option<Instant>,
    stopped_at: Option<Instant>,
    /// Highest [`ONSET_WINDOW`] speed reached within the last
    /// [`SLOW_ONSET_PEAK_AGE`], and when.
    peak: f64,
    peak_at: Option<Instant>,
    phase: WheelPhase,
}

impl Default for FreeSpinDetector {
    fn default() -> Self {
        Self {
            reports: VecDeque::new(),
            gesture_start: None,
            direction: 0.0,
            free_spin_since: None,
            stopped_at: None,
            peak: 0.0,
            peak_at: None,
            phase: WheelPhase::Ratchet,
        }
    }
}

impl FreeSpinDetector {
    /// The current phase.
    #[must_use]
    pub const fn phase(&self) -> WheelPhase {
        self.phase
    }

    /// Keep a gesture the detector just read as finger-driven in free spin:
    /// the engine decided the push was a fresh flick chained onto a strong
    /// glide, not ratchet scrolling.
    pub fn resume_free_spin(&mut self, at: Instant) {
        self.phase = WheelPhase::FreeSpin;
        self.free_spin_since = Some(at);
        self.stopped_at = None;
    }

    /// The wheel's speed in notches per second over the [`SPEED_WINDOW`]
    /// up to `at`. Measured up to `at`, it eases down through a silence, so
    /// output driven by it slows during the few milliseconds before a stop
    /// is confirmed rather than running on at full pace.
    #[must_use]
    pub fn speed(&self, at: Instant) -> f64 {
        self.rate(at, SPEED_WINDOW)
    }

    /// Feed one movement report of signed `notches` at `at`, and answer the
    /// phase that report belongs to.
    pub fn report(&mut self, at: Instant, notches: f64) -> WheelPhase {
        self.end_gesture_if_idle(at);
        // A report the other way is a new gesture in any phase: a free
        // wheel cannot turn back on its own, and the reports below are
        // unsigned, so a reversal kept in them would read as more speed the
        // old way.
        let reversed = self.gesture_start.is_some() && notches * self.direction < 0.0;
        let settled = self
            .stopped_at
            .is_some_and(|stopped_at| at.saturating_duration_since(stopped_at) >= STOP_SETTLE);
        if reversed || settled {
            *self = Self::default();
        }
        if self.gesture_start.is_none() {
            self.gesture_start = Some(at);
            self.direction = notches.signum();
        }
        self.reports.push_back(Report {
            at,
            notches: notches.abs(),
        });
        while self
            .reports
            .front()
            .is_some_and(|r| at.saturating_duration_since(r.at) > HISTORY)
        {
            self.reports.pop_front();
        }
        self.evaluate(at)
    }

    /// Re-evaluate without a report — for the silence rule, which can only
    /// fire between reports. Call it on the output frame cadence.
    pub fn tick(&mut self, at: Instant) -> WheelPhase {
        if self.end_gesture_if_idle(at) {
            return self.phase;
        }
        if self.gesture_start.is_some() {
            self.evaluate(at)
        } else {
            self.phase
        }
    }

    fn end_gesture_if_idle(&mut self, at: Instant) -> bool {
        let idle = self
            .reports
            .back()
            .is_some_and(|last| at.saturating_duration_since(last.at) >= GESTURE_END);
        if idle {
            *self = Self::default();
        }
        idle
    }

    fn evaluate(&mut self, at: Instant) -> WheelPhase {
        match self.phase {
            WheelPhase::Ratchet => {
                let elapsed = self
                    .gesture_start
                    .map_or(Duration::ZERO, |start| at.saturating_duration_since(start));
                let speed = self.rate(at, ONSET_WINDOW);
                let stale = self
                    .peak_at
                    .is_none_or(|when| at.saturating_duration_since(when) > SLOW_ONSET_PEAK_AGE);
                if stale || speed >= self.peak {
                    self.peak = speed;
                    self.peak_at = Some(at);
                }
                let slow_onset = elapsed >= ONSET_SLOW_AFTER
                    && self.peak >= SLOW_ONSET_PEAK
                    && speed >= ONSET_SLOW
                    && self.continuous(at)
                    && self.coasting(at);
                if speed >= ONSET_FAST || slow_onset {
                    self.phase = WheelPhase::FreeSpin;
                    self.free_spin_since = Some(at);
                }
            }
            WheelPhase::FreeSpin => {
                if self.driven(at) {
                    // A finger is turning the wheel: ratchet scrolling, as a
                    // new gesture, so the onset rules start afresh.
                    self.phase = WheelPhase::Ratchet;
                    self.free_spin_since = None;
                    self.gesture_start = Some(at);
                    self.peak = 0.0;
                    self.peak_at = None;
                } else if let Some(reason) = self.stop_reason(at) {
                    self.phase = WheelPhase::Stopped(reason);
                    self.stopped_at = Some(at);
                }
            }
            WheelPhase::Stopped(_) => {}
        }
        self.phase
    }

    fn stop_reason(&self, at: Instant) -> Option<StopReason> {
        let last = self.reports.back()?.at;
        // The reference speed ends at the last report: measured up to `at`,
        // the silence being judged would drag it down and hide the stop.
        let speed = self.rate(last, LONG_WINDOW);
        if speed >= STOP_MIN_SPEED {
            let silence = at.saturating_duration_since(last);
            if silence > silence_limit(speed) {
                return Some(StopReason::Silence);
            }
            if self.rate(at, BRAKE_WINDOW) < BRAKE_RATIO * speed {
                return Some(StopReason::Brake);
            }
        } else if speed >= SLOW_STOP_MIN_SPEED
            && let Some(limit) = self.slow_silence_limit()
            && at.saturating_duration_since(last) > limit
        {
            return Some(StopReason::Silence);
        }
        let since = self
            .free_spin_since
            .map_or(Duration::ZERO, |start| at.saturating_duration_since(start));
        if since > DECEL_RECENT + LONG_WINDOW {
            let recent_from = at.checked_sub(DECEL_RECENT)?;
            let before = self.rate_between(recent_from.checked_sub(LONG_WINDOW)?, recent_from);
            let recent = self.rate_between(recent_from, at);
            if before >= DECEL_MIN_SPEED && recent < before * (1.0 - DECEL_FRACTION) {
                return Some(StopReason::Decel);
            }
        }
        None
    }

    /// How long a slow wheel may stay quiet on its own: [`SLOW_SILENCE_GAPS`]
    /// times the longest of its last few report gaps, never under
    /// [`SILENCE_FLOOR`]. `None` until there are gaps to go by.
    fn slow_silence_limit(&self) -> Option<Duration> {
        let longest = self
            .reports
            .iter()
            .rev()
            .zip(self.reports.iter().rev().skip(1))
            .take(SLOW_SILENCE_SAMPLES)
            .map(|(later, earlier)| later.at.saturating_duration_since(earlier.at))
            .max()?;
        Some(longest.mul_f64(SLOW_SILENCE_GAPS).max(SILENCE_FLOOR))
    }

    /// Whether the wheel sped up over the last [`DRIVEN_RECENT`] — something
    /// a free-spinning wheel cannot do on its own.
    fn driven(&self, at: Instant) -> bool {
        let since = self
            .free_spin_since
            .map_or(Duration::ZERO, |start| at.saturating_duration_since(start));
        if since < DRIVEN_RECENT + LONG_WINDOW {
            return false;
        }
        let Some(recent_from) = at.checked_sub(DRIVEN_RECENT) else {
            return false;
        };
        let Some(before_from) = recent_from.checked_sub(LONG_WINDOW) else {
            return false;
        };
        let recent = self.rate_between(recent_from, at) * DRIVEN_RECENT.as_secs_f64();
        let expected = self.rate_between(before_from, recent_from) * DRIVEN_RECENT.as_secs_f64();
        recent >= expected * DRIVEN_RATIO && recent - expected >= DRIVEN_EXTRA_NOTCHES
    }

    /// Whether the wheel is not speeding up: the last 100 ms no faster than
    /// the 100 ms before it, within quantization slack.
    fn coasting(&self, at: Instant) -> bool {
        let Some(mid) = at.checked_sub(ONSET_WINDOW) else {
            return false;
        };
        let Some(from) = mid.checked_sub(ONSET_WINDOW) else {
            return false;
        };
        self.rate_between(mid, at) <= self.rate_between(from, mid) * COAST_RATIO + COAST_SLACK
    }

    /// Whether the wheel reported without a long gap over the last
    /// [`CONTINUITY_WINDOW`], up to `at`.
    fn continuous(&self, at: Instant) -> bool {
        let Some(from) = at.checked_sub(CONTINUITY_WINDOW) else {
            return false;
        };
        let mut previous = from;
        for report in self.reports.iter().filter(|r| r.at > from && r.at <= at) {
            if report.at.saturating_duration_since(previous) > CONTINUITY_MAX_GAP {
                return false;
            }
            previous = report.at;
        }
        at.saturating_duration_since(previous) <= CONTINUITY_MAX_GAP
    }

    /// Notches per second over `(at - window, at]`.
    fn rate(&self, at: Instant, window: Duration) -> f64 {
        at.checked_sub(window)
            .map_or(0.0, |from| self.rate_between(from, at))
    }

    /// Notches per second over `(from, to]`.
    fn rate_between(&self, from: Instant, to: Instant) -> f64 {
        let span = to.saturating_duration_since(from).as_secs_f64();
        if span <= 0.0 {
            return 0.0;
        }
        let notches: f64 = self
            .reports
            .iter()
            .filter(|r| r.at > from && r.at <= to)
            .map(|r| r.notches)
            .sum();
        notches / span
    }
}

/// The longest silence a wheel turning at `speed` notches/s produces on its
/// own.
fn silence_limit(speed: f64) -> Duration {
    if speed <= 0.0 {
        return GESTURE_END;
    }
    Duration::from_secs_f64(SILENCE_NOTCHES / speed).max(SILENCE_FLOOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLL: Duration = Duration::from_micros(7_500);

    /// Feed a wheel turning at `speed` notches/s for `duration`, on the poll
    /// grid, starting at `start`. Answers the instant after the last report
    /// and every phase seen.
    fn spin(
        detector: &mut FreeSpinDetector,
        start: Instant,
        speed: impl Fn(f64) -> f64,
        duration: Duration,
    ) -> (Instant, Vec<WheelPhase>) {
        let mut phases = Vec::new();
        let mut at = start;
        let mut carried = 0.0;
        let end = start + duration;
        while at < end {
            let elapsed = at.duration_since(start).as_secs_f64();
            carried += speed(elapsed) * POLL.as_secs_f64();
            let whole = carried.floor();
            if whole >= 1.0 {
                carried -= whole;
                phases.push(detector.report(at, whole));
            } else {
                phases.push(detector.tick(at));
            }
            at += POLL;
        }
        (at, phases)
    }

    #[test]
    fn sustained_ratchet_strokes_never_read_as_free_spin() {
        let mut detector = FreeSpinDetector::default();
        let mut phases = Vec::new();
        let mut at = Instant::now();
        // A second of 80 notches/s strokes, each 150 ms, with the 80 ms
        // finger repositioning between them that ratchet scrolling has.
        for _ in 0..5 {
            let (end, stroke) = spin(&mut detector, at, |_| 80.0, Duration::from_millis(150));
            phases.extend(stroke);
            at = end + Duration::from_millis(80);
        }
        assert!(phases.iter().all(|p| *p == WheelPhase::Ratchet));
    }

    #[test]
    fn a_slow_free_spin_stopped_by_hand_is_caught() {
        let mut detector = FreeSpinDetector::default();
        let start = Instant::now();
        let (next_poll, _) = spin(&mut detector, start, |_| 70.0, Duration::from_millis(800));
        assert_eq!(detector.phase(), WheelPhase::FreeSpin);
        let mut at = next_poll;
        while !matches!(detector.tick(at), WheelPhase::Stopped(_)) {
            at += Duration::from_millis(1);
            assert!(
                at - next_poll < Duration::from_millis(100),
                "slow stop not detected"
            );
        }
    }

    /// Near the end of a spin-down, below the speeds the notch-count rules
    /// watch, a finger stop still ends the glide promptly.
    #[test]
    fn a_stop_at_the_end_of_a_spin_down_is_caught() {
        let mut detector = FreeSpinDetector::default();
        let start = Instant::now();
        // 200 notches/s slowing by friction for 5 s: down to ~35 notches/s.
        let (next_poll, phases) = spin(
            &mut detector,
            start,
            |t| 200.0 - 33.0 * t,
            Duration::from_secs(5),
        );
        assert!(!phases.iter().any(|p| matches!(p, WheelPhase::Stopped(_))));
        assert_eq!(detector.phase(), WheelPhase::FreeSpin);
        let mut at = next_poll;
        while !matches!(detector.tick(at), WheelPhase::Stopped(_)) {
            at += Duration::from_millis(1);
            assert!(
                at - next_poll < Duration::from_millis(120),
                "stop at the end of the spin-down not detected"
            );
        }
    }

    /// The same with high-resolution reports, which arrive every poll with
    /// a fraction of a notch: the stop shows within a few missed polls.
    #[test]
    fn a_high_resolution_stop_at_the_end_of_a_spin_down_is_caught_fast() {
        let mut detector = FreeSpinDetector::default();
        let start = Instant::now();
        let mut at = start;
        while at - start < Duration::from_secs(5) {
            let speed = 200.0 - 33.0 * (at - start).as_secs_f64();
            detector.report(at, speed * POLL.as_secs_f64());
            at += POLL;
        }
        assert_eq!(detector.phase(), WheelPhase::FreeSpin);
        let stopped = at;
        while !matches!(detector.tick(at), WheelPhase::Stopped(_)) {
            at += Duration::from_millis(1);
            assert!(
                at - stopped < Duration::from_millis(45),
                "high-resolution stop not detected"
            );
        }
    }

    #[test]
    fn a_new_scroll_after_a_stop_is_not_swallowed() {
        let mut detector = FreeSpinDetector::default();
        let start = Instant::now();
        let (next_poll, _) = spin(&mut detector, start, |_| 200.0, Duration::from_millis(600));
        let mut at = next_poll;
        while !matches!(detector.tick(at), WheelPhase::Stopped(_)) {
            at += Duration::from_millis(1);
        }
        // A settling tick right after the stop stays swallowed...
        assert!(matches!(
            detector.report(at + Duration::from_millis(5), 1.0),
            WheelPhase::Stopped(_)
        ));
        // ...a scroll the other way starts a new gesture at once...
        assert_eq!(
            detector.report(at + Duration::from_millis(10), -1.0),
            WheelPhase::Ratchet
        );
    }

    /// A free wheel cannot turn back on its own: a report the other way
    /// mid-spin, with no stop detected first, is a finger reversing the
    /// wheel and starts a new gesture. It must not count toward the old
    /// spin's speed and keep it free.
    #[test]
    fn a_reversal_during_a_free_spin_starts_a_new_gesture() {
        let mut detector = FreeSpinDetector::default();
        let start = Instant::now();
        let (next_poll, _) = spin(&mut detector, start, |_| 200.0, Duration::from_millis(400));
        assert_eq!(detector.phase(), WheelPhase::FreeSpin);
        assert_eq!(detector.report(next_poll, -2.0), WheelPhase::Ratchet);
        // The old spin's reports are gone: only the reversal is measured.
        let speed = detector.speed(next_poll);
        assert!(
            speed <= 2.0 / SPEED_WINDOW.as_secs_f64() + 1e-9,
            "the reversal kept the old spin's speed: {speed}"
        );
    }

    #[test]
    fn a_fast_flick_is_free_spin_within_a_hundred_milliseconds() {
        let mut detector = FreeSpinDetector::default();
        let start = Instant::now();
        let (_, phases) = spin(&mut detector, start, |_| 200.0, Duration::from_millis(120));
        assert_eq!(phases.last(), Some(&WheelPhase::FreeSpin));
    }

    #[test]
    fn a_natural_spin_down_is_never_a_stop() {
        let mut detector = FreeSpinDetector::default();
        // Linear friction: 200 notches/s down to 0 over 6 s.
        let (end, phases) = spin(
            &mut detector,
            Instant::now(),
            |t| (200.0 - 33.0 * t).max(0.0),
            Duration::from_secs(6),
        );
        assert!(
            !phases.iter().any(|p| matches!(p, WheelPhase::Stopped(_))),
            "natural decay judged a stop"
        );
        // Quiet afterwards ends the gesture.
        assert_eq!(detector.tick(end + GESTURE_END + POLL), WheelPhase::Ratchet);
    }

    #[test]
    fn a_finger_stop_is_caught_within_fifty_milliseconds() {
        let mut detector = FreeSpinDetector::default();
        let start = Instant::now();
        let (stopped_at, _) = spin(&mut detector, start, |_| 200.0, Duration::from_millis(800));
        assert_eq!(detector.phase(), WheelPhase::FreeSpin);
        let mut at = stopped_at;
        while !matches!(detector.tick(at), WheelPhase::Stopped(_)) {
            at += Duration::from_millis(1);
            assert!(
                at - stopped_at < Duration::from_millis(50),
                "stop not detected"
            );
        }
    }

    #[test]
    fn one_dropped_report_is_not_a_stop() {
        let mut detector = FreeSpinDetector::default();
        let start = Instant::now();
        let (next_poll, _) = spin(&mut detector, start, |_| 260.0, Duration::from_millis(800));
        // At 260 notches/s every poll reports, so the last report was one
        // poll ago. 30 ms of radio silence after it, then the wheel reports
        // again at full speed.
        let last_report = next_poll.checked_sub(POLL).expect("past the start");
        let after_gap = last_report + Duration::from_millis(30);
        for step in 1..30 {
            assert_eq!(
                detector.tick(last_report + Duration::from_millis(step)),
                WheelPhase::FreeSpin
            );
        }
        assert_eq!(detector.report(after_gap, 8.0), WheelPhase::FreeSpin);
    }

    #[test]
    fn a_gentle_brake_is_caught_by_deceleration() {
        let mut detector = FreeSpinDetector::default();
        let start = Instant::now();
        let (braking, _) = spin(&mut detector, start, |_| 150.0, Duration::from_millis(800));
        // 150 → 40 notches/s over 150 ms: far beyond friction.
        let (_, phases) = spin(
            &mut detector,
            braking,
            |t| (150.0 - 730.0 * t).max(40.0),
            Duration::from_millis(250),
        );
        assert!(phases.iter().any(|p| matches!(p, WheelPhase::Stopped(_))));
    }

    /// Replay the recorded gestures through the detector on a 1 ms clock.
    /// Answers the gesture's label, the instant of its last report, and the
    /// first instant each phase was reached.
    fn replay(line: &str) -> (String, Instant, Option<Instant>, Option<Instant>) {
        let mut parts = line.split_whitespace();
        let label = parts.next().expect("label").to_owned();
        let start = Instant::now();
        let reports: Vec<(Instant, f64)> = parts
            .map(|item| {
                let (us, notches) = item.split_once(':').expect("µs:notches");
                (
                    start + Duration::from_micros(us.parse().expect("µs")),
                    notches.parse().expect("notches"),
                )
            })
            .collect();
        let last = reports.last().expect("one report").0;
        let mut detector = FreeSpinDetector::default();
        let (mut free_spin, mut stopped) = (None, None);
        let mut next = reports.iter().peekable();
        let mut at = start;
        while at <= last + Duration::from_millis(300) {
            let mut phase = detector.tick(at);
            while let Some((when, notches)) = next.next_if(|(when, _)| *when <= at) {
                phase = detector.report(*when, *notches);
            }
            match phase {
                WheelPhase::FreeSpin => {
                    free_spin.get_or_insert(at);
                }
                WheelPhase::Stopped(_) => {
                    stopped.get_or_insert(at);
                }
                WheelPhase::Ratchet => {}
            }
            at += Duration::from_millis(1);
        }
        (label, last, free_spin, stopped)
    }

    /// Every recorded gesture: no ratchet gesture reads as free spin, every
    /// free spin is detected, no natural spin-down is called a stop, and
    /// every finger stop is caught within 60 ms of the wheel's last report.
    #[test]
    fn recorded_gestures_are_classified_correctly() {
        let recordings = include_str!("free_spin_recordings.txt");
        let mut checked = 0;
        for line in recordings.lines().filter(|l| !l.starts_with('#')) {
            let (label, last, free_spin, stopped) = replay(line);
            checked += 1;
            if label.starts_with("ratchet") {
                assert!(free_spin.is_none(), "{label}: ratchet read as free spin");
                continue;
            }
            // The first gentle recording is a flick that never freed the wheel.
            if label == "free_spin_gentle" && free_spin.is_none() {
                continue;
            }
            assert!(free_spin.is_some(), "{label}: free spin not detected");
            let early = stopped.filter(|at| *at + Duration::from_millis(150) < last);
            assert!(early.is_none(), "{label}: stopped mid-spin");
            if label == "free_spin_stop_finger" {
                let at = stopped.unwrap_or_else(|| panic!("{label}: stop not detected"));
                assert!(
                    at.saturating_duration_since(last) <= Duration::from_millis(60),
                    "{label}: stop detected late"
                );
            }
        }
        assert_eq!(checked, 66);
    }

    /// The detector's phase on every millisecond of a recorded line, from
    /// its first report to 300 ms past its last. Answers the label, the
    /// millisecond of the last report, and the phases.
    fn timeline(line: &str) -> (String, usize, Vec<WheelPhase>) {
        let mut parts = line.split_whitespace();
        let label = parts.next().expect("label").to_owned();
        let reports: Vec<(u64, f64)> = parts
            .map(|item| {
                let (us, notches) = item.split_once(':').expect("µs:notches");
                (us.parse().expect("µs"), notches.parse().expect("notches"))
            })
            .collect();
        let last_ms = usize::try_from(reports.last().expect("one report").0 / 1000).expect("ms");
        let start = Instant::now();
        let mut detector = FreeSpinDetector::default();
        let mut next = reports.iter().peekable();
        let mut phases = Vec::with_capacity(last_ms + 300);
        for ms in 0..=(last_ms + 300) {
            let at = start + Duration::from_millis(u64::try_from(ms).expect("ms"));
            let mut phase = detector.tick(at);
            while let Some((us, notches)) = next.next_if(|(us, _)| *us / 1000 <= ms as u64) {
                phase = detector.report(start + Duration::from_micros(*us), *notches);
            }
            phases.push(phase);
        }
        (label, last_ms, phases)
    }

    /// A natural spin-down only slows, so it never reads as a finger driving
    /// the wheel: once free, it stays free until its end.
    #[test]
    fn natural_spin_downs_are_never_read_as_driven() {
        let recordings = include_str!("free_spin_recordings.txt");
        for line in recordings.lines().filter(|l| !l.starts_with('#')) {
            let (label, last_ms, phases) = timeline(line);
            if !matches!(
                label.as_str(),
                "free_spin_gentle" | "free_spin_medium" | "free_spin_hard"
            ) {
                continue;
            }
            let Some(first_free) = phases.iter().position(|p| *p == WheelPhase::FreeSpin) else {
                continue;
            };
            let relapse = phases[first_free..last_ms.saturating_sub(150)]
                .iter()
                .position(|p| *p == WheelPhase::Ratchet);
            let relapsed = relapse.is_some();
            assert!(!relapsed, "{label}: natural spin read as driven");
        }
    }

    /// The recorded session: every flick still becomes a free spin,
    /// and the slow, finger-driven scrolling after them stays out of free
    /// spin — it was read as one and its output faded to almost nothing.
    #[test]
    fn recorded_session_keeps_driven_scrolling_out_of_free_spin() {
        let session = include_str!("free_spin_session.txt");
        let lines: Vec<&str> = session.lines().filter(|l| !l.starts_with('#')).collect();
        let (_, _, flicks) = timeline(lines[0]);
        let onsets = flicks
            .windows(2)
            .filter(|w| w[0] != WheelPhase::FreeSpin && w[1] == WheelPhase::FreeSpin)
            .count();
        assert!(onsets >= 20, "only {onsets} free-spin onsets in 24 flicks");
        let (_, _, driven) = timeline(lines[1]);
        let free_ms = driven
            .iter()
            .filter(|p| **p == WheelPhase::FreeSpin)
            .count();
        assert!(
            free_ms * 20 < driven.len(),
            "driven scrolling spent {free_ms} of {} ms in free spin",
            driven.len()
        );
    }
}
