//! What a host-switch session owes the firmware, and how its teardown reports
//! it. The token that carries an unfinished restore is `session::restore`'s.

use std::{fmt, sync::Arc};

use super::{ArmedControl, HostSwitchError, restore_host_controls};
use crate::session::restore::{
    PendingRestore, RestoreOutcome, RestorePlan, SessionFailure, rollback_start,
};
use crate::{ChannelRegistry, DeviceIoGate, SharedChannel, reprog_controls::ReprogControlsV4};

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

/// What a host-switch session writes to hand the keyboard's host controls
/// back: the exact feature index, reporting mode, and original reporting bits
/// of every control it armed.
///
/// Each write is bounded and tried twice (`restore_host_controls`); a control
/// that still fails leaves the whole plan owed.
pub struct HostSwitchRestorePlan {
    feature_index: u8,
    controls: Vec<ArmedControl>,
}

impl fmt::Debug for HostSwitchRestorePlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostSwitchRestorePlan")
            .field("reporting_count", &self.controls.len())
            .finish_non_exhaustive()
    }
}

impl RestorePlan for HostSwitchRestorePlan {
    async fn restore_on(&self, current: &SharedChannel) -> bool {
        let controls = ReprogControlsV4::new(
            Arc::clone(current.channel()),
            current.device_index(),
            self.feature_index,
        );
        restore_host_controls(&controls, &self.controls).await
    }
}

/// Host-control restoration state that outlives the channel it was armed on.
pub type PendingHostSwitchRestore = PendingRestore<HostSwitchRestorePlan>;

/// Result of one bounded pending-restoration attempt.
pub type HostSwitchRestoreOutcome = RestoreOutcome<HostSwitchRestorePlan>;

/// A host-switch setup failure plus any rollback state still owned by OpenLogi.
pub type HostSwitchSessionFailure = SessionFailure<HostSwitchError, HostSwitchRestorePlan>;

impl PendingHostSwitchRestore {
    /// The restore owed for `controls`, or `None` when nothing was armed.
    pub(super) fn new(
        retired: &SharedChannel,
        feature_index: u8,
        controls: Vec<ArmedControl>,
    ) -> Option<Self> {
        (!controls.is_empty()).then(|| {
            Self::owing(
                retired,
                HostSwitchRestorePlan {
                    feature_index,
                    controls,
                },
            )
        })
    }
}

/// Roll back a partially armed session. Unlike capture's rollback this writes
/// nothing while host device I/O is suspended: the ownership is returned for
/// the manager to retry once the gate reopens.
pub(super) async fn rollback_host_switch_start(
    error: HostSwitchError,
    pending: Option<PendingHostSwitchRestore>,
    registry: &ChannelRegistry,
    device_io: &DeviceIoGate,
) -> HostSwitchSessionFailure {
    if device_io.allows_io() {
        return rollback_start(error, pending, registry).await;
    }
    match pending {
        Some(pending) => {
            HostSwitchSessionFailure::with_pending(error, pending.allow_current_channel())
        }
        None => HostSwitchSessionFailure::clean(error),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        HostSwitchStop, HostSwitchStopReason, monitor_host_switch, run_host_switch_session,
    };
    use super::*;
    use crate::DeviceRoute;
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
        assert_eq!(result, HostSwitchStop::ChannelChanged);
    }

    #[tokio::test]
    async fn monitor_names_why_it_stopped_on_a_current_channel() {
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        };
        let registry = ChannelRegistry::default();
        let (raw, _) = ScriptedRawHidChannel::with_responder(|_| None);
        registry.replace_node(
            NodeId::from("keyboard-node".to_owned()),
            [route.clone()],
            scripted_channel(raw).await,
        );
        let current = registry
            .lookup(&route)
            .expect("the keyboard channel should be published");
        let monitored = |stopped, mut presses| {
            let (registry, current) = (registry.clone(), current.clone());
            async move {
                let (_signal, gate) = crate::device_io_channel();
                tokio::time::timeout(
                    std::time::Duration::from_millis(100),
                    monitor_host_switch(stopped, &mut presses, &registry, &current, gate),
                )
                .await
                .expect("a ready stop source needs no further event")
            }
        };

        for (reason, expected) in [
            (
                Some(HostSwitchStopReason::Graceful),
                HostSwitchStop::Shutdown,
            ),
            (
                Some(HostSwitchStopReason::DeviceLost),
                HostSwitchStop::ChannelChanged,
            ),
            // A dropped stop sender is a lost owner, not a graceful request.
            (None, HostSwitchStop::ChannelChanged),
        ] {
            let (stop, stopped) = tokio::sync::oneshot::channel();
            match reason {
                Some(reason) => stop.send(reason).unwrap(),
                None => drop(stop),
            }
            let (_press, presses) = tokio::sync::mpsc::unbounded_channel();
            assert_eq!(monitored(stopped, presses).await, expected, "{reason:?}");
        }

        let (_stop, stopped) = tokio::sync::oneshot::channel();
        let (press, presses) = tokio::sync::mpsc::unbounded_channel();
        press.send(2).unwrap();
        assert_eq!(
            monitored(stopped, presses).await,
            HostSwitchStop::Pressed(2)
        );
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
