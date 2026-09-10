//! Owned firmware restoration state for host-switch capture.

use std::{
    fmt,
    sync::{Arc, Weak},
};

use hidpp::channel::HidppChannel;
use thiserror::Error;

use super::{ArmedControl, HostSwitchError, restore_host_controls};
use crate::{
    ChannelRegistry, DeviceIoGate, DeviceRoute, SharedChannel, reprog_controls::ReprogControlsV4,
};

/// How a host-switch session released its temporary firmware reporting state.
#[must_use = "pending firmware restoration must be retained by the session manager"]
pub enum HostSwitchSessionOutcome {
    /// Every host control was restored before the session returned.
    Restored {
        /// Host requested by the keyboard, if the session ended on a key press.
        requested_host: Option<u8>,
    },
    /// Restoration is incomplete and must precede any successor session.
    RestorePending {
        /// Host requested before teardown began, if any.
        requested_host: Option<u8>,
        /// Owned capability for retrying restoration on a current publication.
        restore: PendingHostSwitchRestore,
    },
}

impl HostSwitchSessionOutcome {
    /// Split the transition intent from any retained firmware ownership.
    #[must_use]
    pub fn into_parts(self) -> (Option<u8>, Option<PendingHostSwitchRestore>) {
        match self {
            Self::Restored { requested_host } => (requested_host, None),
            Self::RestorePending {
                requested_host,
                restore,
            } => (requested_host, Some(restore)),
        }
    }
}

/// A host-switch setup failure plus any rollback state still owned by OpenLogi.
#[derive(Debug, Error)]
#[error("{error}")]
pub struct HostSwitchSessionFailure {
    #[source]
    error: HostSwitchError,
    pending_restore: Option<PendingHostSwitchRestore>,
}

impl HostSwitchSessionFailure {
    pub(super) fn clean(error: HostSwitchError) -> Self {
        Self {
            error,
            pending_restore: None,
        }
    }

    pub(super) fn with_pending(
        error: HostSwitchError,
        pending_restore: PendingHostSwitchRestore,
    ) -> Self {
        Self {
            error,
            pending_restore: Some(pending_restore),
        }
    }

    /// Split the setup error from firmware ownership the caller must retain.
    #[must_use]
    pub fn into_parts(self) -> (HostSwitchError, Option<PendingHostSwitchRestore>) {
        (self.error, self.pending_restore)
    }
}

impl From<HostSwitchError> for HostSwitchSessionFailure {
    fn from(error: HostSwitchError) -> Self {
        Self::clean(error)
    }
}

/// Result of one bounded pending-restoration attempt.
#[must_use = "a failed restoration returns ownership that must be retained"]
pub enum HostSwitchRestoreOutcome {
    /// Every host control was restored on a publication that remained current.
    Restored,
    /// Restoration remains incomplete.
    RestorePending(PendingHostSwitchRestore),
}

#[derive(Clone, Copy)]
enum RetiredChannelPolicy {
    ReplacementOnly,
    CurrentAllowed,
}

/// Opaque host-control restoration state that outlives its original channel.
///
/// The token retains the exact route, feature index, reporting mode, and
/// original reporting bits. It deliberately holds only a weak reference to
/// the retired channel; every retry resolves the exact-route winner from the
/// current inventory publication.
pub struct PendingHostSwitchRestore {
    route: DeviceRoute,
    retired_channel: Weak<HidppChannel>,
    retired_policy: RetiredChannelPolicy,
    feature_index: u8,
    controls: Vec<ArmedControl>,
}

impl fmt::Debug for PendingHostSwitchRestore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingHostSwitchRestore")
            .field("route", &self.route)
            .field("reporting_count", &self.controls.len())
            .finish_non_exhaustive()
    }
}

impl PendingHostSwitchRestore {
    pub(super) fn new(
        retired: &SharedChannel,
        feature_index: u8,
        controls: Vec<ArmedControl>,
    ) -> Option<Self> {
        (!controls.is_empty()).then(|| Self {
            route: retired.route().clone(),
            retired_channel: Arc::downgrade(retired.channel()),
            retired_policy: RetiredChannelPolicy::ReplacementOnly,
            feature_index,
            controls,
        })
    }

    pub(super) fn allow_current_channel(mut self) -> Self {
        self.retired_policy = RetiredChannelPolicy::CurrentAllowed;
        self
    }

    /// Retry through the exact-route channel currently published by inventory.
    ///
    /// A successful write is accepted only if that same publication remains
    /// current after all awaited writes. If it was replaced during the pass,
    /// all original controls remain pending for the replacement.
    pub async fn retry(self, registry: &ChannelRegistry) -> HostSwitchRestoreOutcome {
        let Some(current) = registry.lookup(&self.route) else {
            return HostSwitchRestoreOutcome::RestorePending(self);
        };
        if matches!(self.retired_policy, RetiredChannelPolicy::ReplacementOnly)
            && self
                .retired_channel
                .upgrade()
                .is_some_and(|retired| Arc::ptr_eq(current.channel(), &retired))
        {
            return HostSwitchRestoreOutcome::RestorePending(self);
        }

        let controls = ReprogControlsV4::new(
            Arc::clone(current.channel()),
            current.device_index(),
            self.feature_index,
        );
        let restored = restore_host_controls(&controls, &self.controls).await;
        if restored && registry.is_current(&current) {
            HostSwitchRestoreOutcome::Restored
        } else {
            HostSwitchRestoreOutcome::RestorePending(self)
        }
    }
}

pub(super) async fn rollback_host_switch_start(
    error: HostSwitchError,
    pending: Option<PendingHostSwitchRestore>,
    registry: &ChannelRegistry,
    device_io: &DeviceIoGate,
) -> HostSwitchSessionFailure {
    let Some(pending) = pending else {
        return HostSwitchSessionFailure::clean(error);
    };
    let pending = pending.allow_current_channel();
    if !device_io.allows_io() {
        return HostSwitchSessionFailure::with_pending(error, pending);
    }
    match pending.retry(registry).await {
        HostSwitchRestoreOutcome::Restored => HostSwitchSessionFailure::clean(error),
        HostSwitchRestoreOutcome::RestorePending(pending) => {
            HostSwitchSessionFailure::with_pending(error, pending)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{HostSwitchStopReason, monitor_host_switch, run_host_switch_session};
    use super::*;
    use crate::backend::NodeId;
    use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};
    use crate::reprog_controls::{CidReporting, ControlId};
    use crate::session::host_switch::{ArmedControl, ReportingMode};

    fn capture_keyboard(request: &[u8], fail_second_control: bool) -> Option<Vec<u8>> {
        let mut response = vec![0; 20];
        response[0] = 0x11;
        response[1..4].copy_from_slice(&request[1..4]);
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            (0, 0) => response[4] = 0x22,
            (0x22, 0) => response[4] = if fail_second_control { 2 } else { 1 },
            (0x22, 1) => {
                if request[4] == 1 {
                    return Some(feature_error(request, 0x08));
                }
                // Host key 1, divertable. Unrelated task/group fields are zero.
                response[4..9].copy_from_slice(&[0, 0xd1, 0, 0, 0x20]);
            }
            (0x22, 2) => {
                response[4..6].copy_from_slice(&request[4..6]);
                // Original raw XY, persistent diversion, and force raw XY set.
                response[6] = 0x54;
            }
            (0x22, 3) => return Some(request.to_vec()),
            _ => return None,
        }
        Some(response)
    }

    #[tokio::test]
    async fn failed_session_and_partial_arm_release_old_channels_and_restore_on_reconnect() {
        for partial_arm in [false, true] {
            let route = DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb35b,
            };
            let node = NodeId::from("host-keyboard".to_owned());
            let registry = ChannelRegistry::default();
            let (raw, writes) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
                if request[2] == 0x22 && request[3] >> 4 == 3 && request[6] & 1 == 0 {
                    Some(feature_error(request, 0x08))
                } else {
                    capture_keyboard(request, partial_arm)
                }
            });
            let channel = scripted_channel(raw).await;
            let retired = Arc::downgrade(&channel);
            registry.replace_node(node.clone(), [route.clone()], channel);
            let (stop, stopped) = tokio::sync::oneshot::channel();
            stop.send(HostSwitchStopReason::Graceful).unwrap();
            let (_signal, gate) = crate::device_io_channel();

            let outcome = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                run_host_switch_session(route.clone(), stopped, &registry, gate),
            )
            .await
            .expect("failed restoration must return ownership instead of looping");
            let pending = match outcome {
                Ok(HostSwitchSessionOutcome::RestorePending {
                    requested_host,
                    restore,
                }) => {
                    assert!(!partial_arm);
                    assert_eq!(requested_host, None);
                    restore
                }
                Err(failure) => {
                    assert!(partial_arm);
                    failure
                        .into_parts()
                        .1
                        .expect("partial arm rollback must retain firmware ownership")
                }
                Ok(HostSwitchSessionOutcome::Restored { .. }) => {
                    panic!("failed writes cannot be clean")
                }
            };
            let reporting: Vec<_> = writes
                .written_reports()
                .into_iter()
                .filter(|request| request[2] == 0x22 && request[3] >> 4 == 3)
                .collect();
            assert_eq!(
                reporting.len(),
                3,
                "one arm, then two failed bounded restores"
            );
            assert_eq!(&reporting[0][4..7], &[0, 0xd1, 0x23]);
            assert_eq!(&reporting[1][4..7], &[0, 0xd1, 0x32]);

            registry.remove_node(&node);
            assert!(
                retired.upgrade().is_none(),
                "pending ownership must release the dead channel"
            );
            let pending = match pending.retry(&registry).await {
                HostSwitchRestoreOutcome::RestorePending(pending) => pending,
                HostSwitchRestoreOutcome::Restored => panic!("absent route cannot be restored"),
            };
            let (raw, fresh) =
                ScriptedRawHidChannel::with_responder(|request| Some(request.to_vec()));
            registry.replace_node(node, [route], scripted_channel(raw).await);
            assert!(matches!(
                pending.retry(&registry).await,
                HostSwitchRestoreOutcome::Restored
            ));
            let reporting = fresh.written_reports();
            assert_eq!(reporting.len(), 1);
            assert_eq!(&reporting[0][4..10], &[0, 0xd1, 0x32, 0, 0, 0]);
        }
    }

    #[tokio::test]
    async fn retirement_before_listener_subscription_is_observed_without_another_event() {
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        };
        let registry = ChannelRegistry::default();
        let (raw, _) = ScriptedRawHidChannel::with_responder(|_| None);
        let retired = SharedChannel::new(scripted_channel(raw).await, route);
        let (_stop, stopped) = tokio::sync::oneshot::channel();
        let (_presses, mut presses) = tokio::sync::mpsc::unbounded_channel();
        let (_signal, gate) = crate::device_io_channel();
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            monitor_host_switch(stopped, &mut presses, &registry, &retired, gate),
        )
        .await
        .expect("a retired publication needs no further notification");
        assert_eq!(result, (None, false));
    }

    #[tokio::test]
    async fn replaced_publication_during_successful_write_remains_pending() {
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        };
        let node = NodeId::from("keyboard-node".to_owned());
        let registry = ChannelRegistry::default();
        let (retired_raw, _) = ScriptedRawHidChannel::with_responder(|_| None);
        let retired = SharedChannel::new(scripted_channel(retired_raw).await, route.clone());
        let control = ArmedControl {
            cid: 0x00d3,
            host: 0,
            mode: ReportingMode::Diverted,
            original: CidReporting {
                cid: ControlId(0x00d3),
                diverted: false,
                persistently_diverted: true,
                force_raw_xy: true,
                raw_xy: false,
                remap: Some(ControlId(0x1234)),
                analytics_key_events: false,
                raw_wheel: true,
            },
        };
        let pending = PendingHostSwitchRestore::new(&retired, 0x22, vec![control])
            .expect("one armed control must require restoration");
        let (winner_raw, winner_handle) =
            ScriptedRawHidChannel::with_responder(|request| Some(request.to_vec()));
        let winner = scripted_channel(winner_raw).await;
        let replacement_registry = registry.clone();
        let replacement_node = node.clone();
        let replacement_route = route.clone();
        let (superseded_raw, superseded_handle) =
            ScriptedRawHidChannel::with_dynamic_responder(move |request| {
                replacement_registry.replace_node(
                    replacement_node.clone(),
                    [replacement_route.clone()],
                    winner.clone(),
                );
                Some(request.to_vec())
            });
        registry.replace_node(node, [route], scripted_channel(superseded_raw).await);

        let pending = match pending.retry(&registry).await {
            HostSwitchRestoreOutcome::RestorePending(pending) => pending,
            HostSwitchRestoreOutcome::Restored => panic!("superseded write counted as final"),
        };
        assert_eq!(superseded_handle.written_reports().len(), 1);
        assert!(matches!(
            pending.retry(&registry).await,
            HostSwitchRestoreOutcome::Restored
        ));
        assert_eq!(winner_handle.written_reports().len(), 1);
    }
}
