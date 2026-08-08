//! Agent-owned Actions Ring invocation and selection state.
//!
//! The overlay receives an opaque session and a read-only presentation
//! snapshot. The agent retains the authoritative actions, and IPC commands can
//! select only a slot from that snapshot rather than supply an arbitrary action.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use openlogi_core::binding::{Action, ActionRingLayout, ActionRingSlot};
use openlogi_hid::DeviceRoute;
use tokio::sync::Notify;

use crate::ipc::{ActionRingCommandError, ActionRingInvocation};

const LONG_POLL_HOLD: Duration = Duration::from_secs(20);
const SESSION_LIFETIME: Duration = Duration::from_secs(15);

/// Immutable input used to open one ring session.
pub struct ActionRingSessionSpec {
    /// HID++ route used for optional haptic feedback.
    pub route: Option<DeviceRoute>,
    /// Exact layout the agent will execute for this session.
    pub layout: ActionRingLayout,
    /// Whether device haptics are enabled for ring interactions.
    pub haptics: bool,
}

/// A validated slot activation returned to the action dispatcher.
pub struct ActionRingActivation {
    /// Action snapshotted when the ring opened.
    pub action: Action,
    /// Route of the triggering device, for activation feedback.
    pub route: Option<DeviceRoute>,
    /// Whether activation feedback is enabled.
    pub haptics: bool,
}

/// A validated hover transition that may play feedback.
#[derive(Debug, PartialEq, Eq)]
pub struct ActionRingHover {
    /// Route of the triggering device.
    pub route: Option<DeviceRoute>,
    /// Whether hover feedback is enabled.
    pub haptics: bool,
}

struct Session {
    route: Option<DeviceRoute>,
    layout: ActionRingLayout,
    haptics: bool,
    hovered: Option<ActionRingSlot>,
    opened_at: Instant,
}

#[derive(Default)]
struct State {
    pending: VecDeque<ActionRingInvocation>,
    active: Option<(u64, Session)>,
}

/// Shared ring state used by input dispatch and IPC handlers.
pub struct ActionRingManager {
    next_session: AtomicU64,
    state: Mutex<State>,
    changed: Notify,
}

impl Default for ActionRingManager {
    fn default() -> Self {
        Self {
            next_session: AtomicU64::new(1),
            state: Mutex::new(State::default()),
            changed: Notify::new(),
        }
    }
}

impl ActionRingManager {
    /// Open or replace the current session and wake the overlay long-poll.
    pub fn begin(&self, spec: ActionRingSessionSpec) -> ActionRingInvocation {
        let session_id = self.next_session.fetch_add(1, Ordering::Relaxed);
        let slots = spec
            .layout
            .slots
            .iter()
            .map(|(slot, action)| (*slot, action.action().clone()))
            .collect();
        let invocation = ActionRingInvocation {
            session_id,
            slots,
            icons: spec.layout.icons.clone(),
        };
        if let Ok(mut state) = self.state.lock() {
            state.pending.clear();
            state.pending.push_back(invocation.clone());
            state.active = Some((
                session_id,
                Session {
                    route: spec.route,
                    layout: spec.layout,
                    haptics: spec.haptics,
                    hovered: None,
                    opened_at: Instant::now(),
                },
            ));
        }
        self.changed.notify_one();
        invocation
    }

    /// Wait for the next invocation, returning `None` when the hold window
    /// elapses so the overlay can check its agent connection and poll again.
    pub async fn next_invocation(&self) -> Option<ActionRingInvocation> {
        let deadline = tokio::time::Instant::now() + LONG_POLL_HOLD;
        loop {
            if let Some(invocation) = self.take_pending() {
                return Some(invocation);
            }
            let notified = self.changed.notified();
            // Close the notification race between checking the queue and
            // registering this waiter.
            if let Some(invocation) = self.take_pending() {
                return Some(invocation);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return None;
            }
        }
    }

    /// Record a changed highlighted slot. Repeated hover reports are ignored so
    /// one stationary pointer cannot flood the HID++ haptic queue.
    pub fn hover(
        &self,
        session_id: u64,
        slot: ActionRingSlot,
    ) -> Result<Option<ActionRingHover>, ActionRingCommandError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ActionRingCommandError::Unavailable)?;
        let Some((active_id, session)) = state.active.as_mut() else {
            return Err(ActionRingCommandError::SessionNotFound);
        };
        if *active_id != session_id {
            return Err(ActionRingCommandError::SessionNotFound);
        }
        if session.opened_at.elapsed() > SESSION_LIFETIME {
            state.active = None;
            return Err(ActionRingCommandError::SessionNotFound);
        }
        if !session.layout.slots.contains_key(&slot) {
            return Err(ActionRingCommandError::SlotEmpty);
        }
        if session.hovered == Some(slot) {
            return Ok(None);
        }
        session.hovered = Some(slot);
        Ok(Some(ActionRingHover {
            route: session.route.clone(),
            haptics: session.haptics,
        }))
    }

    /// Consume a session and return the snapshotted action for `slot`.
    pub fn activate(
        &self,
        session_id: u64,
        slot: ActionRingSlot,
    ) -> Result<ActionRingActivation, ActionRingCommandError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ActionRingCommandError::Unavailable)?;
        let Some((active_id, session)) = state.active.as_ref() else {
            return Err(ActionRingCommandError::SessionNotFound);
        };
        if *active_id != session_id {
            return Err(ActionRingCommandError::SessionNotFound);
        }
        if session.opened_at.elapsed() > SESSION_LIFETIME {
            state.active = None;
            return Err(ActionRingCommandError::SessionNotFound);
        }
        if !session.layout.slots.contains_key(&slot) {
            return Err(ActionRingCommandError::SlotEmpty);
        }
        let Some((_, session)) = state.active.take() else {
            return Err(ActionRingCommandError::SessionNotFound);
        };
        let Some(action) = session.layout.slots.get(&slot) else {
            return Err(ActionRingCommandError::SlotEmpty);
        };
        Ok(ActionRingActivation {
            action: action.action().clone(),
            route: session.route,
            haptics: session.haptics,
        })
    }

    /// Cancel `session_id` if it is still active.
    pub fn cancel(&self, session_id: u64) {
        if let Ok(mut state) = self.state.lock()
            && state
                .active
                .as_ref()
                .is_some_and(|(id, _)| *id == session_id)
        {
            state.active = None;
        }
    }

    fn take_pending(&self) -> Option<ActionRingInvocation> {
        self.state
            .lock()
            .ok()
            .and_then(|mut state| state.pending.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::binding::{ActionRingConfig, ActionRingIcon};

    fn spec() -> ActionRingSessionSpec {
        ActionRingSessionSpec {
            route: None,
            layout: ActionRingConfig::default().default,
            haptics: true,
        }
    }

    #[tokio::test]
    async fn invocation_is_queued_before_the_overlay_polls() {
        let manager = ActionRingManager::default();
        let expected = manager.begin(spec());
        assert_eq!(manager.next_invocation().await, Some(expected));
    }

    #[test]
    fn invocation_carries_only_the_layouts_presentation_icons() {
        let manager = ActionRingManager::default();
        let mut spec = spec();
        spec.layout
            .icons
            .insert(ActionRingSlot::Top, ActionRingIcon::Keyboard);
        let invocation = manager.begin(spec);
        assert_eq!(
            invocation.icons.get(&ActionRingSlot::Top),
            Some(&ActionRingIcon::Keyboard)
        );
        assert_eq!(invocation.slots[&ActionRingSlot::Top], Action::Cut);
    }

    #[test]
    fn activation_consumes_the_session() {
        let manager = ActionRingManager::default();
        let invocation = manager.begin(spec());
        let activation = manager
            .activate(invocation.session_id, ActionRingSlot::Top)
            .unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(activation.action, Action::Cut);
        assert!(matches!(
            manager.activate(invocation.session_id, ActionRingSlot::Top),
            Err(ActionRingCommandError::SessionNotFound)
        ));
    }

    #[test]
    fn repeated_hover_is_deduplicated() {
        let manager = ActionRingManager::default();
        let invocation = manager.begin(spec());
        assert!(
            manager
                .hover(invocation.session_id, ActionRingSlot::Top)
                .is_ok_and(|hover| hover.is_some())
        );
        assert_eq!(
            manager.hover(invocation.session_id, ActionRingSlot::Top),
            Ok(None)
        );
    }

    #[test]
    fn replacement_invalidates_the_previous_session() {
        let manager = ActionRingManager::default();
        let first = manager.begin(spec());
        let second = manager.begin(spec());
        assert!(matches!(
            manager.activate(first.session_id, ActionRingSlot::Top),
            Err(ActionRingCommandError::SessionNotFound)
        ));
        assert!(
            manager
                .activate(second.session_id, ActionRingSlot::Top)
                .is_ok()
        );
    }
}
