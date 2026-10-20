#![expect(
    clippy::tests_outside_test_module,
    reason = "Cargo integration tests are test-only crates"
)]

use std::{
    error::Error as StdError,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Instant,
};

use openlogi_flow::{
    clipboard::{
        Announced, ClipboardOffer, FetchError, FilesOffer, OfferedFile, Served, TEXT_MIME,
        TextOffer, fetch_file, fetch_file_list, fetch_text, files_sync_enabled, serve_fetch,
        text_sync_enabled,
    },
    frame::{Envelope, FrameKind, MAX_PAYLOAD_LEN},
    generated as proto,
    pairing::{PairingSession, PeerKeyStore, PersistPeerKeyError},
    sas::{PublicKey, SessionNonce},
    transport::{
        BulkRpcResponse, FlowConnection, FlowEndpoint, MachineIdentity, NotificationEvent,
        PeerTrust, RpcEvent, SessionTrust, TransportError, message_envelope,
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
        Self::connect(first_range, second_range, false, (&[], &[])).await
    }

    async fn pairing() -> TestResult<Self> {
        Self::connect((1, 1), (1, 1), true, (&[], &[])).await
    }

    async fn trusted_clipboard() -> TestResult<Self> {
        let text = &[proto::Capability::ClipboardText];
        Self::connect((1, 1), (1, 1), false, (text, text)).await
    }

    async fn trusted_clipboard_files() -> TestResult<Self> {
        let both = &[
            proto::Capability::ClipboardText,
            proto::Capability::ClipboardFiles,
        ];
        Self::connect((1, 1), (1, 1), false, (both, both)).await
    }

    /// Only `first` lists clipboard text, as against a peer that predates it.
    async fn clipboard_on_first_only() -> TestResult<Self> {
        Self::connect(
            (1, 1),
            (1, 1),
            false,
            (&[proto::Capability::ClipboardText], &[]),
        )
        .await
    }

    async fn connect(
        first_range: (u32, u32),
        second_range: (u32, u32),
        pairing: bool,
        (first_capabilities, second_capabilities): (&[proto::Capability], &[proto::Capability]),
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
            hello_with_capabilities(&first_identity, [1; 16], first_range, first_capabilities),
        )?);
        let second = Arc::new(FlowEndpoint::bind(
            localhost(),
            second_identity.clone(),
            second_trust,
            hello_with_capabilities(&second_identity, [2; 16], second_range, second_capabilities),
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
async fn an_ipv6_endpoint_takes_ipv4_and_ipv6_peers() -> TestResult {
    let listener_identity = MachineIdentity::generate()?;
    let v4_identity = MachineIdentity::generate()?;
    let v6_identity = MachineIdentity::generate()?;
    let listener = Arc::new(FlowEndpoint::bind(
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
        listener_identity.clone(),
        PeerTrust::pinned([v4_identity.public_key(), v6_identity.public_key()]),
        hello(&listener_identity, [1; 16], (1, 1)),
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
            hello(identity, [2; 16], (1, 1)),
        )?;
        let acceptor = Arc::clone(&listener);
        let accept = tokio::spawn(async move { acceptor.accept().await });
        let connection = dialer.connect(target).await?;
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
async fn clipboard_text_round_trips_through_announce_fetch_and_serve() -> TestResult {
    let pair = ConnectedPair::trusted_clipboard().await?;
    assert!(text_sync_enabled(&pair.first));
    assert!(text_sync_enabled(&pair.second));
    // Several chunks, with multi-byte characters straddling their boundaries.
    let text = "Flow \u{2713} clipboard \u{2014} ".repeat(9_000);
    let offer = ClipboardOffer::Text(TextOffer::new(1, text.clone())?);
    pair.first
        .notify(message_envelope(
            FrameKind::ClipboardAnnounce,
            &offer.announce(),
        )?)
        .await?;
    let NotificationEvent::Notification(announce) = pair.second.accept_notification().await? else {
        return Err("ClipboardAnnounce was dropped".into());
    };
    let Announced::Text(announced) =
        Announced::from_announce(&decode::<proto::ClipboardAnnounce>(&announce)?)
    else {
        return Err("the announce carried no fetchable text".into());
    };
    assert_eq!(announced.size_bytes(), text.len());

    let serve = async {
        let RpcEvent::Request(rpc) = pair.first.accept_rpc().await? else {
            return Err("ClipboardFetch was rejected".into());
        };
        TestResult::Ok(serve_fetch(rpc, Some(&offer)).await?)
    };
    let (served, fetched) = tokio::join!(serve, fetch_text(&pair.second, &announced));
    assert_eq!(served?, Served::Text { sequence: 1 });
    assert_eq!(fetched?, text);
    Ok(())
}

#[tokio::test]
async fn a_fetch_for_a_superseded_clipboard_generation_is_refused() -> TestResult {
    let pair = ConnectedPair::trusted_clipboard().await?;
    let stale = TextOffer::new(1, "older".to_owned())?;
    let current = ClipboardOffer::Text(TextOffer::new(2, "newer".to_owned())?);
    let Announced::Text(announced) = Announced::from_announce(&stale.announce()) else {
        return Err("the announce carried no fetchable text".into());
    };

    let serve = async {
        let RpcEvent::Request(rpc) = pair.first.accept_rpc().await? else {
            return Err("ClipboardFetch was rejected".into());
        };
        TestResult::Ok(serve_fetch(rpc, Some(&current)).await?)
    };
    let (served, fetched) = tokio::join!(serve, fetch_text(&pair.second, &announced));
    assert_eq!(served?, Served::Refused);
    assert!(
        matches!(
            fetched,
            Err(FetchError::Refused(code)) if code.as_known() == Some(proto::ErrorCode::Invalid)
        ),
        "a superseded generation must be refused INVALID, got {fetched:?}"
    );
    Ok(())
}

#[tokio::test]
async fn clipboard_frames_need_both_peers_to_list_the_capability() -> TestResult {
    let pair = ConnectedPair::clipboard_on_first_only().await?;
    assert!(!text_sync_enabled(&pair.first));
    assert!(!text_sync_enabled(&pair.second));

    let offer = TextOffer::new(1, "not for a peer without clipboard sync".to_owned())?;
    let announce = message_envelope(FrameKind::ClipboardAnnounce, &offer.announce())?;
    assert!(matches!(
        pair.first.notify(announce).await,
        Err(TransportError::SendGated(FrameKind::ClipboardAnnounce))
    ));
    let Announced::Text(announced) = Announced::from_announce(&offer.announce()) else {
        return Err("the announce carried no fetchable text".into());
    };
    assert!(matches!(
        fetch_text(&pair.second, &announced).await,
        Err(FetchError::Transport(TransportError::SendGated(
            FrameKind::ClipboardFetch
        )))
    ));
    Ok(())
}

#[tokio::test]
async fn a_transfer_longer_than_its_announce_is_rejected() -> TestResult {
    let pair = ConnectedPair::trusted_clipboard().await?;
    let announce = proto::ClipboardAnnounce {
        sequence: 4,
        formats: vec![proto::ClipboardFormat {
            mime: TEXT_MIME.to_owned(),
            size_bytes: 3,
            ..Default::default()
        }],
        ..Default::default()
    };
    let Announced::Text(announced) = Announced::from_announce(&announce) else {
        return Err("the announce carried no fetchable text".into());
    };

    let serve = async {
        let RpcEvent::Request(rpc) = pair.first.accept_rpc().await? else {
            return Err("ClipboardFetch was rejected".into());
        };
        rpc.respond_bulk(
            message_envelope(
                FrameKind::ClipboardData,
                &proto::ClipboardData {
                    sequence: 4,
                    mime: TEXT_MIME.to_owned(),
                    total_size: 3,
                    ..Default::default()
                },
            )?,
            [message_envelope(
                FrameKind::Chunk,
                &proto::Chunk {
                    data: b"abcdef".to_vec(),
                    ..Default::default()
                },
            )?],
            message_envelope(FrameKind::ChunkEnd, &proto::ChunkEnd::default())?,
        )
        .await?;
        TestResult::Ok(())
    };
    // The fetcher stops reading at the first excess byte, so the serving side
    // may see its stream stopped; only the fetcher's verdict matters here.
    let (_served, fetched) = tokio::join!(serve, fetch_text(&pair.second, &announced));
    assert!(
        matches!(fetched, Err(FetchError::Invalid(_))),
        "excess text must fail the fetch, got {fetched:?}"
    );
    Ok(())
}

/// The longest a test waits on a silent peer.
const STALL: std::time::Duration = std::time::Duration::from_secs(5);

/// A file under `root` holding `bytes`, offered as `relative_path`.
fn offered_file(
    root: &std::path::Path,
    relative_path: &str,
    bytes: &[u8],
) -> TestResult<OfferedFile> {
    let source = root.join(relative_path);
    if let Some(folder) = source.parent() {
        std::fs::create_dir_all(folder)?;
    }
    std::fs::write(&source, bytes)?;
    Ok(OfferedFile {
        relative_path: relative_path.to_owned(),
        size_bytes: u64::try_from(bytes.len())?,
        modified_at_ms: 0,
        source,
    })
}

/// Answers the next `requests` fetches on `connection` from `offer`.
async fn serve_fetches(
    connection: &FlowConnection,
    offer: &ClipboardOffer,
    requests: usize,
) -> TestResult<Vec<Served>> {
    let mut served = Vec::with_capacity(requests);
    for _ in 0..requests {
        let RpcEvent::Request(rpc) = connection.accept_rpc().await? else {
            return Err("a clipboard fetch was rejected".into());
        };
        served.push(serve_fetch(rpc, Some(offer)).await?);
    }
    Ok(served)
}

#[tokio::test]
async fn clipboard_files_round_trip_through_the_list_and_file_fetches() -> TestResult {
    let pair = ConnectedPair::trusted_clipboard_files().await?;
    assert!(files_sync_enabled(&pair.first));
    assert!(files_sync_enabled(&pair.second));
    let root = tempfile::tempdir()?;
    // Several chunks, one byte short of a whole number of them; and an empty
    // file, which has no chunk at all.
    let large: Vec<u8> = (0..200_003_u32).map(|index| (index % 251) as u8).collect();
    let files = vec![
        offered_file(root.path(), "notes.txt", b"copied on the other computer")?,
        offered_file(root.path(), "photos/large.bin", &large)?,
        offered_file(root.path(), "photos/empty.txt", b"")?,
    ];
    let offer = ClipboardOffer::Files(FilesOffer::new(3, files)?);
    pair.first
        .notify(message_envelope(
            FrameKind::ClipboardAnnounce,
            &offer.announce(),
        )?)
        .await?;
    let NotificationEvent::Notification(announce) = pair.second.accept_notification().await? else {
        return Err("ClipboardAnnounce was dropped".into());
    };
    let Announced::Files(announced) =
        Announced::from_announce(&decode::<proto::ClipboardAnnounce>(&announce)?)
    else {
        return Err("the announce carried no file list".into());
    };
    assert_eq!(announced.digest(), Some(offer.digest()));

    let fetch = async {
        let listed = fetch_file_list(&pair.second, &announced).await?;
        let mut contents = Vec::new();
        for (index, file) in listed.files().iter().enumerate() {
            let mut sink = Vec::new();
            fetch_file(
                &pair.second,
                announced.sequence(),
                u32::try_from(index)?,
                file,
                &mut sink,
                STALL,
                || true,
            )
            .await?;
            contents.push((file.relative_path().to_owned(), sink));
        }
        TestResult::Ok(contents)
    };
    let (served, fetched) = tokio::join!(serve_fetches(&pair.first, &offer, 4), fetch);
    assert_eq!(served?, vec![Served::Files { sequence: 3 }; 4]);
    let fetched = fetched?;
    assert_eq!(fetched.len(), 3);
    assert_eq!(
        fetched[0],
        (
            "notes.txt".to_owned(),
            b"copied on the other computer".to_vec()
        )
    );
    assert_eq!(fetched[1], ("photos/large.bin".to_owned(), large));
    assert_eq!(fetched[2], ("photos/empty.txt".to_owned(), Vec::new()));
    Ok(())
}

#[tokio::test]
async fn a_file_that_changed_since_its_offer_is_refused() -> TestResult {
    let pair = ConnectedPair::trusted_clipboard_files().await?;
    let root = tempfile::tempdir()?;
    let file = offered_file(root.path(), "draft.txt", b"first version")?;
    let source = file.source.clone();
    let offer = ClipboardOffer::Files(FilesOffer::new(1, vec![file])?);
    let Announced::Files(announced) = Announced::from_announce(&offer.announce()) else {
        return Err("the announce carried no file list".into());
    };
    std::fs::write(&source, b"a longer second version")?;

    let fetch = async {
        let listed = fetch_file_list(&pair.second, &announced).await?;
        let mut sink = Vec::new();
        let result = fetch_file(
            &pair.second,
            1,
            0,
            &listed.files()[0],
            &mut sink,
            STALL,
            || true,
        )
        .await;
        TestResult::Ok(result)
    };
    let (served, fetched) = tokio::join!(serve_fetches(&pair.first, &offer, 2), fetch);
    assert_eq!(
        served?,
        vec![Served::Files { sequence: 1 }, Served::Refused]
    );
    let fetched = fetched?;
    assert!(
        matches!(
            fetched,
            Err(FetchError::Refused(code)) if code.as_known() == Some(proto::ErrorCode::Invalid)
        ),
        "a file that changed since its offer must be refused INVALID, got {fetched:?}"
    );
    Ok(())
}

#[tokio::test]
async fn file_fetches_need_both_peers_to_list_the_files_capability() -> TestResult {
    let pair = ConnectedPair::trusted_clipboard().await?;
    assert!(text_sync_enabled(&pair.second));
    assert!(!files_sync_enabled(&pair.second));
    let root = tempfile::tempdir()?;
    let file = offered_file(root.path(), "a.txt", b"abc")?;
    let offer = FilesOffer::new(1, vec![file])?;
    let Announced::Files(announced) = Announced::from_announce(&offer.announce()) else {
        return Err("the announce carried no file list".into());
    };
    let serve = async {
        let RpcEvent::Request(rpc) = pair.first.accept_rpc().await? else {
            return Err("the file list fetch was rejected".into());
        };
        TestResult::Ok(serve_fetch(rpc, Some(&ClipboardOffer::Files(offer))).await?)
    };
    let fetch = async {
        let listed = fetch_file_list(&pair.second, &announced).await?;
        let mut sink = Vec::new();
        TestResult::Ok(
            fetch_file(
                &pair.second,
                1,
                0,
                &listed.files()[0],
                &mut sink,
                STALL,
                || true,
            )
            .await,
        )
    };
    let (_served, fetched) = tokio::join!(serve, fetch);
    assert!(matches!(
        fetched?,
        Err(FetchError::Transport(TransportError::SendGated(
            FrameKind::FileFetch
        )))
    ));
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
