//! Device-owned side-button gesture state shared by HID++ capture and the OS
//! pointer-motion hook.
//!
//! Back/Forward down/up edges arrive over a device-specific HID++ session. The
//! global OS hook contributes movement only while that verified hold is live,
//! so an unattributed macOS side-button event is never suppressed or remapped.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use openlogi_core::binding::{
    Action, ButtonId, GestureDirection, SwipeAccumulator, default_binding,
};

/// A shared device-owned side-button runtime.
pub type SharedSideGesture = Arc<SideGestureRuntime>;

/// Synchronizes HID++ button edges with the freeze-sensitive OS movement
/// callback. HID++ callers may take the short state lock; the hook only tries
/// it and records interruption cancellation through an atomic flag.
#[derive(Default)]
pub struct SideGestureRuntime {
    state: Mutex<SideGestureState>,
    cancel_pending: AtomicBool,
}

impl SideGestureRuntime {
    fn with_state<T>(&self, f: impl FnOnce(&mut SideGestureState) -> T) -> T {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if self.cancel_pending.swap(false, Ordering::Acquire) {
            state.cancel();
        }
        f(&mut state)
    }

    /// Begin a verified HID++ hold.
    pub fn begin(
        &self,
        device_key: String,
        button: ButtonId,
        directions: BTreeMap<GestureDirection, Action>,
    ) -> Option<SideGestureAction> {
        self.with_state(|state| state.begin(device_key, button, directions))
    }

    /// End a verified HID++ hold.
    pub fn end(&self, device_key: &str, button: ButtonId) -> Option<SideGestureAction> {
        self.with_state(|state| state.end(device_key, button))
    }

    /// Cancel a hold owned by a capture session that stopped.
    pub fn cancel_device(&self, device_key: &str) {
        self.with_state(|state| state.cancel_device(device_key));
    }

    /// Feed movement without ever blocking the OS event-tap callback.
    pub fn try_accumulate(&self, dx: i32, dy: i32) -> Option<SideGestureAction> {
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => return None,
        };
        if self.cancel_pending.swap(false, Ordering::Acquire) {
            state.cancel();
        }
        state.accumulate(dx, dy)
    }

    /// Cancel after an OS-capture interruption without blocking the event tap.
    /// If the state lock is briefly held by a HID++ edge, the atomic flag makes
    /// the next edge or movement apply the cancellation before doing work.
    pub fn interrupt(&self) {
        self.cancel_pending.store(true, Ordering::Release);
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        if self.cancel_pending.swap(false, Ordering::Acquire) {
            state.cancel();
        }
    }
}

/// A resolved action produced by a device-owned side-button gesture.
#[derive(Debug, Clone, PartialEq)]
pub struct SideGestureAction {
    /// Device whose HID++ session supplied the verified button edge.
    pub device_key: String,
    /// Physical side button held for the gesture.
    pub button: ButtonId,
    /// Direction that resolved the action, including [`GestureDirection::Click`].
    pub direction: GestureDirection,
    /// Configured action for `direction`.
    pub action: Action,
}

/// A side-button hold this old is presumed stale. A repeated press then proves
/// the release edge was lost and replaces it instead of leaving gestures dead.
const STALE_HOLD: Duration = Duration::from_secs(10);

struct SideGestureHold {
    device_key: String,
    button: ButtonId,
    since: Instant,
    directions: BTreeMap<GestureDirection, Action>,
    swipe: SwipeAccumulator,
}

impl SideGestureHold {
    fn new(
        device_key: String,
        button: ButtonId,
        directions: BTreeMap<GestureDirection, Action>,
    ) -> Self {
        let mut swipe = SwipeAccumulator::default();
        swipe.begin();
        Self {
            device_key,
            button,
            since: Instant::now(),
            directions,
            swipe,
        }
    }

    fn action_for(&self, direction: GestureDirection) -> SideGestureAction {
        let action = self
            .directions
            .get(&direction)
            .cloned()
            .or_else(|| self.directions.get(&GestureDirection::Click).cloned())
            .unwrap_or_else(|| default_binding(self.button));
        SideGestureAction {
            device_key: self.device_key.clone(),
            button: self.button,
            direction,
            action,
        }
    }
}

/// First-hold-wins gesture state. The button identity is verified by HID++;
/// pointer deltas are supplied separately by the OS hook.
#[derive(Default)]
pub struct SideGestureState {
    hold: Option<SideGestureHold>,
}

impl SideGestureState {
    /// Begin a verified HID++ hold. A different live hold keeps ownership and
    /// the refused press resolves immediately as that button's plain click,
    /// matching the existing OS-hook gesture policy.
    pub fn begin(
        &mut self,
        device_key: String,
        button: ButtonId,
        directions: BTreeMap<GestureDirection, Action>,
    ) -> Option<SideGestureAction> {
        if let Some(held) = &self.hold
            && (held.device_key != device_key || held.button != button)
            && held.since.elapsed() < STALE_HOLD
        {
            let action = directions
                .get(&GestureDirection::Click)
                .cloned()
                .unwrap_or_else(|| default_binding(button));
            return Some(SideGestureAction {
                device_key,
                button,
                direction: GestureDirection::Click,
                action,
            });
        }
        self.hold = Some(SideGestureHold::new(device_key, button, directions));
        None
    }

    /// Feed one OS pointer delta into the active verified hold.
    pub fn accumulate(&mut self, dx: i32, dy: i32) -> Option<SideGestureAction> {
        let hold = self.hold.as_mut()?;
        let direction = hold.swipe.accumulate(dx, dy)?;
        Some(hold.action_for(direction))
    }

    /// End the matching HID++ hold, returning its click action when no swipe
    /// committed. A release from another device/button is ignored.
    pub fn end(&mut self, device_key: &str, button: ButtonId) -> Option<SideGestureAction> {
        let matches = self
            .hold
            .as_ref()
            .is_some_and(|h| h.device_key == device_key && h.button == button);
        if !matches {
            return None;
        }
        let mut hold = self.hold.take()?;
        hold.swipe
            .end()
            .then(|| hold.action_for(GestureDirection::Click))
    }

    /// Cancel a hold owned by a capture session that stopped or was replaced.
    pub fn cancel_device(&mut self, device_key: &str) {
        if self
            .hold
            .as_ref()
            .is_some_and(|hold| hold.device_key == device_key)
        {
            self.hold = None;
        }
    }

    /// Cancel any live hold after the OS movement tap was interrupted.
    pub fn cancel(&mut self) {
        self.hold = None;
    }

    #[cfg(test)]
    fn backdate_for_test(&mut self) {
        if let Some(hold) = &mut self.hold {
            hold.since = Instant::now()
                .checked_sub(STALE_HOLD)
                .unwrap_or_else(Instant::now);
            hold.swipe.backdate_hold_for_test();
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "expect is idiomatic for test-only state-machine assertions"
)]
mod tests {
    use super::*;

    fn directions(click: Action, right: Action) -> BTreeMap<GestureDirection, Action> {
        BTreeMap::from([
            (GestureDirection::Click, click),
            (GestureDirection::Right, right),
        ])
    }

    #[test]
    fn verified_hold_resolves_swipe_once_and_not_click() {
        let mut state = SideGestureState::default();
        assert_eq!(
            state.begin(
                "mouse-a".into(),
                ButtonId::Forward,
                directions(Action::MissionControl, Action::NextDesktop),
            ),
            None
        );
        state.backdate_for_test();
        let swipe = state
            .accumulate(100, 0)
            .expect("threshold-crossing motion should resolve a swipe");
        assert_eq!(swipe.device_key, "mouse-a");
        assert_eq!(swipe.button, ButtonId::Forward);
        assert_eq!(swipe.direction, GestureDirection::Right);
        assert_eq!(swipe.action, Action::NextDesktop);
        assert_eq!(state.accumulate(100, 0), None, "fires once per hold");
        assert_eq!(state.end("mouse-a", ButtonId::Forward), None);
    }

    #[test]
    fn verified_press_release_resolves_click() {
        let mut state = SideGestureState::default();
        state.begin(
            "mouse-a".into(),
            ButtonId::Back,
            directions(Action::MissionControl, Action::NextDesktop),
        );
        let click = state
            .end("mouse-a", ButtonId::Back)
            .expect("unswiped hold should resolve its click");
        assert_eq!(click.direction, GestureDirection::Click);
        assert_eq!(click.action, Action::MissionControl);
    }

    #[test]
    fn other_device_release_cannot_end_verified_hold() {
        let mut state = SideGestureState::default();
        state.begin(
            "mouse-a".into(),
            ButtonId::Forward,
            directions(Action::MissionControl, Action::NextDesktop),
        );
        assert_eq!(state.end("mouse-b", ButtonId::Forward), None);
        assert!(state.end("mouse-a", ButtonId::Forward).is_some());
    }

    #[test]
    fn second_live_hold_fires_plain_click_without_stealing_first() {
        let mut state = SideGestureState::default();
        state.begin(
            "mouse-a".into(),
            ButtonId::Back,
            directions(Action::MissionControl, Action::NextDesktop),
        );
        let refused = state
            .begin(
                "mouse-b".into(),
                ButtonId::Forward,
                directions(Action::AppExpose, Action::PreviousDesktop),
            )
            .expect("second hold should resolve as its click");
        assert_eq!(refused.device_key, "mouse-b");
        assert_eq!(refused.action, Action::AppExpose);
        assert!(state.end("mouse-a", ButtonId::Back).is_some());
    }

    #[test]
    fn stale_hold_is_replaced() {
        let mut state = SideGestureState::default();
        state.begin(
            "mouse-a".into(),
            ButtonId::Back,
            directions(Action::MissionControl, Action::NextDesktop),
        );
        state.backdate_for_test();
        assert_eq!(
            state.begin(
                "mouse-b".into(),
                ButtonId::Forward,
                directions(Action::AppExpose, Action::PreviousDesktop),
            ),
            None
        );
        assert!(state.end("mouse-b", ButtonId::Forward).is_some());
    }

    #[test]
    fn cancelling_owner_drops_hold_without_click() {
        let mut state = SideGestureState::default();
        state.begin(
            "mouse-a".into(),
            ButtonId::Back,
            directions(Action::MissionControl, Action::NextDesktop),
        );
        state.cancel_device("mouse-a");
        assert_eq!(state.end("mouse-a", ButtonId::Back), None);
    }

    #[test]
    fn runtime_interruption_cancels_before_release_can_click() {
        let runtime = SideGestureRuntime::default();
        runtime.begin(
            "mouse-a".into(),
            ButtonId::Back,
            directions(Action::MissionControl, Action::NextDesktop),
        );
        runtime.interrupt();
        assert_eq!(runtime.end("mouse-a", ButtonId::Back), None);
    }
}
