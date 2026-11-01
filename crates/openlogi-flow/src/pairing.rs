//! Pairing ceremony state machine and injected peer-key persistence.
//!
//! The ceremony is commit-then-reveal (FLOW-PROTOCOL.md §Pairing):
//!
//! ```text
//! initiator                                 responder
//! start(nI)        ── PairStart{c} ──────►  receive_start(nR)
//! receive_prompted ◄─ PairPrompted{nR} ───
//!                  ── PairReveal{nI} ────►  receive_reveal   (checks c)
//! receive_revealed ◄─ PairRevealed ───────
//!         both sides now show sas_code() and exchange PairConfirm
//! ```
//!
//! The state machine performs no I/O and draws no randomness: callers inject
//! the nonce ([`PairingNonce::random`] in production) and the clock.

use std::time::{Duration, Instant};

use thiserror::Error;

use crate::{
    generated as proto,
    sas::{
        FixedBytesError, HelloDigest, NonceCommitment, PairingNonce, PairingParty, PublicKey,
        SasCode, derive_sas,
    },
};

/// Default time for users to compare and confirm the pairing code.
pub const DEFAULT_PAIRING_TIMEOUT: Duration = Duration::from_secs(120);

/// Durable storage used when a pairing session becomes authenticated.
pub trait PeerKeyStore {
    /// Persists a newly authenticated peer key.
    ///
    /// Implementations must durably commit the key before returning success.
    fn persist_peer_key(&mut self, key: PublicKey) -> Result<(), PersistPeerKeyError>;
}

/// Failure while durably persisting an authenticated peer key.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("failed to persist peer key: {detail}")]
pub struct PersistPeerKeyError {
    detail: String,
}

impl PersistPeerKeyError {
    /// Creates a persistence error from storage-specific detail.
    #[must_use]
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

/// A pairing abort reason after protobuf validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingAbortReason {
    /// A user cancelled the ceremony.
    UserCancelled,
    /// A user reported that the displayed codes differ, or the initiator's
    /// revealed nonce did not match its commitment.
    CodeMismatch,
    /// The confirmation deadline expired.
    Timeout,
}

impl From<PairingAbortReason> for proto::PairAbortReason {
    fn from(reason: PairingAbortReason) -> Self {
        match reason {
            PairingAbortReason::UserCancelled => Self::UserCancelled,
            PairingAbortReason::CodeMismatch => Self::CodeMismatch,
            PairingAbortReason::Timeout => Self::Timeout,
        }
    }
}

/// Which side of the ceremony a session plays.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingRole {
    /// This side sent `PairStart` and committed to its nonce first.
    Initiator,
    /// This side answered `PairStart` with its own nonce.
    Responder,
}

/// Externally observable pairing state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingState {
    /// No ceremony has started on this connection.
    Idle,
    /// The initiator sent its commitment and waits for `PairPrompted`.
    AwaitingPrompt,
    /// The responder sent its nonce and waits for `PairReveal`.
    AwaitingReveal {
        /// Responder-declared timeout in milliseconds.
        timeout_ms: u32,
    },
    /// The initiator revealed its nonce and waits for `PairRevealed`.
    AwaitingRevealed {
        /// Responder-declared timeout in milliseconds.
        timeout_ms: u32,
    },
    /// Both nonces are known and verified; the code may be shown and
    /// confirmations are pending.
    Prompted {
        /// Responder-declared timeout in milliseconds.
        timeout_ms: u32,
        /// Whether this side has sent `PairConfirm`.
        local_confirmed: bool,
        /// Whether this side has received `PairConfirm`.
        peer_confirmed: bool,
    },
    /// Both confirmations were observed and the peer key was persisted.
    Paired,
    /// A user declined the ceremony.
    Rejected,
    /// The responder-declared deadline expired.
    TimedOut,
    /// Either peer aborted the ceremony.
    Aborted(PairingAbortReason),
}

/// Invalid state, wire value, or persistence failure during pairing.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PairingError {
    /// The requested transition is not legal in the current state.
    #[error("pairing transition is invalid while in {0:?}")]
    InvalidTransition(PairingState),
    /// A required open enum carried an unspecified or unknown value.
    #[error("invalid pairing enum value {0}")]
    InvalidEnum(i32),
    /// A fixed-width pairing field had the wrong length.
    #[error("pairing field `{field}` is malformed: {source}")]
    InvalidField {
        /// Protobuf field name.
        field: &'static str,
        /// Length mismatch detail.
        source: FixedBytesError,
    },
    /// The revealed nonce does not open the initiator's commitment.
    ///
    /// The session is aborted with [`PairingAbortReason::CodeMismatch`].
    #[error("revealed pairing nonce does not match its commitment")]
    CommitmentMismatch,
    /// The peer key could not be durably persisted.
    #[error(transparent)]
    Persistence(#[from] PersistPeerKeyError),
}

#[derive(Clone, Copy, Debug)]
struct Deadline {
    timeout_ms: u32,
    at: Instant,
}

impl Deadline {
    fn new(now: Instant, timeout: Duration) -> Self {
        let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        Self {
            timeout_ms,
            at: now + Duration::from_millis(u64::from(timeout_ms)),
        }
    }

    fn expired(self, now: Instant) -> bool {
        now >= self.at
    }
}

#[derive(Clone, Copy, Debug)]
struct Prompted {
    code: SasCode,
    deadline: Deadline,
    local_confirmed: bool,
    peer_confirmed: bool,
}

impl Prompted {
    const fn new(code: SasCode, deadline: Deadline) -> Self {
        Self {
            code,
            deadline,
            local_confirmed: false,
            peer_confirmed: false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Idle,
    AwaitingPrompt {
        nonce: PairingNonce,
    },
    AwaitingReveal {
        commitment: NonceCommitment,
        nonce: PairingNonce,
        deadline: Deadline,
    },
    AwaitingRevealed {
        code: SasCode,
        deadline: Deadline,
        /// The responder's `PairConfirm` overtook its `PairRevealed`.
        peer_confirmed: bool,
    },
    Prompted(Prompted),
    Paired,
    Rejected,
    TimedOut,
    Aborted(PairingAbortReason),
}

/// One connection-scoped Flow pairing ceremony.
///
/// The session owns its nonces and code and discards them on every terminal
/// path. A retry therefore requires a fresh connection and fresh nonces.
#[derive(Clone, Debug)]
pub struct PairingSession {
    local_key: PublicKey,
    peer_key: PublicKey,
    local_hello: HelloDigest,
    peer_hello: HelloDigest,
    role: Option<PairingRole>,
    phase: Phase,
}

impl PairingSession {
    /// Creates an idle session from the TLS identities and the digests of the
    /// two `Hello` payloads exchanged on this connection.
    #[must_use]
    pub const fn new(
        local_key: PublicKey,
        peer_key: PublicKey,
        local_hello: HelloDigest,
        peer_hello: HelloDigest,
    ) -> Self {
        Self {
            local_key,
            peer_key,
            local_hello,
            peer_hello,
            role: None,
            phase: Phase::Idle,
        }
    }

    /// Returns the current externally observable state.
    #[must_use]
    pub const fn state(&self) -> PairingState {
        match self.phase {
            Phase::Idle => PairingState::Idle,
            Phase::AwaitingPrompt { .. } => PairingState::AwaitingPrompt,
            Phase::AwaitingReveal { deadline, .. } => PairingState::AwaitingReveal {
                timeout_ms: deadline.timeout_ms,
            },
            Phase::AwaitingRevealed { deadline, .. } => PairingState::AwaitingRevealed {
                timeout_ms: deadline.timeout_ms,
            },
            Phase::Prompted(prompted) => PairingState::Prompted {
                timeout_ms: prompted.deadline.timeout_ms,
                local_confirmed: prompted.local_confirmed,
                peer_confirmed: prompted.peer_confirmed,
            },
            Phase::Paired => PairingState::Paired,
            Phase::Rejected => PairingState::Rejected,
            Phase::TimedOut => PairingState::TimedOut,
            Phase::Aborted(reason) => PairingState::Aborted(reason),
        }
    }

    /// Returns the role this side took, once the ceremony started.
    #[must_use]
    pub const fn role(&self) -> Option<PairingRole> {
        self.role
    }

    /// Returns the TLS-authenticated peer key this ceremony can persist.
    #[must_use]
    pub const fn peer_key(&self) -> PublicKey {
        self.peer_key
    }

    /// Returns whether this ceremony was created for exactly these
    /// identities and `Hello` transcript.
    pub(crate) fn is_bound_to(
        &self,
        local_key: PublicKey,
        peer_key: PublicKey,
        local_hello: HelloDigest,
        peer_hello: HelloDigest,
    ) -> bool {
        self.local_key == local_key
            && self.peer_key == peer_key
            && self.local_hello == local_hello
            && self.peer_hello == peer_hello
    }

    /// Returns the code to show, only while confirmations are pending.
    ///
    /// The code exists only after the responder verified the initiator's
    /// commitment, so neither side shows a code an attacker could have chosen.
    #[must_use]
    pub const fn sas_code(&self) -> Option<SasCode> {
        match self.phase {
            Phase::Prompted(prompted) => Some(prompted.code),
            _ => None,
        }
    }

    /// Begins an initiating-side ceremony and creates its `PairStart` request.
    pub fn start(&mut self, nonce: PairingNonce) -> Result<proto::PairStart, PairingError> {
        self.ensure_idle()?;
        let commitment = NonceCommitment::commit(self.local_key, self.peer_key, nonce);
        self.role = Some(PairingRole::Initiator);
        self.phase = Phase::AwaitingPrompt { nonce };
        Ok(proto::PairStart {
            nonce_commitment: commitment.as_bytes().to_vec(),
            ..Default::default()
        })
    }

    /// Handles `PairStart` and answers with this side's nonce and deadline.
    ///
    /// A malformed commitment leaves the session idle; answer it with
    /// `Error INVALID`.
    pub fn receive_start(
        &mut self,
        start: &proto::PairStart,
        nonce: PairingNonce,
        now: Instant,
        timeout: Option<Duration>,
    ) -> Result<proto::PairPrompted, PairingError> {
        self.ensure_idle()?;
        let commitment = NonceCommitment::try_from(start.nonce_commitment.as_slice())
            .map_err(|source| invalid_field("nonce_commitment", source))?;
        let deadline = Deadline::new(now, timeout.unwrap_or(DEFAULT_PAIRING_TIMEOUT));
        self.role = Some(PairingRole::Responder);
        self.phase = Phase::AwaitingReveal {
            commitment,
            nonce,
            deadline,
        };
        Ok(proto::PairPrompted {
            timeout_ms: deadline.timeout_ms,
            nonce: nonce.as_bytes().to_vec(),
            ..Default::default()
        })
    }

    /// Handles `PairPrompted` on the initiator and creates its `PairReveal`.
    ///
    /// A malformed nonce leaves the session waiting; the caller should
    /// [`abort`](Self::abort) it.
    pub fn receive_prompted(
        &mut self,
        prompted: &proto::PairPrompted,
        now: Instant,
    ) -> Result<proto::PairReveal, PairingError> {
        let Phase::AwaitingPrompt { nonce } = self.phase else {
            return Err(PairingError::InvalidTransition(self.state()));
        };
        let peer_nonce = PairingNonce::try_from(prompted.nonce.as_slice())
            .map_err(|source| invalid_field("nonce", source))?;
        let timeout = if prompted.timeout_ms == 0 {
            DEFAULT_PAIRING_TIMEOUT
        } else {
            Duration::from_millis(u64::from(prompted.timeout_ms))
        };
        let code = derive_sas(&self.local_party(nonce), &self.peer_party(peer_nonce));
        self.phase = Phase::AwaitingRevealed {
            code,
            deadline: Deadline::new(now, timeout),
            peer_confirmed: false,
        };
        Ok(proto::PairReveal {
            nonce: nonce.as_bytes().to_vec(),
            ..Default::default()
        })
    }

    /// Handles `PairReveal` on the responder and verifies the commitment.
    ///
    /// On [`PairingError::CommitmentMismatch`] the session is already aborted;
    /// answer with `Error INVALID`.
    pub fn receive_reveal(
        &mut self,
        reveal: &proto::PairReveal,
        now: Instant,
    ) -> Result<proto::PairRevealed, PairingError> {
        let Phase::AwaitingReveal {
            commitment,
            nonce,
            deadline,
        } = self.phase
        else {
            return Err(PairingError::InvalidTransition(self.state()));
        };
        if deadline.expired(now) {
            self.terminal(Phase::TimedOut);
            return Err(PairingError::InvalidTransition(self.state()));
        }
        let peer_nonce = PairingNonce::try_from(reveal.nonce.as_slice())
            .map_err(|source| invalid_field("nonce", source))?;
        if !commitment.opens_to(self.peer_key, self.local_key, peer_nonce) {
            self.terminal(Phase::Aborted(PairingAbortReason::CodeMismatch));
            return Err(PairingError::CommitmentMismatch);
        }
        let code = derive_sas(&self.peer_party(peer_nonce), &self.local_party(nonce));
        self.phase = Phase::Prompted(Prompted::new(code, deadline));
        Ok(proto::PairRevealed::default())
    }

    /// Handles `PairRevealed` on the initiator; the code may now be shown.
    ///
    /// A peer confirmation that arrived first is kept, so the local confirm
    /// completes the pairing.
    pub fn receive_revealed(&mut self, now: Instant) -> Result<(), PairingError> {
        let Phase::AwaitingRevealed {
            code,
            deadline,
            peer_confirmed,
        } = self.phase
        else {
            return Err(PairingError::InvalidTransition(self.state()));
        };
        if deadline.expired(now) {
            self.terminal(Phase::TimedOut);
            return Err(PairingError::InvalidTransition(self.state()));
        }
        self.phase = Phase::Prompted(Prompted {
            peer_confirmed,
            ..Prompted::new(code, deadline)
        });
        Ok(())
    }

    /// Records that this side sent `PairConfirm`.
    ///
    /// If the peer confirmation was already received, this persists the peer
    /// key and atomically moves the session to [`PairingState::Paired`].
    pub fn confirm_local(
        &mut self,
        now: Instant,
        store: &mut impl PeerKeyStore,
    ) -> Result<proto::PairResult, PairingError> {
        self.confirm(ConfirmationSide::Local, now, store)
    }

    /// Handles a peer `PairConfirm` and returns its idempotent RPC outcome.
    ///
    /// The responder may confirm as soon as it shows its code, and its
    /// `PairConfirm` travels on another stream than its `PairRevealed`, so it
    /// can reach the initiator first. That confirmation is kept and answered
    /// `PENDING_LOCAL`; this side still cannot confirm or trust the peer
    /// before `PairRevealed`.
    pub fn receive_confirm(
        &mut self,
        now: Instant,
        store: &mut impl PeerKeyStore,
    ) -> Result<proto::PairOutcome, PairingError> {
        let result = self.confirm(ConfirmationSide::Peer, now, store)?;
        Ok(pair_outcome(result))
    }

    /// Applies a peer-provided `PairOutcome` after validating its open enum.
    ///
    /// The two confirms cross on the wire, so a valid ceremony can deliver
    /// either reply late: `PAIRED` while the peer's own `PairConfirm` is still
    /// in flight, or `PENDING_LOCAL` after that confirm already completed the
    /// pairing here. Neither moves the state: completion still needs the
    /// peer's confirm, and a completed pairing stays complete.
    pub fn receive_outcome(&mut self, outcome: &proto::PairOutcome) -> Result<(), PairingError> {
        match outcome.result.as_known() {
            Some(proto::PairResult::Paired) => match self.phase {
                Phase::Paired => Ok(()),
                Phase::Prompted(prompted) if prompted.local_confirmed => Ok(()),
                _ => Err(PairingError::InvalidTransition(self.state())),
            },
            Some(proto::PairResult::PendingLocal) if matches!(self.phase, Phase::Paired) => Ok(()),
            Some(proto::PairResult::PendingLocal) => self.ensure_prompted(),
            Some(proto::PairResult::Rejected) => {
                self.ensure_prompted()?;
                self.terminal(Phase::Rejected);
                Ok(())
            }
            Some(proto::PairResult::Timeout) => {
                self.ensure_prompted()?;
                self.terminal(Phase::TimedOut);
                Ok(())
            }
            Some(proto::PairResult::Unspecified) | None => {
                Err(PairingError::InvalidEnum(outcome.result.to_i32()))
            }
        }
    }

    /// Rejects the local prompt and returns the terminal RPC outcome.
    pub fn reject(&mut self) -> Result<proto::PairOutcome, PairingError> {
        self.ensure_active()?;
        self.terminal(Phase::Rejected);
        Ok(pair_outcome(proto::PairResult::Rejected))
    }

    /// Aborts the current ceremony and discards its nonces and code.
    pub fn abort(&mut self, reason: PairingAbortReason) -> Result<proto::PairAbort, PairingError> {
        self.ensure_active()?;
        self.terminal(Phase::Aborted(reason));
        Ok(proto::PairAbort {
            reason: proto::PairAbortReason::from(reason).into(),
            ..Default::default()
        })
    }

    /// Handles a peer `PairAbort` after validating its open enum.
    pub fn receive_abort(&mut self, abort: &proto::PairAbort) -> Result<(), PairingError> {
        self.ensure_active()?;
        let reason = match abort.reason.as_known() {
            Some(proto::PairAbortReason::UserCancelled) => PairingAbortReason::UserCancelled,
            Some(proto::PairAbortReason::CodeMismatch) => PairingAbortReason::CodeMismatch,
            Some(proto::PairAbortReason::Timeout) => PairingAbortReason::Timeout,
            Some(proto::PairAbortReason::Unspecified) | None => {
                return Err(PairingError::InvalidEnum(abort.reason.to_i32()));
            }
        };
        self.terminal(Phase::Aborted(reason));
        Ok(())
    }

    /// Transitions an expired session to `TIMEOUT`.
    ///
    /// Returns `None` when no deadline is running, it has not expired, or the
    /// session is already terminal.
    pub fn check_timeout(&mut self, now: Instant) -> Option<proto::PairOutcome> {
        let deadline = match self.phase {
            Phase::AwaitingReveal { deadline, .. } | Phase::AwaitingRevealed { deadline, .. } => {
                deadline
            }
            Phase::Prompted(prompted) => prompted.deadline,
            _ => return None,
        };
        if !deadline.expired(now) {
            return None;
        }
        self.terminal(Phase::TimedOut);
        Some(pair_outcome(proto::PairResult::Timeout))
    }

    const fn local_party(&self, nonce: PairingNonce) -> PairingParty {
        PairingParty {
            key: self.local_key,
            hello: self.local_hello,
            nonce,
        }
    }

    const fn peer_party(&self, nonce: PairingNonce) -> PairingParty {
        PairingParty {
            key: self.peer_key,
            hello: self.peer_hello,
            nonce,
        }
    }

    fn confirm(
        &mut self,
        side: ConfirmationSide,
        now: Instant,
        store: &mut impl PeerKeyStore,
    ) -> Result<proto::PairResult, PairingError> {
        if matches!(self.phase, Phase::Paired) {
            return Ok(proto::PairResult::Paired);
        }
        if matches!(side, ConfirmationSide::Peer) {
            match self.phase {
                Phase::Rejected => return Ok(proto::PairResult::Rejected),
                Phase::TimedOut => return Ok(proto::PairResult::Timeout),
                Phase::AwaitingRevealed { deadline, .. } if deadline.expired(now) => {
                    self.terminal(Phase::TimedOut);
                    return Ok(proto::PairResult::Timeout);
                }
                Phase::AwaitingRevealed { code, deadline, .. } => {
                    self.phase = Phase::AwaitingRevealed {
                        code,
                        deadline,
                        peer_confirmed: true,
                    };
                    return Ok(proto::PairResult::PendingLocal);
                }
                _ => {}
            }
        }
        let Phase::Prompted(mut prompted) = self.phase else {
            return Err(PairingError::InvalidTransition(self.state()));
        };
        if prompted.deadline.expired(now) {
            self.terminal(Phase::TimedOut);
            return Ok(proto::PairResult::Timeout);
        }
        match side {
            ConfirmationSide::Local => prompted.local_confirmed = true,
            ConfirmationSide::Peer => prompted.peer_confirmed = true,
        }
        self.phase = Phase::Prompted(prompted);
        if prompted.local_confirmed && prompted.peer_confirmed {
            store.persist_peer_key(self.peer_key)?;
            self.terminal(Phase::Paired);
            Ok(proto::PairResult::Paired)
        } else {
            Ok(proto::PairResult::PendingLocal)
        }
    }

    fn ensure_idle(&self) -> Result<(), PairingError> {
        if matches!(self.phase, Phase::Idle) {
            Ok(())
        } else {
            Err(PairingError::InvalidTransition(self.state()))
        }
    }

    fn ensure_prompted(&self) -> Result<(), PairingError> {
        if matches!(self.phase, Phase::Prompted(_)) {
            Ok(())
        } else {
            Err(PairingError::InvalidTransition(self.state()))
        }
    }

    fn ensure_active(&self) -> Result<(), PairingError> {
        if matches!(
            self.phase,
            Phase::AwaitingPrompt { .. }
                | Phase::AwaitingReveal { .. }
                | Phase::AwaitingRevealed { .. }
                | Phase::Prompted(_)
        ) {
            Ok(())
        } else {
            Err(PairingError::InvalidTransition(self.state()))
        }
    }

    fn terminal(&mut self, phase: Phase) {
        self.phase = phase;
    }
}

#[derive(Clone, Copy)]
enum ConfirmationSide {
    Local,
    Peer,
}

const fn invalid_field(field: &'static str, source: FixedBytesError) -> PairingError {
    PairingError::InvalidField { field, source }
}

fn pair_outcome(result: proto::PairResult) -> proto::PairOutcome {
    proto::PairOutcome {
        result: result.into(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests;
