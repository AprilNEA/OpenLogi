use std::time::Duration;

use super::{
    test_support::{hello, localhost, stalled_dial},
    *,
};

#[tokio::test]
async fn stalled_handshake_does_not_delay_a_concurrent_accept() {
    let server_identity = MachineIdentity::generate().unwrap();
    let staller_identity = MachineIdentity::generate().unwrap();
    let peer_identity = MachineIdentity::generate().unwrap();
    let server = FlowEndpoint::bind_with_limits(
        localhost(),
        server_identity.clone(),
        PeerTrust::pinned([staller_identity.public_key(), peer_identity.public_key()]),
        hello(&server_identity),
        EndpointLimits {
            handshake_timeout: Duration::from_secs(1),
            ..EndpointLimits::default()
        },
    )
    .unwrap();
    let address = server.local_addr().unwrap();
    let server_trust = PeerTrust::pinned([server_identity.public_key()]);

    // The client side of TLS finishes only once the server accepts it.
    let silent_dial = tokio::spawn({
        let trust = server_trust.clone();
        async move { stalled_dial(&staller_identity, trust, address).await }
    });
    let stuck_handshake = tokio::spawn(server.accept_incoming().await.unwrap().establish());
    let _silent_peer = silent_dial.await.unwrap();

    let peer = FlowEndpoint::bind(
        localhost(),
        peer_identity.clone(),
        server_trust,
        hello(&peer_identity),
    )
    .unwrap();
    let dial = tokio::spawn(async move {
        let connection = peer.connect(address).await;
        (peer, connection)
    });
    let accepted = server
        .accept_incoming()
        .await
        .unwrap()
        .establish()
        .await
        .unwrap();
    assert_eq!(accepted.peer_key(), peer_identity.public_key());
    dial.await.unwrap().1.unwrap();
    assert!(
        !stuck_handshake.is_finished(),
        "the healthy peer finished only after the stalled handshake gave up"
    );

    assert!(matches!(
        stuck_handshake.await.unwrap(),
        Err(TransportError::HandshakeTimeout)
    ));
}

#[tokio::test]
async fn notification_without_fin_times_out() {
    let first_identity = MachineIdentity::generate().unwrap();
    let second_identity = MachineIdentity::generate().unwrap();
    let limits = EndpointLimits {
        stream_read_timeout: Duration::from_millis(200),
        ..EndpointLimits::default()
    };
    let first = FlowEndpoint::bind_with_limits(
        localhost(),
        first_identity.clone(),
        PeerTrust::pinned([second_identity.public_key()]),
        hello(&first_identity),
        limits,
    )
    .unwrap();
    let second = Arc::new(
        FlowEndpoint::bind_with_limits(
            localhost(),
            second_identity.clone(),
            PeerTrust::pinned([first_identity.public_key()]),
            hello(&second_identity),
            limits,
        )
        .unwrap(),
    );
    let acceptor = Arc::clone(&second);
    let accept = tokio::spawn(async move { acceptor.accept().await });
    let sender = first.connect(second.local_addr().unwrap()).await.unwrap();
    let receiver = accept.await.unwrap().unwrap();

    // A complete frame whose FIN never arrives.
    let frame = message_envelope(FrameKind::PeerState, &proto::PeerState::default())
        .unwrap()
        .to_bytes()
        .unwrap();
    let mut send = sender.connection.open_uni().await.unwrap();
    send.write_all(&frame).await.unwrap();

    assert!(matches!(
        receiver.accept_notification().await,
        Err(TransportError::StreamTimeout)
    ));
}
