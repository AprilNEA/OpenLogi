//! Agent-owned state for the transient keyboard backlight indicator.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use openlogi_core::config::AppSettings;
use openlogi_ipc::{BacklightObservation, Generation, OBSERVE_HOLD};
use tokio::sync::watch;

/// How long the indicator shows one level change.
pub(crate) const DISPLAY_LIFETIME: Duration = Duration::from_millis(1500);

#[derive(Clone)]
pub struct BacklightOverlayManager {
    state: Arc<Mutex<State>>,
    published: watch::Sender<BacklightObservation>,
}

struct State {
    generation: Generation,
}

impl Default for BacklightOverlayManager {
    fn default() -> Self {
        let initial = BacklightObservation {
            generation: 0,
            current_level: 0,
            levels: 0,
            visible: false,
        };
        let (published, _) = watch::channel(initial);
        Self {
            state: Arc::new(Mutex::new(State { generation: 0 })),
            published,
        }
    }
}

impl BacklightOverlayManager {
    /// Show a keyboard's new backlight level, unless the user turned
    /// [`AppSettings::backlight_notifications`] off. Answers whether the
    /// level is shown.
    ///
    /// Call it inside a Tokio runtime: a task hides the level when its time
    /// is up.
    #[must_use]
    pub fn announce(
        &self,
        current_level: u8,
        levels: u8,
        visible: bool,
        settings: &AppSettings,
    ) -> bool {
        if !settings.backlight_notifications {
            return false;
        }
        self.show(current_level, levels, visible);
        true
    }

    /// Show the latest event and hide it after the transient display lifetime.
    fn show(&self, current_level: u8, levels: u8, visible: bool) {
        let generation = {
            let mut state = self.state();
            state.generation = state.generation.wrapping_add(1);
            self.published.send_replace(BacklightObservation {
                generation: state.generation,
                current_level,
                levels,
                visible,
            });
            state.generation
        };
        let published = self.published.clone();
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            tokio::time::sleep(DISPLAY_LIFETIME).await;
            let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.generation == generation {
                state.generation = state.generation.wrapping_add(1);
                published.send_modify(|observation| {
                    observation.generation = state.generation;
                    observation.visible = false;
                });
            }
        });
    }

    /// Observe the current indicator state.
    pub async fn observe(&self, since: Generation) -> BacklightObservation {
        let mut receiver = self.published.subscribe();
        let changed = receiver.wait_for(|state| state.generation != since);
        match tokio::time::timeout(OBSERVE_HOLD, changed).await {
            Ok(Ok(state)) => state.clone(),
            Ok(Err(_)) | Err(_) => self.published.borrow().clone(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(manager.published.borrow().generation, latest.generation);
        assert!(manager.published.borrow().visible);

        tokio::time::advance(Duration::from_secs(1)).await;
        let hidden = manager.observe(latest.generation).await;
        assert!(!hidden.visible);
    }

    #[tokio::test(start_paused = true)]
    async fn a_level_shows_only_while_the_user_wants_the_indicator() {
        let manager = BacklightOverlayManager::default();
        let muted = AppSettings {
            backlight_notifications: false,
            ..AppSettings::default()
        };

        assert!(!manager.announce(3, 8, true, &muted));
        assert_eq!(
            manager.published.borrow().generation,
            0,
            "the cell never moved"
        );

        assert!(manager.announce(3, 8, true, &AppSettings::default()));
        let shown = manager.observe(0).await;
        assert!(shown.visible);
        assert_eq!(shown.current_level, 3);
    }
}
