//! Agent-owned state for the transient keyboard backlight indicator.

use openlogi_ipc::{BacklightObservation, Generation};
use tokio::sync::watch;

use crate::indicator::Indicator;

#[derive(Clone)]
pub struct BacklightOverlayManager {
    indicator: Indicator<BacklightObservation>,
}

impl Default for BacklightOverlayManager {
    fn default() -> Self {
        let initial = BacklightObservation {
            generation: 0,
            current_level: 0,
            levels: 0,
            visible: false,
        };
        Self {
            indicator: Indicator::new(initial),
        }
    }
}

impl BacklightOverlayManager {
    /// Publish the latest event, allowing a bounded cold start before its
    /// first delivery starts the transient display lifetime.
    pub fn show(&self, current_level: u8, levels: u8, visible: bool) {
        self.indicator.show(BacklightObservation {
            generation: 0,
            current_level,
            levels,
            visible,
        });
    }

    /// Observe the current indicator state.
    pub async fn observe(&self, since: Generation) -> BacklightObservation {
        self.indicator.observe(since).await
    }

    /// Follow state changes without marking the indicator as delivered.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<BacklightObservation> {
        self.indicator.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indicator::DISPLAY_LIFETIME;
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn a_cold_overlay_still_receives_the_first_backlight_change() {
        let manager = BacklightOverlayManager::default();
        manager.show(3, 8, true);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;

        let shown = manager.observe(0).await;
        assert!(shown.visible, "startup must not consume the first hint");
        assert_eq!(shown.current_level, 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_press_after_expiry_has_a_new_generation() {
        let manager = BacklightOverlayManager::default();
        manager.show(3, 8, true);
        let shown = manager.observe(0).await;
        assert!(shown.visible);

        tokio::task::yield_now().await;
        tokio::time::advance(DISPLAY_LIFETIME).await;
        let hidden = manager.observe(shown.generation).await;
        assert!(!hidden.visible);

        manager.show(4, 8, true);
        let next = manager.observe(hidden.generation).await;
        assert!(next.visible);
        assert_eq!(next.current_level, 4);
        assert!(next.generation > hidden.generation);
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_press_extends_the_display_lifetime() {
        let manager = BacklightOverlayManager::default();
        manager.show(3, 8, true);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;

        manager.show(4, 8, true);
        let latest = manager.observe(1).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(manager.subscribe().borrow().generation, latest.generation);
        assert!(manager.subscribe().borrow().visible);

        tokio::time::advance(Duration::from_secs(1)).await;
        let hidden = manager.observe(latest.generation).await;
        assert!(!hidden.visible);
    }
}
