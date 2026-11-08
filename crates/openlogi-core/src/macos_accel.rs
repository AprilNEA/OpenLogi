//! Apple IOHID parametric acceleration curve, ported from
//! ReneXiong/macos-trackpad-libinput. Generates a libinput custom
//! acceleration profile for Hyprland.

/// Apple linear gain coefficient.
pub const GLINEAR: f64 = 62915.0 / 65536.0;
/// Apple parabolic gain coefficient.
pub const GPARABOLIC: f64 = 54395.0 / 65536.0;
/// Apple cubic gain coefficient.
pub const GCUBIC: f64 = 7864.0 / 65536.0;
/// Apple quartic gain coefficient.
pub const GQUARTIC: f64 = 0.0 / 65536.0;
/// Lower threshold where the curve switches from polynomial to linear.
pub const T0: f64 = 465_306.0 / 65_536.0;
/// Upper threshold where the curve switches from linear to square-root.
pub const T1: f64 = 1_179_648.0 / 65_536.0;
/// Apple reference frame rate in Hz.
pub const APPLE_FRAME_RATE: f64 = 67.0;
/// Apple cursor scale factor.
pub const APPLE_CURSOR_SCALE: f64 = 96.0 / 67.0;
/// Default gain multiplier.
pub const DEFAULT_GAIN: f64 = 4.8;

/// Apple IOHID polynomial term.
#[must_use]
pub fn apple_polynomial(q: f64) -> f64 {
    GLINEAR * q + (GPARABOLIC * q).powi(2) + (GCUBIC * q).powi(3) + (GQUARTIC * q).powi(4)
}

/// Derivative of [`apple_polynomial`].
#[must_use]
pub fn apple_derivative(q: f64) -> f64 {
    GLINEAR
        + 2.0 * q * GPARABOLIC.powi(2)
        + 3.0 * q.powi(2) * GCUBIC.powi(3)
        + 4.0 * q.powi(3) * GQUARTIC.powi(4)
}

/// Apple parametric acceleration curve, piecewise continuous.
#[must_use]
pub fn apple_accel(q: f64) -> f64 {
    if q < T0 {
        return apple_polynomial(q);
    }
    let y0 = apple_polynomial(T0);
    let m0 = apple_derivative(T0);
    let b0 = y0 - m0 * T0;
    if q < T1 {
        return m0 * q + b0;
    }
    let y1 = m0 * T1 + b0;
    let m1 = 2.0 * y1 * m0;
    let b1 = y1.powi(2) - m1 * T1;
    (m1 * q + b1).sqrt()
}

/// Generate `(step, points)` for a libinput custom accel profile.
#[must_use]
pub fn generate(dpi: f64, report_rate: f64, step: f64, count: usize, gain: f64) -> (f64, Vec<f64>) {
    let mut points = Vec::with_capacity(count);
    for i in 0..count {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a LUT has a few dozen points, far below f64's 2^53 exact-integer range"
        )]
        let x = i as f64 * step;
        let q = x * 1000.0 * APPLE_FRAME_RATE / (dpi * report_rate);
        let a = apple_accel(q);
        let y = a * APPLE_CURSOR_SCALE * (report_rate / 1000.0) * gain;
        points.push(y);
    }
    (step, points)
}

/// Format points as a space-separated string for hyprctl.
#[must_use]
pub fn format_lut(step: f64, points: &[f64]) -> String {
    let pts: Vec<String> = points.iter().map(|p| format!("{p:.6}")).collect();
    format!("custom {} {}", step, pts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_accel_zero() {
        assert_eq!(apple_accel(0.0), 0.0);
    }

    #[test]
    fn monotonicity() {
        let (_, pts) = generate(1000.0, 120.0, 0.5, 64, 4.8);
        for w in pts.windows(2) {
            assert!(w[0] <= w[1] + 1e-12, "{} > {}", w[0], w[1]);
        }
    }

    #[test]
    fn continuity_t0() {
        let eps = 1e-9;
        let left = apple_accel(T0 - eps);
        let right = apple_accel(T0);
        assert!((left - right).abs() < 1e-6);
    }

    #[test]
    fn continuity_t1() {
        let eps = 1e-9;
        let left = apple_accel(T1 - eps);
        let right = apple_accel(T1);
        assert!((left - right).abs() < 1e-6);
    }

    #[test]
    fn count_64() {
        let (_, pts) = generate(1000.0, 120.0, 0.5, 64, 4.8);
        assert_eq!(pts.len(), 64);
    }

    #[test]
    fn first_point_zero() {
        let (_, pts) = generate(1000.0, 120.0, 0.5, 64, 4.8);
        assert_eq!(pts[0], 0.0);
    }

    #[test]
    fn format_lut_prefix() {
        let (_, pts) = generate(1000.0, 120.0, 0.5, 64, 4.8);
        let s = format_lut(0.5, &pts);
        assert!(s.starts_with("custom 0.5"), "got: {s}");
    }

    #[test]
    fn gain_scaling() {
        let (_, a) = generate(1000.0, 120.0, 0.5, 64, 1.0);
        let (_, b) = generate(1000.0, 120.0, 0.5, 64, 2.0);
        for (x, y) in a.iter().zip(b.iter()) {
            assert!((y - 2.0 * x).abs() < 1e-9, "{} vs {}", y, 2.0 * x);
        }
    }

    #[test]
    fn non_negative() {
        for i in 0..1000 {
            let q = f64::from(i) * 0.01;
            assert!(apple_accel(q) >= 0.0);
        }
    }
}
