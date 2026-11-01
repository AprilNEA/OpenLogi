use super::*;

#[derive(Default)]
struct MemoryStore(Vec<PublicKey>);

impl PeerKeyStore for MemoryStore {
    fn persist_peer_key(&mut self, key: PublicKey) -> Result<(), PersistPeerKeyError> {
        self.0.push(key);
        Ok(())
    }
}

struct FailingStore;

impl PeerKeyStore for FailingStore {
    fn persist_peer_key(&mut self, _key: PublicKey) -> Result<(), PersistPeerKeyError> {
        Err(PersistPeerKeyError::new("disk full"))
    }
}

const INITIATOR_KEY: PublicKey = PublicKey::new([1; 32]);
const RESPONDER_KEY: PublicKey = PublicKey::new([2; 32]);
const INITIATOR_NONCE: PairingNonce = PairingNonce::new([3; 16]);
const RESPONDER_NONCE: PairingNonce = PairingNonce::new([4; 16]);

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

/// Drives both sides to `Prompted` with an optional responder timeout.
fn prompted_pair(now: Instant, timeout: Option<Duration>) -> (PairingSession, PairingSession) {
    let (mut initiator, mut responder) = sessions();
    let start = initiator.start(INITIATOR_NONCE).unwrap();
    let prompted = responder
        .receive_start(&start, RESPONDER_NONCE, now, timeout)
        .unwrap();
    let reveal = initiator.receive_prompted(&prompted, now).unwrap();
    responder.receive_reveal(&reveal, now).unwrap();
    initiator.receive_revealed(now).unwrap();
    (initiator, responder)
}

#[test]
fn commit_reveal_agrees_on_one_code_and_pairs() {
    let now = Instant::now();
    let (mut initiator, mut responder) = sessions();
    let start = initiator.start(INITIATOR_NONCE).unwrap();
    assert_eq!(initiator.role(), Some(PairingRole::Initiator));
    assert_eq!(
        start.nonce_commitment,
        NonceCommitment::commit(INITIATOR_KEY, RESPONDER_KEY, INITIATOR_NONCE)
            .as_bytes()
            .to_vec()
    );

    let prompted = responder
        .receive_start(&start, RESPONDER_NONCE, now, None)
        .unwrap();
    assert_eq!(responder.role(), Some(PairingRole::Responder));
    assert_eq!(prompted.nonce, RESPONDER_NONCE.as_bytes().to_vec());
    assert_eq!(
        responder.state(),
        PairingState::AwaitingReveal {
            timeout_ms: 120_000
        }
    );
    assert_eq!(responder.sas_code(), None);

    let reveal = initiator.receive_prompted(&prompted, now).unwrap();
    assert_eq!(reveal.nonce, INITIATOR_NONCE.as_bytes().to_vec());
    assert_eq!(initiator.sas_code(), None, "code waits for PairRevealed");

    responder.receive_reveal(&reveal, now).unwrap();
    initiator.receive_revealed(now).unwrap();
    let code = initiator.sas_code().unwrap();
    assert_eq!(responder.sas_code(), Some(code));

    let mut initiator_store = MemoryStore::default();
    let mut responder_store = MemoryStore::default();
    assert_eq!(
        initiator.confirm_local(now, &mut initiator_store).unwrap(),
        proto::PairResult::PendingLocal
    );
    assert_eq!(
        responder
            .receive_confirm(now, &mut responder_store)
            .unwrap()
            .result
            .as_known(),
        Some(proto::PairResult::PendingLocal)
    );
    assert_eq!(
        responder.confirm_local(now, &mut responder_store).unwrap(),
        proto::PairResult::Paired
    );
    let outcome = initiator
        .receive_confirm(now, &mut initiator_store)
        .unwrap();
    assert_eq!(outcome.result.as_known(), Some(proto::PairResult::Paired));
    responder.receive_outcome(&outcome).unwrap();

    assert_eq!(initiator.state(), PairingState::Paired);
    assert_eq!(responder.state(), PairingState::Paired);
    assert_eq!(initiator_store.0, [RESPONDER_KEY]);
    assert_eq!(responder_store.0, [INITIATOR_KEY]);
    assert_eq!(
        initiator.sas_code(),
        None,
        "terminal state discards the code"
    );
}

#[test]
fn reveal_of_a_different_nonce_aborts_the_responder() {
    let now = Instant::now();
    let (mut initiator, mut responder) = sessions();
    let start = initiator.start(INITIATOR_NONCE).unwrap();
    responder
        .receive_start(&start, RESPONDER_NONCE, now, None)
        .unwrap();

    // An attacker that learned the responder nonce tries to open the
    // commitment to a nonce of its choosing.
    let forged = proto::PairReveal {
        nonce: vec![9; 16],
        ..Default::default()
    };
    assert_eq!(
        responder.receive_reveal(&forged, now),
        Err(PairingError::CommitmentMismatch)
    );
    assert_eq!(
        responder.state(),
        PairingState::Aborted(PairingAbortReason::CodeMismatch)
    );
    assert_eq!(responder.sas_code(), None);
}

#[test]
fn commitment_is_bound_to_the_ordered_identities() {
    let now = Instant::now();
    let (_, mut responder) = sessions();
    // The initiator's own nonce, but committed for the wrong responder: a
    // commitment relayed from another session cannot be opened here.
    let relayed = proto::PairStart {
        nonce_commitment: NonceCommitment::commit(
            INITIATOR_KEY,
            PublicKey::new([7; 32]),
            INITIATOR_NONCE,
        )
        .as_bytes()
        .to_vec(),
        ..Default::default()
    };
    responder
        .receive_start(&relayed, RESPONDER_NONCE, now, None)
        .unwrap();
    let reveal = proto::PairReveal {
        nonce: INITIATOR_NONCE.as_bytes().to_vec(),
        ..Default::default()
    };
    assert_eq!(
        responder.receive_reveal(&reveal, now),
        Err(PairingError::CommitmentMismatch)
    );
}

#[test]
fn malformed_fields_are_rejected_without_advancing() {
    let now = Instant::now();
    let (mut initiator, mut responder) = sessions();
    let short_commitment = proto::PairStart {
        nonce_commitment: vec![0; 31],
        ..Default::default()
    };
    assert!(matches!(
        responder.receive_start(&short_commitment, RESPONDER_NONCE, now, None),
        Err(PairingError::InvalidField {
            field: "nonce_commitment",
            ..
        })
    ));
    assert_eq!(responder.state(), PairingState::Idle);

    let start = initiator.start(INITIATOR_NONCE).unwrap();
    let prompted = responder
        .receive_start(&start, RESPONDER_NONCE, now, None)
        .unwrap();
    let short_nonce = proto::PairPrompted {
        nonce: vec![0; 15],
        ..prompted.clone()
    };
    assert!(matches!(
        initiator.receive_prompted(&short_nonce, now),
        Err(PairingError::InvalidField { field: "nonce", .. })
    ));
    assert_eq!(initiator.state(), PairingState::AwaitingPrompt);

    let long_reveal = proto::PairReveal {
        nonce: vec![0; 17],
        ..Default::default()
    };
    assert!(matches!(
        responder.receive_reveal(&long_reveal, now),
        Err(PairingError::InvalidField { field: "nonce", .. })
    ));
    assert!(matches!(
        responder.state(),
        PairingState::AwaitingReveal { .. }
    ));
}

#[test]
fn steps_out_of_order_are_invalid() {
    let now = Instant::now();
    let (mut initiator, mut responder) = sessions();
    let mut store = MemoryStore::default();
    let prompted = proto::PairPrompted {
        timeout_ms: 1_000,
        nonce: RESPONDER_NONCE.as_bytes().to_vec(),
        ..Default::default()
    };
    assert_eq!(
        initiator.receive_prompted(&prompted, now),
        Err(PairingError::InvalidTransition(PairingState::Idle))
    );
    assert_eq!(
        responder.receive_reveal(&proto::PairReveal::default(), now),
        Err(PairingError::InvalidTransition(PairingState::Idle))
    );
    assert_eq!(
        initiator.receive_revealed(now),
        Err(PairingError::InvalidTransition(PairingState::Idle))
    );

    let start = initiator.start(INITIATOR_NONCE).unwrap();
    assert_eq!(
        initiator.start(INITIATOR_NONCE),
        Err(PairingError::InvalidTransition(
            PairingState::AwaitingPrompt
        ))
    );
    let prompted = responder
        .receive_start(&start, RESPONDER_NONCE, now, None)
        .unwrap();
    // Confirming before the commitment opened would let a user approve a
    // code that does not exist yet.
    assert_eq!(
        responder.confirm_local(now, &mut store),
        Err(PairingError::InvalidTransition(
            PairingState::AwaitingReveal {
                timeout_ms: 120_000
            }
        ))
    );
    initiator.receive_prompted(&prompted, now).unwrap();
    assert!(matches!(
        initiator.confirm_local(now, &mut store),
        Err(PairingError::InvalidTransition(
            PairingState::AwaitingRevealed { .. }
        ))
    ));
    assert!(store.0.is_empty());
}

/// The responder confirms as soon as it shows its code; its `PairConfirm`
/// overtakes the `PairRevealed` answer on another stream.
#[test]
fn a_peer_confirm_may_overtake_pair_revealed() {
    let now = Instant::now();
    let (mut initiator, mut responder) = sessions();
    let mut initiator_store = MemoryStore::default();
    let mut responder_store = MemoryStore::default();
    let start = initiator.start(INITIATOR_NONCE).unwrap();
    let prompted = responder
        .receive_start(&start, RESPONDER_NONCE, now, None)
        .unwrap();
    let reveal = initiator.receive_prompted(&prompted, now).unwrap();
    responder.receive_reveal(&reveal, now).unwrap();
    responder.confirm_local(now, &mut responder_store).unwrap();

    let early = initiator
        .receive_confirm(now, &mut initiator_store)
        .unwrap();
    assert_eq!(
        early.result.as_known(),
        Some(proto::PairResult::PendingLocal)
    );
    assert!(matches!(
        initiator.state(),
        PairingState::AwaitingRevealed { .. }
    ));
    assert_eq!(initiator.sas_code(), None, "no code before PairRevealed");
    assert!(matches!(
        initiator.confirm_local(now, &mut initiator_store),
        Err(PairingError::InvalidTransition(_))
    ));
    assert!(initiator_store.0.is_empty());

    initiator.receive_revealed(now).unwrap();
    assert!(matches!(
        initiator.state(),
        PairingState::Prompted {
            local_confirmed: false,
            peer_confirmed: true,
            ..
        }
    ));
    let result = initiator.confirm_local(now, &mut initiator_store).unwrap();
    assert_eq!(result, proto::PairResult::Paired);
    assert_eq!(initiator_store.0, [RESPONDER_KEY]);
}

#[test]
fn reveal_after_the_deadline_times_out() {
    let now = Instant::now();
    let (mut initiator, mut responder) = sessions();
    let start = initiator.start(INITIATOR_NONCE).unwrap();
    let prompted = responder
        .receive_start(&start, RESPONDER_NONCE, now, Some(Duration::from_secs(1)))
        .unwrap();
    let reveal = initiator.receive_prompted(&prompted, now).unwrap();

    let late = now + Duration::from_secs(1);
    assert_eq!(
        responder.receive_reveal(&reveal, late),
        Err(PairingError::InvalidTransition(PairingState::TimedOut))
    );
    assert_eq!(
        initiator.receive_revealed(late),
        Err(PairingError::InvalidTransition(PairingState::TimedOut))
    );
}

#[test]
fn confirmation_at_deadline_times_out_without_persisting() {
    let now = Instant::now();
    let (mut initiator, _) = prompted_pair(now, Some(Duration::from_secs(1)));
    let mut store = MemoryStore::default();

    assert_eq!(
        initiator.confirm_local(now, &mut store).unwrap(),
        proto::PairResult::PendingLocal
    );
    assert_eq!(
        initiator
            .receive_confirm(now + Duration::from_secs(1), &mut store)
            .unwrap()
            .result
            .as_known(),
        Some(proto::PairResult::Timeout)
    );
    assert_eq!(initiator.state(), PairingState::TimedOut);
    assert!(store.0.is_empty());
    assert_eq!(initiator.sas_code(), None);
}

#[test]
fn timeout_covers_every_waiting_phase_and_uses_the_default() {
    let now = Instant::now();
    let (mut initiator, mut responder) = sessions();
    assert_eq!(
        initiator.check_timeout(now + Duration::from_secs(999)),
        None
    );

    let start = initiator.start(INITIATOR_NONCE).unwrap();
    assert_eq!(
        initiator.check_timeout(now + Duration::from_secs(999)),
        None,
        "no deadline runs before the responder declares one"
    );
    let prompted = responder
        .receive_start(&start, RESPONDER_NONCE, now, None)
        .unwrap();
    assert_eq!(prompted.timeout_ms, 120_000);
    initiator.receive_prompted(&prompted, now).unwrap();

    for session in [&mut initiator, &mut responder] {
        assert_eq!(session.check_timeout(now + Duration::from_secs(119)), None);
        assert_eq!(
            session
                .check_timeout(now + Duration::from_secs(120))
                .unwrap()
                .result
                .as_known(),
            Some(proto::PairResult::Timeout)
        );
        assert_eq!(session.state(), PairingState::TimedOut);
    }

    let (mut prompted_initiator, _) = prompted_pair(now, None);
    assert!(
        prompted_initiator
            .check_timeout(now + Duration::from_secs(120))
            .is_some()
    );
    assert_eq!(prompted_initiator.sas_code(), None);
}

#[test]
fn zero_timeout_from_the_responder_means_default() {
    let now = Instant::now();
    let (mut initiator, _) = sessions();
    initiator.start(INITIATOR_NONCE).unwrap();
    initiator
        .receive_prompted(
            &proto::PairPrompted {
                timeout_ms: 0,
                nonce: RESPONDER_NONCE.as_bytes().to_vec(),
                ..Default::default()
            },
            now,
        )
        .unwrap();
    assert_eq!(
        initiator.state(),
        PairingState::AwaitingRevealed {
            timeout_ms: 120_000
        }
    );
}

#[test]
fn abort_is_legal_in_every_active_phase_and_terminal() {
    let now = Instant::now();
    for reason in [
        PairingAbortReason::UserCancelled,
        PairingAbortReason::CodeMismatch,
        PairingAbortReason::Timeout,
    ] {
        let (mut initiator, mut responder) = sessions();
        let start = initiator.start(INITIATOR_NONCE).unwrap();
        initiator.clone().abort(reason).unwrap();
        responder
            .receive_start(&start, RESPONDER_NONCE, now, None)
            .unwrap();
        let abort = responder.abort(reason).unwrap();
        assert_eq!(responder.state(), PairingState::Aborted(reason));
        assert_eq!(
            responder.abort(reason),
            Err(PairingError::InvalidTransition(PairingState::Aborted(
                reason
            )))
        );

        initiator.receive_abort(&abort).unwrap();
        assert_eq!(initiator.state(), PairingState::Aborted(reason));

        let (mut prompted, _) = prompted_pair(now, None);
        prompted.abort(reason).unwrap();
        assert_eq!(prompted.sas_code(), None);
    }
}

#[test]
fn unspecified_abort_reason_is_invalid() {
    let now = Instant::now();
    let (mut initiator, _) = prompted_pair(now, None);
    assert_eq!(
        initiator.receive_abort(&proto::PairAbort::default()),
        Err(PairingError::InvalidEnum(0))
    );
    assert!(matches!(initiator.state(), PairingState::Prompted { .. }));
}

#[test]
fn outcomes_cannot_start_or_overwrite_a_session() {
    let now = Instant::now();
    let (mut initiator, _) = sessions();
    for result in [
        proto::PairResult::PendingLocal,
        proto::PairResult::Rejected,
        proto::PairResult::Timeout,
    ] {
        assert_eq!(
            initiator.receive_outcome(&pair_outcome(result)),
            Err(PairingError::InvalidTransition(PairingState::Idle))
        );
    }
    assert_eq!(
        initiator.receive_outcome(&pair_outcome(proto::PairResult::Unspecified)),
        Err(PairingError::InvalidEnum(0))
    );

    let (mut prompted, _) = prompted_pair(now, None);
    prompted.reject().unwrap();
    assert_eq!(
        prompted.receive_outcome(&pair_outcome(proto::PairResult::Timeout)),
        Err(PairingError::InvalidTransition(PairingState::Rejected))
    );
}

/// Both users confirm at once: the responder answers the initiator's confirm
/// with `PAIRED` before the responder's own confirm reaches the initiator.
#[test]
fn a_paired_outcome_may_overtake_the_peer_confirm() {
    let now = Instant::now();
    let (mut initiator, mut responder) = prompted_pair(now, None);
    let mut initiator_store = MemoryStore::default();
    let mut responder_store = MemoryStore::default();
    initiator.confirm_local(now, &mut initiator_store).unwrap();
    responder.confirm_local(now, &mut responder_store).unwrap();
    let outcome = responder
        .receive_confirm(now, &mut responder_store)
        .unwrap();
    assert_eq!(outcome.result.as_known(), Some(proto::PairResult::Paired));

    initiator.receive_outcome(&outcome).unwrap();
    assert!(
        matches!(initiator.state(), PairingState::Prompted { .. }),
        "completion waits for the peer's own confirm"
    );
    assert!(initiator_store.0.is_empty());

    let late = initiator
        .receive_confirm(now, &mut initiator_store)
        .unwrap();
    assert_eq!(late.result.as_known(), Some(proto::PairResult::Paired));
    assert_eq!(initiator.state(), PairingState::Paired);
    assert_eq!(initiator_store.0, [RESPONDER_KEY]);
}

/// The peer confirms while its `PENDING_LOCAL` answer to the initiator's
/// confirm is still in flight; the late answer must not undo the pairing.
#[test]
fn a_pending_outcome_may_arrive_after_pairing() {
    let now = Instant::now();
    let (mut initiator, mut responder) = prompted_pair(now, None);
    let mut initiator_store = MemoryStore::default();
    let mut responder_store = MemoryStore::default();
    initiator.confirm_local(now, &mut initiator_store).unwrap();
    let pending = responder
        .receive_confirm(now, &mut responder_store)
        .unwrap();
    responder.confirm_local(now, &mut responder_store).unwrap();
    initiator
        .receive_confirm(now, &mut initiator_store)
        .unwrap();
    assert_eq!(initiator.state(), PairingState::Paired);

    initiator.receive_outcome(&pending).unwrap();
    assert_eq!(initiator.state(), PairingState::Paired);
    assert_eq!(initiator_store.0, [RESPONDER_KEY]);
}

#[test]
fn a_paired_outcome_before_the_local_confirm_is_invalid() {
    let now = Instant::now();
    let (mut initiator, _) = prompted_pair(now, None);
    assert!(matches!(
        initiator.receive_outcome(&pair_outcome(proto::PairResult::Paired)),
        Err(PairingError::InvalidTransition(PairingState::Prompted {
            local_confirmed: false,
            ..
        }))
    ));
}

#[test]
fn repeated_peer_confirm_replays_terminal_outcome() {
    let now = Instant::now();
    let (mut responder_side, _) = prompted_pair(now, None);
    let mut store = MemoryStore::default();
    responder_side.reject().unwrap();
    for _ in 0..2 {
        assert_eq!(
            responder_side
                .receive_confirm(now, &mut store)
                .unwrap()
                .result
                .as_known(),
            Some(proto::PairResult::Rejected)
        );
    }
    assert!(store.0.is_empty());
}

#[test]
fn persistence_failure_keeps_the_ceremony_unpaired() {
    let now = Instant::now();
    let (mut initiator, _) = prompted_pair(now, None);
    initiator.confirm_local(now, &mut FailingStore).unwrap();
    assert_eq!(
        initiator.receive_confirm(now, &mut FailingStore),
        Err(PairingError::Persistence(PersistPeerKeyError::new(
            "disk full"
        )))
    );
    assert_ne!(initiator.state(), PairingState::Paired);
}

/// Runs a ceremony between two arbitrary sessions and returns both codes.
fn codes(mut initiator: PairingSession, mut responder: PairingSession) -> (SasCode, SasCode) {
    let now = Instant::now();
    let start = initiator.start(INITIATOR_NONCE).unwrap();
    let prompted = responder
        .receive_start(&start, RESPONDER_NONCE, now, None)
        .unwrap();
    let reveal = initiator.receive_prompted(&prompted, now).unwrap();
    responder.receive_reveal(&reveal, now).unwrap();
    initiator.receive_revealed(now).unwrap();
    (initiator.sas_code().unwrap(), responder.sas_code().unwrap())
}

#[test]
fn tampered_hello_gives_the_two_sides_different_codes() {
    // An attacker that terminates TLS can rewrite a Hello, for example to
    // lower proto_max or strip a capability. Each side then hashes a
    // different transcript.
    let honest = HelloDigest::of(b"initiator hello: proto_max=2");
    let downgraded = HelloDigest::of(b"initiator hello: proto_max=1");
    let responder_hello = HelloDigest::of(b"responder hello");
    let (initiator_code, responder_code) = codes(
        PairingSession::new(INITIATOR_KEY, RESPONDER_KEY, honest, responder_hello),
        PairingSession::new(RESPONDER_KEY, INITIATOR_KEY, responder_hello, downgraded),
    );
    assert_ne!(initiator_code, responder_code);
}

#[test]
fn relaying_attacker_cannot_reuse_nonces_to_match_codes() {
    // A man in the middle runs A→M and M→B with its own key. Its best
    // nonce-only strategy is to replay the same nonces on both legs; the
    // codes still differ because each leg binds a different key pair.
    let attacker = PublicKey::new([9; 32]);
    let hello_a = HelloDigest::of(b"a");
    let hello_m = HelloDigest::of(b"m");
    let hello_b = HelloDigest::of(b"b");
    let (code_at_a, _) = codes(
        PairingSession::new(INITIATOR_KEY, attacker, hello_a, hello_m),
        PairingSession::new(attacker, INITIATOR_KEY, hello_m, hello_a),
    );
    let (_, code_at_b) = codes(
        PairingSession::new(attacker, RESPONDER_KEY, hello_m, hello_b),
        PairingSession::new(RESPONDER_KEY, attacker, hello_b, hello_m),
    );
    assert_ne!(code_at_a, code_at_b);
}
