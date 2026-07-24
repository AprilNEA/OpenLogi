use std::assert_matches;

use super::*;
use hidpp::feature::smartshift::WheelMode;

use hidpp::feature::onboard_profiles::OnboardMode;

use crate::write::onboard_profiles::{onboard_mode_to_profiles, profiles_to_onboard_mode};
use crate::write::smartshift::{
    is_missing_enhanced, smartshift_to_wheel, wheel_mode_to_smartshift,
};
use crate::{ProfilesMode, SmartShiftMode};

#[test]
fn capabilities_sort_and_deduplicate_values() -> Result<(), WriteError> {
    let caps = DpiCapabilities::new(vec![1600, 400, 800, 800])?;

    assert_eq!(caps.values(), [400, 800, 1600]);
    assert_eq!(caps.min(), 400);
    assert_eq!(caps.max(), 1600);
    Ok(())
}

#[test]
fn capabilities_reject_empty_list() {
    assert_matches!(
        DpiCapabilities::new(Vec::new()),
        Err(WriteError::EmptyDpiList)
    );
}

#[test]
fn nearest_returns_closest_supported_value() -> Result<(), WriteError> {
    let caps = DpiCapabilities::new(vec![400, 800, 1600])?;

    assert_eq!(caps.nearest(390), 400);
    assert_eq!(caps.nearest(1000), 800);
    assert_eq!(caps.nearest(2000), 1600);
    Ok(())
}

#[test]
fn step_hint_returns_smallest_positive_gap() -> Result<(), WriteError> {
    let caps = DpiCapabilities::new(vec![400, 800, 1200, 2000])?;

    assert_eq!(caps.step_hint(), 400);
    Ok(())
}

#[test]
fn adjacent_test_target_prefers_next_then_previous_value() -> Result<(), WriteError> {
    let caps = DpiCapabilities::new(vec![400, 800, 1600])?;

    assert_eq!(caps.adjacent_test_target(400), Some(800));
    assert_eq!(caps.adjacent_test_target(800), Some(1600));
    assert_eq!(caps.adjacent_test_target(1600), Some(800));
    Ok(())
}

#[test]
fn adjacent_test_target_handles_current_outside_list() -> Result<(), WriteError> {
    let caps = DpiCapabilities::new(vec![400, 800, 1600])?;

    assert_eq!(caps.adjacent_test_target(1000), Some(1600));
    assert_eq!(caps.adjacent_test_target(2000), Some(1600));
    Ok(())
}

#[test]
fn smartshift_and_wheel_mode_byte_encodings_match() {
    // The whole design relies on 0x2110 WheelMode and 0x2111
    // SmartShiftMode sharing one wire encoding (Free/Freespin = 1,
    // Ratchet = 2). If the fork ever renumbers WheelMode this fails loudly.
    assert_eq!(
        u8::from(SmartShiftMode::Free),
        u8::from(WheelMode::Freespin)
    );
    assert_eq!(
        u8::from(SmartShiftMode::Ratchet),
        u8::from(WheelMode::Ratchet)
    );
}

#[test]
fn wheel_mode_maps_to_smartshift_mode() {
    assert_eq!(
        wheel_mode_to_smartshift(WheelMode::Freespin),
        SmartShiftMode::Free
    );
    assert_eq!(
        wheel_mode_to_smartshift(WheelMode::Ratchet),
        SmartShiftMode::Ratchet
    );
}

#[test]
fn smartshift_to_wheel_round_trips() {
    // smartshift_to_wheel is the inverse of wheel_mode_to_smartshift.
    for mode in [SmartShiftMode::Free, SmartShiftMode::Ratchet] {
        assert_eq!(wheel_mode_to_smartshift(smartshift_to_wheel(mode)), mode);
    }
}

#[test]
fn onboard_mode_maps_to_profiles_mode() {
    assert_eq!(
        onboard_mode_to_profiles(OnboardMode::Host),
        ProfilesMode::Host
    );
    assert_eq!(
        onboard_mode_to_profiles(OnboardMode::Onboard),
        ProfilesMode::Onboard
    );
}

#[test]
fn profiles_to_onboard_mode_round_trips() {
    // profiles_to_onboard_mode is the inverse of onboard_mode_to_profiles.
    for mode in [ProfilesMode::Host, ProfilesMode::Onboard] {
        assert_eq!(
            onboard_mode_to_profiles(profiles_to_onboard_mode(mode)),
            mode
        );
    }
}

#[test]
fn missing_enhanced_triggers_fallback() {
    assert!(is_missing_enhanced(&WriteError::FeatureUnsupported {
        feature_hex: 0x2111,
    }));
}

#[test]
fn missing_legacy_does_not_trigger_fallback() {
    // A device missing 0x2110 must NOT loop back — it genuinely has no
    // SmartShift.
    assert!(!is_missing_enhanced(&WriteError::FeatureUnsupported {
        feature_hex: 0x2110,
    }));
}

#[test]
fn transport_errors_do_not_trigger_fallback() {
    // Real failures must propagate, not be masked by a fallback attempt.
    assert!(!is_missing_enhanced(&WriteError::DeviceUnreachable {
        index: 0xff,
    }));
    assert!(!is_missing_enhanced(&WriteError::Hidpp("boom".into())));
}
