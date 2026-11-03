#![expect(
    clippy::tests_outside_test_module,
    reason = "Cargo integration tests are test-only crates"
)]

use std::{
    error::Error as StdError,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use openlogi_flow::{
    frame::{Envelope, FrameKind, MAX_PAYLOAD_LEN},
    generated as proto,
    pairing::{PairingState, PeerKeyStore, PersistPeerKeyError},
    sas::{PairingNonce, PublicKey},
    transport::{
        BulkRpcResponse, EndpointLimits, FlowConnection, FlowEndpoint, MachineIdentity,
        NotificationEvent, PeerTrust, RpcEvent, SessionTrust, TransportError, message_envelope,
    },
};
use tokio::task::JoinSet;

type TestResult<T = ()> = Result<T, Box<dyn StdError + Send + Sync>>;

struct ConnectedPair {
    first_endpoint: Arc<FlowEndpoint>,
    second_endpoint: Arc<FlowEndpoint>,
    first: FlowConnection,
    second: FlowConnection,
}

impl ConnectedPair {
    async fn trusted(first_range: (u32, u32), second_range: (u32, u32)) -> TestResult<Self> {
        Self::connect(
            first_range,
            second_range,
            false,
            &[],
            EndpointLimits::default(),
        )
        .await
    }

    async fn pairing() -> TestResult<Self> {
        Self::connect((1, 1), (1, 1), true, &[], EndpointLimits::default()).await
    }

    async fn trusted_clipboard() -> TestResult<Self> {
        Self::connect(
            (1, 1),
            (1, 1),
            false,
            &[proto::Capability::ClipboardText],
            EndpointLimits::default(),
        )
        .await
    }

    async fn trusted_with_limits(limits: EndpointLimits) -> TestResult<Self> {
        Self::connect((1, 1), (1, 1), false, &[], limits).await
    }

    async fn connect(
        first_range: (u32, u32),
        second_range: (u32, u32),
        pairing: bool,
        capabilities: &[proto::Capability],
        limits: EndpointLimits,
    ) -> TestResult<Self> {
        let first_identity = MachineIdentity::generate()?;
        let second_identity = MachineIdentity::generate()?;
        let first_trust = if pairing {
            PeerTrust::pairing([])
        } else {
            PeerTrust::pinned([second_identity.public_key()])
        };
        let second_trust = if pairing {
            PeerTrust::pairing([])
        } else {
            PeerTrust::pinned([first_identity.public_key()])
        };
        let first = Arc::new(FlowEndpoint::bind_with_limits(
            localhost(),
            first_identity.clone(),
            first_trust,
            hello_with_capabilities(&first_identity, first_range, capabilities),
            limits,
        )?);
        let second = Arc::new(FlowEndpoint::bind_with_limits(
            localhost(),
            second_identity.clone(),
            second_trust,
            hello_with_capabilities(&second_identity, second_range, capabilities),
            limits,
        )?);
        let (first_connection, second_connection) = dial(&first, &second).await?;
        Ok(Self {
            first_endpoint: first,
            second_endpoint: second,
            first: first_connection,
            second: second_connection,
        })
    }

    /// Opens another connection between the same two endpoints.
    async fn reconnect(&self) -> TestResult<(FlowConnection, FlowConnection)> {
        dial(&self.first_endpoint, &self.second_endpoint).await
    }
}

async fn dial(
    first: &Arc<FlowEndpoint>,
    second: &Arc<FlowEndpoint>,
) -> TestResult<(FlowConnection, FlowConnection)> {
    let second_acceptor = Arc::clone(second);
    let accept = tokio::spawn(async move { second_acceptor.accept().await });
    let first_connection = first.connect(second.local_addr()?).await?;
    let second_connection = accept.await??;
    Ok((first_connection, second_connection))
}

#[derive(Default)]
struct MemoryKeyStore(Vec<PublicKey>);

impl PeerKeyStore for MemoryKeyStore {
    fn persist_peer_key(&mut self, key: PublicKey) -> Result<(), PersistPeerKeyError> {
        self.0.push(key);
        Ok(())
    }
}

#[tokio::test]
async fn hello_negotiates_over_mutually_pinned_quic() -> TestResult {
    let pair = ConnectedPair::trusted((1, 3), (2, 4)).await?;
    assert_eq!(pair.first.negotiated().version, 3);
    assert_eq!(pair.second.negotiated().version, 3);
    assert_eq!(pair.first.trust(), SessionTrust::Trusted);
    assert_eq!(pair.second.trust(), SessionTrust::Trusted);
    assert_eq!(pair.first.peer_key(), pair.first.peer_hello_key()?);
    Ok(())
}

#[tokio::test]
async fn an_ipv6_wildcard_endpoint_takes_ipv4_and_ipv6_peers() -> TestResult {
    let listener_identity = MachineIdentity::generate()?;
    let v4_identity = MachineIdentity::generate()?;
    let v6_identity = MachineIdentity::generate()?;
    let listener = Arc::new(FlowEndpoint::bind(
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
        listener_identity.clone(),
        PeerTrust::pinned([v4_identity.public_key(), v6_identity.public_key()]),
        hello(&listener_identity, (1, 1)),
    )?);
    let port = listener.local_addr()?.port();
    for (identity, local, target) in [
        (
            &v4_identity,
            localhost(),
            SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        ),
        (
            &v6_identity,
            SocketAddr::from((Ipv6Addr::LOCALHOST, 0)),
            SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
        ),
    ] {
        let dialer = FlowEndpoint::bind(
            local,
            identity.clone(),
            PeerTrust::pinned([listener_identity.public_key()]),
            hello(identity, (1, 1)),
        )?;
        let acceptor = Arc::clone(&listener);
        let accept = tokio::spawn(async move { acceptor.accept().await });
        let connection = dialer.connect(target).await?;
        accept.await??;
        assert_eq!(connection.trust(), SessionTrust::Trusted);

        // The same socket dials each family back.
        let dialer = Arc::new(dialer);
        let peer_address = dialer.local_addr()?;
        let accept = tokio::spawn(async move { dialer.accept().await });
        let connection = listener.connect(peer_address).await?;
        accept.await??;
        assert_eq!(connection.trust(), SessionTrust::Trusted);
    }
    Ok(())
}

#[tokio::test]
async fn wrong_server_pin_fails_tls_authentication() -> TestResult {
    assert_pin_failure(false, true).await
}

#[tokio::test]
async fn wrong_client_pin_fails_tls_authentication() -> TestResult {
    assert_pin_failure(true, false).await
}

#[tokio::test]
async fn version_disjoint_hello_is_rejected() -> TestResult {
    let first_identity = MachineIdentity::generate()?;
    let second_identity = MachineIdentity::generate()?;
    let first = Arc::new(FlowEndpoint::bind(
        localhost(),
        first_identity.clone(),
        PeerTrust::pinned([second_identity.public_key()]),
        hello(&first_identity, (1, 1)),
    )?);
    let second = Arc::new(FlowEndpoint::bind(
        localhost(),
        second_identity.clone(),
        PeerTrust::pinned([first_identity.public_key()]),
        hello(&second_identity, (2, 2)),
    )?);
    let second_acceptor = Arc::clone(&second);
    let accepted = tokio::spawn(async move { second_acceptor.accept().await });

    let dialed = first.connect(second.local_addr()?).await;
    dialed.unwrap_err();
    accepted.await?.unwrap_err();
    Ok(())
}

#[tokio::test]
async fn untrusted_session_rejects_non_pairing_rpc() -> TestResult {
    let pair = ConnectedPair::pairing().await?;
    assert_eq!(pair.first.trust(), SessionTrust::Untrusted);
    assert_eq!(pair.second.trust(), SessionTrust::Untrusted);

    let request = message_envelope(FrameKind::GetPeerInfo, &proto::GetPeerInfo::default())?;
    assert!(matches!(
        pair.first.call(request.clone()).await,
        Err(TransportError::NotPaired(FrameKind::GetPeerInfo))
    ));
    let client = async {
        let (mut send, mut receive) = pair.first.open_rpc_stream().await?;
        request.write_to(&mut send).await?;
        send.finish()?;
        let response = Envelope::read_from(&mut receive).await?;
        TestResult::Ok(response)
    };
    let (accepted, response) = tokio::join!(pair.second.accept_rpc(), client);
    assert!(matches!(
        accepted?,
        RpcEvent::Rejected(proto::ErrorCode::NotPaired)
    ));
    let response = response?;
    assert_eq!(response.kind, FrameKind::Error);
    assert_eq!(
        decode::<proto::Error>(&response)?.code.as_known(),
        Some(proto::ErrorCode::NotPaired)
    );
    Ok(())
}

#[tokio::test]
async fn malformed_pairing_request_is_answered_invalid() -> TestResult {
    let pair = ConnectedPair::pairing().await?;
    let client = async {
        let (mut send, mut receive) = pair.first.open_rpc_stream().await?;
        Envelope::new(FrameKind::PairStart, 0, vec![0xff])?
            .write_to(&mut send)
            .await?;
        send.finish()?;
        let response = Envelope::read_from(&mut receive).await?;
        TestResult::Ok(response)
    };
    let (accepted, response) = tokio::join!(pair.second.accept_rpc(), client);
    assert!(matches!(
        accepted?,
        RpcEvent::Rejected(proto::ErrorCode::Invalid)
    ));
    let error = decode::<proto::Error>(&response?)?;
    assert_eq!(error.code.as_known(), Some(proto::ErrorCode::Invalid));
    Ok(())
}

#[tokio::test]
async fn full_pairing_ceremony_agrees_on_sas_and_promotes_trust() -> TestResult {
    let pair = ConnectedPair::pairing().await?;
    let mut first_session = pair.first.pairing_session();
    let mut second_session = pair.second.pairing_session();

    let start = message_envelope(
        FrameKind::PairStart,
        &first_session.start(PairingNonce::random()?)?,
    )?;
    let server_side = async {
        let RpcEvent::Request(request) = pair.second.accept_rpc().await? else {
            return Err("PairStart was rejected".into());
        };
        let start = decode::<proto::PairStart>(request.request())?;
        let prompted =
            second_session.receive_start(&start, PairingNonce::random()?, Instant::now(), None)?;
        request
            .respond(message_envelope(FrameKind::PairPrompted, &prompted)?)
            .await?;
        TestResult::Ok(())
    };
    let (server_result, prompted) = tokio::join!(server_side, pair.first.call(start));
    server_result?;
    let reveal = first_session
        .receive_prompted(&decode::<proto::PairPrompted>(&prompted?)?, Instant::now())?;
    assert_eq!(first_session.sas_code(), None);
    assert_eq!(second_session.sas_code(), None);

    let reveal = message_envelope(FrameKind::PairReveal, &reveal)?;
    let server_side = async {
        let RpcEvent::Request(request) = pair.second.accept_rpc().await? else {
            return Err("PairReveal was rejected".into());
        };
        let reveal = decode::<proto::PairReveal>(request.request())?;
        let revealed = second_session.receive_reveal(&reveal, Instant::now())?;
        request
            .respond(message_envelope(FrameKind::PairRevealed, &revealed)?)
            .await?;
        TestResult::Ok(())
    };
    let (server_result, revealed) = tokio::join!(server_side, pair.first.call(reveal));
    server_result?;
    assert_eq!(revealed?.kind, FrameKind::PairRevealed);
    first_session.receive_revealed(Instant::now())?;
    let code = first_session.sas_code().ok_or("initiator has no code")?;
    assert_eq!(second_session.sas_code(), Some(code));

    let mut first_store = MemoryKeyStore::default();
    let mut second_store = MemoryKeyStore::default();
    assert_eq!(
        first_session.confirm_local(Instant::now(), &mut first_store)?,
        proto::PairResult::PendingLocal
    );
    let first_confirm = message_envelope(FrameKind::PairConfirm, &proto::PairConfirm::default())?;
    let server_side = async {
        let RpcEvent::Request(request) = pair.second.accept_rpc().await? else {
            return Err("first PairConfirm was rejected".into());
        };
        let outcome = second_session.receive_confirm(Instant::now(), &mut second_store)?;
        request
            .respond(message_envelope(FrameKind::PairOutcome, &outcome)?)
            .await?;
        TestResult::Ok(())
    };
    let (server_result, outcome) = tokio::join!(server_side, pair.first.call(first_confirm));
    server_result?;
    first_session.receive_outcome(&decode::<proto::PairOutcome>(&outcome?)?)?;

    assert_eq!(
        second_session.confirm_local(Instant::now(), &mut second_store)?,
        proto::PairResult::Paired
    );
    let second_confirm = message_envelope(FrameKind::PairConfirm, &proto::PairConfirm::default())?;
    let server_side = async {
        let RpcEvent::Request(request) = pair.first.accept_rpc().await? else {
            return Err("second PairConfirm was rejected".into());
        };
        let outcome = first_session.receive_confirm(Instant::now(), &mut first_store)?;
        request
            .respond(message_envelope(FrameKind::PairOutcome, &outcome)?)
            .await?;
        TestResult::Ok(())
    };
    let (server_result, outcome) = tokio::join!(server_side, pair.second.call(second_confirm));
    server_result?;
    second_session.receive_outcome(&decode::<proto::PairOutcome>(&outcome?)?)?;

    pair.first.promote_after_pairing(&first_session)?;
    pair.second.promote_after_pairing(&second_session)?;
    assert_eq!(pair.first.trust(), SessionTrust::Trusted);
    assert_eq!(pair.second.trust(), SessionTrust::Trusted);
    assert_eq!(first_store.0, [pair.first.peer_key()]);
    assert_eq!(second_store.0, [pair.second.peer_key()]);
    Ok(())
}

#[tokio::test]
async fn hello_nonce_is_fresh_on_every_connection() -> TestResult {
    let pair = ConnectedPair::trusted((1, 1), (1, 1)).await?;
    let (first_again, second_again) = pair.reconnect().await?;

    let nonces = [
        &pair.first.peer_hello().session_nonce,
        &pair.second.peer_hello().session_nonce,
        &first_again.peer_hello().session_nonce,
        &second_again.peer_hello().session_nonce,
    ];
    for (index, nonce) in nonces.iter().enumerate() {
        assert_eq!(nonce.len(), 16);
        for other in &nonces[index + 1..] {
            assert_ne!(nonce, other, "two connections reused a Hello nonce");
        }
    }
    Ok(())
}

#[tokio::test]
async fn bind_rejects_a_caller_supplied_session_nonce() -> TestResult {
    let identity = MachineIdentity::generate()?;
    let mut fixed_nonce = hello(&identity, (1, 1));
    fixed_nonce.session_nonce = vec![7; 16];
    assert!(matches!(
        FlowEndpoint::bind(localhost(), identity, PeerTrust::pinned([]), fixed_nonce),
        Err(TransportError::InvalidLocalHello(_))
    ));
    Ok(())
}

#[tokio::test]
async fn ceremony_from_an_earlier_connection_cannot_promote_a_new_one() -> TestResult {
    let pair = ConnectedPair::pairing().await?;
    let (first_session, second_session) = pair_in_memory(&pair.first, &pair.second)?;
    assert_eq!(first_session.state(), PairingState::Paired);

    let ConnectedPair {
        first_endpoint,
        second_endpoint,
        first,
        second,
    } = pair;
    drop((first, second));
    let (first_again, second_again) = dial(&first_endpoint, &second_endpoint).await?;

    assert!(matches!(
        first_again.promote_after_pairing(&first_session),
        Err(TransportError::PairingNotComplete)
    ));
    assert!(matches!(
        second_again.promote_after_pairing(&second_session),
        Err(TransportError::PairingNotComplete)
    ));
    assert_eq!(first_again.trust(), SessionTrust::Untrusted);
    Ok(())
}

#[tokio::test]
async fn pairing_slot_is_claimed_only_by_completed_handshakes() -> TestResult {
    let pairing_identity = MachineIdentity::generate()?;
    let pairing_trust = PeerTrust::pairing([]);
    let acceptor = Arc::new(FlowEndpoint::bind(
        localhost(),
        pairing_identity.clone(),
        pairing_trust.clone(),
        hello(&pairing_identity, (1, 1)),
    )?);
    let candidate = |range| -> TestResult<Arc<FlowEndpoint>> {
        let identity = MachineIdentity::generate()?;
        Ok(Arc::new(FlowEndpoint::bind(
            localhost(),
            identity.clone(),
            PeerTrust::pairing([]),
            hello(&identity, range),
        )?))
    };

    // TLS succeeds but Hello is rejected: the dial must not keep the slot.
    let stale = candidate((2, 2))?;
    dial(&stale, &acceptor).await.unwrap_err();
    assert_eq!(pairing_trust.pairing_candidate(), None);

    let first = candidate((1, 1))?;
    let (first_side, accepted) = dial(&first, &acceptor).await?;
    assert_eq!(pairing_trust.pairing_candidate(), Some(first.public_key()));

    // A second unknown peer is refused while the first one holds the slot.
    let second = candidate((1, 1))?;
    let refused = {
        let acceptor = Arc::clone(&acceptor);
        tokio::spawn(async move { acceptor.accept().await })
    };
    let _ = second.connect(acceptor.local_addr()?).await;
    refused.await?.unwrap_err();
    assert_eq!(pairing_trust.pairing_candidate(), Some(first.public_key()));

    drop((first_side, accepted));
    assert_eq!(pairing_trust.pairing_candidate(), None);
    let (_second_side, _accepted) = dial(&second, &acceptor).await?;
    assert_eq!(pairing_trust.pairing_candidate(), Some(second.public_key()));
    Ok(())
}

#[tokio::test]
async fn promotion_frees_the_pairing_slot_and_keeps_the_connection() -> TestResult {
    let pairing_identity = MachineIdentity::generate()?;
    let pairing_trust = PeerTrust::pairing([]);
    let acceptor = Arc::new(FlowEndpoint::bind(
        localhost(),
        pairing_identity.clone(),
        pairing_trust.clone(),
        hello(&pairing_identity, (1, 1)),
    )?);
    let candidate = || -> TestResult<Arc<FlowEndpoint>> {
        let identity = MachineIdentity::generate()?;
        Ok(Arc::new(FlowEndpoint::bind(
            localhost(),
            identity.clone(),
            PeerTrust::pairing([]),
            hello(&identity, (1, 1)),
        )?))
    };

    let first = candidate()?;
    let (first_side, accepted) = dial(&first, &acceptor).await?;
    let (first_session, accepted_session) = pair_in_memory(&first_side, &accepted)?;
    first_side.promote_after_pairing(&first_session)?;
    accepted.promote_after_pairing(&accepted_session)?;
    assert_eq!(accepted.trust(), SessionTrust::Trusted);
    assert_eq!(pairing_trust.pairing_candidate(), None);

    let second = candidate()?;
    let (_second_side, _second_accepted) = dial(&second, &acceptor).await?;
    assert_eq!(pairing_trust.pairing_candidate(), Some(second.public_key()));
    assert_eq!(accepted.trust(), SessionTrust::Trusted);
    first_side.send_datagram(&message_envelope(FrameKind::Ping, &proto::Ping::default())?)?;
    let ping = tokio::time::timeout(Duration::from_secs(5), accepted.read_datagram()).await??;
    assert_eq!(ping.kind, FrameKind::Ping);
    Ok(())
}

#[tokio::test]
async fn rpc_stream_that_never_finishes_times_out() -> TestResult {
    let pair = ConnectedPair::trusted_with_limits(EndpointLimits {
        stream_read_timeout: Duration::from_millis(200),
        ..EndpointLimits::default()
    })
    .await?;
    let frame =
        message_envelope(FrameKind::GetPeerInfo, &proto::GetPeerInfo::default())?.to_bytes()?;

    // Half a header, then silence.
    let (mut rpc_send, _rpc_receive) = pair.first.open_rpc_stream().await?;
    rpc_send.write_all(&frame[..4]).await?;
    let started = Instant::now();
    assert!(matches!(
        pair.second.accept_rpc().await,
        Err(TransportError::StreamTimeout)
    ));
    assert!(started.elapsed() < Duration::from_secs(5));
    Ok(())
}

#[tokio::test]
async fn select_loop_over_stream_accepts_loses_no_request() -> TestResult {
    let pair = ConnectedPair::trusted((1, 1), (1, 1)).await?;
    let request =
        message_envelope(FrameKind::GetPeerInfo, &proto::GetPeerInfo::default())?.to_bytes()?;
    let client = async {
        // The request arrives in two parts with a notification in between, so
        // a loop that read inside `select!` would drop the half-read request.
        let (mut send, mut receive) = pair.first.open_rpc_stream().await?;
        send.write_all(&request[..4]).await?;
        pair.first
            .notify(message_envelope(
                FrameKind::PeerState,
                &proto::PeerState::default(),
            )?)
            .await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        send.write_all(&request[4..]).await?;
        send.finish()?;
        TestResult::Ok(Envelope::read_from(&mut receive).await?)
    };
    let server = async {
        let mut reads = JoinSet::new();
        let (mut notified, mut answered) = (false, false);
        while !(notified && answered) {
            tokio::select! {
                rpc = pair.second.accept_rpc_stream() => {
                    reads.spawn(rpc?.read());
                }
                notification = pair.second.accept_notification_stream() => {
                    let NotificationEvent::Notification(frame) = notification?.read().await? else {
                        return Err("PeerState was dropped".into());
                    };
                    assert_eq!(frame.kind, FrameKind::PeerState);
                    notified = true;
                }
                Some(read) = reads.join_next(), if !reads.is_empty() => {
                    let RpcEvent::Request(request) = read?? else {
                        return Err("GetPeerInfo was rejected".into());
                    };
                    request
                        .respond(message_envelope(FrameKind::PeerInfo, &proto::PeerInfo::default())?)
                        .await?;
                    answered = true;
                }
            }
        }
        TestResult::Ok(())
    };
    let (server, response) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(server, client)
    })
    .await?;
    server?;
    assert_eq!(response?.kind, FrameKind::PeerInfo);
    Ok(())
}

/// Runs a whole ceremony on two connections' state machines without I/O.
fn pair_in_memory(
    first: &FlowConnection,
    second: &FlowConnection,
) -> TestResult<(
    openlogi_flow::pairing::PairingSession,
    openlogi_flow::pairing::PairingSession,
)> {
    let now = Instant::now();
    let mut initiator = first.pairing_session();
    let mut responder = second.pairing_session();
    let start = initiator.start(PairingNonce::random()?)?;
    let prompted = responder.receive_start(&start, PairingNonce::random()?, now, None)?;
    let reveal = initiator.receive_prompted(&prompted, now)?;
    responder.receive_reveal(&reveal, now)?;
    initiator.receive_revealed(now)?;
    let (mut first_store, mut second_store) =
        (MemoryKeyStore::default(), MemoryKeyStore::default());
    initiator.confirm_local(now, &mut first_store)?;
    responder.receive_confirm(now, &mut second_store)?;
    responder.confirm_local(now, &mut second_store)?;
    initiator.receive_confirm(now, &mut first_store)?;
    Ok((initiator, responder))
}

#[tokio::test]
async fn trusted_get_peer_info_rpc_round_trips() -> TestResult {
    let pair = ConnectedPair::trusted((1, 1), (1, 1)).await?;
    let request = message_envelope(FrameKind::GetPeerInfo, &proto::GetPeerInfo::default())?;
    let server_side = async {
        let RpcEvent::Request(request) = pair.second.accept_rpc().await? else {
            return Err("GetPeerInfo was rejected".into());
        };
        request
            .request()
            .decode::<proto::GetPeerInfo>(openlogi_flow::frame::InboundRole::Request)
            .map_err(|_| "GetPeerInfo payload was invalid")?;
        request
            .respond(message_envelope(
                FrameKind::PeerInfo,
                &proto::PeerInfo {
                    machine_name: "peer-b".to_owned(),
                    revision: 7,
                    ..Default::default()
                },
            )?)
            .await?;
        TestResult::Ok(())
    };
    let (server_result, response) = tokio::join!(server_side, pair.first.call(request));
    server_result?;
    let response = decode::<proto::PeerInfo>(&response?)?;
    assert_eq!(response.machine_name, "peer-b");
    assert_eq!(response.revision, 7);
    Ok(())
}

#[tokio::test]
async fn bulk_rpc_streams_head_chunks_and_terminal_frame() -> TestResult {
    let pair = ConnectedPair::trusted_clipboard().await?;
    let request = message_envelope(
        FrameKind::ClipboardFetch,
        &proto::ClipboardFetch {
            sequence: 7,
            mime: "text/plain".to_owned(),
            ..Default::default()
        },
    )?;
    let server_side = async {
        let RpcEvent::Request(request) = pair.second.accept_rpc().await? else {
            return Err("ClipboardFetch was rejected".into());
        };
        request
            .respond_bulk(
                message_envelope(
                    FrameKind::ClipboardData,
                    &proto::ClipboardData {
                        sequence: 7,
                        mime: "text/plain".to_owned(),
                        total_size: 6,
                        ..Default::default()
                    },
                )?,
                [
                    message_envelope(
                        FrameKind::Chunk,
                        &proto::Chunk {
                            data: b"abc".to_vec(),
                            ..Default::default()
                        },
                    )?,
                    message_envelope(
                        FrameKind::Chunk,
                        &proto::Chunk {
                            data: b"def".to_vec(),
                            ..Default::default()
                        },
                    )?,
                ],
                message_envelope(FrameKind::ChunkEnd, &proto::ChunkEnd::default())?,
            )
            .await?;
        TestResult::Ok(())
    };
    let client_side = async {
        let BulkRpcResponse::Data(mut response) = pair.first.call_bulk(request).await? else {
            return Err("bulk RPC returned Error".into());
        };
        assert_eq!(
            decode::<proto::ClipboardData>(response.head())?.total_size,
            6
        );
        let first = response
            .next_frame()
            .await?
            .ok_or("bulk response ended before its first chunk")?;
        let second = response
            .next_frame()
            .await?
            .ok_or("bulk response ended before its second chunk")?;
        let end = response
            .next_frame()
            .await?
            .ok_or("bulk response omitted ChunkEnd")?;
        assert_eq!(decode::<proto::Chunk>(&first)?.data, b"abc");
        assert_eq!(decode::<proto::Chunk>(&second)?.data, b"def");
        assert_eq!(end.kind, FrameKind::ChunkEnd);
        assert_eq!(response.next_frame().await?, None);
        TestResult::Ok(())
    };
    let (server_result, client_result) = tokio::join!(server_side, client_side);
    server_result?;
    client_result?;
    Ok(())
}

#[tokio::test]
async fn oversized_rpc_frame_is_answered_too_large() -> TestResult {
    let pair = ConnectedPair::trusted((1, 1), (1, 1)).await?;
    let client = async {
        let (mut send, mut receive) = pair.first.open_rpc_stream().await?;
        let mut header = [0_u8; 8];
        header[..2].copy_from_slice(&FrameKind::GetPeerInfo.wire_value().to_le_bytes());
        header[4..].copy_from_slice(
            &u32::try_from(MAX_PAYLOAD_LEN + 1)
                .expect("test length fits u32")
                .to_le_bytes(),
        );
        send.write_all(&header).await?;
        send.finish()?;
        let response = Envelope::read_from(&mut receive).await?;
        TestResult::Ok(response)
    };
    let (accepted, response) = tokio::join!(pair.second.accept_rpc(), client);
    assert!(matches!(
        accepted?,
        RpcEvent::Rejected(proto::ErrorCode::TooLarge)
    ));
    let error = decode::<proto::Error>(&response?)?;
    assert_eq!(error.code.as_known(), Some(proto::ErrorCode::TooLarge));
    Ok(())
}

fn localhost() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
}

async fn assert_pin_failure(
    server_pin_is_correct: bool,
    client_pin_is_correct: bool,
) -> TestResult {
    let client_identity = MachineIdentity::generate()?;
    let server_identity = MachineIdentity::generate()?;
    let unrelated_identity = MachineIdentity::generate()?;
    let server_pin = if server_pin_is_correct {
        server_identity.public_key()
    } else {
        unrelated_identity.public_key()
    };
    let client_pin = if client_pin_is_correct {
        client_identity.public_key()
    } else {
        unrelated_identity.public_key()
    };
    let client = Arc::new(FlowEndpoint::bind(
        localhost(),
        client_identity.clone(),
        PeerTrust::pinned([server_pin]),
        hello(&client_identity, (1, 1)),
    )?);
    let server = Arc::new(FlowEndpoint::bind(
        localhost(),
        server_identity.clone(),
        PeerTrust::pinned([client_pin]),
        hello(&server_identity, (1, 1)),
    )?);
    let server_acceptor = Arc::clone(&server);
    let accepted = tokio::spawn(async move { server_acceptor.accept().await });

    if client.connect(server.local_addr()?).await.is_ok() {
        return Err("client accepted an incorrectly pinned server".into());
    }
    if accepted.await?.is_ok() {
        return Err("server accepted an incorrectly pinned client".into());
    }
    Ok(())
}

fn hello(identity: &MachineIdentity, (proto_min, proto_max): (u32, u32)) -> proto::Hello {
    proto::Hello {
        proto_min,
        proto_max,
        public_key: identity.public_key().as_bytes().to_vec(),
        machine_name: "test-peer".to_owned(),
        app_version: "test".to_owned(),
        ..Default::default()
    }
}

fn hello_with_capabilities(
    identity: &MachineIdentity,
    range: (u32, u32),
    capabilities: &[proto::Capability],
) -> proto::Hello {
    proto::Hello {
        capabilities: capabilities.iter().copied().map(Into::into).collect(),
        ..hello(identity, range)
    }
}

fn decode<M: buffa::Message>(envelope: &Envelope) -> TestResult<M> {
    envelope
        .decode(openlogi_flow::frame::InboundRole::Request)
        .map_err(|decision| format!("protobuf payload rejected: {decision:?}").into())
}

trait PeerHelloKey {
    fn peer_hello_key(&self) -> TestResult<PublicKey>;
}

impl PeerHelloKey for FlowConnection {
    fn peer_hello_key(&self) -> TestResult<PublicKey> {
        Ok(PublicKey::try_from(
            self.peer_hello().public_key.as_slice(),
        )?)
    }
}
