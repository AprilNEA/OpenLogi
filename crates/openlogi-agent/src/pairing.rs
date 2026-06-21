//! Agent-side device pairing, exposed to the GUI over IPC.
//!
//! The agent owns all device I/O, so pairing — which opens the receiver — must
//! run here: a GUI that opened a receiver channel would clash with the agent's
//! live capture session on the same Bolt receiver (one process can't read the
//! same HID node through two channels). The GUI drives this over IPC
//! (`start_pairing` / `pair_device` / `cancel_pairing` + a `next_pairing`
//! long-poll for the event stream).
//!
//! While a session runs, the agent holds an exclusive receiver lease through
//! [`SharedRuntime::receiver_access`], so `run_pairing` can own the receiver's
//! HID node. Dropping that lease lets HID++ capture resume when the session ends
//! (every end — including cancel — emits a terminal event).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use openlogi_agent_core::ipc::{FoundDevice, PairingCommandError, PairingUpdate};
use openlogi_agent_core::orchestrator::SharedRuntime;
use openlogi_agent_core::receiver_access::PairingReceiverLease;
use openlogi_agent_core::watchers::pairing::{self, Control};
use openlogi_hid::{DiscoveredDevice, PairingEvent, ReceiverSelector};
use tokio::sync::{Mutex, mpsc};
use tracing::warn;

/// How long the agent holds a `next_pairing` long-poll before returning `None`.
/// Comfortably under the client's request deadline so the agent answers first.
const HOLD: Duration = Duration::from_secs(20);

/// How long pairing waits for HID++ capture to release the receiver lease.
const RECEIVER_LEASE_TIMEOUT: Duration = Duration::from_secs(5);

/// Address-keyed cache of the full discovered devices, so the GUI can pair by
/// address without round-tripping the non-serializable `DiscoveredDevice`.
type DeviceCache = Arc<StdMutex<HashMap<[u8; 6], DiscoveredDevice>>>;

/// Owns the pairing watcher and translates its event stream for the IPC layer.
pub struct PairingManager {
    ctrl: mpsc::UnboundedSender<Control>,
    updates: Mutex<mpsc::UnboundedReceiver<PairingUpdate>>,
    devices: DeviceCache,
    /// Count of outstanding pairing sessions. The watcher is single-session,
    /// so `start` atomically transitions this 0 → 1. The translator decrements
    /// it on each terminal event and releases the receiver lease when it returns
    /// to zero. Balanced: one accepted `start` ⇒ exactly one terminal.
    sessions: Arc<AtomicUsize>,
    receiver_lease: Arc<StdMutex<Option<PairingReceiverLease>>>,
    shared: SharedRuntime,
}

impl PairingManager {
    /// Spawn the pairing watcher and its event translator. One per agent; must
    /// be called inside the tokio runtime (it spawns the translator task).
    #[must_use]
    pub fn new(shared: SharedRuntime) -> Self {
        let (ctrl, raw_events) = pairing::spawn();
        let (upd_tx, upd_rx) = mpsc::unbounded_channel();
        let devices: DeviceCache = Arc::new(StdMutex::new(HashMap::new()));
        let sessions = Arc::new(AtomicUsize::new(0));
        let receiver_lease = Arc::new(StdMutex::new(None));
        tokio::spawn(translate(
            raw_events,
            upd_tx.clone(),
            Arc::clone(&devices),
            Arc::clone(&sessions),
            Arc::clone(&receiver_lease),
        ));
        Self {
            ctrl,
            updates: Mutex::new(upd_rx),
            devices,
            sessions,
            receiver_lease,
            shared,
        }
    }

    /// Begin a session: forget the previous discovery, pause capture, then start.
    pub async fn start(&self, selector: ReceiverSelector) -> Result<(), PairingCommandError> {
        if self
            .sessions
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            warn!("pairing start requested while a session is already active");
            return Err(PairingCommandError::AlreadyActive);
        }
        let admission = SessionAdmission::new(Arc::clone(&self.sessions));

        if let Ok(mut devices) = self.devices.lock() {
            devices.clear();
        }
        let Ok(receiver_lease) = tokio::time::timeout(
            RECEIVER_LEASE_TIMEOUT,
            self.shared.receiver_access.acquire_for_pairing(),
        )
        .await
        else {
            warn!("timed out waiting for receiver capture to stop; pairing not started");
            return Err(PairingCommandError::ReceiverBusy);
        };
        if let Ok(mut slot) = self.receiver_lease.lock() {
            *slot = Some(receiver_lease);
        } else {
            warn!("pairing receiver lease lock poisoned; aborting start");
            return Err(PairingCommandError::ReceiverAccessUnavailable);
        }
        if let Err(e) = self.ctrl.send(Control::Start(selector)) {
            self.release_receiver_lease();
            warn!(error = %e, "could not start pairing session; pairing watcher is unavailable");
            return Err(PairingCommandError::WatcherUnavailable);
        }
        admission.commit();
        Ok(())
    }

    /// Pair with a previously discovered device by address.
    pub fn pair(&self, address: [u8; 6]) -> Result<(), PairingCommandError> {
        let device = self
            .devices
            .lock()
            .ok()
            .and_then(|devices| devices.get(&address).cloned());
        if let Some(device) = device {
            self.ctrl
                .send(Control::Pair(device))
                .map_err(|_| PairingCommandError::WatcherUnavailable)
        } else {
            warn!(?address, "pair requested for an unknown device");
            Err(PairingCommandError::UnknownDevice)
        }
    }

    /// Cancel the in-progress session. The resulting `Failed(Cancelled)` event
    /// releases the receiver lease via the translator — don't release it here, or
    /// capture could re-acquire the receiver while `run_pairing` still holds it.
    pub fn cancel(&self) -> Result<(), PairingCommandError> {
        if self.sessions.load(Ordering::Acquire) == 0 {
            return Ok(());
        }
        self.ctrl
            .send(Control::Cancel)
            .map_err(|_| PairingCommandError::WatcherUnavailable)
    }

    /// Long-poll the next pairing step; `None` when the hold window elapses.
    pub async fn next_update(&self) -> Option<PairingUpdate> {
        let mut rx = self.updates.lock().await;
        tokio::time::timeout(HOLD, rx.recv()).await.ok().flatten()
    }

    fn release_receiver_lease(&self) {
        if let Ok(mut slot) = self.receiver_lease.lock() {
            *slot = None;
        }
    }
}

struct SessionAdmission {
    sessions: Arc<AtomicUsize>,
    committed: bool,
}

impl SessionAdmission {
    fn new(sessions: Arc<AtomicUsize>) -> Self {
        Self {
            sessions,
            committed: false,
        }
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for SessionAdmission {
    fn drop(&mut self) {
        if !self.committed {
            self.sessions.store(0, Ordering::Release);
        }
    }
}

/// Translate raw [`PairingEvent`]s into wire [`PairingUpdate`]s: cache each
/// discovered device by address (so `pair_device` can look it up), and resume
/// the agent's capture on every terminal event.
async fn translate(
    mut raw: mpsc::UnboundedReceiver<PairingEvent>,
    upd_tx: mpsc::UnboundedSender<PairingUpdate>,
    devices: DeviceCache,
    sessions: Arc<AtomicUsize>,
    receiver_lease: Arc<StdMutex<Option<PairingReceiverLease>>>,
) {
    while let Some(event) = raw.recv().await {
        let update = match event {
            PairingEvent::Searching => PairingUpdate::Searching,
            PairingEvent::DeviceFound(device) => {
                let found = FoundDevice {
                    address: device.address,
                    name: device.name.clone(),
                };
                if let Ok(mut devices) = devices.lock() {
                    devices.insert(device.address, device);
                }
                PairingUpdate::DeviceFound(found)
            }
            PairingEvent::Passkey(method) => PairingUpdate::Passkey(method),
            PairingEvent::Paired { slot } => PairingUpdate::Paired { slot },
            PairingEvent::Failed(error) => PairingUpdate::Failed(error.into()),
        };
        if matches!(
            update,
            PairingUpdate::Paired { .. } | PairingUpdate::Failed(_)
        ) {
            // Lift the capture pause when the accepted single session ends.
            // Balanced: `start()` admits one active session, and that session
            // emits exactly one terminal event.
            if sessions.fetch_sub(1, Ordering::Relaxed) == 1
                && let Ok(mut lease) = receiver_lease.lock()
            {
                *lease = None;
            }
        }
        if upd_tx.send(update).is_err() {
            break; // the manager (and its receiver) is gone
        }
    }
    // The watcher channel closed — its thread exited, most likely because
    // run_pairing panicked and unwound the watcher thread, dropping evt_tx before
    // any terminal event. Don't leave the receiver lease held: release it so
    // gesture / DPI-cycle / thumbwheel remapping keeps working (only pairing
    // itself is then unavailable until the agent restarts).
    sessions.store(0, Ordering::Relaxed);
    if let Ok(mut lease) = receiver_lease.lock() {
        *lease = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;
    use std::sync::RwLock;
    use std::sync::atomic::AtomicBool;

    use openlogi_agent_core::DpiCycleState;
    use openlogi_agent_core::hook_runtime::HookMaps;
    use openlogi_agent_core::receiver_access::ReceiverAccess;

    fn shared_runtime() -> SharedRuntime {
        SharedRuntime {
            hook_maps: Arc::new(RwLock::new(HookMaps::default())),
            gesture_bindings: Arc::new(RwLock::new(BTreeMap::new())),
            dpi_cycle: Arc::new(RwLock::new(DpiCycleState::default())),
            thumbwheel_sensitivity: Arc::new(0.into()),
            invert_scroll: Arc::new(AtomicBool::new(false)),
            capture_channel: Arc::new(RwLock::new(None)),
            receiver_access: ReceiverAccess::default(),
        }
    }

    fn manager_with_ctrl(ctrl: mpsc::UnboundedSender<Control>) -> PairingManager {
        let (_, upd_rx) = mpsc::unbounded_channel();
        PairingManager {
            ctrl,
            updates: Mutex::new(upd_rx),
            devices: Arc::new(StdMutex::new(HashMap::new())),
            sessions: Arc::new(AtomicUsize::new(0)),
            receiver_lease: Arc::new(StdMutex::new(None)),
            shared: shared_runtime(),
        }
    }

    #[tokio::test]
    async fn start_rolls_back_pause_when_watcher_send_fails() {
        let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
        drop(ctrl_rx);
        let manager = manager_with_ctrl(ctrl_tx);

        let result = manager.start(ReceiverSelector::First).await;

        assert_eq!(result, Err(PairingCommandError::WatcherUnavailable));
        assert_eq!(manager.sessions.load(Ordering::Acquire), 0);
        assert!(!manager.shared.receiver_access.pairing_requested());
        assert!(
            manager
                .shared
                .receiver_access
                .try_acquire_for_capture()
                .is_some()
        );
    }

    #[tokio::test]
    async fn cancel_without_active_session_is_a_noop_success() {
        let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
        let manager = manager_with_ctrl(ctrl_tx);

        let result = manager.cancel();

        assert_eq!(result, Ok(()));
        assert!(ctrl_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn start_ignores_overlapping_session_without_clearing_or_sending() {
        let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
        let manager = manager_with_ctrl(ctrl_tx);
        manager.sessions.store(1, Ordering::Release);
        {
            let Ok(mut devices) = manager.devices.lock() else {
                panic!("test device cache lock should not be poisoned");
            };
            devices.insert(
                [1, 2, 3, 4, 5, 6],
                DiscoveredDevice {
                    address: [1, 2, 3, 4, 5, 6],
                    authentication: 0,
                    kind: openlogi_hid::pairing::BoltDeviceKind::Unknown,
                    name: "existing".to_string(),
                },
            );
        }

        let result = manager.start(ReceiverSelector::First).await;

        assert_eq!(result, Err(PairingCommandError::AlreadyActive));
        assert_eq!(manager.sessions.load(Ordering::Acquire), 1);
        let Ok(devices) = manager.devices.lock() else {
            panic!("test device cache lock should not be poisoned");
        };
        assert_eq!(devices.len(), 1);
        assert!(ctrl_rx.try_recv().is_err());
    }
}
