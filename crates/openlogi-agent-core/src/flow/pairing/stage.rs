//! Where one pairing attempt stands, apart from its I/O.

use std::time::Duration;

use openlogi_flow::pairing::PairingState;
use openlogi_ipc::{FlowPairingFailure, FlowPairingPeer, FlowPairingPhase};
use tokio::time::Instant;

use super::{CALL_TIMEOUT, LINGER, Link, Role, SETUP_TIMEOUT};

#[cfg(test)]
mod tests;

/// Where a pairing attempt stands, and what it holds there: the peer once
/// known, the link while connected, and one deadline whose meaning the
/// stage gives.
///
/// `C` is the connection, `Arc<FlowConnection>` in the agent. Only the
/// named transitions change the stage; one the stage does not allow
/// answers [`InvalidTransition`] and changes nothing.
pub(super) struct Stage<C> {
    step: Step<C>,
}

enum Step<C> {
    /// This side dials the computer the user picked: `peer` is the one
    /// found, `None` for an address the user typed.
    Dialing {
        peer: Option<FlowPairingPeer>,
        timeout: Instant,
    },
    /// Another computer connects to this one. The user sees nothing yet.
    Accepting { timeout: Instant },
    /// Connected; the setup timeout still bounds the way to the code.
    Connected { live: Live<C>, timeout: Instant },
    /// The responder set how long the code stays valid, which now bounds
    /// the attempt. Both sides show it: connecting until both nonces are
    /// verified, then the code, then waiting for the peer once this side
    /// confirmed.
    Code {
        live: Live<C>,
        timeout: Instant,
        /// This side's `PairConfirm` is out and unanswered.
        confirming: bool,
    },
    /// The attempt ended, and its ceremony with it. The connection stays
    /// open until the peer closes it or `linger` passes.
    Ending {
        connection: C,
        ending: Ending,
        linger: Instant,
    },
}

/// A connected attempt that has not ended.
#[derive(Clone)]
pub(super) struct Live<C> {
    role: Role,
    peer: FlowPairingPeer,
    pub(super) link: Link<C>,
}

impl<C> Live<C> {
    /// Which side of the ceremony this one plays.
    pub(super) const fn role(&self) -> Role {
        self.role
    }

    /// The other computer, as it introduced itself on the connection.
    pub(super) const fn peer(&self) -> &FlowPairingPeer {
        &self.peer
    }
}

/// Why an ended attempt still holds its connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Ending {
    /// Its last frames are on their way to the peer.
    Flushing,
    /// The peer is saved; this side's `PairConfirm`, which the peer needs to
    /// save this side, may still be on its way.
    Paired,
}

/// When an attempt's deadline passes, and what that means.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Deadline {
    /// The running attempt fails with `failure`.
    Timeout {
        at: Instant,
        failure: FlowPairingFailure,
    },
    /// The ended attempt closes its connection.
    Linger(Instant),
}

impl Deadline {
    pub(super) const fn at(&self) -> Instant {
        match self {
            Self::Timeout { at, .. } | Self::Linger(at) => *at,
        }
    }
}

/// A transition the attempt's stage does not allow.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct InvalidTransition;

impl<C> Stage<C> {
    /// Dialing the computer the user picked, from `now`.
    pub(super) fn dialing(peer: Option<FlowPairingPeer>, now: Instant) -> Self {
        Self {
            step: Step::Dialing {
                peer,
                timeout: now + SETUP_TIMEOUT,
            },
        }
    }

    /// Accepting a computer that connects, from `now`.
    pub(super) fn accepting(now: Instant) -> Self {
        Self {
            step: Step::Accepting {
                timeout: now + SETUP_TIMEOUT,
            },
        }
    }

    pub(super) fn deadline(&self) -> Deadline {
        match &self.step {
            // A dial that never connected found nobody to pair with.
            Step::Dialing { timeout, .. } => Deadline::Timeout {
                at: *timeout,
                failure: FlowPairingFailure::Unreachable,
            },
            Step::Accepting { timeout }
            | Step::Connected { timeout, .. }
            | Step::Code { timeout, .. } => Deadline::Timeout {
                at: *timeout,
                failure: FlowPairingFailure::TimedOut,
            },
            Step::Ending { linger, .. } => Deadline::Linger(*linger),
        }
    }

    /// Whether the user sees the attempt. One another computer started
    /// shows once its code's validity runs; an ended one no longer shows.
    pub(super) fn shown(&self) -> bool {
        match &self.step {
            Step::Dialing { .. } | Step::Code { .. } => true,
            Step::Connected { live, .. } => live.role == Role::Initiator,
            Step::Accepting { .. } | Step::Ending { .. } => false,
        }
    }

    /// What the user sees of the attempt, `None` while it is not shown.
    pub(super) fn phase(&self) -> Option<FlowPairingPhase> {
        if !self.shown() {
            return None;
        }
        let Some(live) = self.live() else {
            return Some(FlowPairingPhase::Connecting {
                peer: self.peer().cloned(),
            });
        };
        let peer = live.peer.clone();
        let phase = match (live.link.session.state(), live.link.session.sas_code()) {
            (
                PairingState::Prompted {
                    local_confirmed, ..
                },
                Some(code),
            ) => {
                let code = code.to_string();
                if local_confirmed {
                    FlowPairingPhase::WaitingForPeer { peer, code }
                } else {
                    FlowPairingPhase::ShowingCode { peer, code }
                }
            }
            _ => FlowPairingPhase::Connecting { peer: Some(peer) },
        };
        Some(phase)
    }

    /// The other computer, while the attempt runs and knows it.
    pub(super) fn peer(&self) -> Option<&FlowPairingPeer> {
        match &self.step {
            Step::Dialing { peer, .. } => peer.as_ref(),
            Step::Connected { live, .. } | Step::Code { live, .. } => Some(&live.peer),
            Step::Accepting { .. } | Step::Ending { .. } => None,
        }
    }

    /// The connected attempt, until it ends.
    pub(super) const fn live(&self) -> Option<&Live<C>> {
        match &self.step {
            Step::Connected { live, .. } | Step::Code { live, .. } => Some(live),
            _ => None,
        }
    }

    pub(super) const fn live_mut(&mut self) -> Option<&mut Live<C>> {
        match &mut self.step {
            Step::Connected { live, .. } | Step::Code { live, .. } => Some(live),
            _ => None,
        }
    }

    pub(super) const fn connection(&self) -> Option<&C> {
        match &self.step {
            Step::Connected { live, .. } | Step::Code { live, .. } => Some(&live.link.connection),
            Step::Ending { connection, .. } => Some(connection),
            Step::Dialing { .. } | Step::Accepting { .. } => None,
        }
    }

    pub(super) const fn ending(&self) -> Option<Ending> {
        match &self.step {
            Step::Ending { ending, .. } => Some(*ending),
            _ => None,
        }
    }

    /// The dial or the inbound handshake connected to `peer` over `link`;
    /// the setup timeout runs on. Answers the role the connection gives
    /// this side: the side that dialed starts the ceremony.
    pub(super) fn connect(
        &mut self,
        peer: FlowPairingPeer,
        link: Link<C>,
    ) -> Result<Role, InvalidTransition> {
        let (role, timeout) = self.ensure_connecting()?;
        self.step = Step::Connected {
            live: Live { role, peer, link },
            timeout,
        };
        Ok(role)
    }

    /// This side's `PairConfirm` went out.
    pub(super) fn confirm_sent(&mut self) -> Result<(), InvalidTransition> {
        *self.ensure_code()? = true;
        Ok(())
    }

    /// This side's `PairConfirm` was answered.
    pub(super) fn confirm_answered(&mut self) -> Result<(), InvalidTransition> {
        *self.ensure_code()? = false;
        Ok(())
    }

    fn ensure_connecting(&self) -> Result<(Role, Instant), InvalidTransition> {
        match &self.step {
            Step::Dialing { timeout, .. } => Ok((Role::Initiator, *timeout)),
            Step::Accepting { timeout } => Ok((Role::Responder, *timeout)),
            _ => Err(InvalidTransition),
        }
    }

    const fn ensure_connected(&self) -> Result<&Live<C>, InvalidTransition> {
        match &self.step {
            Step::Connected { live, .. } => Ok(live),
            _ => Err(InvalidTransition),
        }
    }

    /// The code step's `confirming`.
    const fn ensure_code(&mut self) -> Result<&mut bool, InvalidTransition> {
        match &mut self.step {
            Step::Code { confirming, .. } => Ok(confirming),
            _ => Err(InvalidTransition),
        }
    }

    fn ensure_live(&self) -> Result<&Live<C>, InvalidTransition> {
        self.live().ok_or(InvalidTransition)
    }
}

impl<C: Clone> Stage<C> {
    /// The responder's `PairPrompted` keeps the code valid for `valid_for`
    /// from `now`, which replaces the setup timeout.
    pub(super) fn prompted(
        &mut self,
        valid_for: Duration,
        now: Instant,
    ) -> Result<(), InvalidTransition> {
        let live = self.ensure_connected()?.clone();
        self.step = Step::Code {
            live,
            timeout: now + valid_for,
            confirming: false,
        };
        Ok(())
    }

    /// End the attempt and let what it sent reach the peer.
    pub(super) fn flush(&mut self, now: Instant) -> Result<(), InvalidTransition> {
        self.end(Ending::Flushing, now + LINGER).map(drop)
    }

    /// Both users confirmed and this side saved the peer: end the attempt,
    /// handing back its ceremony. While this side's `PairConfirm` is out,
    /// the connection waits for its answer.
    pub(super) fn paired(&mut self, now: Instant) -> Result<Live<C>, InvalidTransition> {
        let wait = if *self.ensure_code()? {
            CALL_TIMEOUT
        } else {
            LINGER
        };
        self.end(Ending::Paired, now + wait)
    }

    fn end(&mut self, ending: Ending, linger: Instant) -> Result<Live<C>, InvalidTransition> {
        let live = self.ensure_live()?.clone();
        self.step = Step::Ending {
            connection: live.link.connection.clone(),
            ending,
            linger,
        };
        Ok(live)
    }
}
