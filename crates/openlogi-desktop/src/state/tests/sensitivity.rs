//! The two wheel-speed settings: that each reaches only its own value, and that
//! a device override clears itself when it lands back on the app-wide default.

use super::*;

/// The device key `state_with_a_known_mouse` writes per-device settings under.
fn known_mouse(state: &AppState) -> DeviceKey {
    let key = state
        .current_record()
        .map(DeviceRecord::device_key)
        .expect("the known mouse is selected");
    assert_eq!(
        key.as_str(),
        KNOWN_MOUSE_KEY,
        "the fixture's mouse resolves to its identity key"
    );
    key
}

/// The zoom slider is the device's only zoom-speed control, so landing back on
/// the app-wide default must *clear* the override rather than pin today's
/// default forever — otherwise a later change in Settings → General would
/// silently skip every device the user had ever touched the slider on.
#[test]
fn committing_the_app_default_clears_the_device_zoom_override() {
    let mut state = state_with_a_known_mouse();
    let key = known_mouse(&state);
    let app_default = state.app_settings().zoom_sensitivity;

    let _ = state.commit_device_zoom_sensitivity(&key, ThumbwheelSensitivity::MAX);
    assert_eq!(
        state.device_zoom_sensitivity(KNOWN_MOUSE_KEY),
        ThumbwheelSensitivity::MAX
    );
    assert_eq!(
        state
            .config
            .devices
            .get(KNOWN_MOUSE_KEY)
            .and_then(|device| device.zoom_sensitivity),
        Some(ThumbwheelSensitivity::MAX),
        "a value away from the default is stored as an override"
    );

    let _ = state.commit_device_zoom_sensitivity(&key, app_default);
    assert_eq!(state.device_zoom_sensitivity(KNOWN_MOUSE_KEY), app_default);
    assert!(
        state
            .config
            .devices
            .get(KNOWN_MOUSE_KEY)
            .and_then(|device| device.zoom_sensitivity)
            .is_none(),
        "landing on the app default must clear the override, not store it"
    );
}

/// Zoom and scroll speed are separate sliders on the same panel; moving one
/// must not drag the other along.
#[test]
fn the_zoom_slider_does_not_move_the_scroll_slider() {
    let mut state = state_with_a_known_mouse();
    let key = known_mouse(&state);

    let _ = state.commit_device_zoom_sensitivity(&key, ThumbwheelSensitivity::MAX);

    assert_eq!(
        state.device_zoom_sensitivity(KNOWN_MOUSE_KEY),
        ThumbwheelSensitivity::MAX
    );
    assert_eq!(
        state.device_thumbwheel_sensitivity(KNOWN_MOUSE_KEY),
        ThumbwheelSensitivity::DEFAULT,
        "the zoom slider must leave scroll sensitivity where it was"
    );

    let _ = state.commit_device_thumbwheel_sensitivity(&key, ThumbwheelSensitivity::MIN);
    assert_eq!(
        state.device_zoom_sensitivity(KNOWN_MOUSE_KEY),
        ThumbwheelSensitivity::MAX,
        "and the scroll slider must leave zoom where it was"
    );
}

/// The General page's zoom slider writes the app-wide default, which every
/// device without its own override follows. Without this commit the setting
/// would exist in the config file with nothing in the UI able to reach it.
#[test]
fn the_app_wide_zoom_default_applies_to_devices_without_an_override() {
    let mut state = state_with_a_known_mouse();

    let _ = state.commit_zoom_sensitivity(ThumbwheelSensitivity::MAX);

    assert_eq!(
        state.app_settings().zoom_sensitivity,
        ThumbwheelSensitivity::MAX
    );
    assert_eq!(
        state.device_zoom_sensitivity(KNOWN_MOUSE_KEY),
        ThumbwheelSensitivity::MAX,
        "a device with no override follows the app-wide zoom default"
    );
    assert_eq!(
        state.app_settings().thumbwheel_sensitivity,
        ThumbwheelSensitivity::DEFAULT,
        "the zoom default must not move scroll sensitivity"
    );
}
