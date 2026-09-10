//! Pairing ceremony state machine and injected peer-key persistence.

use std::time::{Duration, Instant};

use thiserror::Error;

use crate::{
    generated as proto,
    sas::{PublicKey, SasCode, SessionNonce, derive_sas},
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
    /// A user reported that the displayed codes differ.
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

/// Externally observable pairing state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingState {
    /// No `PairStart` / `PairPrompted` exchange has completed.
    Idle,
    /// This side sent `PairStart` and is waiting for `PairPrompted`.
    AwaitingPrompt,
    /// Both prompts may be shown and confirmations are pending.
    Prompted {
        /// Receiver-declared timeout in milliseconds.
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
    /// The receiver-declared confirmation deadline expired.
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
    /// The peer key could not be durably persisted.
    #[error(transparent)]
    Persistence(#[from] PersistPeerKeyError),
}

#[derive(Clone, Copy, Debug)]
struct Prompted {
    timeout_ms: u32,
    deadline: Instant,
    local_confirmed: bool,
    peer_confirmed: bool,
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Idle,
    AwaitingPrompt,
    Prompted(Prompted),
    Paired,
    Rejected,
    TimedOut,
    Aborted(PairingAbortReason),
}

/// One connection-scoped, symmetric Flow pairing ceremony.
///
/// The session owns the two nonces and discards them on every terminal path.
/// A retry therefore requires a fresh connection and fresh [`SessionNonce`]s.
#[derive(Clone, Debug)]
pub struct PairingSession {
    peer_key: PublicKey,
    local_key: PublicKey,
    nonces: Option<(SessionNonce, SessionNonce)>,
    phase: Phase,
}

impl PairingSession {
    /// Creates an idle session from the authenticated TLS identities and Hello nonces.
    #[must_use]
    pub const fn new(
        local_key: PublicKey,
        peer_key: PublicKey,
        local_nonce: SessionNonce,
        peer_nonce: SessionNonce,
    ) -> Self {
        Self {
            peer_key,
            local_key,
            nonces: Some((local_nonce, peer_nonce)),
            phase: Phase::Idle,
        }
    }

    /// Returns the current externally observable state.
    #[must_use]
    pub const fn state(&self) -> PairingState {
        match self.phase {
            Phase::Idle => PairingState::Idle,
            Phase::AwaitingPrompt => PairingState::AwaitingPrompt,
            Phase::Prompted(prompted) => PairingState::Prompted {
                timeout_ms: prompted.timeout_ms,
                local_confirmed: prompted.local_confirmed,
                peer_confirmed: prompted.peer_confirmed,
            },
            Phase::Paired => PairingState::Paired,
            Phase::Rejected => PairingState::Rejected,
            Phase::TimedOut => PairingState::TimedOut,
            Phase::Aborted(reason) => PairingState::Aborted(reason),
        }
    }

    /// Returns the TLS-authenticated peer key this ceremony can persist.
    #[must_use]
    pub const fn peer_key(&self) -> PublicKey {
        self.peer_key
    }

    /// Derives the code shown on this side while the ceremony is active.
    #[must_use]
    pub fn sas_code(&self) -> Option<SasCode> {
        self.nonces.map(|(local_nonce, peer_nonce)| {
            derive_sas(self.local_key, self.peer_key, local_nonce, peer_nonce)
        })
    }

    /// Begins an initiating-side ceremony and creates its `PairStart` request.
    pub fn start(&mut self) -> Result<proto::PairStart, PairingError> {
        self.ensure_idle()?;
        self.phase = Phase::AwaitingPrompt;
        Ok(proto::PairStart::default())
    }

    /// Handles `PairStart` and returns the receiver-declared prompt timeout.
    pub fn receive_start(
        &mut self,
        now: Instant,
        timeout: Option<Duration>,
    ) -> Result<proto::PairPrompted, PairingError> {
        self.ensure_idle()?;
        let timeout = timeout.unwrap_or(DEFAULT_PAIRING_TIMEOUT);
        Ok(self.enter_prompted(now, timeout))
    }

    /// Handles a `PairPrompted` response on the initiating side.
    pub fn receive_prompted(
        &mut self,
        prompted: &proto::PairPrompted,
        now: Instant,
    ) -> Result<(), PairingError> {
        if !matches!(self.phase, Phase::AwaitingPrompt) {
            return Err(PairingError::InvalidTransition(self.state()));
        }
        let timeout = if prompted.timeout_ms == 0 {
            DEFAULT_PAIRING_TIMEOUT
        } else {
            Duration::from_millis(u64::from(prompted.timeout_ms))
        };
        self.enter_prompted(now, timeout);
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
    pub fn receive_confirm(
        &mut self,
        now: Instant,
        store: &mut impl PeerKeyStore,
    ) -> Result<proto::PairOutcome, PairingError> {
        let result = self.confirm(ConfirmationSide::Peer, now, store)?;
        Ok(proto::PairOutcome {
            result: result.into(),
            ..Default::default()
        })
    }

    /// Applies a peer-provided `PairOutcome` after validating its open enum.
    pub fn receive_outcome(&mut self, outcome: &proto::PairOutcome) -> Result<(), PairingError> {
        match outcome.result.as_known() {
            Some(proto::PairResult::Paired) => {
                if matches!(self.phase, Phase::Paired) {
                    Ok(())
                } else {
                    Err(PairingError::InvalidTransition(self.state()))
                }
            }
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
        self.ensure_prompted()?;
        self.terminal(Phase::Rejected);
        Ok(pair_outcome(proto::PairResult::Rejected))
    }

    /// Aborts the current ceremony and discards its nonces.
    pub fn abort(&mut self, reason: PairingAbortReason) -> Result<proto::PairAbort, PairingError> {
        self.ensure_prompted()?;
        self.terminal(Phase::Aborted(reason));
        Ok(proto::PairAbort {
            reason: proto::PairAbortReason::from(reason).into(),
            ..Default::default()
        })
    }

    /// Handles a peer `PairAbort` after validating its open enum.
    pub fn receive_abort(&mut self, abort: &proto::PairAbort) -> Result<(), PairingError> {
        self.ensure_prompted()?;
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

    /// Transitions an expired prompted session to `TIMEOUT`.
    ///
    /// Returns `None` when the deadline has not expired or the session is
    /// already terminal.
    pub fn check_timeout(&mut self, now: Instant) -> Option<proto::PairOutcome> {
        let Phase::Prompted(prompted) = self.phase else {
            return None;
        };
        if now < prompted.deadline {
            return None;
        }
        self.terminal(Phase::TimedOut);
        Some(pair_outcome(proto::PairResult::Timeout))
    }

    fn enter_prompted(&mut self, now: Instant, timeout: Duration) -> proto::PairPrompted {
        let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        self.phase = Phase::Prompted(Prompted {
            timeout_ms,
            deadline: now + Duration::from_millis(u64::from(timeout_ms)),
            local_confirmed: false,
            peer_confirmed: false,
        });
        proto::PairPrompted {
            timeout_ms,
            ..Default::default()
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
                _ => {}
            }
        }
        let Phase::Prompted(mut prompted) = self.phase else {
            return Err(PairingError::InvalidTransition(self.state()));
        };
        if now >= prompted.deadline {
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

    fn terminal(&mut self, phase: Phase) {
        self.nonces = None;
        self.phase = phase;
    }
}

#[derive(Clone, Copy)]
enum ConfirmationSide {
    Local,
    Peer,
}

fn pair_outcome(result: proto::PairResult) -> proto::PairOutcome {
    proto::PairOutcome {
        result: result.into(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MemoryStore(Vec<PublicKey>);

    impl PeerKeyStore for MemoryStore {
        fn persist_peer_key(&mut self, key: PublicKey) -> Result<(), PersistPeerKeyError> {
            self.0.push(key);
            Ok(())
        }
    }

    fn session() -> PairingSession {
        PairingSession::new(
            PublicKey::new([1; 32]),
            PublicKey::new([2; 32]),
            SessionNonce::new([3; 16]),
            SessionNonce::new([4; 16]),
        )
    }

    #[test]
    fn both_confirms_persist_once_and_pair() {
        let now = Instant::now();
        let mut session = session();
        let mut store = MemoryStore::default();
        session.receive_start(now, None).unwrap();

        assert_eq!(
            session.confirm_local(now, &mut store).unwrap(),
            proto::PairResult::PendingLocal
        );
        assert_eq!(
            session
                .receive_confirm(now, &mut store)
                .unwrap()
                .result
                .as_known(),
            Some(proto::PairResult::Paired)
        );
        assert_eq!(session.state(), PairingState::Paired);
        assert_eq!(store.0, [PublicKey::new([2; 32])]);
        assert!(session.sas_code().is_none());

        assert_eq!(
            session
                .receive_confirm(now, &mut store)
                .unwrap()
                .result
                .as_known(),
            Some(proto::PairResult::Paired)
        );
        assert_eq!(store.0.len(), 1);
    }

    #[test]
    fn confirmation_at_deadline_times_out_without_persisting() {
        let now = Instant::now();
        let mut session = session();
        let mut store = MemoryStore::default();
        session
            .receive_start(now, Some(Duration::from_secs(1)))
            .unwrap();

        assert_eq!(
            session.confirm_local(now, &mut store).unwrap(),
            proto::PairResult::PendingLocal
        );
        assert_eq!(
            session
                .receive_confirm(now + Duration::from_secs(1), &mut store)
                .unwrap()
                .result
                .as_known(),
            Some(proto::PairResult::Timeout)
        );
        assert_eq!(session.state(), PairingState::TimedOut);
        assert!(store.0.is_empty());
        assert!(session.sas_code().is_none());
    }

    #[test]
    fn timeout_uses_default_and_discards_nonce() {
        let now = Instant::now();
        let mut session = session();
        let prompted = session.receive_start(now, None).unwrap();
        assert_eq!(prompted.timeout_ms, 120_000);
        assert!(
            session
                .check_timeout(now + Duration::from_secs(119))
                .is_none()
        );
        assert_eq!(
            session
                .check_timeout(now + Duration::from_secs(120))
                .unwrap()
                .result
                .as_known(),
            Some(proto::PairResult::Timeout)
        );
        assert_eq!(session.state(), PairingState::TimedOut);
        assert!(session.sas_code().is_none());
    }

    #[test]
    fn abort_paths_are_terminal() {
        for reason in [
            PairingAbortReason::UserCancelled,
            PairingAbortReason::CodeMismatch,
            PairingAbortReason::Timeout,
        ] {
            let mut session = session();
            session.receive_start(Instant::now(), None).unwrap();
            session.abort(reason).unwrap();
            assert_eq!(session.state(), PairingState::Aborted(reason));
            assert!(session.sas_code().is_none());
        }
    }

    #[test]
    fn outcomes_cannot_start_or_overwrite_a_session() {
        let mut session = session();
        for result in [
            proto::PairResult::PendingLocal,
            proto::PairResult::Rejected,
            proto::PairResult::Timeout,
        ] {
            assert_eq!(
                session.receive_outcome(&pair_outcome(result)),
                Err(PairingError::InvalidTransition(PairingState::Idle))
            );
        }

        session.receive_start(Instant::now(), None).unwrap();
        session.reject().unwrap();
        assert_eq!(
            session.receive_outcome(&pair_outcome(proto::PairResult::Timeout)),
            Err(PairingError::InvalidTransition(PairingState::Rejected))
        );
    }

    #[test]
    fn prompted_requires_a_locally_started_session() {
        let mut session = session();
        let prompted = proto::PairPrompted {
            timeout_ms: 1_000,
            ..Default::default()
        };
        assert_eq!(
            session.receive_prompted(&prompted, Instant::now()),
            Err(PairingError::InvalidTransition(PairingState::Idle))
        );
        session.start().unwrap();
        session.receive_prompted(&prompted, Instant::now()).unwrap();
        assert!(matches!(
            session.state(),
            PairingState::Prompted {
                timeout_ms: 1_000,
                ..
            }
        ));
    }

    #[test]
    fn repeated_peer_confirm_replays_terminal_outcome() {
        let now = Instant::now();
        let mut session = session();
        let mut store = MemoryStore::default();
        session.receive_start(now, None).unwrap();
        session.reject().unwrap();
        assert_eq!(
            session
                .receive_confirm(now, &mut store)
                .unwrap()
                .result
                .as_known(),
            Some(proto::PairResult::Rejected)
        );
        assert!(store.0.is_empty());
    }
}
