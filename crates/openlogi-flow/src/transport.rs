//! QUIC transport binding for Flow frames, streams, datagrams, and mutual trust.

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
mod tls;

use std::{
    io,
    net::SocketAddr,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use buffa::Message;
use quinn::{Connection, Endpoint, EndpointConfig, RecvStream, SendStream, VarInt};
use rustls::pki_types::CertificateDer;
use socket2::{Domain, Protocol, Socket, Type};
use thiserror::Error;
use tokio::time::{Instant, timeout, timeout_at};

use crate::{
    frame::{Envelope, FrameError, FrameKind, InboundRole, InvalidDecision},
    generated as proto,
    negotiation::{HelloRejection, Negotiated, SendGate, negotiate},
    pairing::{PairingSession, PairingState},
    sas::{HelloDigest, PublicKey, RandomError, SessionNonce},
};

pub use tls::{
    ALPN, CandidateLease, IdentityError, MachineIdentity, PeerTrust, SessionTrust,
    public_key_from_certificate,
};

const CLOSE_PROTOCOL: VarInt = VarInt::from_u32(1);
const STOP_OVERSIZED: VarInt = VarInt::from_u32(2);
const CLOSE_PAIRING_BUSY: VarInt = VarInt::from_u32(3);
const STOP_TIMEOUT: VarInt = VarInt::from_u32(4);

/// Deadlines that keep one slow or silent peer from holding local resources.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EndpointLimits {
    /// Deadline for TLS plus the `Hello` exchange, for dials and accepts.
    pub handshake_timeout: Duration,
    /// Deadline for reading one inbound request or notification frame and
    /// its stream FIN.
    pub stream_read_timeout: Duration,
}

impl Default for EndpointLimits {
    fn default() -> Self {
        Self {
            handshake_timeout: Duration::from_secs(10),
            stream_read_timeout: Duration::from_secs(10),
        }
    }
}

/// Whether the local endpoint initiated a Flow connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionDirection {
    /// The local endpoint dialed the peer.
    Outgoing,
    /// The local endpoint accepted the peer's dial.
    Incoming,
}

/// A local QUIC endpoint configured for pinned-key mutual authentication.
#[derive(Debug)]
pub struct FlowEndpoint {
    endpoint: Endpoint,
    shared: Arc<EndpointShared>,
}

#[derive(Debug)]
struct EndpointShared {
    identity: MachineIdentity,
    trust: PeerTrust,
    hello: proto::Hello,
    limits: EndpointLimits,
}

impl FlowEndpoint {
    /// Binds a Flow endpoint and configures both TLS directions with one trust policy.
    ///
    /// `hello` is the template for every connection. Its `public_key` must be
    /// this identity's key and its `session_nonce` must be empty: the endpoint
    /// draws a fresh nonce for each connection.
    ///
    /// The IPv6 wildcard (`[::]`) binds dual-stack, so the one endpoint also
    /// accepts and dials IPv4 peers; Windows makes an IPv6 socket IPv6-only
    /// unless told otherwise.
    pub fn bind(
        address: SocketAddr,
        identity: MachineIdentity,
        trust: PeerTrust,
        hello: proto::Hello,
    ) -> Result<Self, TransportError> {
        Self::bind_with_limits(address, identity, trust, hello, EndpointLimits::default())
    }

    /// Binds a Flow endpoint with explicit handshake and stream deadlines.
    pub fn bind_with_limits(
        address: SocketAddr,
        identity: MachineIdentity,
        trust: PeerTrust,
        hello: proto::Hello,
        limits: EndpointLimits,
    ) -> Result<Self, TransportError> {
        validate_local_hello(&hello, identity.public_key())?;
        let (server_config, client_config) = quic_configs(&identity, trust.clone())?;
        let socket = Socket::new(
            Domain::for_address(address),
            Type::DGRAM,
            Some(Protocol::UDP),
        )?;
        // Only the wildcard can take both families: Windows refuses to bind a
        // specific IPv6 address, `::1` say, that is not IPv6-only.
        if address.is_ipv6() && address.ip().is_unspecified() {
            socket.set_only_v6(false)?;
        }
        socket.bind(&address.into())?;
        let runtime = quinn::default_runtime()
            .ok_or_else(|| io::Error::other("no async runtime for the Flow endpoint"))?;
        let mut endpoint = Endpoint::new(
            EndpointConfig::default(),
            Some(server_config),
            socket.into(),
            runtime,
        )?;
        endpoint.set_default_client_config(client_config);
        Ok(Self {
            endpoint,
            shared: Arc::new(EndpointShared {
                identity,
                trust,
                hello,
                limits,
            }),
        })
    }

    /// Returns the bound local socket address.
    pub fn local_addr(&self) -> Result<SocketAddr, TransportError> {
        self.endpoint.local_addr().map_err(TransportError::from)
    }

    /// Returns this endpoint's stable machine identity.
    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        self.shared.identity.public_key()
    }

    /// Dials and performs the control-stream Hello exchange within the
    /// handshake deadline.
    pub async fn connect(&self, address: SocketAddr) -> Result<FlowConnection, TransportError> {
        self.dial(address).await?.establish().await
    }

    /// Dials and runs only TLS, within the handshake deadline.
    ///
    /// The peer counts a connection once its `Hello` exchange completes. A
    /// caller that races several addresses of one peer establishes one
    /// winner and drops the rest, so both ends keep the same connection.
    pub async fn dial(&self, address: SocketAddr) -> Result<DialedConnection, TransportError> {
        let deadline = Instant::now() + self.shared.limits.handshake_timeout;
        let connecting = self.endpoint.connect(address, "openlogi-flow")?;
        let connection = timeout_at(deadline, connecting)
            .await
            .map_err(|_| TransportError::HandshakeTimeout)??;
        let peer_key = connection_peer_key(&connection)?;
        Ok(DialedConnection {
            connection,
            peer_key,
            deadline,
            shared: Arc::clone(&self.shared),
        })
    }

    /// Waits for the next inbound dial without spending a handshake on it.
    ///
    /// Cancel-safe: dropping the future never loses a dial. Run
    /// [`IncomingConnection::establish`] concurrently (for example in a
    /// spawned task) so that one slow peer cannot delay the next.
    pub async fn accept_incoming(&self) -> Result<IncomingConnection, TransportError> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or(TransportError::EndpointClosed)?;
        Ok(IncomingConnection {
            incoming,
            shared: Arc::clone(&self.shared),
        })
    }

    /// Accepts one dial and performs its handshake inline.
    ///
    /// Convenience for single-peer callers and tests. Long-lived acceptors
    /// use [`Self::accept_incoming`] so that handshakes run concurrently.
    pub async fn accept(&self) -> Result<FlowConnection, TransportError> {
        self.accept_incoming().await?.establish().await
    }

    /// Gracefully closes all connections owned by this endpoint.
    pub async fn close(self) {
        self.endpoint.close(VarInt::from_u32(0), b"endpoint closed");
        self.endpoint.wait_idle().await;
    }
}

/// An outgoing connection that finished TLS and has not exchanged `Hello`.
///
/// Dropping it closes the connection before the peer counts it.
#[derive(Debug)]
pub struct DialedConnection {
    connection: Connection,
    peer_key: PublicKey,
    deadline: Instant,
    shared: Arc<EndpointShared>,
}

impl DialedConnection {
    /// Returns the peer key TLS authenticated.
    #[must_use]
    pub const fn peer_key(&self) -> PublicKey {
        self.peer_key
    }

    /// Runs the `Hello` exchange within the rest of the dial's handshake
    /// deadline.
    pub async fn establish(self) -> Result<FlowConnection, TransportError> {
        let Self {
            connection,
            deadline,
            shared,
            ..
        } = self;
        timeout_at(
            deadline,
            establish(&shared, connection, ConnectionDirection::Outgoing),
        )
        .await
        .map_err(|_| TransportError::HandshakeTimeout)?
    }
}

/// An inbound dial whose TLS and `Hello` handshake has not run yet.
#[derive(Debug)]
pub struct IncomingConnection {
    incoming: quinn::Incoming,
    shared: Arc<EndpointShared>,
}

impl IncomingConnection {
    /// Returns the dialing peer's claimed address. It is a hint, never identity.
    #[must_use]
    pub fn remote_address(&self) -> SocketAddr {
        self.incoming.remote_address()
    }

    /// Refuses the dial without running a TLS handshake.
    pub fn refuse(self) {
        self.incoming.refuse();
    }

    /// Runs TLS and the `Hello` exchange within the handshake deadline.
    pub async fn establish(self) -> Result<FlowConnection, TransportError> {
        let Self { incoming, shared } = self;
        timeout(shared.limits.handshake_timeout, async {
            let connection = incoming.await?;
            establish(&shared, connection, ConnectionDirection::Incoming).await
        })
        .await
        .map_err(|_| TransportError::HandshakeTimeout)?
    }
}

async fn establish(
    shared: &EndpointShared,
    connection: Connection,
    direction: ConnectionDirection,
) -> Result<FlowConnection, TransportError> {
    verify_alpn(&connection)?;
    let peer_key = connection_peer_key(&connection)?;
    let initial_trust = shared
        .trust
        .classify(peer_key)
        .map_err(|error| TransportError::Trust(error.to_string()))?;
    let (mut control_send, mut control_recv) = match direction {
        ConnectionDirection::Outgoing => connection.open_bi().await?,
        ConnectionDirection::Incoming => connection.accept_bi().await?,
    };

    let mut local_hello = shared.hello.clone();
    local_hello.session_nonce = SessionNonce::random()?.as_bytes().to_vec();
    let local_frame = message_envelope(FrameKind::Hello, &local_hello)?;
    let local_hello_digest = HelloDigest::of(&local_frame.payload);
    local_frame.write_to(&mut control_send).await?;
    let peer_frame = Envelope::read_from(&mut control_recv).await?;
    if peer_frame.flags != 0 {
        reject_hello(
            &connection,
            &mut control_send,
            &local_hello,
            proto::RejectReason::Malformed,
            "Hello frame used reserved flags",
        )
        .await?;
        return Err(TransportError::InvalidControlFrame);
    }
    if peer_frame.kind == FrameKind::HelloReject {
        let rejection = peer_frame
            .decode::<proto::HelloReject>(InboundRole::Request)
            .map_err(|_| TransportError::InvalidControlFrame)?;
        return Err(TransportError::PeerHelloRejected(rejection.reason.to_i32()));
    }
    if peer_frame.kind != FrameKind::Hello {
        reject_hello(
            &connection,
            &mut control_send,
            &local_hello,
            proto::RejectReason::Malformed,
            "control stream did not carry Hello",
        )
        .await?;
        return Err(TransportError::InvalidControlFrame);
    }
    let peer_hello = peer_frame
        .decode::<proto::Hello>(InboundRole::Request)
        .map_err(|_| TransportError::InvalidControlFrame)?;
    let negotiated = match negotiate(&local_hello, &peer_hello, &peer_key) {
        Ok(negotiated) => negotiated,
        Err(rejection) => {
            reject_hello(
                &connection,
                &mut control_send,
                &local_hello,
                rejection.reason(),
                &rejection.to_string(),
            )
            .await?;
            return Err(TransportError::HelloRejected(rejection));
        }
    };

    // Only a completed handshake may claim the single pairing slot, so a
    // failed or abandoned dial can never lock pairing out.
    let candidate_lease = match initial_trust {
        SessionTrust::Trusted => None,
        SessionTrust::Untrusted => {
            let Some(lease) = shared.trust.lease_candidate(peer_key) else {
                connection.close(CLOSE_PAIRING_BUSY, b"another peer is pairing");
                return Err(TransportError::PairingBusy);
            };
            Some(lease)
        }
    };

    let trusted = Arc::new(AtomicBool::new(initial_trust == SessionTrust::Trusted));
    let monitor_connection = connection.clone();
    let control_monitor = tokio::spawn(async move {
        let _ = control_recv.read_chunk(1, true).await;
        monitor_connection.close(CLOSE_PROTOCOL, b"control stream closed");
    });
    Ok(FlowConnection {
        connection,
        _control_send: control_send,
        control_monitor,
        direction,
        local_key: shared.identity.public_key(),
        peer_key,
        local_hello_digest,
        peer_hello_digest: HelloDigest::of(&peer_frame.payload),
        peer_hello,
        inbound: InboundPolicy {
            trusted,
            send_gate: SendGate::new(negotiated.clone()),
            read_timeout: shared.limits.stream_read_timeout,
        },
        negotiated,
        candidate_lease: Mutex::new(candidate_lease),
    })
}

/// An authenticated Flow connection after Hello negotiation.
#[derive(Debug)]
pub struct FlowConnection {
    connection: Connection,
    _control_send: SendStream,
    control_monitor: tokio::task::JoinHandle<()>,
    direction: ConnectionDirection,
    local_key: PublicKey,
    peer_key: PublicKey,
    local_hello_digest: HelloDigest,
    peer_hello_digest: HelloDigest,
    peer_hello: proto::Hello,
    inbound: InboundPolicy,
    negotiated: Negotiated,
    /// The pairing slot an untrusted connection holds until it is promoted
    /// or dropped.
    candidate_lease: Mutex<Option<CandidateLease>>,
}

/// Validation state shared by a connection and the streams it accepts.
#[derive(Clone, Debug)]
struct InboundPolicy {
    trusted: Arc<AtomicBool>,
    send_gate: SendGate,
    read_timeout: Duration,
}

impl InboundPolicy {
    fn trust(&self) -> SessionTrust {
        if self.trusted.load(Ordering::Acquire) {
            SessionTrust::Trusted
        } else {
            SessionTrust::Untrusted
        }
    }
}

impl FlowConnection {
    /// Returns whether this connection was dialed or accepted locally.
    #[must_use]
    pub const fn direction(&self) -> ConnectionDirection {
        self.direction
    }

    /// Returns this endpoint's own key.
    #[must_use]
    pub const fn local_key(&self) -> PublicKey {
        self.local_key
    }

    /// Returns the TLS-authenticated peer key.
    #[must_use]
    pub const fn peer_key(&self) -> PublicKey {
        self.peer_key
    }

    /// Returns the peer's negotiated Hello message.
    #[must_use]
    pub const fn peer_hello(&self) -> &proto::Hello {
        &self.peer_hello
    }

    /// Returns the negotiated protocol version and capability intersection.
    #[must_use]
    pub const fn negotiated(&self) -> &Negotiated {
        &self.negotiated
    }

    /// Returns this connection's current trust classification.
    #[must_use]
    pub fn trust(&self) -> SessionTrust {
        self.inbound.trust()
    }

    /// Creates the idle pairing ceremony bound to this connection's identities
    /// and `Hello` transcript.
    #[must_use]
    pub const fn pairing_session(&self) -> PairingSession {
        PairingSession::new(
            self.local_key,
            self.peer_key,
            self.local_hello_digest,
            self.peer_hello_digest,
        )
    }

    /// Promotes an untrusted connection after its own pairing ceremony
    /// persisted this peer key, and frees the pairing slot for the next
    /// unknown peer while this connection stays open.
    ///
    /// A ceremony from another connection is refused even for the same peer:
    /// its code was derived from a different `Hello` transcript.
    pub fn promote_after_pairing(&self, pairing: &PairingSession) -> Result<(), TransportError> {
        if pairing.state() != PairingState::Paired
            || !pairing.is_bound_to(
                self.local_key,
                self.peer_key,
                self.local_hello_digest,
                self.peer_hello_digest,
            )
        {
            return Err(TransportError::PairingNotComplete);
        }
        self.inbound.trusted.store(true, Ordering::Release);
        self.candidate_lease
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        Ok(())
    }

    /// Applies the simultaneous-dial tiebreak after the caller detects a race.
    ///
    /// The lexicographically smaller local public key closes its outgoing
    /// connection and keeps the incoming connection. Returns whether this
    /// outgoing connection was closed.
    #[must_use]
    pub fn close_if_outgoing_loses_tie(&self) -> bool {
        let loses =
            self.direction == ConnectionDirection::Outgoing && self.local_key < self.peer_key;
        if loses {
            self.connection
                .close(VarInt::from_u32(0), b"simultaneous dial tiebreak");
        }
        loses
    }

    /// Opens the one bidirectional stream for a raw RPC exchange.
    ///
    /// This low-level API supports bulk responses and protocol conformance
    /// tests. Callers must write exactly one request frame before reading the
    /// response sequence and must finish their send direction.
    pub async fn open_rpc_stream(&self) -> Result<(SendStream, RecvStream), TransportError> {
        self.connection
            .open_bi()
            .await
            .map_err(TransportError::from)
    }

    /// Calls one single-response RPC and validates its paired response kind.
    pub async fn call(&self, request: Envelope) -> Result<Envelope, TransportError> {
        self.ensure_send_allowed(request.kind)?;
        if is_bulk_request(request.kind) {
            return Err(TransportError::BulkResponseRequired(request.kind));
        }
        let expected = expected_response(request.kind)
            .ok_or(TransportError::InvalidRequestKind(request.kind))?;
        validate_payload(&request)?;
        let (mut send, mut receive) = self.open_rpc_stream().await?;
        request.write_to(&mut send).await?;
        send.finish()?;
        let response = Envelope::read_from(&mut receive).await?;
        ensure_stream_finished(&mut receive).await?;
        validate_response_envelope(&response)?;
        if response.kind != FrameKind::Error && !expected.contains(&response.kind) {
            return Err(TransportError::UnexpectedResponse(response.kind));
        }
        Ok(response)
    }

    /// Calls a clipboard/file RPC and validates its ordered bulk response sequence.
    pub async fn call_bulk(&self, request: Envelope) -> Result<BulkRpcResponse, TransportError> {
        self.ensure_send_allowed(request.kind)?;
        if !is_bulk_request(request.kind) {
            return Err(TransportError::InvalidRequestKind(request.kind));
        }
        validate_payload(&request)?;
        let (mut send, mut receive) = self.open_rpc_stream().await?;
        request.write_to(&mut send).await?;
        send.finish()?;

        let head = Envelope::read_from(&mut receive).await?;
        validate_response_envelope(&head)?;
        if head.kind == FrameKind::Error {
            ensure_stream_finished(&mut receive).await?;
            return Ok(BulkRpcResponse::Error(head));
        }
        if head.kind != FrameKind::ClipboardData {
            return Err(TransportError::UnexpectedResponse(head.kind));
        }
        Ok(BulkRpcResponse::Data(BulkResponse {
            head,
            receive,
            finished: false,
        }))
    }

    /// Waits for the peer to open its next RPC stream without reading it.
    ///
    /// Cancel-safe: dropping the future never loses a stream, so it can sit in
    /// a `select!` loop next to [`Self::accept_notification_stream`]. Read the
    /// request with [`InboundRpc::read`], typically in a spawned task.
    pub async fn accept_rpc_stream(&self) -> Result<InboundRpc, TransportError> {
        let (send, receive) = self.connection.accept_bi().await?;
        Ok(InboundRpc {
            send,
            receive,
            policy: self.inbound.clone(),
        })
    }

    /// Accepts and reads one inbound RPC stream.
    ///
    /// Not cancel-safe: dropping the future after the stream was accepted
    /// abandons that request. Event loops use [`Self::accept_rpc_stream`].
    pub async fn accept_rpc(&self) -> Result<RpcEvent, TransportError> {
        self.accept_rpc_stream().await?.read().await
    }

    /// Sends one notification frame on its own unidirectional stream.
    pub async fn notify(&self, notification: Envelope) -> Result<(), TransportError> {
        self.ensure_send_allowed(notification.kind)?;
        if !is_notification_kind(notification.kind) {
            return Err(TransportError::InvalidNotificationKind(notification.kind));
        }
        validate_payload(&notification)?;
        let mut send = self.connection.open_uni().await?;
        notification.write_to(&mut send).await?;
        send.finish()?;
        Ok(())
    }

    /// Waits for the peer to open its next notification stream without reading it.
    ///
    /// Cancel-safe, like [`Self::accept_rpc_stream`]. Read the frame with
    /// [`InboundNotification::read`].
    pub async fn accept_notification_stream(&self) -> Result<InboundNotification, TransportError> {
        let receive = self.connection.accept_uni().await?;
        Ok(InboundNotification {
            receive,
            policy: self.inbound.clone(),
        })
    }

    /// Accepts and reads one notification stream.
    ///
    /// Not cancel-safe; event loops use [`Self::accept_notification_stream`].
    pub async fn accept_notification(&self) -> Result<NotificationEvent, TransportError> {
        self.accept_notification_stream().await?.read().await
    }

    /// Sends a `Ping` or `Pong` envelope as a QUIC datagram.
    pub fn send_datagram(&self, datagram: &Envelope) -> Result<(), TransportError> {
        if !matches!(datagram.kind, FrameKind::Ping | FrameKind::Pong) {
            return Err(TransportError::InvalidDatagramKind(datagram.kind));
        }
        self.ensure_send_allowed(datagram.kind)?;
        validate_payload(datagram)?;
        self.connection
            .send_datagram(buffa::bytes::Bytes::from(datagram.to_bytes()?))?;
        Ok(())
    }

    /// Reads and validates one `Ping` or `Pong` QUIC datagram.
    pub async fn read_datagram(&self) -> Result<Envelope, TransportError> {
        let bytes = self.connection.read_datagram().await?;
        let datagram = Envelope::from_bytes(&bytes)?;
        if datagram.flags != 0 || !matches!(datagram.kind, FrameKind::Ping | FrameKind::Pong) {
            return Err(TransportError::InvalidDatagramKind(datagram.kind));
        }
        validate_payload(&datagram)?;
        Ok(datagram)
    }

    /// Closes this QUIC connection.
    pub fn close(&self) {
        self.connection
            .close(VarInt::from_u32(0), b"Flow connection closed");
    }

    pub(crate) async fn wait_closed(&self) {
        let _ = self.connection.closed().await;
    }

    fn ensure_send_allowed(&self, kind: FrameKind) -> Result<(), TransportError> {
        if self.trust() == SessionTrust::Untrusted && !kind.is_pretrust() {
            return Err(TransportError::NotPaired(kind));
        }
        if !self.inbound.send_gate.may_send_kind(kind) {
            return Err(TransportError::SendGated(kind));
        }
        Ok(())
    }
}

impl Drop for FlowConnection {
    fn drop(&mut self) {
        self.connection
            .close(VarInt::from_u32(0), b"Flow connection dropped");
        self.control_monitor.abort();
    }
}

/// An accepted RPC stream whose request has not been read yet.
#[derive(Debug)]
pub struct InboundRpc {
    send: SendStream,
    receive: RecvStream,
    policy: InboundPolicy,
}

impl InboundRpc {
    /// Reads and validates the request within the stream-read deadline.
    ///
    /// Envelope, stream-binding, size, and pre-pairing violations are answered
    /// automatically with the mandated `Error` code. A peer that does not
    /// deliver the frame and its FIN in time gets both stream directions reset
    /// and [`TransportError::StreamTimeout`].
    pub async fn read(self) -> Result<RpcEvent, TransportError> {
        let Self {
            mut send,
            mut receive,
            policy,
        } = self;
        let deadline = Instant::now() + policy.read_timeout;
        let request = match timeout_at(deadline, Envelope::read_from(&mut receive)).await {
            Err(_) => return Err(abandon_stream(&mut receive, Some(&mut send))),
            Ok(Ok(request)) => request,
            Ok(Err(error @ FrameError::TooLarge { .. })) => {
                let _ = receive.stop(STOP_OVERSIZED);
                return reject_rpc(send, proto::ErrorCode::TooLarge, error).await;
            }
            Ok(Err(error)) => return Err(error.into()),
        };
        match timeout_at(deadline, ensure_stream_finished(&mut receive)).await {
            Err(_) => return Err(abandon_stream(&mut receive, Some(&mut send))),
            Ok(finished) => finished?,
        }
        if let Some(decision) = request.policy(InboundRole::Request) {
            let InvalidDecision::Respond(code) = decision else {
                return Err(TransportError::InvalidControlFrame);
            };
            return reject_rpc(send, code, "invalid request envelope").await;
        }
        if !is_request_kind(request.kind) {
            return reject_rpc(
                send,
                proto::ErrorCode::Invalid,
                "invalid RPC stream binding",
            )
            .await;
        }
        if policy.trust() == SessionTrust::Untrusted && !request.kind.is_pretrust() {
            return reject_rpc(send, proto::ErrorCode::NotPaired, "peer is not paired").await;
        }
        if !policy.send_gate.may_send_kind(request.kind) {
            return reject_rpc(
                send,
                proto::ErrorCode::Invalid,
                "request kind was not enabled by Hello negotiation",
            )
            .await;
        }
        if !payload_decodes(&request) {
            return reject_rpc(
                send,
                proto::ErrorCode::Invalid,
                "request payload is not valid protobuf for its frame kind",
            )
            .await;
        }
        Ok(RpcEvent::Request(IncomingRpc { request, send }))
    }
}

/// An accepted notification stream whose frame has not been read yet.
#[derive(Debug)]
pub struct InboundNotification {
    receive: RecvStream,
    policy: InboundPolicy,
}

impl InboundNotification {
    /// Reads and validates the notification within the stream-read deadline.
    pub async fn read(self) -> Result<NotificationEvent, TransportError> {
        let Self {
            mut receive,
            policy,
        } = self;
        let deadline = Instant::now() + policy.read_timeout;
        let notification = match timeout_at(deadline, Envelope::read_from(&mut receive)).await {
            Err(_) => return Err(abandon_stream(&mut receive, None)),
            Ok(Ok(notification)) => notification,
            Ok(Err(FrameError::TooLarge { .. })) => {
                let _ = receive.stop(STOP_OVERSIZED);
                return Ok(NotificationEvent::Dropped(proto::ErrorCode::TooLarge));
            }
            Ok(Err(error)) => return Err(error.into()),
        };
        match timeout_at(deadline, ensure_stream_finished(&mut receive)).await {
            Err(_) => return Err(abandon_stream(&mut receive, None)),
            Ok(finished) => finished?,
        }
        if let Some(InvalidDecision::Drop) = notification.policy(InboundRole::Notification) {
            let code = if notification.flags == 0 {
                proto::ErrorCode::UnsupportedKind
            } else {
                proto::ErrorCode::UnsupportedFlags
            };
            return Ok(NotificationEvent::Dropped(code));
        }
        if !is_notification_kind(notification.kind) {
            return Ok(NotificationEvent::Dropped(proto::ErrorCode::Invalid));
        }
        if policy.trust() == SessionTrust::Untrusted && !notification.kind.is_pretrust() {
            return Ok(NotificationEvent::Dropped(proto::ErrorCode::NotPaired));
        }
        if !policy.send_gate.may_send_kind(notification.kind) {
            return Ok(NotificationEvent::Dropped(proto::ErrorCode::Invalid));
        }
        if !payload_decodes(&notification) {
            return Ok(NotificationEvent::Dropped(proto::ErrorCode::Invalid));
        }
        Ok(NotificationEvent::Notification(notification))
    }
}

/// Result of accepting an RPC stream.
#[derive(Debug)]
pub enum RpcEvent {
    /// A validated request requiring an application response.
    Request(IncomingRpc),
    /// A rejected request already answered by the transport.
    Rejected(proto::ErrorCode),
}

/// A validated request and its response-bearing QUIC stream direction.
#[derive(Debug)]
pub struct IncomingRpc {
    request: Envelope,
    send: SendStream,
}

/// The head of a validated response to `ClipboardFetch` or `FileFetch`.
#[derive(Debug)]
pub enum BulkRpcResponse {
    /// The peer rejected the request with one generic error frame.
    Error(Envelope),
    /// A streaming data response beginning with `ClipboardData`.
    Data(BulkResponse),
}

/// A streaming bulk response after its `ClipboardData` head was validated.
#[derive(Debug)]
pub struct BulkResponse {
    head: Envelope,
    receive: RecvStream,
    finished: bool,
}

impl BulkResponse {
    /// Returns the `ClipboardData` response head.
    #[must_use]
    pub const fn head(&self) -> &Envelope {
        &self.head
    }

    /// Returns the next `Chunk` or terminal `ChunkEnd` frame.
    ///
    /// The first call after `ChunkEnd` returns `None`. Any other frame kind,
    /// malformed payload, or trailing bytes fail the response.
    pub async fn next_frame(&mut self) -> Result<Option<Envelope>, TransportError> {
        if self.finished {
            return Ok(None);
        }
        let frame = Envelope::read_from(&mut self.receive).await?;
        validate_response_envelope(&frame)?;
        match frame.kind {
            FrameKind::Chunk => Ok(Some(frame)),
            FrameKind::ChunkEnd => {
                ensure_stream_finished(&mut self.receive).await?;
                self.finished = true;
                Ok(Some(frame))
            }
            kind => Err(TransportError::UnexpectedResponse(kind)),
        }
    }
}

impl IncomingRpc {
    /// Returns the decoded envelope supplied by the peer.
    #[must_use]
    pub const fn request(&self) -> &Envelope {
        &self.request
    }

    /// Sends the request's paired response or a generic `Error`.
    pub async fn respond(mut self, response: Envelope) -> Result<(), TransportError> {
        let expected = expected_response(self.request.kind)
            .ok_or(TransportError::InvalidRequestKind(self.request.kind))?;
        if is_bulk_request(self.request.kind) && response.kind != FrameKind::Error {
            return Err(TransportError::BulkResponseRequired(self.request.kind));
        }
        if response.kind != FrameKind::Error && !expected.contains(&response.kind) {
            return Err(TransportError::UnexpectedResponse(response.kind));
        }
        validate_payload(&response)?;
        response.write_to(&mut self.send).await?;
        self.send.finish()?;
        Ok(())
    }

    /// Sends `ClipboardData`, zero or more `Chunk`s, and one `ChunkEnd`.
    pub async fn respond_bulk(
        mut self,
        head: Envelope,
        chunks: impl IntoIterator<Item = Envelope>,
        end: Envelope,
    ) -> Result<(), TransportError> {
        if !is_bulk_request(self.request.kind) {
            return Err(TransportError::InvalidRequestKind(self.request.kind));
        }
        validate_sequence_frame(&head, FrameKind::ClipboardData)?;
        head.write_to(&mut self.send).await?;
        for chunk in chunks {
            validate_sequence_frame(&chunk, FrameKind::Chunk)?;
            chunk.write_to(&mut self.send).await?;
        }
        validate_sequence_frame(&end, FrameKind::ChunkEnd)?;
        end.write_to(&mut self.send).await?;
        self.send.finish()?;
        Ok(())
    }
}

/// Result of accepting a unidirectional notification stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotificationEvent {
    /// A valid notification for application handling.
    Notification(Envelope),
    /// A dropped notification and the diagnostic code that would apply to an RPC.
    Dropped(proto::ErrorCode),
}

/// Encodes a generated protobuf message into a zero-flags envelope.
pub fn message_envelope(
    kind: FrameKind,
    message: &impl Message,
) -> Result<Envelope, TransportError> {
    let payload = message.try_encode_to_vec()?;
    Envelope::new(kind, 0, payload).map_err(TransportError::from)
}

/// Builds the generated generic error payload used in an RPC response position.
pub fn error_envelope(
    code: proto::ErrorCode,
    detail: impl Into<String>,
) -> Result<Envelope, TransportError> {
    message_envelope(
        FrameKind::Error,
        &proto::Error {
            code: code.into(),
            detail: detail.into(),
            retryable: code == proto::ErrorCode::Busy,
            ..Default::default()
        },
    )
}

/// Errors from identity setup, QUIC, framing, negotiation, or stream binding.
#[derive(Debug, Error)]
pub enum TransportError {
    /// Machine identity setup failed.
    #[error(transparent)]
    Identity(#[from] IdentityError),
    /// Socket or endpoint setup failed.
    #[error("endpoint I/O failed: {0}")]
    Io(#[from] io::Error),
    /// A QUIC dial could not be started.
    #[error("QUIC connect setup failed: {0}")]
    Connect(#[from] quinn::ConnectError),
    /// The QUIC handshake or connection failed.
    #[error("QUIC connection failed: {0}")]
    Connection(#[from] quinn::ConnectionError),
    /// Finishing a stream failed because it was already closed.
    #[error("QUIC stream is already closed")]
    ClosedStream(#[from] quinn::ClosedStream),
    /// Reading a QUIC stream failed.
    #[error("QUIC stream read failed: {0}")]
    Read(#[from] quinn::ReadError),
    /// Sending a QUIC datagram failed.
    #[error("QUIC datagram send failed: {0}")]
    SendDatagram(#[from] quinn::SendDatagramError),
    /// Frame encoding or decoding failed.
    #[error(transparent)]
    Frame(#[from] FrameError),
    /// Protobuf encoding failed.
    #[error("protobuf encoding failed: {0}")]
    ProtobufEncode(#[from] buffa::EncodeError),
    /// Rustls rejected the local certificate/key configuration.
    #[error("TLS identity configuration failed: {0}")]
    Rustls(#[from] rustls::Error),
    /// TLS/QUIC configuration could not be constructed.
    #[error("TLS configuration failed: {0}")]
    Config(String),
    /// The authenticated key no longer satisfies the endpoint trust policy.
    #[error("peer trust classification failed: {0}")]
    Trust(String),
    /// The endpoint was closed before another connection arrived.
    #[error("endpoint is closed")]
    EndpointClosed,
    /// The negotiated TLS session did not select `olf/1`.
    #[error("TLS did not negotiate the Flow ALPN")]
    AlpnMismatch,
    /// Quinn did not expose exactly one peer certificate.
    #[error("TLS peer identity is missing or malformed")]
    MissingPeerIdentity,
    /// The local Hello does not match the local TLS identity or wire invariants.
    #[error("invalid local Hello: {0}")]
    InvalidLocalHello(&'static str),
    /// The peer's control stream violated the Hello binding.
    #[error("invalid Flow control stream")]
    InvalidControlFrame,
    /// Local negotiation rejected the peer's Hello.
    #[error(transparent)]
    HelloRejected(#[from] HelloRejection),
    /// The peer sent a generated HelloReject reason.
    #[error("peer rejected Hello with reason {0}")]
    PeerHelloRejected(i32),
    /// The caller attempted to send a kind excluded by negotiation.
    #[error("negotiation does not permit sending {0:?}")]
    SendGated(FrameKind),
    /// The caller attempted to send a post-trust kind before pairing completed.
    #[error("cannot send {0:?} before the peer is paired")]
    NotPaired(FrameKind),
    /// A kind does not occupy an RPC request position.
    #[error("{0:?} is not an RPC request kind")]
    InvalidRequestKind(FrameKind),
    /// A response does not match its request stream.
    #[error("unexpected RPC response kind {0:?}")]
    UnexpectedResponse(FrameKind),
    /// A clipboard/file RPC must use the ordered bulk-response API.
    #[error("{0:?} requires a ClipboardData + Chunk* + ChunkEnd response")]
    BulkResponseRequired(FrameKind),
    /// The protobuf payload does not match its frame kind.
    #[error("invalid protobuf payload for {0:?}")]
    InvalidPayload(FrameKind),
    /// A kind does not occupy a notification stream.
    #[error("{0:?} is not a notification kind")]
    InvalidNotificationKind(FrameKind),
    /// A kind is not legal in a Flow datagram.
    #[error("{0:?} is not a Ping/Pong datagram kind")]
    InvalidDatagramKind(FrameKind),
    /// A stream contained bytes after its one permitted frame.
    #[error("one-frame stream contained trailing data")]
    TrailingStreamData,
    /// The pairing session did not authenticate this connection's peer key.
    #[error("pairing is not complete for this peer")]
    PairingNotComplete,
    /// The operating system's secure random generator failed.
    #[error(transparent)]
    Random(#[from] RandomError),
    /// TLS plus the `Hello` exchange did not finish within the handshake deadline.
    #[error("Flow handshake did not finish in time")]
    HandshakeTimeout,
    /// An inbound stream did not deliver its frame and FIN within the read deadline.
    #[error("inbound stream did not finish in time")]
    StreamTimeout,
    /// Another unknown peer holds the single pairing slot.
    #[error("another unknown peer is already pairing")]
    PairingBusy,
}

fn quic_configs(
    identity: &MachineIdentity,
    trust: PeerTrust,
) -> Result<(quinn::ServerConfig, quinn::ClientConfig), TransportError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_verifier = Arc::new(tls::PinnedServerVerifier::new(
        trust.clone(),
        provider.clone(),
    ));
    let client_verifier = Arc::new(tls::PinnedClientVerifier::new(trust, provider.clone()));

    let mut client_tls = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|error| TransportError::Config(error.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(server_verifier)
        .with_client_auth_cert(vec![identity.certificate()], identity.private_key())?;
    client_tls.alpn_protocols = vec![ALPN.to_vec()];
    client_tls.resumption = rustls::client::Resumption::disabled();

    let mut server_tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|error| TransportError::Config(error.to_string()))?
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(vec![identity.certificate()], identity.private_key())?;
    server_tls.alpn_protocols = vec![ALPN.to_vec()];
    server_tls.max_early_data_size = 0;

    let client_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(client_tls)
        .map_err(|error| TransportError::Config(error.to_string()))?;
    let server_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(server_tls)
        .map_err(|error| TransportError::Config(error.to_string()))?;
    Ok((
        quinn::ServerConfig::with_crypto(Arc::new(server_crypto)),
        quinn::ClientConfig::new(Arc::new(client_crypto)),
    ))
}

fn validate_local_hello(hello: &proto::Hello, public_key: PublicKey) -> Result<(), TransportError> {
    if hello.public_key.as_slice() != public_key.as_bytes() {
        return Err(TransportError::InvalidLocalHello(
            "public key differs from TLS identity",
        ));
    }
    if !hello.session_nonce.is_empty() {
        return Err(TransportError::InvalidLocalHello(
            "session nonce must be empty; the endpoint draws one per connection",
        ));
    }
    if hello.proto_min == 0 || hello.proto_min > hello.proto_max {
        return Err(TransportError::InvalidLocalHello(
            "protocol version range is invalid",
        ));
    }
    Ok(())
}

fn verify_alpn(connection: &Connection) -> Result<(), TransportError> {
    let handshake = connection
        .handshake_data()
        .and_then(|data| data.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
        .ok_or(TransportError::AlpnMismatch)?;
    if handshake.protocol.as_deref() == Some(ALPN) {
        Ok(())
    } else {
        Err(TransportError::AlpnMismatch)
    }
}

fn connection_peer_key(connection: &Connection) -> Result<PublicKey, TransportError> {
    let certificates = connection
        .peer_identity()
        .and_then(|identity| identity.downcast::<Vec<CertificateDer<'static>>>().ok())
        .ok_or(TransportError::MissingPeerIdentity)?;
    if certificates.len() != 1 {
        return Err(TransportError::MissingPeerIdentity);
    }
    public_key_from_certificate(certificates[0].as_ref()).map_err(TransportError::from)
}

async fn reject_hello(
    connection: &Connection,
    send: &mut SendStream,
    local: &proto::Hello,
    reason: proto::RejectReason,
    detail: &str,
) -> Result<(), TransportError> {
    message_envelope(
        FrameKind::HelloReject,
        &proto::HelloReject {
            reason: reason.into(),
            proto_min: local.proto_min,
            proto_max: local.proto_max,
            detail: detail.to_owned(),
            ..Default::default()
        },
    )?
    .write_to(send)
    .await?;
    send.finish()?;
    connection.close(CLOSE_PROTOCOL, b"Hello rejected");
    Ok(())
}

async fn ensure_stream_finished(receive: &mut RecvStream) -> Result<(), TransportError> {
    match receive.read_chunk(1, true).await? {
        None => Ok(()),
        Some(_) => Err(TransportError::TrailingStreamData),
    }
}

/// Resets both directions of a stream whose peer missed the read deadline.
fn abandon_stream(receive: &mut RecvStream, send: Option<&mut SendStream>) -> TransportError {
    let _ = receive.stop(STOP_TIMEOUT);
    if let Some(send) = send {
        let _ = send.reset(STOP_TIMEOUT);
    }
    TransportError::StreamTimeout
}

async fn reject_rpc(
    mut send: SendStream,
    code: proto::ErrorCode,
    detail: impl ToString,
) -> Result<RpcEvent, TransportError> {
    error_envelope(code, detail.to_string())?
        .write_to(&mut send)
        .await?;
    send.finish()?;
    Ok(RpcEvent::Rejected(code))
}

fn expected_response(request: FrameKind) -> Option<&'static [FrameKind]> {
    match request {
        FrameKind::PairStart => Some(&[FrameKind::PairPrompted]),
        FrameKind::PairReveal => Some(&[FrameKind::PairRevealed]),
        FrameKind::PairConfirm => Some(&[FrameKind::PairOutcome]),
        FrameKind::GetPeerInfo => Some(&[FrameKind::PeerInfo]),
        FrameKind::HandoffRequest => Some(&[FrameKind::HandoffAccept, FrameKind::HandoffReject]),
        FrameKind::ClipboardFetch | FrameKind::FileFetch => Some(&[FrameKind::ClipboardData]),
        _ => None,
    }
}

const fn is_bulk_request(kind: FrameKind) -> bool {
    matches!(kind, FrameKind::ClipboardFetch | FrameKind::FileFetch)
}

fn is_request_kind(kind: FrameKind) -> bool {
    expected_response(kind).is_some()
}

fn is_notification_kind(kind: FrameKind) -> bool {
    matches!(
        kind,
        FrameKind::PairAbort
            | FrameKind::AnnounceDevices
            | FrameKind::PeerState
            | FrameKind::HandoffResult
            | FrameKind::HandoffCancel
            | FrameKind::ClipboardAnnounce
    )
}

fn validate_sequence_frame(envelope: &Envelope, expected: FrameKind) -> Result<(), TransportError> {
    if envelope.kind != expected {
        return Err(TransportError::UnexpectedResponse(envelope.kind));
    }
    validate_payload(envelope)
}

fn validate_response_envelope(envelope: &Envelope) -> Result<(), TransportError> {
    if envelope.policy(InboundRole::Request).is_some() || !payload_decodes(envelope) {
        Err(TransportError::InvalidPayload(envelope.kind))
    } else {
        Ok(())
    }
}

fn validate_payload(envelope: &Envelope) -> Result<(), TransportError> {
    if envelope.flags != 0 || !payload_decodes(envelope) {
        Err(TransportError::InvalidPayload(envelope.kind))
    } else {
        Ok(())
    }
}

fn payload_decodes(envelope: &Envelope) -> bool {
    macro_rules! decodes {
        ($message:ty) => {
            <$message as Message>::decode_from_slice(&envelope.payload).is_ok()
        };
    }

    match envelope.kind {
        FrameKind::Hello => decodes!(proto::Hello),
        FrameKind::HelloReject => decodes!(proto::HelloReject),
        FrameKind::Error => decodes!(proto::Error),
        FrameKind::Ping => decodes!(proto::Ping),
        FrameKind::Pong => decodes!(proto::Pong),
        FrameKind::PairStart => decodes!(proto::PairStart),
        FrameKind::PairPrompted => decodes!(proto::PairPrompted),
        FrameKind::PairConfirm => decodes!(proto::PairConfirm),
        FrameKind::PairOutcome => decodes!(proto::PairOutcome),
        FrameKind::PairAbort => decodes!(proto::PairAbort),
        FrameKind::PairReveal => decodes!(proto::PairReveal),
        FrameKind::PairRevealed => decodes!(proto::PairRevealed),
        FrameKind::GetPeerInfo => decodes!(proto::GetPeerInfo),
        FrameKind::PeerInfo => decodes!(proto::PeerInfo),
        FrameKind::AnnounceDevices => decodes!(proto::AnnounceDevices),
        FrameKind::PeerState => decodes!(proto::PeerState),
        FrameKind::HandoffRequest => decodes!(proto::HandoffRequest),
        FrameKind::HandoffAccept => decodes!(proto::HandoffAccept),
        FrameKind::HandoffReject => decodes!(proto::HandoffReject),
        FrameKind::HandoffResult => decodes!(proto::HandoffResult),
        FrameKind::HandoffCancel => decodes!(proto::HandoffCancel),
        FrameKind::ClipboardAnnounce => decodes!(proto::ClipboardAnnounce),
        FrameKind::ClipboardFetch => decodes!(proto::ClipboardFetch),
        FrameKind::ClipboardData => decodes!(proto::ClipboardData),
        FrameKind::Chunk => decodes!(proto::Chunk),
        FrameKind::ChunkEnd => decodes!(proto::ChunkEnd),
        FrameKind::FileFetch => decodes!(proto::FileFetch),
        FrameKind::Unspecified | FrameKind::Unknown(_) => false,
    }
}

impl FrameKind {
    const fn is_pretrust(self) -> bool {
        matches!(self.wire_value(), 0x0001..=0x001f)
    }
}
