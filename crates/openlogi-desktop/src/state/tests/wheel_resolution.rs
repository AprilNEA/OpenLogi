//! Saving a supported wheel resolution and ignoring an unsupported one.

use super::*;

#[test]
fn gui_state_saves_and_clears_supported_wheel_resolution() {
    let mut config = Config::ephemeral();
    assert!(set_scroll_resolution_if_supported(
        &mut config,
        "mouse",
        true,
        Some(ScrollResolution::Low),
    ));
    assert_eq!(
        config.scroll_resolution("mouse"),
        Some(ScrollResolution::Low)
    );

    assert!(set_scroll_resolution_if_supported(
        &mut config,
        "mouse",
        true,
        None,
    ));
    assert_eq!(config.scroll_resolution("mouse"), None);
}

#[test]
fn gui_state_ignores_unsupported_wheel_resolution() {
    let mut config = Config::ephemeral();
    assert!(!set_scroll_resolution_if_supported(
        &mut config,
        "mouse",
        false,
        Some(ScrollResolution::High),
    ));
    assert_eq!(config.scroll_resolution("mouse"), None);
}
