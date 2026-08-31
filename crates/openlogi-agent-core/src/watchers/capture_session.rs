//! Shared lifecycle state for HID++ capture managers.
//!
//! Gesture and keyboard capture deliberately keep separate manager loops: their
//! event ordering, cardinality and dispatch state differ. This module shares
//! only the invariants they have in common: one tracked hardware epoch stays
//! authoritative until its asynchronous teardown reports completion, and a
//! running epoch is mutually exclusive with post-session recovery.

use tokio::sync::oneshot;
use tokio::time::Instant;

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

/// Firmware restoration and restart pacing retained after a capture task has
/// completed. Both may be present after an unexpected completion.
pub(super) struct CaptureRecovery<Restore> {
    pub(super) pending_restore: Option<Restore>,
    pub(super) restart_at: Option<Instant>,
}

impl<Restore> CaptureRecovery<Restore> {
    pub(super) fn is_empty(&self) -> bool {
        self.pending_restore.is_none() && self.restart_at.is_none()
    }
}

/// One manager-owned hardware slot. A session remains in `Running` while it
/// drains; only its matching ordered completion moves the slot to recovery.
pub(super) enum CaptureSlot<Target, Dispatch, Restore> {
    Running(CaptureSession<Target, Dispatch>),
    Recovering(CaptureRecovery<Restore>),
}

impl<Target, Dispatch, Restore> CaptureSlot<Target, Dispatch, Restore> {
    pub(super) fn running(session: CaptureSession<Target, Dispatch>) -> Self {
        Self::Running(session)
    }

    pub(super) fn recovering(
        pending_restore: Option<Restore>,
        restart_at: Option<Instant>,
    ) -> Self {
        Self::Recovering(CaptureRecovery {
            pending_restore,
            restart_at,
        })
    }

    pub(super) fn session(&self) -> Option<&CaptureSession<Target, Dispatch>> {
        let Self::Running(session) = self else {
            return None;
        };
        Some(session)
    }

    pub(super) fn session_mut(&mut self) -> Option<&mut CaptureSession<Target, Dispatch>> {
        let Self::Running(session) = self else {
            return None;
        };
        Some(session)
    }

    pub(super) fn recovery(&self) -> Option<&CaptureRecovery<Restore>> {
        let Self::Recovering(recovery) = self else {
            return None;
        };
        Some(recovery)
    }

    pub(super) fn recovery_mut(&mut self) -> Option<&mut CaptureRecovery<Restore>> {
        let Self::Recovering(recovery) = self else {
            return None;
        };
        Some(recovery)
    }

    /// Settle one ordered completion. Stale epochs and reports received after
    /// the slot has already entered recovery leave the slot untouched.
    pub(super) fn complete(
        &mut self,
        done_session: &HidppSessionId,
        pending_restore: Option<Restore>,
        restart_after_unexpected: Option<Instant>,
    ) -> Option<(HidppSessionId, bool)> {
        let Self::Running(session) = self else {
            return None;
        };
        let CompletionAction::Remove { unexpected } = session.completion(done_session) else {
            return None;
        };
        let dispatch_session = session.id().clone();
        *self = Self::recovering(
            pending_restore,
            if unexpected {
                restart_after_unexpected
            } else {
                None
            },
        );
        Some((dispatch_session, unexpected))
    }
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

    #[test]
    fn matching_completion_moves_the_active_slot_to_recovery() {
        let mut slot = CaptureSlot::<_, _, ()>::running(session());
        let restart_at = Instant::now();

        let (_, unexpected) = slot
            .complete(
                &HidppSessionId::with_epoch("mouse-a", 7),
                Some(()),
                Some(restart_at),
            )
            .expect("the current epoch should complete");

        assert!(unexpected);
        assert!(
            slot.session().is_none(),
            "recovery cannot retain a running epoch"
        );
        let recovery = slot.recovery().expect("the slot should now be recovering");
        assert!(
            recovery.pending_restore.is_some(),
            "firmware restoration remains pending"
        );
        assert_eq!(
            recovery.restart_at,
            Some(restart_at),
            "restore and restart pacing may legitimately coexist"
        );
    }

    #[test]
    fn stale_completion_cannot_displace_the_running_epoch_or_recovery() {
        let mut slot = CaptureSlot::<_, _, &'static str>::running(session());

        assert!(
            slot.complete(
                &HidppSessionId::with_epoch("mouse-a", 6),
                Some("stale"),
                Some(Instant::now()),
            )
            .is_none()
        );
        assert_eq!(
            slot.session().map(CaptureSession::id),
            Some(&HidppSessionId::with_epoch("mouse-a", 7))
        );

        assert!(
            slot.complete(
                &HidppSessionId::with_epoch("mouse-a", 7),
                Some("current"),
                None,
            )
            .is_some()
        );
        assert!(
            slot.complete(
                &HidppSessionId::with_epoch("mouse-a", 7),
                Some("duplicate"),
                Some(Instant::now()),
            )
            .is_none()
        );
        let recovery = slot.recovery().expect("the original recovery must remain");
        assert_eq!(recovery.pending_restore, Some("current"));
        assert_eq!(recovery.restart_at, None);
    }
}
