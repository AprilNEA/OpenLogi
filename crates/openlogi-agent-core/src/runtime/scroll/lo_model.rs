//! Pure model of the mouse-wheel behaviour recorded from Logi Options+ on macOS.
//!
//! Each wheel report becomes one pixel delta, shaped only by an EMA-smoothed
//! speed that selects a gain; there is no timer, easing or momentum. The
//! structure and constants were matched against recorded Options+ event traces
//! and are validated by the replay tests below (their fixtures live in
//! `fixtures/`), not by anything the traces cannot show: the hysteresis
//! constants and the curve above the speeds the traces reach are assumptions,
//! flagged where they are declared. Nothing here reads a clock or does I/O;
//! callers pass `Instant`s. Ratchet detection is not modelled, and neither is
//! the way Options+ flushes its line accumulator (see `take_line_delta`).
//!
//! Timing caveat: the EMA resets on any gap of 100 ms or more, so a wheel that
//! ticks at 10 notches per second or slower always runs at gain 1 and one
//! notch is a fixed ~13 px; the gain only builds up above that rate. On the
//! hook path the interval comes from the instant the tap callback ran rather
//! than from the OS event's own timestamp, so events delivered in one batch
//! collapse onto the 8 ms floor and read as a faster flick than they were.

use std::time::Instant;

use openlogi_core::scroll::{WHEEL_PIXELS_PER_NOTCH, WHEEL_REFERENCE_DPI};

/// Lower clamp of the per-report period in seconds.
const PERIOD_MIN_SECS: f64 = 0.008;
/// Upper clamp of the per-report period, and the gap at which the EMA resets.
const PERIOD_MAX_SECS: f64 = 0.1;
/// EMA retention and new-sample weight.
const EMA_HISTORY_WEIGHT: f64 = 0.95;
const EMA_SAMPLE_WEIGHT: f64 = 0.05;
/// The speed setting in [-1,1] maps linearly onto this range.
const SPEED_MIN: f64 = 0.06;
const SPEED_MAX: f64 = 1.2;
/// Curve thresholds in EMA inches/second. The replayed traces stay far below
/// the upper thresholds (their smoothed speed peaks near 13 inches/second), so
/// the 60 and 120 breakpoints and the falling ramp between them are
/// extrapolated, not measured.
const CURVE_X: [f64; 5] = [0.0, 0.3, 0.6, 60.0, 120.0];
/// Base gains at `CURVE_X`, each scaled to `1 + (G - 1) * s`.
const CURVE_Y: [f64; 5] = [1.0, 1.0, 12.0, 12.0, 6.0];
/// Pixels per line unit.
const PIXELS_PER_LINE: f64 = 10.0;
/// Hi-res steps per notch the hysteresis counts in (the wheel's `0x2121`
/// multiplier, 15 on the MX Master 3S).
const STEPS_PER_NOTCH: f64 = 15.0;
/// Steps of dead band held after an idle gap, and the cap on how much travel
/// is remembered. Both are assumptions: the effect is a step or two, about a
/// pixel, and scored by per-burst pixel totals on two recordings of Options+,
/// variants from none to a base of 4 differ by under 0.1%, so the data neither
/// confirms nor rules out base 1 and cap 1. The point of carrying them is the
/// unit: a reversal loses at most one hi-res step, not the whole notches a
/// notch-based filter would swallow.
const HYSTERESIS_BASE_STEPS: f64 = 1.0;
const HYSTERESIS_CAP_STEPS: f64 = 1.0;
/// An idle gap this long relaxes the accumulators, one step per whole period.
const HYSTERESIS_RELAX_SECS: f64 = 1.0;
/// Steps below this are rounding dust, not wheel movement.
const STEP_EPSILON: f64 = 1e-6;
/// A report larger than this cannot come from a wheel and is dropped like a
/// non-finite one. It also keeps every product below inside `f64` and `i32`
/// range, so no input can turn the state into NaN.
const MAX_REPORT_NOTCHES: f64 = 1e6;

/// Direction-change dead band, in hi-res steps.
///
/// Travel in one direction is remembered (up to a cap) and cancels the first
/// steps of travel the other way; after an idle gap both memories relax to the
/// base, so the first step of any new movement is swallowed.
#[derive(Clone, Debug, Default)]
struct StepHysteresis {
    negative: f64,
    positive: f64,
    last: Option<Instant>,
}

impl StepHysteresis {
    fn relax_toward_base(value: f64, steps: f64) -> f64 {
        if value >= HYSTERESIS_BASE_STEPS {
            (value - steps).max(HYSTERESIS_BASE_STEPS)
        } else {
            (value + steps).min(HYSTERESIS_BASE_STEPS)
        }
    }

    /// Filter signed `steps` arriving at `now`; the result keeps the sign (or
    /// is zero) and is never larger in magnitude.
    fn filter(&mut self, steps: f64, now: Instant) -> f64 {
        if let Some(last) = self.last {
            let gap = now.saturating_duration_since(last).as_secs_f64();
            if gap >= HYSTERESIS_RELAX_SECS {
                let relax = (gap / HYSTERESIS_RELAX_SECS).floor();
                self.negative = Self::relax_toward_base(self.negative, relax);
                self.positive = Self::relax_toward_base(self.positive, relax);
            }
        }
        self.last = Some(now);
        if steps > 0.0 {
            self.positive = (self.positive + steps).min(HYSTERESIS_CAP_STEPS);
            let used = self.negative.min(steps);
            self.negative -= used;
            steps - used
        } else if steps < 0.0 {
            self.negative = (self.negative - steps).min(HYSTERESIS_CAP_STEPS);
            let used = self.positive.min(-steps);
            self.positive -= used;
            steps + used
        } else {
            0.0
        }
    }
}

/// Result of one `LoWheelModel::process` call.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct LoWheelOutput {
    /// Whole pixels to post this event (sub-pixel remainder is carried).
    pub(super) pixels: i32,
    /// Line delta (`accumulated pixels / 10`, at least 1 in magnitude when
    /// non-zero); zero while fewer than 10 pixels have accumulated. Posted as
    /// `trunc` in the integer line field and `round` in the fixed-point field,
    /// which is what the recorded Options+ events carry.
    pub(super) line_delta: f64,
    /// Gain applied to this report (diagnostics).
    pub(super) gain: f64,
    /// EMA of |finger inches/second| after this report (diagnostics).
    pub(super) ema_ips: f64,
}

/// Stateful wheel model for one axis.
#[derive(Clone, Debug)]
pub(super) struct LoWheelModel {
    speed_scalar: f64,
    ema_ips: f64,
    last_event: Option<Instant>,
    pixel_remainder: f64,
    line_pixels: i64,
    hysteresis: StepHysteresis,
}

impl LoWheelModel {
    /// `speed_setting` is the UI value in [-1,1] (clamped).
    pub(super) fn new(speed_setting: f64) -> Self {
        Self {
            speed_scalar: speed_scalar(speed_setting),
            ema_ips: 0.0,
            last_event: None,
            pixel_remainder: 0.0,
            line_pixels: 0,
            hysteresis: StepHysteresis::default(),
        }
    }

    /// Update the user speed setting in [-1,1] without touching history.
    pub(super) fn set_speed_setting(&mut self, speed_setting: f64) {
        self.speed_scalar = speed_scalar(speed_setting);
    }

    /// Process one wheel report of signed `notches` (fractional: a device
    /// reporting 15 hi-res steps per notch passes `steps / 15`, and the
    /// multiplier cancels out of the physics), arriving at `now`.
    ///
    /// Zero, non-finite or implausibly large (over `MAX_REPORT_NOTCHES`)
    /// reports, and reports the direction-change dead band swallows, return
    /// zero output without touching the speed estimate: like Options+, they
    /// neither post nor count as movement.
    pub(super) fn process(&mut self, notches: f64, now: Instant) -> LoWheelOutput {
        if !notches.is_finite() || notches.abs() > MAX_REPORT_NOTCHES {
            return self.silent();
        }
        let steps = self.hysteresis.filter(notches * STEPS_PER_NOTCH, now);
        if steps.abs() < STEP_EPSILON {
            return self.silent();
        }

        let period = self.advance_clock(now);
        // Pixels this report moves at unit gain; the model's speed is the
        // same distance in inches over the report period.
        let unit_pixels = steps / STEPS_PER_NOTCH * WHEEL_PIXELS_PER_NOTCH;
        let finger_ips = unit_pixels / WHEEL_REFERENCE_DPI / period;
        self.ema_ips = EMA_HISTORY_WEIGHT * self.ema_ips + EMA_SAMPLE_WEIGHT * finger_ips.abs();
        let gain = curve_gain(self.ema_ips, self.speed_scalar);

        let exact = unit_pixels * gain + self.pixel_remainder;
        // The integer part is posted, the fractional part carried.
        let whole = exact.trunc();
        self.pixel_remainder = exact - whole;
        let pixels = trunc_to_i32(whole);

        self.line_pixels += i64::from(pixels);
        LoWheelOutput {
            pixels,
            line_delta: self.take_line_delta(),
            gain,
            ema_ips: self.ema_ips,
        }
    }

    /// The output of a report that posts nothing and leaves the state alone.
    fn silent(&self) -> LoWheelOutput {
        LoWheelOutput {
            pixels: 0,
            line_delta: 0.0,
            gain: 1.0,
            ema_ips: self.ema_ips,
        }
    }

    /// Record a report at `now` and return its period, resetting the EMA
    /// after a gap. The first report has no predecessor: the EMA starts at 0
    /// and the period is the maximum.
    fn advance_clock(&mut self, now: Instant) -> f64 {
        let period = match self.last_event {
            Some(previous) => {
                let gap = now.saturating_duration_since(previous).as_secs_f64();
                if gap >= PERIOD_MAX_SECS {
                    self.ema_ips = 0.0;
                }
                gap.clamp(PERIOD_MIN_SECS, PERIOD_MAX_SECS)
            }
            None => PERIOD_MAX_SECS,
        };
        self.last_event = Some(now);
        period
    }

    /// Simplified flush of the line accumulator: Options+ flushes on a
    /// threshold the traces do not explain, so this flushes once
    /// |pixels| >= 10. Totals match the recorded lines; the flush cadence does
    /// not.
    fn take_line_delta(&mut self) -> f64 {
        // Pixel counts are small; exact in f64.
        #[expect(
            clippy::cast_precision_loss,
            reason = "accumulator stays far below 2^52"
        )]
        let lines = self.line_pixels as f64 / PIXELS_PER_LINE;
        if lines.abs() < 1.0 {
            return 0.0;
        }
        self.line_pixels = 0;
        lines
    }
}

/// `s = lo + (v - min)/(max - min) * (hi - lo)` with v clamped to [-1,1].
fn speed_scalar(setting: f64) -> f64 {
    let clamped = if setting.is_finite() {
        setting.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    SPEED_MIN + f64::midpoint(clamped, 1.0) * (SPEED_MAX - SPEED_MIN)
}

/// Gain for smoothed speed `ema_ips` and speed scalar `s`.
///
/// Piecewise linear over `CURVE_X`: 0.3-0.6 ramps up, 0.6-60 is flat, 60-120
/// ramps down to `1 + 5s`. At or above 120 ips the gain holds that value: no
/// trace reaches such speeds, and a steeper fall could stop or reverse the
/// wheel, which a flick must never do. The region is unreachable for a real
/// wheel (120 ips is over 900 notches per second), but the hook path feeds OS
/// line deltas that can be many notches per event.
fn curve_gain(ema_ips: f64, s: f64) -> f64 {
    let gain_at = |index: usize| 1.0 + (CURVE_Y[index] - 1.0) * s;
    let speed = ema_ips.abs();
    for segment in 1..CURVE_X.len() {
        if speed < CURVE_X[segment] {
            let t = (speed - CURVE_X[segment - 1]) / (CURVE_X[segment] - CURVE_X[segment - 1]);
            return gain_at(segment - 1) + t * (gain_at(segment) - gain_at(segment - 1));
        }
    }
    gain_at(CURVE_Y.len() - 1)
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "saturating float-to-int, value pre-truncated"
)]
fn trunc_to_i32(whole: f64) -> i32 {
    whole as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Notches a 15-steps-per-notch device reports for `steps` hi-res steps.
    fn notches(steps: i64) -> f64 {
        #[expect(clippy::cast_precision_loss, reason = "test steps are tiny")]
        let steps = steps as f64;
        steps / 15.0
    }

    #[test]
    fn speed_mapping_covers_the_documented_range() {
        assert!((speed_scalar(-1.0) - 0.06).abs() < 1e-12);
        assert!((speed_scalar(1.0) - 1.2).abs() < 1e-12);
        assert!((speed_scalar(0.0) - 0.63).abs() < 1e-12);
        assert!((speed_scalar(5.0) - 1.2).abs() < 1e-12);
        assert!((speed_scalar(f64::NAN) - 0.63).abs() < 1e-12);
    }

    #[test]
    fn slow_single_tick_is_about_one_notch_at_gain_one() {
        let mut model = LoWheelModel::new(0.0);
        let out = model.process(1.0, Instant::now());
        // 12.963 px per notch; first event T=0.1 -> ema 0.065 < 0.3.
        assert!((out.gain - 1.0).abs() < 1e-12);
        assert_eq!(out.pixels, 12);
        assert!((model.pixel_remainder - 0.963).abs() < 1e-3);
    }

    #[test]
    fn default_speed_reaches_plateau_gain() {
        let mut model = LoWheelModel::new(0.0);
        let t0 = Instant::now();
        let mut last = None;
        for i in 0..200u32 {
            last = Some(model.process(1.0, t0 + Duration::from_millis(10 * u64::from(i))));
        }
        let out = last.unwrap();
        assert!((out.gain - 7.93).abs() < 1e-9, "gain {}", out.gain);
        // One notch every 10 ms against the reference resolution.
        let expected = 100.0 * WHEEL_PIXELS_PER_NOTCH / WHEEL_REFERENCE_DPI;
        assert!((out.ema_ips - expected).abs() < 0.05);
    }

    #[test]
    fn ema_resets_after_gap() {
        let mut model = LoWheelModel::new(0.0);
        let t0 = Instant::now();
        for i in 0..100u32 {
            model.process(1.0, t0 + Duration::from_millis(10 * u64::from(i)));
        }
        assert!(model.ema_ips > 0.6);
        let out = model.process(1.0, t0 + Duration::from_millis(990 + 100));
        // Reset then one sample: 0.05 * (0.1296/0.1).
        assert!((out.ema_ips - 0.05 * 1.2963).abs() < 1e-3);
        assert!((out.gain - 1.0).abs() < 1e-12);
    }

    /// The tick-rate cliff documented on the module: at 10 notches per second
    /// or slower every gap is >= 100 ms, so the gain never leaves 1.
    #[test]
    fn ten_ticks_per_second_or_slower_stays_at_gain_one() {
        let mut model = LoWheelModel::new(1.0);
        let t0 = Instant::now();
        for i in 0..50u64 {
            let out = model.process(1.0, t0 + Duration::from_millis(100 * i));
            assert!((out.gain - 1.0).abs() < 1e-12, "tick {i}: {}", out.gain);
        }
    }

    #[test]
    fn remainder_carry_conserves_total() {
        let mut model = LoWheelModel::new(-1.0);
        let t0 = Instant::now();
        let mut total = 0i64;
        let mut exact = 0.0;
        for i in 0..50u32 {
            // Slow: 200 ms apart, 3/8 notch each, gain stays 1.
            let out = model.process(3.0 / 8.0, t0 + Duration::from_millis(200 * u64::from(i)));
            total += i64::from(out.pixels);
            exact += 3.0 / 8.0 * WHEEL_PIXELS_PER_NOTCH;
        }
        let carried = f64::from(i32::try_from(total).unwrap()) + model.pixel_remainder;
        assert!((carried - exact).abs() < 1e-9, "{carried} vs {exact}");
    }

    #[test]
    fn sign_follows_notches_and_truncates_toward_zero() {
        let mut model = LoWheelModel::new(0.0);
        let out = model.process(-1.0, Instant::now());
        assert_eq!(out.pixels, -12);
        assert!(model.pixel_remainder < 0.0);
        assert!(out.ema_ips > 0.0);
    }

    #[test]
    fn zero_and_non_finite_notches_leave_the_state_alone() {
        let mut model = LoWheelModel::new(0.0);
        let t0 = Instant::now();
        for bad in [0.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(model.process(bad, t0).pixels, 0);
        }
        assert!(model.last_event.is_none());
    }

    /// Finite but absurd reports (a corrupt hook delta) overflow every
    /// intermediate if let through; they must neither post nor poison the
    /// state, which keeps working afterwards.
    #[test]
    fn huge_finite_reports_are_dropped_without_poisoning_the_state() {
        let mut model = LoWheelModel::new(1.0);
        let t0 = Instant::now();
        for huge in [f64::MAX, -f64::MAX, 1e300, -1e200, 2.0 * MAX_REPORT_NOTCHES] {
            let out = model.process(huge, t0);
            assert_eq!((out.pixels, out.line_delta), (0, 0.0), "{huge}");
            assert!(out.ema_ips.is_finite() && out.gain.is_finite());
        }
        assert!(model.last_event.is_none());
        assert!(model.pixel_remainder.abs() < f64::EPSILON);
        let out = model.process(1.0, t0);
        assert_eq!(out.pixels, 12);
        assert!(model.ema_ips.is_finite() && model.pixel_remainder.is_finite());
    }

    /// The largest accepted report stays finite end to end and fits the pixel
    /// type, at the highest speed setting.
    #[test]
    fn the_largest_accepted_report_stays_in_range() {
        for sign in [1.0, -1.0] {
            let mut model = LoWheelModel::new(1.0);
            let out = model.process(sign * MAX_REPORT_NOTCHES, Instant::now());
            assert!(out.pixels != 0 && (out.pixels > 0) == (sign > 0.0));
            assert!(out.pixels.abs() < i32::MAX, "{}", out.pixels);
            assert!(out.gain.is_finite() && out.ema_ips.is_finite());
        }
    }

    #[test]
    fn line_delta_accumulates_pixels_over_ten() {
        let mut model = LoWheelModel::new(0.0);
        let t0 = Instant::now();
        let a = model.process(1.0, t0);
        assert_eq!((a.pixels, a.line_delta), (12, 1.2));
        // The reversal loses the one step the hysteresis remembers (14 of 15
        // steps move), and the 0.96 px carried from the first report adds
        // back: -12.09 + 0.96 truncates to -11.
        let b = model.process(-1.0, t0 + Duration::from_millis(500));
        assert_eq!((b.pixels, b.line_delta), (-11, -1.1));
    }

    #[test]
    fn hysteresis_swallows_one_step_of_a_reversal_and_of_a_start_after_idle() {
        let mut h = StepHysteresis::default();
        let t0 = Instant::now();
        // A fresh wheel passes everything.
        assert_eq!(h.filter(3.0, t0), 3.0);
        // Reversing straight away loses the step the first move remembered.
        assert_eq!(h.filter(-3.0, t0 + Duration::from_millis(10)), -2.0);
        // That memory is spent: the next reverse report passes whole.
        assert_eq!(h.filter(-3.0, t0 + Duration::from_millis(20)), -3.0);
        // After an idle second both memories relax to one step, so the first
        // step of the next movement in either direction is swallowed.
        assert_eq!(h.filter(3.0, t0 + Duration::from_millis(2_000)), 2.0);
        assert_eq!(h.filter(0.0, t0 + Duration::from_millis(2_010)), 0.0);
    }

    #[test]
    fn a_report_swallowed_by_hysteresis_does_not_feed_the_speed_estimate() {
        let mut model = LoWheelModel::new(0.0);
        let t0 = Instant::now();
        model.process(0.2, t0);
        let ema = model.ema_ips;
        // One step back right after three forward: exactly the remembered step.
        let swallowed = model.process(-1.0 / 15.0, t0 + Duration::from_millis(8));
        assert_eq!(swallowed.pixels, 0);
        assert_eq!(model.last_event, Some(t0));
        assert_eq!(model.ema_ips, ema);
    }

    #[test]
    fn gain_is_monotonic_in_the_rising_and_falling_regions() {
        let s = speed_scalar(0.0);
        let mut prev = curve_gain(0.0, s);
        let mut v = 0.0;
        while v < 59.9 {
            v += 0.05;
            let g = curve_gain(v, s);
            assert!(g >= prev - 1e-12, "rising region at {v}");
            prev = g;
        }
        v = 60.0;
        prev = curve_gain(v, s);
        while v < 119.99 {
            v += 0.05;
            let g = curve_gain(v, s);
            assert!(g <= prev + 1e-12, "falling region at {v}");
            prev = g;
        }
    }

    #[test]
    fn curve_segments_match_table() {
        let s = 0.5;
        assert!((curve_gain(0.29, s) - 1.0).abs() < 1e-12);
        assert!((curve_gain(0.45, s) - (1.0 + 5.5 / 2.0)).abs() < 1e-9);
        assert!((curve_gain(30.0, s) - 6.5).abs() < 1e-12);
        // Midpoint of the 60-120 ramp: halfway between 1+11s=6.5 and 1+5s=3.5.
        assert!((curve_gain(90.0, s) - 5.0).abs() < 1e-9);
    }

    /// Past the end of the falling ramp the gain holds the value the ramp ends
    /// on, `1 + 5s`, and is always positive.
    #[test]
    fn at_or_above_120_ips_gain_holds_the_end_of_the_falling_ramp() {
        for s in [SPEED_MIN, 0.5, 0.63, SPEED_MAX] {
            let held = 1.0 + 5.0 * s;
            assert!((curve_gain(119.999, s) - held).abs() < 0.01);
            assert!((curve_gain(120.0, s) - held).abs() < 1e-12);
            assert!((curve_gain(500.0, s) - held).abs() < 1e-12);
            assert!(curve_gain(1e6, s) > 0.0);
        }
    }

    /// A sustained hook-style burst (OS line deltas of several notches per
    /// event at the 8 ms floor) drives the EMA far past 120 ips. It must keep
    /// its direction and never emit an opposite-sign pixel.
    #[test]
    fn sustained_fast_burst_keeps_its_direction() {
        for (sign, speed) in [(1.0, 0.0), (-1.0, 0.0), (1.0, 1.0), (-1.0, 1.0)] {
            let mut model = LoWheelModel::new(speed);
            let t0 = Instant::now();
            let mut peak: f64 = 0.0;
            for i in 0..400u64 {
                let out = model.process(sign * 10.0, t0 + Duration::from_millis(8 * i));
                peak = peak.max(out.ema_ips);
                assert!(out.gain > 0.0, "gain {} at {i}", out.gain);
                assert!(
                    out.pixels == 0 || (out.pixels > 0) == (sign > 0.0),
                    "report {i} emitted {} for sign {sign}",
                    out.pixels
                );
            }
            assert!(peak >= 120.0, "burst never reached the held region: {peak}");
        }
    }

    /// Replays recorded Options+ reports (see each CSV header) and compares
    /// with the pixels it actually posted. Rows with a blank pixel column had
    /// no paired event and are only fed to the model. Every slice starts at a
    /// gap of 100 ms or more, so the model starts from the same empty history.
    /// Row 206 of `lo_plus_reversal.csv`: Options+ posts 2 px for a 2-step
    /// report 8 ms after a 5-step one and the model posts 14. Unexplained, so
    /// it is kept in the fixture, fed to the model and asserted to still
    /// diverge, but left out of the totals.
    const KNOWN_DIVERGENCES: [usize; 1] = [206];

    struct Replay {
        rows: usize,
        model_pixels: i64,
        capture_pixels: i64,
        /// Sum of the integer line field the model would post:
        /// `trunc(line_delta)` per event.
        model_posted_lines: i64,
        capture_lines: i64,
        worst: i64,
        reversals: usize,
        mid_stream_gaps: usize,
    }

    fn replay(csv: &str, speed: f64, known_divergences: &[usize]) -> Replay {
        let mut model = LoWheelModel::new(speed);
        let mut at = Instant::now();
        let mut out = Replay {
            rows: 0,
            model_pixels: 0,
            capture_pixels: 0,
            model_posted_lines: 0,
            capture_lines: 0,
            worst: 0,
            reversals: 0,
            mid_stream_gaps: 0,
        };
        let mut previous_steps = 0i64;
        for line in csv.lines().filter(|line| !line.starts_with('#')) {
            let mut fields = line.split(',');
            let mut next = || fields.next().unwrap();
            let gap_ns: u64 = next().parse().unwrap();
            let steps: i64 = next().parse().unwrap();
            let expected_pixels = next().parse::<i64>().ok();
            let expected_lines = next().parse::<i64>().ok();

            at += Duration::from_nanos(gap_ns);
            if out.rows > 0 && gap_ns >= 100_000_000 {
                out.mid_stream_gaps += 1;
            }
            if previous_steps * steps < 0 {
                out.reversals += 1;
            }
            previous_steps = steps;

            let result = model.process(notches(steps), at);
            if known_divergences.contains(&out.rows) {
                // Still fed to the model so its state follows the capture, but
                // kept out of the totals and asserted to really differ.
                let expected = expected_pixels.unwrap();
                assert!(
                    (i64::from(result.pixels) - expected).abs() > 1,
                    "row {} no longer diverges: drop it from the known divergences",
                    out.rows
                );
            } else if let Some(expected) = expected_pixels {
                // The line total the injector posts is the truncated integer
                // field, not the unposted f64.
                out.model_posted_lines += posted_line_field(result.line_delta);
                out.capture_lines += expected_lines.unwrap_or(0);
                let pixels = i64::from(result.pixels);
                out.worst = out.worst.max((pixels - expected).abs());
                out.model_pixels += pixels;
                out.capture_pixels += expected;
                assert!(
                    (pixels - expected).abs() <= 1,
                    "row {}: model {pixels} vs Options+ {expected}",
                    out.rows
                );
            }
            out.rows += 1;
        }
        out
    }

    /// The integer line field `post_wheel_pixels` writes for `line_delta`.
    fn posted_line_field(line_delta: f64) -> i64 {
        #[expect(clippy::cast_possible_truncation, reason = "a few lines at most")]
        let field = line_delta.trunc() as i64;
        field
    }

    /// Totals within 0.5% of pixels and 10% of the posted line total (the line
    /// flush cadence is simplified), with matching signs.
    fn assert_totals(replay: &Replay) {
        assert!(
            (replay.model_pixels - replay.capture_pixels).abs() * 200 < replay.capture_pixels.abs(),
            "pixels {} vs {}",
            replay.model_pixels,
            replay.capture_pixels
        );
        assert!(
            (replay.model_posted_lines - replay.capture_lines).abs() * 10
                < replay.capture_lines.abs(),
            "posted lines {} vs {}",
            replay.model_posted_lines,
            replay.capture_lines
        );
    }

    #[test]
    fn replays_recorded_options_plus_events() {
        let replay = replay(include_str!("fixtures/lo_plus_replay.csv"), 0.0, &[]);
        assert_eq!(replay.rows, 200);
        assert_eq!((replay.reversals, replay.mid_stream_gaps), (0, 0));
        assert_totals(&replay);
    }

    /// One burst that scrolls down, reverses direction mid-stream and then
    /// pauses (>= 100 ms, EMA reset) three times.
    #[test]
    fn replays_a_reversal_and_mid_stream_gaps() {
        let replay = replay(
            include_str!("fixtures/lo_plus_reversal.csv"),
            0.0,
            &KNOWN_DIVERGENCES,
        );
        assert_eq!(replay.rows, 229);
        assert_eq!(replay.reversals, 1);
        assert_eq!(replay.mid_stream_gaps, 3);
        assert_totals(&replay);
    }

    /// A reversal across a gap, followed by further gaps.
    #[test]
    fn replays_a_reversal_across_a_gap() {
        let replay = replay(
            include_str!("fixtures/lo_plus_reversal_after_gap.csv"),
            0.0,
            &[],
        );
        assert_eq!(replay.rows, 60);
        assert!(replay.mid_stream_gaps >= 3);
        assert_totals(&replay);
    }
}
