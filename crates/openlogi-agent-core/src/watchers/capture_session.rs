//! Shared lifecycle state for HID++ capture managers.
//!
//! Gesture and keyboard capture deliberately keep separate manager loops: their
//! event ordering, cardinality and dispatch state differ. This module shares
//! only the invariant they have in common — one tracked hardware epoch stays
//! authoritative until its asynchronous teardown reports completion.

use tokio::sync::oneshot;

use crate::runtime::HidppSessionId;

/// Effect of reconciling one tracked session against the latest wanted plan.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ReconcileAction {
    /// Nothing visible to the manager changed.
    None,
    /// Hardware remains armed, but dispatch state changed. The manager must
    /// cancel input lifecycles admitted under the previous dispatch plan.
    DispatchChanged,
    /// Hardware teardown started. The retiring dispatch plan stays frozen and
    /// authoritative until completion.
    Retiring,
}

/// How a completion report affects the currently tracked slot.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CompletionAction {
    /// The report belongs to an untracked or superseded epoch.
    Ignore,
    /// Remove the tracked epoch. `unexpected` means an active task exited
    /// without first being asked to drain.
    Remove { unexpected: bool },
}

enum SessionPhase {
    Active(oneshot::Sender<()>),
    Draining,
}

/// One capture epoch, including its hardware identity, dispatch state and
/// acknowledged teardown phase.
pub(super) struct CaptureSession<Target, Dispatch> {
    id: HidppSessionId,
    target: Target,
    dispatch: Dispatch,
    phase: SessionPhase,
}

impl<Target, Dispatch> CaptureSession<Target, Dispatch> {
    /// Begin tracking an active capture task.
    pub(super) fn active(
        id: HidppSessionId,
        target: Target,
        dispatch: Dispatch,
        stop: oneshot::Sender<()>,
    ) -> Self {
        Self {
            id,
            target,
            dispatch,
            phase: SessionPhase::Active(stop),
        }
    }

    /// Exact device + epoch identity carried by captured events.
    pub(super) fn id(&self) -> &HidppSessionId {
        &self.id
    }

    /// Hardware capture identity that decides whether rearming is required.
    #[cfg(test)]
    pub(super) fn target(&self) -> &Target {
        &self.target
    }

    /// Dispatch state owned by this epoch.
    pub(super) fn dispatch(&self) -> &Dispatch {
        &self.dispatch
    }

    /// Whether this task has not yet been asked to drain.
    pub(super) fn is_active(&self) -> bool {
        matches!(&self.phase, SessionPhase::Active(_))
    }

    /// Whether an event belongs to this tracked epoch. Draining epochs remain
    /// owners until their task acknowledges teardown completion.
    pub(super) fn owns(&self, event_session: &HidppSessionId) -> bool {
        self.id.same_epoch(event_session)
    }

    /// Move future action dispatch for this hardware epoch to a newly adopted
    /// config namespace. The task's queued events keep their original ID but
    /// remain attributable through [`Self::owns`]'s epoch comparison.
    pub(super) fn rekey(&mut self, device_key: &str) {
        self.id.rekey(device_key);
    }

    /// Classify a task-completion report against this tracked epoch.
    pub(super) fn completion(&self, done_session: &HidppSessionId) -> CompletionAction {
        if self.owns(done_session) {
            CompletionAction::Remove {
                unexpected: self.is_active(),
            }
        } else {
            CompletionAction::Ignore
        }
    }
}

impl<Target: PartialEq, Dispatch: Clone + PartialEq> CaptureSession<Target, Dispatch> {
    /// Reconcile against the latest wanted target and dispatch state. A target
    /// change begins teardown exactly once; dispatch-only changes hot-refresh
    /// the plan while preserving the hardware epoch.
    pub(super) fn reconcile(&mut self, wanted: Option<(&Target, &Dispatch)>) -> ReconcileAction {
        if !self.is_active() {
            return ReconcileAction::None;
        }
        if let Some((target, dispatch)) = wanted
            && self.target == *target
        {
            if self.dispatch == *dispatch {
                return ReconcileAction::None;
            }
            self.dispatch.clone_from(dispatch);
            return ReconcileAction::DispatchChanged;
        }
        let SessionPhase::Active(stop) = std::mem::replace(&mut self.phase, SessionPhase::Draining)
        else {
            return ReconcileAction::None;
        };
        let _ = stop.send(());
        ReconcileAction::Retiring
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> CaptureSession<u8, &'static str> {
        let (stop, _stop_rx) = oneshot::channel();
        CaptureSession::active(HidppSessionId::with_epoch("mouse-a", 7), 1, "old", stop)
    }

    #[test]
    fn dispatch_refresh_keeps_the_hardware_epoch_active() {
        let mut session = session();

        assert_eq!(
            session.reconcile(Some((&1, &"new"))),
            ReconcileAction::DispatchChanged
        );
        assert!(session.is_active());
        assert_eq!(session.dispatch(), &"new");
    }

    #[test]
    fn target_change_freezes_dispatch_and_drains_once() {
        let mut session = session();

        assert_eq!(
            session.reconcile(Some((&2, &"new"))),
            ReconcileAction::Retiring
        );
        assert!(!session.is_active());
        assert_eq!(session.dispatch(), &"old");
        assert_eq!(
            session.reconcile(Some((&1, &"later"))),
            ReconcileAction::None
        );
        assert_eq!(session.dispatch(), &"old");
    }

    #[test]
    fn completion_distinguishes_active_draining_and_stale_epochs() {
        let mut session = session();
        assert_eq!(
            session.completion(&HidppSessionId::with_epoch("mouse-a", 7)),
            CompletionAction::Remove { unexpected: true }
        );
        assert_eq!(session.reconcile(None), ReconcileAction::Retiring);
        assert_eq!(
            session.completion(&HidppSessionId::with_epoch("mouse-a", 7)),
            CompletionAction::Remove { unexpected: false }
        );
        assert_eq!(
            session.completion(&HidppSessionId::with_epoch("mouse-a", 6)),
            CompletionAction::Ignore
        );
    }

    #[test]
    fn config_rekey_preserves_hardware_epoch_ownership() {
        let mut session = session();
        let queued = HidppSessionId::with_epoch("legacy-key", 7);

        session.rekey("unit:00000001");

        assert_eq!(session.id().device_key(), "unit:00000001");
        assert!(
            session.owns(&queued),
            "queued task events remain attributable after a hot config rekey"
        );
    }
}
