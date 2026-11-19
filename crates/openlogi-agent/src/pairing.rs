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
//! [`SharedHandles::receiver_access`], so `run_pairing` can own the receiver's
//! HID node. Dropping that lease lets HID++ capture resume when the session ends
//! (every end — including cancel — emits a terminal event).

use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use openlogi_agent_core::observable::ObservableState;
use openlogi_agent_core::orchestrator::SharedHandles;
use openlogi_agent_core::receiver_access::{ExclusiveAccessReason, ExclusiveReceiverLease};
use openlogi_agent_core::watchers::pairing::{
    self, PairingControl, PairingSessionEvent, PairingSessionId,
};
use openlogi_hid::{DeviceRoute, ReceiverSelector};
use openlogi_ipc::{PairingCommandError, PairingFailure, PairingUpdate};
use tokio::sync::{Mutex, mpsc};
use tracing::{info, warn};

mod session;

use session::SessionOwner;

/// How long the agent holds a `next_pairing` long-poll before returning `None`.
/// Comfortably under the client's request deadline so the agent answers first.
const PAIRING_HOLD: Duration = Duration::from_secs(20);

/// How long pairing waits for HID++ capture to release the receiver lease.
const RECEIVER_LEASE_TIMEOUT: Duration = Duration::from_secs(5);

/// The session owner and the cell its phase is published to, behind one lock.
///
/// [`Self::transition`] is the only way to the owner, and it publishes
/// [`SessionOwner::phase`] before it releases the lock. A publication is
/// therefore never older than the state it projects: whatever order the
/// command handlers and the event translator take the lock in, the last
/// publication describes the last transition.
struct PublishedSession {
    owner: StdMutex<SessionOwner>,
    /// Where the session's progress is published for the GUI to observe. The
    /// update stream is the same information as a stream; this is the form
    /// that survives a missed poll or a reconnect.
    observable: Arc<ObservableState>,
}

impl PublishedSession {
    fn new(observable: Arc<ObservableState>) -> Self {
        Self {
            owner: StdMutex::default(),
            observable,
        }
    }

    /// Run one transition on the owner, then publish its projection.
    fn transition<T>(&self, transition: impl FnOnce(&mut SessionOwner) -> T) -> T {
        let mut owner = self.owner.lock().unwrap_or_else(|poisoned| {
            warn!("pairing session owner lock poisoned; recovering session state");
            poisoned.into_inner()
        });
        let output = transition(&mut owner);
        self.observable.set_pairing(owner.phase());
        output
    }
}

/// Owns the pairing watcher and translates its event stream for the IPC layer.
pub struct PairingManager {
    ctrl: mpsc::UnboundedSender<PairingControl>,
    updates: Mutex<mpsc::UnboundedReceiver<PairingUpdate>>,
    session: Arc<PublishedSession>,
    shared: SharedHandles,
}

impl PairingManager {
    /// Spawn the pairing watcher and its event translator. One per agent; must
    /// be called inside the tokio runtime (it spawns the translator task).
    #[must_use]
    pub fn new(shared: SharedHandles, observable: Arc<ObservableState>) -> Self {
        let (ctrl, raw_events) = pairing::spawn_with_hardware(shared.hardware());
        let (upd_tx, upd_rx) = mpsc::unbounded_channel();
        let session = Arc::new(PublishedSession::new(observable));
        tokio::spawn(translate(raw_events, upd_tx.clone(), Arc::clone(&session)));
        Self {
            ctrl,
            updates: Mutex::new(upd_rx),
            session,
            shared,
        }
    }

    /// Begin a session: forget the previous discovery, pause capture, then start.
    pub async fn start(&self, selector: ReceiverSelector) -> Result<(), PairingCommandError> {
        if !self.shared.device_io.allows_io() {
            return Err(PairingCommandError::ReceiverBusy);
        }
        let admission = match SessionAdmission::new(Arc::clone(&self.session)) {
            Ok(admission) => admission,
            Err(error) => {
                debug_assert_eq!(error, PairingCommandError::AlreadyActive);
                warn!("pairing start requested while a session is already active");
                return Err(error);
            }
        };
        let Ok(receiver_lease) = tokio::time::timeout(
            RECEIVER_LEASE_TIMEOUT,
            self.shared
                .receiver_access
                .acquire_exclusive(ExclusiveAccessReason::Pairing),
        )
        .await
        else {
            warn!("timed out waiting for receiver capture to stop; pairing not started");
            return Err(PairingCommandError::ReceiverBusy);
        };
        admission.accept(receiver_lease, selector, &self.ctrl)
    }

    /// Remove the device `route` names from its receiver, then have inventory
    /// rescan so the freed slot leaves the snapshot. Takes the receiver the
    /// way a pairing session does, so it waits out capture and is refused
    /// while a session holds the receiver.
    pub async fn unpair(&self, route: &DeviceRoute) -> Result<(), PairingFailure> {
        if !self.shared.device_io.allows_io() {
            return Err(PairingFailure::ReceiverBusy);
        }
        let Ok(receiver_lease) = tokio::time::timeout(
            RECEIVER_LEASE_TIMEOUT,
            self.shared
                .receiver_access
                .acquire_exclusive(ExclusiveAccessReason::Pairing),
        )
        .await
        else {
            warn!(%route, "timed out waiting for the receiver; device not unpaired");
            return Err(PairingFailure::ReceiverBusy);
        };
        let result = self.shared.hardware().unpair(route).await;
        drop(receiver_lease);
        match result {
            Ok(()) => {
                info!(%route, "device unpaired");
                self.shared.request_receiver_rescan();
                Ok(())
            }
            Err(error) => {
                warn!(%route, %error, "unpair failed");
                Err(error.into())
            }
        }
    }

    /// Pair with a previously discovered device by address.
    pub fn pair(&self, address: [u8; 6]) -> Result<(), PairingCommandError> {
        let result = self.session.transition(|owner| {
            owner.select(address, |session, device| {
                self.ctrl
                    .send(PairingControl::Pair { session, device })
                    .map_err(|_| PairingCommandError::WatcherUnavailable)
            })
        });
        match result {
            Err(PairingCommandError::NoActiveSession) => {
                warn!(?address, "pair requested without an active session");
            }
            Err(PairingCommandError::UnknownDevice) => {
                warn!(?address, "pair requested for an unknown device");
            }
            _ => {}
        }
        result
    }

    /// Cancel and wait for terminal cleanup to release the receiver lease.
    /// The watcher restores receiver state before the translator ends the
    /// session; only then may the caller admit another pairing attempt.
    pub async fn cancel(&self) -> Result<(), PairingCommandError> {
        let pending = self.session.transition(|owner| {
            let Some((session, completion)) = owner.subscribe_completion() else {
                // Nothing running, so this is the GUI dismissing a *finished*
                // session's result. Clearing it is the whole job.
                owner.dismiss();
                return Ok(None);
            };
            self.ctrl
                .send(PairingControl::Cancel { session })
                .map_err(|_| PairingCommandError::WatcherUnavailable)?;
            Ok(Some((session, completion)))
        })?;
        if let Some((session, mut completion)) = pending {
            // This sender never publishes values: closing it is the terminal
            // cleanup signal, including watcher failure and process shutdown.
            let _ = completion.changed().await;
            // Pairing success/timeout can beat the cancellation command.
            // Dismiss that result, but only this session's.
            self.session
                .transition(|owner| owner.dismiss_result_of(session));
        }
        Ok(())
    }

    /// Long-poll the next pairing step; `None` when the hold window elapses.
    pub async fn next_update(&self) -> Option<PairingUpdate> {
        let mut rx = self.updates.lock().await;
        tokio::time::timeout(PAIRING_HOLD, rx.recv())
            .await
            .ok()
            .flatten()
    }
}

struct SessionAdmission {
    session: Arc<PublishedSession>,
    id: PairingSessionId,
    finished: bool,
}

impl SessionAdmission {
    fn new(session: Arc<PublishedSession>) -> Result<Self, PairingCommandError> {
        let id = session.transition(SessionOwner::begin_admission)?;
        Ok(Self {
            session,
            id,
            finished: false,
        })
    }

    fn accept(
        mut self,
        receiver_lease: ExclusiveReceiverLease,
        selector: ReceiverSelector,
        ctrl: &mpsc::UnboundedSender<PairingControl>,
    ) -> Result<(), PairingCommandError> {
        let start = |session| {
            ctrl.send(PairingControl::Start { session, selector })
                .map_err(|error| {
                    warn!(%error, "could not start pairing session; pairing watcher is unavailable");
                    PairingCommandError::WatcherUnavailable
                })
        };
        let result = self
            .session
            .transition(|owner| owner.activate(self.id, receiver_lease, start));
        self.finished = true;
        result
    }
}

impl Drop for SessionAdmission {
    fn drop(&mut self) {
        if !self.finished {
            self.session
                .transition(|owner| owner.roll_back_admission(self.id));
        }
    }
}

/// Apply one session-tagged watcher event and return its wire update.
fn apply_session_event(
    event: PairingSessionEvent,
    session: &PublishedSession,
) -> Option<PairingUpdate> {
    session.transition(|owner| owner.apply(event))
}

/// Translate raw [`PairingSessionEvent`]s into wire [`PairingUpdate`]s and release the
/// active session's receiver lease on its one terminal event.
async fn translate(
    mut raw: mpsc::UnboundedReceiver<PairingSessionEvent>,
    upd_tx: mpsc::UnboundedSender<PairingUpdate>,
    session: Arc<PublishedSession>,
) {
    while let Some(event) = raw.recv().await {
        let Some(update) = apply_session_event(event, &session) else {
            continue;
        };
        if upd_tx.send(update).is_err() {
            break;
        }
    }
    // The watcher channel closed — its thread exited, most likely because its
    // session panicked. Drop any admission/session resource so capture resumes.
    session.transition(SessionOwner::abort);
}

#[cfg(test)]
mod tests;
