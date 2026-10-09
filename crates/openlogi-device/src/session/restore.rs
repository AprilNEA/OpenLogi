//! Firmware ownership that outlives the session which took it.
//!
//! A session that diverts controls owes the firmware a restore. When that
//! restore cannot complete — inventory replaced the transport, or a write
//! failed — the debt leaves the session as a [`PendingRestore`] token, which
//! the caller retains and retries on whatever channel inventory publishes
//! next. The capture sessions and the host-switch session share everything
//! about that token except *what* it writes back, which is their
//! [`RestorePlan`].

use std::fmt;
use std::future::Future;
use std::sync::{Arc, Weak};

use hidpp::channel::HidppChannel;
use thiserror::Error;

use crate::{ChannelRegistry, DeviceRoute, SharedChannel};

/// What one kind of session writes to hand its controls back to the firmware.
pub trait RestorePlan {
    /// Write every recorded control back through `current`, the channel
    /// inventory publishes now, and report whether all of them landed.
    ///
    /// How a write is bounded and whether it is repeated is the plan's own
    /// policy: the host-switch plan times each write and tries it twice, the
    /// capture plan makes one untimed attempt.
    fn restore_on(&self, current: &SharedChannel) -> impl Future<Output = bool> + Send;
}

/// Whether a retry may use the transport on which the session originally ran.
#[derive(Clone, Copy)]
enum RetiredChannelPolicy {
    /// Inventory declared the transport obsolete; wait for another current
    /// publication instead of writing underneath its replacement.
    ReplacementOnly,
    /// The session stopped normally but a restore write failed, so retrying
    /// the still-current original transport is safe.
    CurrentAllowed,
}

/// Opaque firmware ownership that survives the transport which armed it.
///
/// The token owns its original route and its [`RestorePlan`], and holds only a
/// weak reference to the retired channel: every retry resolves the exact-route
/// winner from the current inventory publication. It is consumed by every
/// retry, and a failed retry returns it through
/// [`RestoreOutcome::RestorePending`], so callers cannot accidentally treat a
/// borrowed `false` as completion.
pub struct PendingRestore<P> {
    route: DeviceRoute,
    retired_channel: Weak<HidppChannel>,
    retired_policy: RetiredChannelPolicy,
    plan: P,
}

impl<P: fmt::Debug> fmt::Debug for PendingRestore<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingRestore")
            .field("route", &self.route)
            .field("plan", &self.plan)
            .finish_non_exhaustive()
    }
}

impl<P> PendingRestore<P> {
    /// Record that `plan` is owed to the device behind `retired`.
    pub(crate) fn owing(retired: &SharedChannel, plan: P) -> Self {
        Self {
            route: retired.route().clone(),
            retired_channel: Arc::downgrade(retired.channel()),
            retired_policy: RetiredChannelPolicy::ReplacementOnly,
            plan,
        }
    }

    /// Permit a retry on the original channel after a normal teardown write
    /// failed while that publication was still current.
    pub(crate) fn allow_current_channel(mut self) -> Self {
        self.retired_policy = RetiredChannelPolicy::CurrentAllowed;
        self
    }
}

impl<P: RestorePlan> PendingRestore<P> {
    /// Retry through the exact-route channel currently published by inventory.
    ///
    /// Success is accepted only if the same publication remains current after
    /// every awaited restore write. A concurrent replacement returns this
    /// token as pending, with every control still owed, so the new winner is
    /// restored on the next attempt.
    pub async fn retry(self, registry: &ChannelRegistry) -> RestoreOutcome<P> {
        let Some(current) = registry.lookup(&self.route) else {
            return RestoreOutcome::RestorePending(self);
        };
        if matches!(self.retired_policy, RetiredChannelPolicy::ReplacementOnly)
            && self
                .retired_channel
                .upgrade()
                .is_some_and(|retired| Arc::ptr_eq(current.channel(), &retired))
        {
            return RestoreOutcome::RestorePending(self);
        }
        let restored = self.plan.restore_on(&current).await;
        if restored && registry.is_current(&current) {
            RestoreOutcome::Restored
        } else {
            RestoreOutcome::RestorePending(self)
        }
    }
}

/// How a session, or one later retry, left its firmware restoration.
#[must_use = "a pending restore must be retained until firmware ownership is released"]
pub enum RestoreOutcome<P> {
    /// Every control was restored on a publication that remained current.
    Restored,
    /// Restoration is incomplete. The caller must retain this token and retry
    /// it before arming a successor for the same physical device.
    RestorePending(PendingRestore<P>),
}

/// A session setup failure plus any firmware ownership its rollback could not
/// release.
#[derive(Debug, Error)]
#[error("{error}")]
pub struct SessionFailure<E, P> {
    #[source]
    error: E,
    /// Boxed: only a failed rollback carries one, and unboxed it would make
    /// every session `Result` as large as the token.
    pending_restore: Option<Box<PendingRestore<P>>>,
}

impl<E, P> SessionFailure<E, P> {
    pub(crate) fn clean(error: E) -> Self {
        Self {
            error,
            pending_restore: None,
        }
    }

    pub(crate) fn with_pending(error: E, pending: PendingRestore<P>) -> Self {
        Self {
            error,
            pending_restore: Some(Box::new(pending)),
        }
    }

    /// Split the setup error from firmware ownership the caller must retain.
    #[must_use]
    pub fn into_parts(self) -> (E, Option<PendingRestore<P>>) {
        (self.error, self.pending_restore.map(|pending| *pending))
    }
}

impl<E, P> From<E> for SessionFailure<E, P> {
    fn from(error: E) -> Self {
        Self::clean(error)
    }
}

/// Roll back a partially armed session without losing firmware ownership when
/// any compensating write fails.
pub(crate) async fn rollback_start<E, P: RestorePlan>(
    error: E,
    pending: Option<PendingRestore<P>>,
    registry: &ChannelRegistry,
) -> SessionFailure<E, P> {
    let Some(pending) = pending else {
        return SessionFailure::clean(error);
    };
    match pending.allow_current_channel().retry(registry).await {
        RestoreOutcome::Restored => SessionFailure::clean(error),
        RestoreOutcome::RestorePending(pending) => SessionFailure::with_pending(error, pending),
    }
}
