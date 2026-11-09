//! The receiver pairing session as one state machine.
//!
//! [`SessionOwner`] owns admission, the live session's resources and
//! discovery, and the result a finished session leaves for the GUI. The
//! published [`PairingPhase`] is [`SessionOwner::phase`], a projection of that
//! state rather than a second copy of it. Each stage is a type, a stage change
//! consumes the stage it leaves, and the fields are private to this module, so
//! nothing outside it can move the phase except through a named transition.
//!
//! ```text
//! Idle ──begin_admission──► Admitting ──activate──► Active ──terminal event──► Idle
//!  ▲                          │                       │
//!  └──── roll_back_admission ─┘                       └── abort / failed start ─► Idle
//! ```

use openlogi_agent_core::receiver_access::ExclusiveReceiverLease;
use openlogi_agent_core::watchers::pairing::{PairingSessionEvent, PairingSessionId};
use openlogi_hid::{DiscoveredDevice, PairingEvent, PasskeyMethod};
use openlogi_ipc::{FoundDevice, PairingCommandError, PairingFailure, PairingPhase, PairingUpdate};
use tokio::sync::watch;
use tracing::warn;

/// The one owner of pairing admission, live-session resources, discovery, and
/// the result a finished session leaves behind.
#[derive(Default)]
pub(super) struct SessionOwner {
    next_id: u64,
    state: SessionState,
}

enum SessionState {
    Idle(Idle),
    Admitting(Admitting),
    Active(ActiveSession),
}

impl Default for SessionState {
    fn default() -> Self {
        Self::Idle(Idle::default())
    }
}

/// No session. Keeps the last session's result until the GUI dismisses it or
/// the next session goes live, so a result cannot fall between two
/// observations.
#[derive(Default)]
struct Idle {
    shown: Option<Outcome>,
}

/// A start waiting for the receiver lease. The previous result stays shown
/// until the session goes live, and is shown again if the admission rolls back.
struct Admitting {
    id: PairingSessionId,
    shown: Option<Outcome>,
}

/// A live session. It holds the exclusive receiver lease, so HID++ capture
/// stays paused for exactly as long as this stage exists.
struct ActiveSession {
    id: PairingSessionId,
    /// Every device discovered so far, in discovery order, so the GUI can pair
    /// by address without round-tripping the non-serializable device. Kept
    /// once discovery closes so the user can select again.
    devices: Vec<DiscoveredDevice>,
    phase: ActivePhase,
    _receiver_lease: ExclusiveReceiverLease,
    // Dropped after the receiver lease. Cancellation subscribers wait for
    // closure, so acknowledgement cannot precede terminal resource cleanup.
    completion: watch::Sender<()>,
}

enum ActivePhase {
    /// Discovery (Bolt) or the pairing lock (Unifying) is open.
    Discovering,
    /// A selection was sent to the watcher; discovery is closed.
    Pairing,
    /// The device asked for a passkey; discovery is closed.
    Passkey(PasskeyMethod),
}

/// The result a finished session leaves for the GUI, tagged with the session
/// that produced it so a late dismissal cannot clear a newer session's result.
struct Outcome {
    session: PairingSessionId,
    /// The slot the device paired into, or why the session failed.
    result: Result<u8, PairingFailure>,
}

impl SessionOwner {
    /// What the GUI sees: the live session's phase, or the result the last
    /// session left. The only source of the published [`PairingPhase`].
    pub(super) fn phase(&self) -> Option<PairingPhase> {
        match &self.state {
            SessionState::Idle(Idle { shown })
            | SessionState::Admitting(Admitting { shown, .. }) => {
                shown.as_ref().map(Outcome::phase)
            }
            SessionState::Active(active) => Some(active.phase()),
        }
    }

    /// Reserve the next session identity while no session is admitted or live.
    pub(super) fn begin_admission(&mut self) -> Result<PairingSessionId, PairingCommandError> {
        let id = PairingSessionId::new(self.next_id);
        self.step(|state| match state {
            SessionState::Idle(idle) => (SessionState::Admitting(idle.admit(id)), Ok(())),
            busy => (busy, Err(PairingCommandError::AlreadyActive)),
        })?;
        self.next_id = self.next_id.wrapping_add(1);
        Ok(id)
    }

    /// Withdraw admission `id`. A stale identity leaves a newer admission or
    /// session alone.
    pub(super) fn roll_back_admission(&mut self, id: PairingSessionId) {
        self.step(|state| match state {
            SessionState::Admitting(admitting) if admitting.id == id => {
                (SessionState::Idle(admitting.roll_back()), ())
            }
            other => (other, ()),
        });
    }

    /// Make admission `id` the live session, holding `receiver_lease`, then
    /// let `start` hand the watcher its start command. The session is live
    /// before the watcher can produce an event for it; if `start` fails the
    /// watcher never saw it, so it closes without a result.
    pub(super) fn activate(
        &mut self,
        id: PairingSessionId,
        receiver_lease: ExclusiveReceiverLease,
        start: impl FnOnce(PairingSessionId) -> Result<(), PairingCommandError>,
    ) -> Result<(), PairingCommandError> {
        self.step(|state| match state {
            SessionState::Admitting(admitting) if admitting.id == id => {
                let active = admitting.activate(receiver_lease);
                match start(id) {
                    Ok(()) => (SessionState::Active(active), Ok(())),
                    Err(error) => (SessionState::Idle(Idle::default()), Err(error)),
                }
            }
            other => {
                warn!(session = ?id, "pairing admission changed before activation");
                (other, Err(PairingCommandError::WatcherUnavailable))
            }
        })
    }

    /// Select the discovered device at `address`: `send` hands it to the
    /// watcher, and only once it has does discovery close.
    pub(super) fn select(
        &mut self,
        address: [u8; 6],
        send: impl FnOnce(PairingSessionId, DiscoveredDevice) -> Result<(), PairingCommandError>,
    ) -> Result<(), PairingCommandError> {
        let SessionState::Active(active) = &mut self.state else {
            return Err(PairingCommandError::NoActiveSession);
        };
        active.select(address, send)
    }

    /// Apply one watcher event and return the update to stream to clients.
    /// Events from any session but the live one are dropped, duplicate
    /// terminals included, so they cannot touch a replacement session.
    pub(super) fn apply(&mut self, event: PairingSessionEvent) -> Option<PairingUpdate> {
        self.step(|state| match state {
            SessionState::Active(active) if active.id == event.session => active.apply(event.event),
            other => (other, None),
        })
    }

    /// The live session's identity and a receiver that closes once its
    /// resources are released.
    pub(super) fn subscribe_completion(&self) -> Option<(PairingSessionId, watch::Receiver<()>)> {
        match &self.state {
            SessionState::Active(active) => Some((active.id, active.completion.subscribe())),
            SessionState::Idle(_) | SessionState::Admitting(_) => None,
        }
    }

    /// Clear the result a finished session left. A live session is untouched.
    pub(super) fn dismiss(&mut self) {
        if let Some(shown) = self.shown_mut() {
            *shown = None;
        }
    }

    /// Clear the result `session` left, but never a newer session's result or
    /// a newer session itself.
    pub(super) fn dismiss_result_of(&mut self, session: PairingSessionId) {
        if let Some(shown) = self.shown_mut()
            && shown
                .as_ref()
                .is_some_and(|outcome| outcome.session == session)
        {
            *shown = None;
        }
    }

    /// The watcher is gone: drop any admission or live session, and with it
    /// the receiver lease, so capture resumes. A finished session's result
    /// stays.
    pub(super) fn abort(&mut self) {
        self.step(|state| match state {
            SessionState::Admitting(_) | SessionState::Active(_) => {
                (SessionState::Idle(Idle::default()), ())
            }
            idle @ SessionState::Idle(_) => (idle, ()),
        });
    }

    fn shown_mut(&mut self) -> Option<&mut Option<Outcome>> {
        match &mut self.state {
            SessionState::Idle(Idle { shown })
            | SessionState::Admitting(Admitting { shown, .. }) => Some(shown),
            SessionState::Active(_) => None,
        }
    }

    /// Replace the state with what `transition` makes of it.
    fn step<T>(&mut self, transition: impl FnOnce(SessionState) -> (SessionState, T)) -> T {
        let (state, output) = transition(std::mem::take(&mut self.state));
        self.state = state;
        output
    }
}

impl Idle {
    fn admit(self, id: PairingSessionId) -> Admitting {
        Admitting {
            id,
            shown: self.shown,
        }
    }
}

impl Admitting {
    fn roll_back(self) -> Idle {
        Idle { shown: self.shown }
    }

    /// Go live. The previous session's result gives way to this session.
    fn activate(self, receiver_lease: ExclusiveReceiverLease) -> ActiveSession {
        ActiveSession {
            id: self.id,
            devices: Vec::new(),
            phase: ActivePhase::Discovering,
            _receiver_lease: receiver_lease,
            completion: watch::channel(()).0,
        }
    }
}

impl ActiveSession {
    fn phase(&self) -> PairingPhase {
        match &self.phase {
            ActivePhase::Discovering if self.devices.is_empty() => PairingPhase::Searching,
            ActivePhase::Discovering => {
                PairingPhase::Found(self.devices.iter().map(found_device).collect())
            }
            ActivePhase::Pairing => PairingPhase::Pairing,
            ActivePhase::Passkey(method) => PairingPhase::Passkey(method.clone()),
        }
    }

    fn select(
        &mut self,
        address: [u8; 6],
        send: impl FnOnce(PairingSessionId, DiscoveredDevice) -> Result<(), PairingCommandError>,
    ) -> Result<(), PairingCommandError> {
        let device = self
            .devices
            .iter()
            .find(|device| device.address == address)
            .cloned()
            .ok_or(PairingCommandError::UnknownDevice)?;
        send(self.id, device)?;
        // Events are accepted under the same lock as this selection, so any
        // discovery the watcher queued before it now meets a closed phase.
        self.phase = ActivePhase::Pairing;
        Ok(())
    }

    /// Advance on one of this session's events. A terminal event consumes the
    /// session, so it ends exactly once.
    fn apply(mut self, event: PairingEvent) -> (SessionState, Option<PairingUpdate>) {
        let update = match event {
            PairingEvent::Searching => self.discovering().then_some(PairingUpdate::Searching),
            PairingEvent::DeviceFound(device) => {
                self.discover(device).map(PairingUpdate::DeviceFound)
            }
            PairingEvent::Passkey(method) => {
                self.phase = ActivePhase::Passkey(method.clone());
                Some(PairingUpdate::Passkey(method))
            }
            PairingEvent::Paired { slot } => {
                let idle = self.finish(Ok(slot));
                return (
                    SessionState::Idle(idle),
                    Some(PairingUpdate::Paired { slot }),
                );
            }
            PairingEvent::Failed(error) => {
                let failure = PairingFailure::from(error);
                let idle = self.finish(Err(failure.clone()));
                return (
                    SessionState::Idle(idle),
                    Some(PairingUpdate::Failed(failure)),
                );
            }
        };
        (SessionState::Active(self), update)
    }

    fn discovering(&self) -> bool {
        matches!(self.phase, ActivePhase::Discovering)
    }

    /// Record a discovered device while discovery is open. A device seen again
    /// keeps its place and takes the newer data.
    fn discover(&mut self, device: DiscoveredDevice) -> Option<FoundDevice> {
        if !self.discovering() {
            return None;
        }
        let found = found_device(&device);
        match self
            .devices
            .iter_mut()
            .find(|seen| seen.address == device.address)
        {
            Some(seen) => *seen = device,
            None => self.devices.push(device),
        }
        Some(found)
    }

    /// End with the session's result, releasing the receiver lease and then
    /// closing `completion`. A cancelled session leaves nothing to show: the
    /// user asked it to stop, so the session simply stops existing.
    fn finish(self, result: Result<u8, PairingFailure>) -> Idle {
        let shown = match result {
            Err(PairingFailure::Cancelled) => None,
            result => Some(Outcome {
                session: self.id,
                result,
            }),
        };
        Idle { shown }
    }
}

impl Outcome {
    fn phase(&self) -> PairingPhase {
        match &self.result {
            Ok(slot) => PairingPhase::Paired { slot: *slot },
            Err(failure) => PairingPhase::Failed(failure.clone()),
        }
    }
}

fn found_device(device: &DiscoveredDevice) -> FoundDevice {
    FoundDevice {
        address: device.address,
        name: device.name.clone(),
    }
}

#[cfg(test)]
mod tests;
