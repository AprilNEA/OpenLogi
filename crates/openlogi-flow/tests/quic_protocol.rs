#![expect(
    clippy::tests_outside_test_module,
    reason = "Cargo integration tests are test-only crates"
)]

use std::{
    error::Error as StdError,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Instant,
};

use openlogi_flow::{
    frame::{Envelope, FrameKind, MAX_PAYLOAD_LEN},
    generated as proto,
    pairing::{PairingSession, PeerKeyStore, PersistPeerKeyError},
    sas::{PublicKey, SessionNonce},
    transport::{
        BulkRpcResponse, FlowConnection, FlowEndpoint, MachineIdentity, PeerTrust, RpcEvent,
        SessionTrust, TransportError, message_envelope,
    },
};

type TestResult<T = ()> = Result<T, Box<dyn StdError + Send + Sync>>;

struct ConnectedPair {
    _first_endpoint: Arc<FlowEndpoint>,
    _second_endpoint: Arc<FlowEndpoint>,
    first: FlowConnection,
    second: FlowConnection,
}

impl ConnectedPair {
    async fn trusted(first_range: (u32, u32), second_range: (u32, u32)) -> TestResult<Self> {
        Self::connect(first_range, second_range, false, &[]).await
    }

    async fn pairing() -> TestResult<Self> {
        Self::connect((1, 1), (1, 1), true, &[]).await
    }

    async fn trusted_clipboard() -> TestResult<Self> {
        Self::connect((1, 1), (1, 1), false, &[proto::Capability::ClipboardText]).await
    }

    async fn connect(
        first_range: (u32, u32),
        second_range: (u32, u32),
        pairing: bool,
        capabilities: &[proto::Capability],
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
        let first = Arc::new(FlowEndpoint::bind(
            localhost(),
            first_identity.clone(),
            first_trust,
            hello_with_capabilities(&first_identity, [1; 16], first_range, capabilities),
        )?);
        let second = Arc::new(FlowEndpoint::bind(
            localhost(),
            second_identity.clone(),
            second_trust,
            hello_with_capabilities(&second_identity, [2; 16], second_range, capabilities),
        )?);
        let second_acceptor = Arc::clone(&second);
        let accept = tokio::spawn(async move { second_acceptor.accept().await });
        let first_connection = first.connect(second.local_addr()?).await?;
        let second_connection = accept.await??;
        Ok(Self {
            _first_endpoint: first,
            _second_endpoint: second,
            first: first_connection,
            second: second_connection,
        })
    }
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
        hello(&first_identity, [1; 16], (1, 1)),
    )?);
    let second = Arc::new(FlowEndpoint::bind(
        localhost(),
        second_identity.clone(),
        PeerTrust::pinned([first_identity.public_key()]),
        hello(&second_identity, [2; 16], (2, 2)),
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
    let first_nonce = SessionNonce::try_from(pair.second.peer_hello().session_nonce.as_slice())?;
    let second_nonce = SessionNonce::try_from(pair.first.peer_hello().session_nonce.as_slice())?;
    let mut first_session = PairingSession::new(
        pair.second.peer_key(),
        pair.first.peer_key(),
        first_nonce,
        second_nonce,
    );
    let mut second_session = PairingSession::new(
        pair.first.peer_key(),
        pair.second.peer_key(),
        second_nonce,
        first_nonce,
    );
    assert_eq!(first_session.sas_code(), second_session.sas_code());
    assert!(first_session.sas_code().is_some());

    let start = message_envelope(FrameKind::PairStart, &first_session.start()?)?;
    let server_side = async {
        let RpcEvent::Request(request) = pair.second.accept_rpc().await? else {
            return Err("PairStart was rejected".into());
        };
        request
            .request()
            .decode::<proto::PairStart>(openlogi_flow::frame::InboundRole::Request)
            .map_err(|_| "PairStart payload was invalid")?;
        let prompted = second_session.receive_start(Instant::now(), None)?;
        request
            .respond(message_envelope(FrameKind::PairPrompted, &prompted)?)
            .await?;
        TestResult::Ok(())
    };
    let (server_result, prompted) = tokio::join!(server_side, pair.first.call(start));
    server_result?;
    let prompted = decode::<proto::PairPrompted>(&prompted?)?;
    first_session.receive_prompted(&prompted, Instant::now())?;

    let mut first_store = MemoryKeyStore::default();
    let mut second_store = MemoryKeyStore::default();
    assert_eq!(
        first_session.confirm_local(&mut first_store)?,
        proto::PairResult::PendingLocal
    );
    let first_confirm = message_envelope(FrameKind::PairConfirm, &proto::PairConfirm::default())?;
    let server_side = async {
        let RpcEvent::Request(request) = pair.second.accept_rpc().await? else {
            return Err("first PairConfirm was rejected".into());
        };
        let outcome = second_session.receive_confirm(&mut second_store)?;
        request
            .respond(message_envelope(FrameKind::PairOutcome, &outcome)?)
            .await?;
        TestResult::Ok(())
    };
    let (server_result, outcome) = tokio::join!(server_side, pair.first.call(first_confirm));
    server_result?;
    first_session.receive_outcome(&decode::<proto::PairOutcome>(&outcome?)?)?;

    assert_eq!(
        second_session.confirm_local(&mut second_store)?,
        proto::PairResult::Paired
    );
    let second_confirm = message_envelope(FrameKind::PairConfirm, &proto::PairConfirm::default())?;
    let server_side = async {
        let RpcEvent::Request(request) = pair.first.accept_rpc().await? else {
            return Err("second PairConfirm was rejected".into());
        };
        let outcome = first_session.receive_confirm(&mut first_store)?;
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
        hello(&client_identity, [1; 16], (1, 1)),
    )?);
    let server = Arc::new(FlowEndpoint::bind(
        localhost(),
        server_identity.clone(),
        PeerTrust::pinned([client_pin]),
        hello(&server_identity, [2; 16], (1, 1)),
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

fn hello(
    identity: &MachineIdentity,
    nonce: [u8; 16],
    (proto_min, proto_max): (u32, u32),
) -> proto::Hello {
    proto::Hello {
        proto_min,
        proto_max,
        public_key: identity.public_key().as_bytes().to_vec(),
        session_nonce: nonce.to_vec(),
        machine_name: "test-peer".to_owned(),
        app_version: "test".to_owned(),
        ..Default::default()
    }
}

fn hello_with_capabilities(
    identity: &MachineIdentity,
    nonce: [u8; 16],
    range: (u32, u32),
    capabilities: &[proto::Capability],
) -> proto::Hello {
    proto::Hello {
        capabilities: capabilities.iter().copied().map(Into::into).collect(),
        ..hello(identity, nonce, range)
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
