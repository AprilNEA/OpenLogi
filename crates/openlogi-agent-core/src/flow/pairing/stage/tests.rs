//! The attempt's stages, reached through their transitions alone.

use std::time::Duration;

use openlogi_core::config::FlowPublicKey;
use openlogi_flow::pairing::{PairingSession, PeerKeyStore, PersistPeerKeyError};
use openlogi_flow::sas::{HelloDigest, PairingNonce, PublicKey};
use openlogi_ipc::{FlowPairingFailure, FlowPairingPeer, FlowPairingPhase, FlowPlatform};
use tokio::time::Instant;

use super::{Deadline, Ending, InvalidTransition, Stage};
use crate::flow::pairing::{CALL_TIMEOUT, LINGER, Link, Role, SETUP_TIMEOUT};

/// How long the responder keeps the code valid here: neither the setup
/// timeout nor the protocol default, so a deadline that kept either shows.
const VALID_FOR: Duration = Duration::from_secs(47);
/// The connection every link here carries.
const CONNECTION: u8 = 7;

const INITIATOR_KEY: PublicKey = PublicKey::new([1; 32]);
const RESPONDER_KEY: PublicKey = PublicKey::new([2; 32]);
const INITIATOR_NONCE: PairingNonce = PairingNonce::new([3; 16]);
const RESPONDER_NONCE: PairingNonce = PairingNonce::new([4; 16]);

/// Only one side confirms here, so nothing is ever saved.
struct NothingSaved;

impl PeerKeyStore for NothingSaved {
    fn persist_peer_key(&mut self, _key: PublicKey) -> Result<(), PersistPeerKeyError> {
        Err(PersistPeerKeyError::new("only one side confirmed"))
    }
}

fn peer(name: &str) -> FlowPairingPeer {
    FlowPairingPeer {
        name: name.to_owned(),
        public_key: FlowPublicKey::from_bytes([9; 32]),
        platform: FlowPlatform::Linux,
    }
}

/// The initiator's and the responder's ceremonies on one connection.
fn sessions() -> (PairingSession, PairingSession) {
    let initiator_hello = HelloDigest::of(b"initiator hello");
    let responder_hello = HelloDigest::of(b"responder hello");
    (
        PairingSession::new(
            INITIATOR_KEY,
            RESPONDER_KEY,
            initiator_hello,
            responder_hello,
        ),
        PairingSession::new(
            RESPONDER_KEY,
            INITIATOR_KEY,
            responder_hello,
            initiator_hello,
        ),
    )
}

const fn link(session: PairingSession) -> Link<u8> {
    Link {
        connection: CONNECTION,
        session,
    }
}

fn session(stage: &mut Stage<u8>) -> &mut PairingSession {
    &mut stage.live_mut().expect("a connected attempt").link.session
}

/// A dial that connected at `now`.
fn connected(now: Instant) -> Stage<u8> {
    let mut stage = Stage::dialing(None, now);
    stage
        .connect(peer("responder"), link(sessions().0))
        .expect("a dial connects");
    stage
}

/// A dial whose code became valid at `now`.
fn code(now: Instant) -> Stage<u8> {
    let mut stage = connected(now);
    stage
        .prompted(VALID_FOR, now)
        .expect("a connected attempt is prompted");
    stage
}

/// What a refused transition must leave as it was.
#[derive(Debug, PartialEq)]
struct Observed {
    deadline: Deadline,
    phase: Option<FlowPairingPhase>,
    ending: Option<Ending>,
    connection: Option<u8>,
}

fn observed(stage: &Stage<u8>) -> Observed {
    Observed {
        deadline: stage.deadline(),
        phase: stage.phase(),
        ending: stage.ending(),
        connection: stage.connection().copied(),
    }
}

#[test]
fn a_dial_shows_its_peer_and_fails_unreachable_when_nobody_answers() {
    let now = Instant::now();
    let found = Stage::<u8>::dialing(Some(peer("found")), now);
    assert_eq!(
        found.phase(),
        Some(FlowPairingPhase::Connecting {
            peer: Some(peer("found"))
        })
    );
    assert_eq!(
        found.deadline(),
        Deadline::Timeout {
            at: now + SETUP_TIMEOUT,
            failure: FlowPairingFailure::Unreachable,
        }
    );
    assert_eq!(found.connection(), None);

    let typed = Stage::<u8>::dialing(None, now);
    assert_eq!(
        typed.phase(),
        Some(FlowPairingPhase::Connecting { peer: None })
    );
}

#[test]
fn a_connected_dial_shows_the_computer_that_answered_under_the_setup_timeout() {
    let now = Instant::now();
    let mut stage = Stage::dialing(Some(peer("advertised")), now);
    assert_eq!(
        stage.connect(peer("answered"), link(sessions().0)),
        Ok(Role::Initiator)
    );
    assert_eq!(
        stage.phase(),
        Some(FlowPairingPhase::Connecting {
            peer: Some(peer("answered"))
        })
    );
    // Connecting does not restart the clock: the setup timeout bounds the
    // whole way to the code.
    assert_eq!(
        stage.deadline(),
        Deadline::Timeout {
            at: now + SETUP_TIMEOUT,
            failure: FlowPairingFailure::TimedOut,
        }
    );
    assert_eq!(stage.connection(), Some(&CONNECTION));
}

#[test]
fn an_inbound_attempt_shows_only_once_its_code_runs() {
    let now = Instant::now();
    let setup = Deadline::Timeout {
        at: now + SETUP_TIMEOUT,
        failure: FlowPairingFailure::TimedOut,
    };
    let mut stage = Stage::accepting(now);
    assert_eq!(stage.phase(), None);
    assert_eq!(stage.deadline(), setup);

    assert_eq!(
        stage.connect(peer("initiator"), link(sessions().1)),
        Ok(Role::Responder)
    );
    assert_eq!(stage.phase(), None);
    assert_eq!(stage.deadline(), setup);

    let prompted_at = now + Duration::from_secs(3);
    stage
        .prompted(VALID_FOR, prompted_at)
        .expect("a connected attempt is prompted");
    assert_eq!(
        stage.phase(),
        Some(FlowPairingPhase::Connecting {
            peer: Some(peer("initiator"))
        })
    );
    assert_eq!(
        stage.deadline(),
        Deadline::Timeout {
            at: prompted_at + VALID_FOR,
            failure: FlowPairingFailure::TimedOut,
        }
    );
}

#[test]
fn the_code_shows_once_both_nonces_are_verified_and_waits_once_confirmed() {
    let now = Instant::now();
    let clock = std::time::Instant::now();
    let (initiator, mut responder) = sessions();
    let mut stage = Stage::dialing(None, now);
    stage
        .connect(peer("responder"), link(initiator))
        .expect("a dial connects");

    let start = session(&mut stage).start(INITIATOR_NONCE).expect("start");
    let prompted = responder
        .receive_start(&start, RESPONDER_NONCE, clock, None)
        .expect("PairStart");
    let reveal = session(&mut stage)
        .receive_prompted(&prompted, clock)
        .expect("PairPrompted");
    stage
        .prompted(VALID_FOR, now)
        .expect("a connected attempt is prompted");
    // Its nonce revealed but not yet verified, this side has no code.
    assert_eq!(
        stage.phase(),
        Some(FlowPairingPhase::Connecting {
            peer: Some(peer("responder"))
        })
    );

    responder
        .receive_reveal(&reveal, clock)
        .expect("PairReveal");
    session(&mut stage)
        .receive_revealed(clock)
        .expect("PairRevealed");
    let code = responder
        .sas_code()
        .expect("the responder's code")
        .to_string();
    assert_eq!(
        stage.phase(),
        Some(FlowPairingPhase::ShowingCode {
            peer: peer("responder"),
            code: code.clone(),
        })
    );

    session(&mut stage)
        .confirm_local(clock, &mut NothingSaved)
        .expect("this side confirms");
    assert_eq!(
        stage.phase(),
        Some(FlowPairingPhase::WaitingForPeer {
            peer: peer("responder"),
            code,
        })
    );
}

#[test]
fn the_deadline_is_a_timeout_while_the_attempt_runs_and_a_linger_once_it_ended() {
    let now = Instant::now();
    let ended_at = now + Duration::from_secs(5);
    for (name, mut stage) in [("connected", connected(now)), ("code", code(now))] {
        assert!(
            matches!(stage.deadline(), Deadline::Timeout { .. }),
            "{name}"
        );
        stage.flush(ended_at).expect("a running attempt flushes");
        assert_eq!(
            stage.deadline(),
            Deadline::Linger(ended_at + LINGER),
            "{name}"
        );
        assert_eq!(stage.ending(), Some(Ending::Flushing), "{name}");
        assert_eq!(stage.phase(), None, "{name}");
        assert_eq!(stage.peer(), None, "{name}");
        // The ended attempt keeps its connection to close it.
        assert_eq!(stage.connection(), Some(&CONNECTION), "{name}");
    }
}

#[test]
fn a_paired_attempt_waits_for_the_answer_to_its_confirmation() {
    let now = Instant::now();
    let ended_at = now + Duration::from_secs(5);

    let mut confirming = code(now);
    confirming.confirm_sent().expect("the code stage confirms");
    let ended = confirming.paired(ended_at).expect("the code stage pairs");
    assert_eq!(ended.peer(), &peer("responder"));
    assert_eq!(
        confirming.deadline(),
        Deadline::Linger(ended_at + CALL_TIMEOUT)
    );
    assert_eq!(confirming.ending(), Some(Ending::Paired));
    assert_eq!(confirming.phase(), None);
    assert_eq!(confirming.connection(), Some(&CONNECTION));

    let mut answered = code(now);
    answered.confirm_sent().expect("the code stage confirms");
    answered
        .confirm_answered()
        .expect("the code stage hears the answer");
    answered.paired(ended_at).expect("the code stage pairs");
    assert_eq!(answered.deadline(), Deadline::Linger(ended_at + LINGER));

    let mut unconfirmed = code(now);
    unconfirmed.paired(ended_at).expect("the code stage pairs");
    assert_eq!(unconfirmed.deadline(), Deadline::Linger(ended_at + LINGER));
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Transition {
    Connect,
    Prompted,
    ConfirmSent,
    ConfirmAnswered,
    Flush,
    Paired,
}

impl Transition {
    const ALL: [Self; 6] = [
        Self::Connect,
        Self::Prompted,
        Self::ConfirmSent,
        Self::ConfirmAnswered,
        Self::Flush,
        Self::Paired,
    ];

    fn apply(self, stage: &mut Stage<u8>, now: Instant) -> Result<(), InvalidTransition> {
        match self {
            Self::Connect => stage.connect(peer("other"), link(sessions().0)).map(drop),
            Self::Prompted => stage.prompted(VALID_FOR, now),
            Self::ConfirmSent => stage.confirm_sent(),
            Self::ConfirmAnswered => stage.confirm_answered(),
            Self::Flush => stage.flush(now),
            Self::Paired => stage.paired(now).map(drop),
        }
    }
}

/// A stage, reached at a given time through its transitions, and the
/// transitions it allows.
type Row = (
    &'static str,
    fn(Instant) -> Stage<u8>,
    &'static [Transition],
);

#[test]
fn each_stage_allows_only_its_own_transitions_and_a_refused_one_changes_nothing() {
    use Transition::{ConfirmAnswered, ConfirmSent, Connect, Flush, Paired, Prompted};
    let stages: [Row; 6] = [
        ("dialing", |now| Stage::dialing(None, now), &[Connect]),
        ("accepting", Stage::accepting, &[Connect]),
        ("connected", connected, &[Prompted, Flush]),
        ("code", code, &[ConfirmSent, ConfirmAnswered, Flush, Paired]),
        (
            "flushing",
            |now| {
                let mut stage = code(now);
                stage.flush(now).expect("the code stage flushes");
                stage
            },
            &[],
        ),
        (
            "paired",
            |now| {
                let mut stage = code(now);
                stage.paired(now).expect("the code stage pairs");
                stage
            },
            &[],
        ),
    ];
    let now = Instant::now();
    let later = now + Duration::from_secs(5);
    for (name, stage_at, allowed) in stages {
        for transition in Transition::ALL {
            let mut stage = stage_at(now);
            let before = observed(&stage);
            let applied = transition.apply(&mut stage, later);
            if allowed.contains(&transition) {
                assert_eq!(applied, Ok(()), "{name} allows {transition:?}");
            } else {
                assert_eq!(
                    applied,
                    Err(InvalidTransition),
                    "{name} refuses {transition:?}"
                );
                assert_eq!(observed(&stage), before, "{name} after {transition:?}");
            }
        }
    }
}
