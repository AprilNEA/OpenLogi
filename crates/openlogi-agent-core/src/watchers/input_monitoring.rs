//! Input Monitoring permission polling watcher.

use std::time::Duration;

use tokio::sync::mpsc;

use super::poll::{self, Poll};

/// What this process can do with the Input Monitoring grant, decided once when
/// the agent arms and consulted on every later change.
///
/// macOS resolves `kTCCServiceListenEvent` for a process when it starts, so a
/// grant made afterwards reaches the *next* launch of the identity rather than
/// the running one. A process in [`Access::NeedsSuccessor`] cannot be rescued
/// by retrying: only a successor can open HID devices, so the agent has to
/// become one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// The agent may open HID devices in this process.
    Granted,
    /// This process cannot open HID devices. A grant that arrives later is
    /// real, but it is worth nothing here until the agent restarts into it.
    NeedsSuccessor,
}

/// Whether a grant the watcher just observed has to end this process.
///
/// `granted_now` is what the watcher reports for *this* process, and that is
/// not the same question as whether the user granted anything: macOS applies
/// the decision at process start, so a process that already held the grant and
/// then lost it must stay put. Relaunching on a revoke would put the agent
/// straight back into the consent dialog it just escaped, and a user who
/// declined that dialog on purpose would watch it cycle forever.
#[must_use]
pub const fn needs_successor(access: Access, granted_now: bool) -> bool {
    granted_now && matches!(access, Access::NeedsSuccessor)
}

/// Watch macOS Input Monitoring permission changes.
pub fn spawn(period: Duration) -> mpsc::UnboundedReceiver<bool> {
    if !cfg!(target_os = "macos") {
        // Only macOS gates HID access behind a privacy grant.
        return poll::constant(true);
    }
    Poll {
        name: "openlogi-input-monitoring-watcher",
        period,
        degrades: "the permission status won't auto-refresh",
    }
    .on_change(openlogi_hid::permissions::has_access)
}

#[cfg(test)]
mod tests {
    use super::{Access, needs_successor};

    /// The #1304 sequence: the agent armed while the permission was denied,
    /// the grant then arrived in System Settings with no dialog left to answer,
    /// and the watcher reported the change. Retrying in this process cannot
    /// work, so the agent has to restart into it.
    #[test]
    fn a_grant_this_process_cannot_use_restarts_the_agent() {
        assert!(needs_successor(Access::NeedsSuccessor, true));
    }

    /// The same denial, still denied: the agent keeps running and the inventory
    /// keeps reporting the failed open, rather than restarting into nothing.
    #[test]
    fn a_process_that_is_still_denied_keeps_running() {
        assert!(!needs_successor(Access::NeedsSuccessor, false));
    }

    /// A revoke must not restart the agent: the successor would come straight
    /// back to the consent dialog the user just dismissed.
    #[test]
    fn a_revoke_never_restarts_the_agent() {
        assert!(!needs_successor(Access::Granted, false));
    }

    /// A grant that was already in effect needs no restart. This is also the
    /// state every non-macOS agent is permanently in, where HID access is never
    /// gated, so no observation there can call for one.
    #[test]
    fn a_grant_already_in_effect_never_restarts_the_agent() {
        assert!(!needs_successor(Access::Granted, true));
    }
}
