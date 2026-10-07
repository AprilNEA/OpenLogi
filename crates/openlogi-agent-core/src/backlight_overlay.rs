//! Agent-owned state for the transient keyboard backlight indicator.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use openlogi_ipc::{BacklightObservation, Generation, OBSERVE_HOLD};
use tokio::sync::watch;

const DISPLAY_LIFETIME: Duration = Duration::from_millis(1500);

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
    /// Show the latest event and hide it after the transient display lifetime.
    pub fn show(&self, current_level: u8, levels: u8, visible: bool) {
        let generation = {
            let mut state = self.state();
            state.generation = state.generation.wrapping_add(1);
            state.generation
        };
        self.published.send_replace(BacklightObservation {
            generation,
            current_level,
            levels,
            visible,
        });
        let published = self.published.clone();
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            tokio::time::sleep(DISPLAY_LIFETIME).await;
            let should_hide = {
                let state = state.lock().unwrap_or_else(PoisonError::into_inner);
                state.generation == generation
            };
            if should_hide {
                published.send_modify(|observation| {
                    observation.generation = generation.wrapping_add(1);
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
