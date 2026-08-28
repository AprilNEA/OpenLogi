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
use std::time::Duration;

use openlogi_agent_core::observable::ObservableState;
use openlogi_agent_core::orchestrator::SharedRuntime;
use openlogi_agent_core::receiver_access::{ExclusiveAccessReason, ExclusiveReceiverLease};
use openlogi_agent_core::watchers::pairing::{self, Control, SessionEvent, SessionId};
use openlogi_hid::{DiscoveredDevice, PairingEvent, ReceiverSelector};
use openlogi_ipc::{FoundDevice, PairingCommandError, PairingFailure, PairingPhase, PairingUpdate};
use tokio::sync::{Mutex, mpsc};
use tracing::warn;

/// How long the agent holds a `next_pairing` long-poll before returning `None`.
/// Comfortably under the client's request deadline so the agent answers first.
const HOLD: Duration = Duration::from_secs(20);

/// How long pairing waits for HID++ capture to release the receiver lease.
const RECEIVER_LEASE_TIMEOUT: Duration = Duration::from_secs(5);

/// Address-keyed cache of the full discovered devices, so the GUI can pair by
/// address without round-tripping the non-serializable `DiscoveredDevice`.
type DeviceCache = HashMap<[u8; 6], DiscoveredDevice>;
type SharedSessionOwner = Arc<StdMutex<SessionOwner>>;

/// The one owner of pairing admission, live-session resources, and discovery.
struct SessionOwner {
    next_id: u64,
    state: SessionState,
}

enum SessionState {
    Idle,
    Admitting(SessionId),
    Active(ActiveSession),
}

struct ActiveSession {
    id: SessionId,
    devices: DeviceCache,
    _receiver_lease: ExclusiveReceiverLease,
}

impl Default for SessionOwner {
    fn default() -> Self {
        Self {
            next_id: 0,
            state: SessionState::Idle,
        }
    }
}

impl SessionOwner {
    fn begin_admission(&mut self) -> Result<SessionId, PairingCommandError> {
        if !matches!(self.state, SessionState::Idle) {
            return Err(PairingCommandError::AlreadyActive);
        }
        let id = SessionId::new(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        self.state = SessionState::Admitting(id);
        Ok(id)
    }

    fn activate(&mut self, id: SessionId, receiver_lease: ExclusiveReceiverLease) -> bool {
        if !matches!(self.state, SessionState::Admitting(admitted) if admitted == id) {
            return false;
        }
        self.state = SessionState::Active(ActiveSession {
            id,
            devices: HashMap::new(),
            _receiver_lease: receiver_lease,
        });
        true
    }

    fn roll_back_admission(&mut self, id: SessionId) {
        if matches!(self.state, SessionState::Admitting(admitted) if admitted == id) {
            self.state = SessionState::Idle;
        }
    }

    fn active(&self) -> Option<&ActiveSession> {
        match &self.state {
            SessionState::Active(session) => Some(session),
            SessionState::Idle | SessionState::Admitting(_) => None,
        }
    }

    fn active_mut(&mut self, id: SessionId) -> Option<&mut ActiveSession> {
        match &mut self.state {
            SessionState::Active(session) if session.id == id => Some(session),
            SessionState::Idle | SessionState::Admitting(_) | SessionState::Active(_) => None,
        }
    }

    fn end(&mut self, id: SessionId) -> bool {
        if matches!(&self.state, SessionState::Active(session) if session.id == id) {
            self.state = SessionState::Idle;
            true
        } else {
            false
        }
    }

    fn abort(&mut self) -> bool {
        if matches!(self.state, SessionState::Idle) {
            return false;
        }
        self.state = SessionState::Idle;
        true
    }
}

/// Owns the pairing watcher and translates its event stream for the IPC layer.
pub struct PairingManager {
    ctrl: mpsc::UnboundedSender<Control>,
    updates: Mutex<mpsc::UnboundedReceiver<PairingUpdate>>,
    session: SharedSessionOwner,
    shared: SharedRuntime,
    /// Where the session's progress is published for the GUI to observe. The
    /// event channel above is the same information as a stream; this is the
    /// form that survives a missed poll or a reconnect.
    observable: Arc<ObservableState>,
}

impl PairingManager {
    /// Spawn the pairing watcher and its event translator. One per agent; must
    /// be called inside the tokio runtime (it spawns the translator task).
    #[must_use]
    pub fn new(shared: SharedRuntime, observable: Arc<ObservableState>) -> Self {
        let (ctrl, raw_events) = pairing::spawn();
        let (upd_tx, upd_rx) = mpsc::unbounded_channel();
        let session = Arc::new(StdMutex::new(SessionOwner::default()));
        tokio::spawn(translate(
            raw_events,
            upd_tx.clone(),
            Arc::clone(&session),
            Arc::clone(&observable),
        ));
        Self {
            ctrl,
            updates: Mutex::new(upd_rx),
            session,
            shared,
            observable,
        }
    }

    /// Begin a session: forget the previous discovery, pause capture, then start.
    pub async fn start(&self, selector: ReceiverSelector) -> Result<(), PairingCommandError> {
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
        admission.accept(receiver_lease, selector, &self.ctrl, &self.observable)
    }

    /// Pair with a previously discovered device by address.
    pub fn pair(&self, address: [u8; 6]) -> Result<(), PairingCommandError> {
        with_session_owner(&self.session, |owner| {
            let Some(session) = owner.active() else {
                warn!(?address, "pair requested without an active session");
                return Err(PairingCommandError::NoActiveSession);
            };
            let Some(device) = session.devices.get(&address).cloned() else {
                warn!(?address, "pair requested for an unknown device");
                return Err(PairingCommandError::UnknownDevice);
            };
            self.ctrl
                .send(Control::Pair {
                    session: session.id,
                    device,
                })
                .map_err(|_| PairingCommandError::WatcherUnavailable)?;
            self.observable.set_pairing(Some(PairingPhase::Pairing));
            Ok(())
        })
    }

    /// Cancel the in-progress session. The resulting `Failed(Cancelled)` event
    /// releases the receiver lease via the translator — don't release it here, or
    /// capture could re-acquire the receiver while `run_pairing` still holds it.
    pub fn cancel(&self) -> Result<(), PairingCommandError> {
        with_session_owner(&self.session, |owner| {
            let Some(session) = owner.active() else {
                // Nothing running, so this is the GUI dismissing a *finished*
                // session's result. Clearing the phase is the whole job.
                self.observable.set_pairing(None);
                return Ok(());
            };
            self.ctrl
                .send(Control::Cancel {
                    session: session.id,
                })
                .map_err(|_| PairingCommandError::WatcherUnavailable)
        })
    }

    /// Long-poll the next pairing step; `None` when the hold window elapses.
    pub async fn next_update(&self) -> Option<PairingUpdate> {
        let mut rx = self.updates.lock().await;
        tokio::time::timeout(HOLD, rx.recv()).await.ok().flatten()
    }
}

fn with_session_owner<T>(
    session: &SharedSessionOwner,
    f: impl FnOnce(&mut SessionOwner) -> T,
) -> T {
    match session.lock() {
        Ok(mut owner) => f(&mut owner),
        Err(poisoned) => {
            warn!("pairing session owner lock poisoned; recovering session state");
            let mut owner = poisoned.into_inner();
            f(&mut owner)
        }
    }
}

struct SessionAdmission {
    owner: SharedSessionOwner,
    id: SessionId,
    finished: bool,
}

impl SessionAdmission {
    fn new(owner: SharedSessionOwner) -> Result<Self, PairingCommandError> {
        let id = with_session_owner(&owner, SessionOwner::begin_admission)?;
        Ok(Self {
            owner,
            id,
            finished: false,
        })
    }

    fn accept(
        mut self,
        receiver_lease: ExclusiveReceiverLease,
        selector: ReceiverSelector,
        ctrl: &mpsc::UnboundedSender<Control>,
        observable: &ObservableState,
    ) -> Result<(), PairingCommandError> {
        let result = with_session_owner(&self.owner, |owner| {
            if !owner.activate(self.id, receiver_lease) {
                warn!(session = ?self.id, "pairing admission changed before activation");
                return Err(PairingCommandError::WatcherUnavailable);
            }
            // Publish before the watcher can emit a terminal result; otherwise
            // this call could overwrite that result with `Searching`.
            observable.set_pairing(Some(PairingPhase::Searching));
            if let Err(error) = ctrl.send(Control::Start {
                session: self.id,
                selector,
            }) {
                owner.end(self.id);
                observable.set_pairing(None);
                warn!(error = %error, "could not start pairing session; pairing watcher is unavailable");
                return Err(PairingCommandError::WatcherUnavailable);
            }
            Ok(())
        });
        self.finished = true;
        result
    }
}

impl Drop for SessionAdmission {
    fn drop(&mut self) {
        if !self.finished {
            with_session_owner(&self.owner, |owner| {
                owner.roll_back_admission(self.id);
            });
        }
    }
}

/// Translate one session-tagged event into the wire update and observable
/// phase. Events from an ended session are ignored, including duplicate
/// terminals, so they cannot clean up a replacement session.
fn apply_session_event(
    event: SessionEvent,
    session: &SharedSessionOwner,
    observable: &ObservableState,
) -> Option<PairingUpdate> {
    with_session_owner(session, |owner| {
        if owner.active().map(|session| session.id) != Some(event.session) {
            return None;
        }
        let update = match event.event {
            PairingEvent::Searching => PairingUpdate::Searching,
            PairingEvent::DeviceFound(device) => {
                let found = FoundDevice {
                    address: device.address,
                    name: device.name.clone(),
                };
                owner
                    .active_mut(event.session)?
                    .devices
                    .insert(device.address, device);
                PairingUpdate::DeviceFound(found)
            }
            PairingEvent::Passkey(method) => PairingUpdate::Passkey(method),
            PairingEvent::Paired { slot } => PairingUpdate::Paired { slot },
            PairingEvent::Failed(error) => PairingUpdate::Failed(error.into()),
        };
        match &update {
            PairingUpdate::Searching => observable.set_pairing(Some(PairingPhase::Searching)),
            PairingUpdate::DeviceFound(found) => observable.found_pairing_device(found.clone()),
            PairingUpdate::Passkey(method) => {
                observable.set_pairing(Some(PairingPhase::Passkey(method.clone())));
            }
            PairingUpdate::Paired { slot } => {
                observable.set_pairing(Some(PairingPhase::Paired { slot: *slot }));
            }
            // A cancelled session leaves no result to show: the user asked it
            // to stop, so the session simply stops existing.
            PairingUpdate::Failed(PairingFailure::Cancelled) => observable.set_pairing(None),
            PairingUpdate::Failed(failure) => {
                observable.set_pairing(Some(PairingPhase::Failed(failure.clone())));
            }
        }
        if matches!(
            update,
            PairingUpdate::Paired { .. } | PairingUpdate::Failed(_)
        ) {
            let ended = owner.end(event.session);
            debug_assert!(ended, "matching active pairing session must end once");
        }
        Some(update)
    })
}

/// Translate raw [`SessionEvent`]s into wire [`PairingUpdate`]s and release the
/// active session's receiver lease on its one terminal event.
async fn translate(
    mut raw: mpsc::UnboundedReceiver<SessionEvent>,
    upd_tx: mpsc::UnboundedSender<PairingUpdate>,
    session: SharedSessionOwner,
    observable: Arc<ObservableState>,
) {
    while let Some(event) = raw.recv().await {
        let Some(update) = apply_session_event(event, &session, &observable) else {
            continue;
        };
        if upd_tx.send(update).is_err() {
            break;
        }
    }
    // The watcher channel closed — its thread exited, most likely because its
    // session panicked. Drop any admission/session resource so capture resumes.
    if with_session_owner(&session, SessionOwner::abort) {
        observable.set_pairing(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::RwLock;

    use openlogi_agent_core::DpiCycles;
    use openlogi_agent_core::receiver_access::ReceiverAccess;
    use openlogi_agent_core::runtime::hook::HookMaps;
    use openlogi_agent_core::runtime::scroll::ScrollPreferences;
    use openlogi_core::config::VerticalScrollSensitivity;
    use openlogi_hid::PairingError;

    fn shared_runtime() -> SharedRuntime {
        let (_, capture_plans) = tokio::sync::watch::channel(Arc::new(Vec::new()));
        let (_, keyboard_spec) = tokio::sync::watch::channel(None);
        let (_, host_switch_links) = tokio::sync::watch::channel(Arc::new(Vec::new()));
        SharedRuntime {
            hook_maps: Arc::new(RwLock::new(HookMaps::default())),
            keyboard_bindings: Arc::new(RwLock::new(std::collections::BTreeMap::new())),
            scroll_preferences: Arc::new(ScrollPreferences::new(
                false,
                VerticalScrollSensitivity::DEFAULT,
            )),
            dpi_cycle: Arc::new(RwLock::new(DpiCycles::default())),
            capture_plans,
            capture_channel: Arc::new(RwLock::new(None)),
            channel_registry: openlogi_hid::ChannelRegistry::default(),
            channel_pool: openlogi_hid::host::channel_pool(),
            keyboard_spec,
            keyboard_channel: Arc::new(RwLock::new(None)),
            capture_rearm_generation: Arc::new(0.into()),
            receiver_access: ReceiverAccess::default(),
            host_switch_links,
            lighting: openlogi_agent_core::lighting::LightingHost::default(),
        }
    }

    fn manager_with_ctrl(ctrl: mpsc::UnboundedSender<Control>) -> PairingManager {
        let (_, upd_rx) = mpsc::unbounded_channel();
        PairingManager {
            ctrl,
            updates: Mutex::new(upd_rx),
            session: Arc::new(StdMutex::new(SessionOwner::default())),
            shared: shared_runtime(),
            observable: Arc::new(ObservableState::new("test".to_string())),
        }
    }

    async fn start_session(
        manager: &PairingManager,
        ctrl_rx: &mut mpsc::UnboundedReceiver<Control>,
    ) -> SessionId {
        manager
            .start(ReceiverSelector::First)
            .await
            .expect("test session should start");
        match ctrl_rx.recv().await.expect("start control") {
            Control::Start { session, .. } => session,
            control => panic!("expected start control, got {control:?}"),
        }
    }

    fn discovered_device() -> DiscoveredDevice {
        DiscoveredDevice {
            address: [1, 2, 3, 4, 5, 6],
            authentication: 0,
            kind: openlogi_hid::pairing::BoltDeviceKind::Unknown,
            name: "existing".to_string(),
        }
    }

    fn is_idle(manager: &PairingManager) -> bool {
        with_session_owner(&manager.session, |owner| {
            matches!(owner.state, SessionState::Idle)
        })
    }

    #[tokio::test]
    async fn start_rolls_back_pause_when_watcher_send_fails() {
        let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
        drop(ctrl_rx);
        let manager = manager_with_ctrl(ctrl_tx);

        let result = manager.start(ReceiverSelector::First).await;

        assert_eq!(result, Err(PairingCommandError::WatcherUnavailable));
        assert!(is_idle(&manager));
        assert_eq!(manager.observable.snapshot().pairing, None);
        assert!(!manager.shared.receiver_access.exclusive_requested());
        assert!(
            manager
                .shared
                .receiver_access
                .try_acquire_for_session()
                .is_some()
        );
    }

    #[test]
    fn admission_is_owned_and_rolled_back_by_session_identity() {
        let sessions = Arc::new(StdMutex::new(SessionOwner::default()));
        let first = SessionAdmission::new(Arc::clone(&sessions))
            .expect("idle owner should admit a session");

        assert!(matches!(
            SessionAdmission::new(Arc::clone(&sessions)),
            Err(PairingCommandError::AlreadyActive)
        ));
        let first_id = first.id;
        drop(first);

        let second = SessionAdmission::new(Arc::clone(&sessions))
            .expect("dropping an admission should reopen the owner");
        assert_ne!(second.id, first_id);
        with_session_owner(&sessions, |owner| owner.roll_back_admission(first_id));
        assert!(with_session_owner(&sessions, |owner| {
            matches!(owner.state, SessionState::Admitting(id) if id == second.id)
        }));
    }

    #[tokio::test]
    async fn cancel_without_active_session_is_a_noop_success() {
        let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
        let manager = manager_with_ctrl(ctrl_tx);

        let result = manager.cancel();

        assert_eq!(result, Ok(()));
        let sent = ctrl_rx.try_recv();
        assert!(
            sent.is_err(),
            "cancel without an active session must not reach the watcher, got {sent:?}"
        );
    }

    #[test]
    fn pair_without_active_session_does_not_publish_pairing() {
        let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
        let manager = manager_with_ctrl(ctrl_tx);
        manager
            .observable
            .set_pairing(Some(PairingPhase::Paired { slot: 3 }));

        let result = manager.pair(discovered_device().address);

        assert_eq!(result, Err(PairingCommandError::NoActiveSession));
        assert_eq!(
            manager.observable.snapshot().pairing,
            Some(PairingPhase::Paired { slot: 3 })
        );
        assert!(
            ctrl_rx.try_recv().is_err(),
            "pair without a session must not reach the watcher"
        );
    }

    #[tokio::test]
    async fn start_ignores_overlapping_session_without_clearing_or_sending() {
        let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
        let manager = manager_with_ctrl(ctrl_tx);
        let session = start_session(&manager, &mut ctrl_rx).await;
        apply_session_event(
            SessionEvent {
                session,
                event: PairingEvent::DeviceFound(discovered_device()),
            },
            &manager.session,
            &manager.observable,
        );

        let result = manager.start(ReceiverSelector::First).await;

        assert_eq!(result, Err(PairingCommandError::AlreadyActive));
        assert_eq!(
            with_session_owner(&manager.session, |owner| owner
                .active()
                .map(|active| active.devices.len())),
            Some(1)
        );
        let sent = ctrl_rx.try_recv();
        assert!(
            sent.is_err(),
            "an overlapping start must not reach the watcher, got {sent:?}"
        );
    }

    #[tokio::test]
    async fn terminal_cleanup_is_exactly_once_and_releases_receiver_lease() {
        let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
        let manager = manager_with_ctrl(ctrl_tx);
        let first = start_session(&manager, &mut ctrl_rx).await;
        assert!(
            manager
                .shared
                .receiver_access
                .requested(ExclusiveAccessReason::Pairing)
        );

        let first_terminal = apply_session_event(
            SessionEvent {
                session: first,
                event: PairingEvent::Failed(PairingError::Cancelled),
            },
            &manager.session,
            &manager.observable,
        );

        assert!(matches!(
            first_terminal,
            Some(PairingUpdate::Failed(PairingFailure::Cancelled))
        ));
        assert!(is_idle(&manager));
        assert!(!manager.shared.receiver_access.exclusive_requested());
        assert!(
            manager
                .shared
                .receiver_access
                .try_acquire_for_session()
                .is_some()
        );

        let second = start_session(&manager, &mut ctrl_rx).await;
        assert_ne!(second, first);
        let duplicate = apply_session_event(
            SessionEvent {
                session: first,
                event: PairingEvent::Failed(PairingError::Cancelled),
            },
            &manager.session,
            &manager.observable,
        );

        assert!(duplicate.is_none());
        assert_eq!(
            with_session_owner(&manager.session, |owner| owner
                .active()
                .map(|session| session.id)),
            Some(second)
        );
        assert!(manager.shared.receiver_access.exclusive_requested());
        assert_eq!(
            manager.observable.snapshot().pairing,
            Some(PairingPhase::Searching)
        );

        let second_terminal = apply_session_event(
            SessionEvent {
                session: second,
                event: PairingEvent::Paired { slot: 4 },
            },
            &manager.session,
            &manager.observable,
        );
        assert!(matches!(
            second_terminal,
            Some(PairingUpdate::Paired { slot: 4 })
        ));
        assert!(is_idle(&manager));
        assert!(!manager.shared.receiver_access.exclusive_requested());
    }
}
