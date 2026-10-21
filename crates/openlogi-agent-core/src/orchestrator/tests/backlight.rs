//! The backlight indicator: whether the agent shows a keyboard's new level.

use openlogi_core::config::Config;

use super::orchestrator;
use crate::backlight_overlay::BacklightOverlayManager;

#[tokio::test(start_paused = true)]
async fn a_backlight_change_shows_by_default() {
    let orch = orchestrator(Config::default());
    let overlay = BacklightOverlayManager::default();

    orch.announce_backlight(&overlay, 3, 8, true);

    let shown = overlay.observe(0).await;
    assert!(shown.visible);
    assert_eq!(shown.current_level, 3);
}

#[tokio::test(start_paused = true)]
async fn nothing_shows_with_the_indicator_turned_off() {
    let mut config = Config::default();
    config.app_settings.backlight_notifications = false;
    let orch = orchestrator(config);
    let overlay = BacklightOverlayManager::default();

    orch.announce_backlight(&overlay, 3, 8, true);

    let current = overlay.observe(0).await;
    assert!(!current.visible);
    assert_eq!(current.generation, 0, "the cell never moved");
}

#[tokio::test(start_paused = true)]
async fn turning_the_indicator_on_again_shows_the_next_change() {
    let mut config = Config::default();
    config.app_settings.backlight_notifications = false;
    let mut orch = orchestrator(config.clone());
    let overlay = BacklightOverlayManager::default();
    orch.announce_backlight(&overlay, 3, 8, true);

    config.app_settings.backlight_notifications = true;
    orch.replace_config(config);
    orch.announce_backlight(&overlay, 4, 8, true);

    assert_eq!(overlay.observe(0).await.current_level, 4);
}
