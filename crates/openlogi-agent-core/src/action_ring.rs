//! Agent-owned Actions Ring invocation and selection state.
//!
//! The overlay receives an opaque session and a read-only presentation
//! snapshot. Executable actions remain in the agent, and IPC commands can
//! select only a slot from that authoritative snapshot.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use openlogi_core::binding::{Action, ActionRingIcon, ActionRingLayout, ActionRingSlot};
use openlogi_hid::DeviceRoute;
use tokio::sync::Notify;

use crate::ipc::{ActionRingCommandError, ActionRingInvocation, ActionRingPresentation};

const LONG_POLL_HOLD: Duration = Duration::from_secs(20);
const SESSION_LIFETIME: Duration = Duration::from_secs(15);

/// Immutable input used to open one ring session.
pub struct ActionRingSessionSpec {
    /// Config key of the device whose control opened the ring.
    pub device_key: String,
    /// HID++ route used for feedback when both config and capabilities allow it.
    pub haptic_route: Option<DeviceRoute>,
    /// Exact layout the agent will execute for this session.
    pub layout: ActionRingLayout,
    /// Configured UI locale, or `None` to follow the overlay host's system locale.
    pub language: Option<String>,
}

/// A validated slot activation returned to the action dispatcher.
pub struct ActionRingActivation {
    /// Config key of the device whose control opened the ring.
    pub device_key: String,
    /// Action snapshotted when the ring opened.
    pub action: Action,
    /// Route of the triggering device when activation feedback is available.
    pub haptic_route: Option<DeviceRoute>,
}

/// A validated hover transition that may play feedback.
#[derive(Debug, PartialEq, Eq)]
pub struct ActionRingHover {
    /// Route of the triggering device when hover feedback is available.
    pub haptic_route: Option<DeviceRoute>,
}

struct Session {
    invocation: ActionRingInvocation,
    pending: bool,
    device_key: String,
    haptic_route: Option<DeviceRoute>,
    actions: BTreeMap<ActionRingSlot, Action>,
    hovered: Option<ActionRingSlot>,
    opened_at: Instant,
}

#[derive(Default)]
struct State {
    active: Option<Session>,
}

impl State {
    fn expire(&mut self) {
        if self
            .active
            .as_ref()
            .is_some_and(|session| session.opened_at.elapsed() > SESSION_LIFETIME)
        {
            self.active = None;
        }
    }

    fn active_session(&mut self, session_id: u64) -> Result<&mut Session, ActionRingCommandError> {
        self.expire();
        match self.active.as_mut() {
            Some(session) if session.invocation.session_id == session_id => Ok(session),
            _ => Err(ActionRingCommandError::SessionNotFound),
        }
    }
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
        let mut actions = BTreeMap::new();
        let mut slots = BTreeMap::new();
        for (slot, entry) in spec.layout.slots {
            let (action, custom_icon, custom_label) = entry.into_parts();
            let literal = custom_label.is_some();
            slots.insert(
                slot,
                ActionRingPresentation {
                    label: custom_label.unwrap_or_else(|| action.label()),
                    literal,
                    icon: custom_icon.unwrap_or_else(|| ActionRingIcon::for_action(&action)),
                },
            );
            actions.insert(slot, action);
        }
        let invocation = ActionRingInvocation {
            session_id,
            slots,
            language: spec.language,
        };
        let mut state = self.state();
        state.active = Some(Session {
            invocation: invocation.clone(),
            pending: true,
            device_key: spec.device_key,
            haptic_route: spec.haptic_route,
            actions,
            hovered: None,
            opened_at: Instant::now(),
        });
        drop(state);
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
            // Close the notification race between checking the slot and
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
        let mut state = self.state();
        let session = state.active_session(session_id)?;
        if !session.actions.contains_key(&slot) {
            return Err(ActionRingCommandError::SlotEmpty);
        }
        if session.hovered == Some(slot) {
            return Ok(None);
        }
        session.hovered = Some(slot);
        Ok(Some(ActionRingHover {
            haptic_route: session.haptic_route.clone(),
        }))
    }

    /// Consume a session and return the snapshotted action for `slot`.
    pub fn activate(
        &self,
        session_id: u64,
        slot: ActionRingSlot,
    ) -> Result<ActionRingActivation, ActionRingCommandError> {
        let mut state = self.state();
        if !state
            .active_session(session_id)?
            .actions
            .contains_key(&slot)
        {
            return Err(ActionRingCommandError::SlotEmpty);
        }
        let Some(mut session) = state.active.take() else {
            return Err(ActionRingCommandError::SessionNotFound);
        };
        let Some(action) = session.actions.remove(&slot) else {
            return Err(ActionRingCommandError::SlotEmpty);
        };
        Ok(ActionRingActivation {
            device_key: session.device_key,
            action,
            haptic_route: session.haptic_route,
        })
    }

    /// Cancel `session_id` if it is still active.
    pub fn cancel(&self, session_id: u64) {
        let mut state = self.state();
        if state
            .active
            .as_ref()
            .is_some_and(|session| session.invocation.session_id == session_id)
        {
            state.active = None;
        }
    }

    fn take_pending(&self) -> Option<ActionRingInvocation> {
        let mut state = self.state();
        state.expire();
        let session = state.active.as_mut()?;
        if !session.pending {
            return None;
        }
        session.pending = false;
        Some(session.invocation.clone())
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::binding::ActionRingConfig;

    fn spec() -> ActionRingSessionSpec {
        ActionRingSessionSpec {
            device_key: "mouse-a".to_string(),
            haptic_route: None,
            layout: ActionRingConfig::default().default,
            language: None,
        }
    }

    #[tokio::test]
    async fn invocation_is_queued_before_the_overlay_polls() {
        let manager = ActionRingManager::default();
        let expected = manager.begin(spec());
        assert_eq!(manager.next_invocation().await, Some(expected));
    }

    #[test]
    fn invocation_contains_presentation_but_not_execution_payloads() {
        let manager = ActionRingManager::default();
        let mut spec = spec();
        spec.layout
            .set_icon(ActionRingSlot::Top, Some(ActionRingIcon::Keyboard));
        spec.language = Some("fr".to_string());
        let invocation = manager.begin(spec);
        assert_eq!(
            invocation.slots[&ActionRingSlot::Top],
            ActionRingPresentation {
                label: "Cut".to_string(),
                literal: false,
                icon: ActionRingIcon::Keyboard,
            }
        );
        assert_eq!(invocation.language.as_deref(), Some("fr"));
    }

    #[test]
    fn custom_slot_labels_override_the_action_label() {
        let manager = ActionRingManager::default();
        let mut spec = spec();
        spec.layout
            .set_label(ActionRingSlot::Top, Some("Copy Invoice".to_string()));
        let invocation = manager.begin(spec);
        assert_eq!(invocation.slots[&ActionRingSlot::Top].label, "Copy Invoice");
        // Custom labels are literal so the overlay renders them verbatim even
        // when they collide with a localization key.
        assert!(invocation.slots[&ActionRingSlot::Top].literal);
    }

    #[test]
    fn activation_consumes_the_session() {
        let manager = ActionRingManager::default();
        let invocation = manager.begin(spec());
        let activation = manager
            .activate(invocation.session_id, ActionRingSlot::Top)
            .unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(activation.device_key, "mouse-a");
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
    fn cancellation_discards_an_unclaimed_invocation() {
        let manager = ActionRingManager::default();
        let invocation = manager.begin(spec());
        manager.cancel(invocation.session_id);
        assert_eq!(manager.take_pending(), None);
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

    #[test]
    fn expired_session_rejects_interaction() {
        let manager = ActionRingManager::default();
        let invocation = manager.begin(spec());
        let mut state = manager.state();
        let session = state
            .active
            .as_mut()
            .unwrap_or_else(|| panic!("begin creates a session"));
        session.opened_at = Instant::now()
            .checked_sub(SESSION_LIFETIME + Duration::from_secs(1))
            .unwrap_or_else(|| panic!("test instant has sufficient history"));
        drop(state);

        assert!(matches!(
            manager.activate(invocation.session_id, ActionRingSlot::Top),
            Err(ActionRingCommandError::SessionNotFound)
        ));
        assert_eq!(manager.take_pending(), None);
    }
}
