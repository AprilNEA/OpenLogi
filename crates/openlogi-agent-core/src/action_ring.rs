//! Agent-owned Actions Ring invocation and selection state.
//!
//! The overlay receives an opaque session and a read-only presentation
//! snapshot. Executable actions remain in the agent, and IPC commands can
//! select only a slot from that authoritative snapshot.

use parking_lot::{Mutex, MutexGuard};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use openlogi_core::action_ring::DISPLAY_LIFETIME;
use openlogi_core::binding::{Action, ActionRingIcon, ActionRingLayout, ActionRingSlot};
use openlogi_hid::DeviceRoute;
use openlogi_ipc::{
    ActionRingCommandError, ActionRingInvocation, ActionRingPresentation, Generation, OBSERVE_HOLD,
    RingObservation,
};
use tokio::sync::watch;

/// Slack between the window closing and the session expiring.
///
/// The two clocks do not start together: a session is stamped in `begin`,
/// while the overlay's timer starts only once the long poll has delivered the
/// invocation and the window is up. Giving them equal durations expired the
/// session *before* the ring the user could still click disappeared, and a
/// click landing in that tail returned `SessionNotFound` — the window closed
/// and the action silently did not run. This covers that gap plus the click's
/// round-trip back.
const SESSION_GRACE: Duration = Duration::from_secs(3);

const SESSION_LIFETIME: Duration = DISPLAY_LIFETIME.saturating_add(SESSION_GRACE);

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
    /// What the overlay observes. Derived from [`Self::state`] after every
    /// change, so "no ring" is simply the absence of one — there is no closed
    /// message to invent, and an overlay that restarts mid-ring reads the live
    /// one instead of having missed its invocation.
    published: watch::Sender<RingObservation>,
}

impl Default for ActionRingManager {
    fn default() -> Self {
        // Generation 1: 0 is the observer's "seen nothing" sentinel.
        let (published, _) = watch::channel(RingObservation {
            generation: 1,
            invocation: None,
        });
        Self {
            next_session: AtomicU64::new(1),
            state: Mutex::new(State::default()),
            published,
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
            device_key: spec.device_key,
            haptic_route: spec.haptic_route,
            actions,
            hovered: None,
            opened_at: Instant::now(),
        });
        self.publish(&state);
        invocation
    }

    /// Dismiss the showing session, if any, and return whether one was
    /// dismissed — which is what lets a second press of the ring trigger toggle
    /// the ring closed.
    ///
    /// Dismissing simply removes the session. The old wire format could only
    /// carry invocations, so a dismissal had to be encoded as an *empty*
    /// invocation for the overlay to acknowledge; observing state needs no such
    /// placeholder, and a trigger press racing the close now finds no session
    /// and opens a fresh ring.
    pub fn dismiss_active(&self) -> bool {
        let mut state = self.state();
        state.expire();
        let dismissed = matches!(&state.active, Some(session) if !session.actions.is_empty());
        if dismissed {
            state.active = None;
        }
        self.publish(&state);
        dismissed
    }

    /// Serve one [`Agent::observe_action_ring`](openlogi_ipc::Agent::observe_action_ring).
    pub async fn observe(&self, since: Generation) -> RingObservation {
        let mut rx = self.published.subscribe();
        let changed = rx.wait_for(|observed| observed.generation != since);
        match tokio::time::timeout(OBSERVE_HOLD, changed).await {
            Ok(Ok(observed)) => observed.clone(),
            // Hold elapsed, or the manager is gone: answer with what we have.
            Ok(Err(_)) | Err(_) => self.published.borrow().clone(),
        }
    }

    /// Republish what the overlay should be showing. Called after every change
    /// to [`Self::state`], and a republish that says the same thing wakes
    /// nobody.
    fn publish(&self, state: &State) {
        let invocation = state
            .active
            .as_ref()
            .map(|session| session.invocation.clone());
        self.published.send_if_modified(|observed| {
            if observed.invocation == invocation {
                return false;
            }
            observed.invocation = invocation;
            observed.generation += 1;
            true
        });
    }

    /// Record a changed highlighted slot. Repeated hover reports are ignored so
    /// one stationary pointer cannot flood the HID++ haptic queue.
    pub fn hover(
        &self,
        session_id: u64,
        slot: ActionRingSlot,
    ) -> Result<Option<ActionRingHover>, ActionRingCommandError> {
        let mut state = self.state();
        // `active_session` expires a stale session, which the overlay must hear
        // about even though the hover itself then fails.
        let session = match state.active_session(session_id) {
            Ok(session) => session,
            Err(error) => {
                self.publish(&state);
                return Err(error);
            }
        };
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
        self.publish(&state);
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
        self.publish(&state);
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock()
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
    async fn an_overlay_that_has_seen_nothing_gets_the_showing_ring() {
        let manager = ActionRingManager::default();
        let expected = manager.begin(spec());
        // Generation 0 is "seen nothing", so this answers at once — which is
        // also how an overlay restarted mid-ring picks up the live one.
        let observed = manager.observe(0).await;
        assert_eq!(observed.invocation, Some(expected));
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

    #[tokio::test]
    async fn second_trigger_press_dismisses_and_third_reopens() {
        let manager = ActionRingManager::default();

        // Nothing showing yet: the first press must open, not dismiss.
        assert!(!manager.dismiss_active());
        let opened = manager.begin(spec());
        let showing = manager.observe(0).await;
        assert_eq!(showing.invocation, Some(opened.clone()));

        // Second press: dismissed, and the overlay observes "no ring" rather
        // than a placeholder invocation it has to acknowledge.
        assert!(manager.dismiss_active());
        let dismissed = manager.observe(showing.generation).await;
        assert_eq!(dismissed.invocation, None);

        // Nothing is showing, so a third press opens again.
        assert!(!manager.dismiss_active());
        let reopened = manager.begin(spec());
        assert!(!reopened.slots.is_empty());

        // A stale Cancel for the dismissed session must not kill the new one.
        manager.cancel(opened.session_id);
        assert!(manager.dismiss_active());
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
            .expect("a live session must activate its top slot");
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

    #[tokio::test]
    async fn cancellation_stops_the_ring_being_shown() {
        let manager = ActionRingManager::default();
        let invocation = manager.begin(spec());
        manager.cancel(invocation.session_id);
        assert_eq!(manager.observe(0).await.invocation, None);
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
        manager
            .activate(second.session_id, ActionRingSlot::Top)
            .expect("the replacement session must still be activatable");
    }

    /// The two clocks start at different moments — the session when `begin`
    /// stamps it, the window once the long poll has delivered and the overlay
    /// is up — so equal durations expire the session while the ring is still
    /// on screen, and the click that lands there is silently dropped.
    #[test]
    fn a_session_outlives_the_window_it_serves() {
        assert!(
            SESSION_LIFETIME > DISPLAY_LIFETIME,
            "a click on a still-visible ring must still find its session"
        );
    }

    #[test]
    fn expired_session_rejects_interaction() {
        let manager = ActionRingManager::default();
        let invocation = manager.begin(spec());
        let mut state = manager.state();
        let session = state.active.as_mut().expect("begin creates a session");
        session.opened_at = Instant::now()
            .checked_sub(SESSION_LIFETIME + Duration::from_secs(1))
            .expect("test instant has sufficient history");
        drop(state);

        assert!(matches!(
            manager.activate(invocation.session_id, ActionRingSlot::Top),
            Err(ActionRingCommandError::SessionNotFound)
        ));
    }
}
