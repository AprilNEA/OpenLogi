//! SmartShift write feedback against stale reads.

use super::*;

#[test]
fn smartshift_write_feedback_requires_the_written_value() {
    let expected = SmartShiftStatus {
        mode: SmartShiftMode::Ratchet,
        auto_disengage: SmartShiftAutoDisengage::Threshold(SmartShiftThreshold::from_rounded(12.0)),
        tunable_torque: None,
    };
    assert_eq!(smartshift_write_outcome(expected, None), None);
    assert_eq!(
        smartshift_write_outcome(expected, Some(&Load::Ready(Arc::new(expected)))),
        Some(ConfirmationOutcome::Confirmed)
    );
    assert_eq!(
        smartshift_write_outcome(
            expected,
            Some(&Load::Ready(Arc::new(SmartShiftStatus {
                auto_disengage: SmartShiftAutoDisengage::Threshold(
                    SmartShiftThreshold::from_rounded(13.0),
                ),
                ..expected
            }))),
        ),
        Some(ConfirmationOutcome::Failed)
    );
    assert_eq!(
        smartshift_write_outcome(
            expected,
            Some(&Load::<Arc<SmartShiftStatus>>::Failed(
                "timeout".to_string(),
            ))
        ),
        Some(ConfirmationOutcome::Failed)
    );
}

#[test]
fn stale_smartshift_reads_do_not_resolve_newer_writes() {
    let expected = SmartShiftStatus {
        mode: SmartShiftMode::Ratchet,
        auto_disengage: SmartShiftAutoDisengage::Threshold(SmartShiftThreshold::from_rounded(12.0)),
        tunable_torque: None,
    };
    let mut write = SmartShiftDeviceState::default();
    write.queue(expected, 2);

    assert!(
        !smartshift_read_is_current(Some(2), Some(&write)),
        "a tagged read cannot land before its confirmation request starts"
    );
    assert!(!smartshift_read_is_current(None, Some(&write)));
    assert_eq!(write.begin_confirmation(), Some(2));
    assert!(smartshift_read_is_current(Some(2), Some(&write)));
    assert!(!smartshift_read_is_current(Some(1), Some(&write)));
    assert!(!smartshift_read_is_current(None, Some(&write)));
    write.reset();
    assert!(!smartshift_read_is_current(Some(2), Some(&write)));
    assert!(smartshift_read_is_current(None, None));
}
