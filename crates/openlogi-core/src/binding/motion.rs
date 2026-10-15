//! Shared per-report movement response for directional gestures and wheel actions.

/// Transform one signed report, truncating fractional counts toward zero.
/// Large OS reports saturate rather than overflowing; accumulation happens later.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    reason = "the response contract truncates after bounding to the signed count range"
)]
pub fn accelerate_report_delta(count: i32) -> i32 {
    let magnitude = f64::from(count).abs().powf(1.4);
    let signed = if count < 0 { -magnitude } else { magnitude };
    signed.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::accelerate_report_delta;

    #[test]
    fn report_response_preserves_sign_and_truncates() {
        for (raw, expected) in [(0, 0), (1, 1), (-1, -1), (3, 4), (-3, -4)] {
            assert_eq!(accelerate_report_delta(raw), expected);
        }
        assert_eq!(accelerate_report_delta(i32::MAX), i32::MAX);
        assert_eq!(accelerate_report_delta(i32::MIN), i32::MIN);
    }

    #[test]
    fn separate_reports_are_transformed_before_accumulation() {
        assert_eq!(accelerate_report_delta(2) + accelerate_report_delta(2), 4);
        assert_eq!(accelerate_report_delta(4), 6);
    }
}
