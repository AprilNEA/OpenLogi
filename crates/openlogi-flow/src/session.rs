//! Long-lived peer sessions over discovered address hints.
//!
//! The manager owns connection establishment and Ping/Pong datagrams. It does
//! not consume RPC or notification streams; embedders obtain the current
//! [`FlowConnection`] through [`PeerSessionHandle`] and retain ownership of
//! application protocol handling.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
    time::Duration,
};

use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    runtime::Handle,
    sync::{mpsc, watch},
    task::{JoinHandle, JoinSet},
    time::{Instant, MissedTickBehavior},
};

use crate::{
    discovery::{CandidateSource, collect_candidates},
    frame::{FrameKind, InboundRole},
    generated as proto,
    sas::PublicKey,
    transport::{
        ConnectionDirection, FlowConnection, FlowEndpoint, SessionTrust, message_envelope,
    },
};

const INCOMING_QUEUE: usize = 4;
const TRUST_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Churn-safe state of a configured peer session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkState {
    /// QUIC and Hello are established and liveness is healthy or in its initial grace period.
    Connected,
    /// QUIC remains established, but no valid application-level Pong arrived within the limit.
    Degraded,
    /// No established QUIC connection exists.
    Lost,
}

/// Timing policy for dialing and application-level liveness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionPolicy {
    /// Delay before the first retry, doubled for each consecutive failure.
    pub retry_base: Duration,
    /// Maximum retry delay after deterministic ±25% per-peer jitter.
    pub retry_max: Duration,
    /// Interval between Ping datagrams.
    pub ping_interval: Duration,
    /// Time without a valid Pong before [`LinkState::Degraded`].
    pub degraded_after: Duration,
}

impl Default for SessionPolicy {
    fn default() -> Self {
        Self {
            retry_base: Duration::from_millis(250),
            retry_max: Duration::from_secs(30),
            ping_interval: Duration::from_secs(5),
            degraded_after: Duration::from_secs(15),
        }
    }
}

impl SessionPolicy {
    fn validate(self) -> Result<(), SessionManagerError> {
        if self.retry_base.is_zero() {
            return Err(SessionManagerError::InvalidPolicy(
                "retry_base must be nonzero",
            ));
        }
        if self.retry_max < self.retry_base {
            return Err(SessionManagerError::InvalidPolicy(
                "retry_max must not be shorter than retry_base",
            ));
        }
        if self.ping_interval.is_zero() {
            return Err(SessionManagerError::InvalidPolicy(
                "ping_interval must be nonzero",
            ));
        }
        if self.degraded_after <= self.ping_interval {
            return Err(SessionManagerError::InvalidPolicy(
                "degraded_after must be longer than ping_interval",
            ));
        }
        Ok(())
    }
}

/// Discovery configuration for one pinned peer identity.
pub struct PeerConfig {
    /// TLS-authenticated Ed25519 peer key.
    pub public_key: PublicKey,
    /// Concurrent sources of untrusted address hints.
    pub sources: Vec<Arc<dyn CandidateSource>>,
}

impl fmt::Debug for PeerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PeerConfig")
            .field("public_key", &self.public_key)
            .field("source_count", &self.sources.len())
            .finish()
    }
}

/// Whole-state payloads sent once whenever a session becomes trusted.
#[derive(Clone, Debug, Default)]
pub struct TrustedInitialState {
    /// Complete current device inventory.
    pub announce_devices: proto::AnnounceDevices,
    /// Complete current coarse peer state.
    pub peer_state: proto::PeerState,
}

/// Embedder-owned provider for state that has no dependency on OpenLogi device types.
pub trait TrustedStateProvider: Send + Sync {
    /// Returns the current whole-state notifications for `peer_key`.
    fn initial_state(&self, peer_key: PublicKey) -> TrustedInitialState;
}

/// Watchable state and current connection for one configured peer.
#[derive(Clone, Debug)]
pub struct PeerSessionHandle {
    public_key: PublicKey,
    state: watch::Receiver<LinkState>,
    connection: watch::Receiver<Option<Arc<FlowConnection>>>,
}

impl PeerSessionHandle {
    /// Returns the configured peer identity.
    #[must_use]
    pub const fn public_key(&self) -> PublicKey {
        self.public_key
    }

    /// Returns the most recently published coarse state.
    #[must_use]
    pub fn state(&self) -> LinkState {
        *self.state.borrow()
    }

    /// Subscribes to coarse state changes.
    #[must_use]
    pub fn subscribe_state(&self) -> watch::Receiver<LinkState> {
        self.state.clone()
    }

    /// Returns the current live connection, if one exists.
    ///
    /// The manager retains authority to close and replace this connection.
    #[must_use]
    pub fn connection(&self) -> Option<Arc<FlowConnection>> {
        self.connection.borrow().clone()
    }

    /// Subscribes to live-connection replacement.
    #[must_use]
    pub fn subscribe_connection(&self) -> watch::Receiver<Option<Arc<FlowConnection>>> {
        self.connection.clone()
    }
}

/// Owns one reconnecting session worker per configured peer key.
pub struct SessionManager {
    shutdown: watch::Sender<bool>,
    accept_task: Option<JoinHandle<()>>,
    peer_tasks: Vec<JoinHandle<()>>,
    peers: BTreeMap<PublicKey, PeerSessionHandle>,
    _provider: Arc<dyn TrustedStateProvider>,
}

impl fmt::Debug for SessionManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionManager")
            .field("peer_count", &self.peers.len())
            .finish_non_exhaustive()
    }
}

impl SessionManager {
    /// Starts accepting and dialing sessions on `endpoint`.
    ///
    /// The endpoint's TLS trust policy remains the source of peer identity;
    /// configured candidate addresses never grant trust.
    pub fn start(
        endpoint: Arc<FlowEndpoint>,
        peers: impl IntoIterator<Item = PeerConfig>,
        provider: Arc<dyn TrustedStateProvider>,
        policy: SessionPolicy,
    ) -> Result<Self, SessionManagerError> {
        policy.validate()?;
        let runtime = Handle::try_current()
            .map_err(|error| SessionManagerError::Runtime(error.to_string()))?;
        let peers = peers.into_iter().collect::<Vec<_>>();
        let mut unique = BTreeSet::new();
        for peer in &peers {
            if peer.public_key == endpoint.public_key() {
                return Err(SessionManagerError::LocalPeer(peer.public_key));
            }
            if !unique.insert(peer.public_key) {
                return Err(SessionManagerError::DuplicatePeer(peer.public_key));
            }
        }

        let (shutdown, shutdown_rx) = watch::channel(false);
        let mut routes = BTreeMap::new();
        let mut handles = BTreeMap::new();
        let mut peer_tasks = Vec::with_capacity(peers.len());
        for peer in peers {
            let (incoming_tx, incoming_rx) = mpsc::channel(INCOMING_QUEUE);
            let (state_tx, state_rx) = watch::channel(LinkState::Lost);
            let (connection_tx, connection_rx) = watch::channel(None);
            let public_key = peer.public_key;
            routes.insert(public_key, incoming_tx);
            handles.insert(
                public_key,
                PeerSessionHandle {
                    public_key,
                    state: state_rx,
                    connection: connection_rx,
                },
            );
            let signals = PeerSignals {
                state: state_tx,
                connection: connection_tx,
            };
            peer_tasks.push(runtime.spawn(run_peer(
                Arc::clone(&endpoint),
                peer,
                incoming_rx,
                signals,
                Arc::clone(&provider),
                policy,
                shutdown_rx.clone(),
            )));
        }
        let accept_task = runtime.spawn(accept_connections(endpoint, routes, shutdown_rx));
        Ok(Self {
            shutdown,
            accept_task: Some(accept_task),
            peer_tasks,
            peers: handles,
            _provider: provider,
        })
    }

    /// Returns the handle for one configured peer.
    #[must_use]
    pub fn peer(&self, public_key: PublicKey) -> Option<&PeerSessionHandle> {
        self.peers.get(&public_key)
    }

    /// Iterates all configured peer handles in public-key order.
    #[must_use]
    pub fn peers(&self) -> impl ExactSizeIterator<Item = &PeerSessionHandle> {
        self.peers.values()
    }

    /// Stops all workers and closes their live connections without closing the shared endpoint.
    pub async fn shutdown(mut self) {
        self.signal_shutdown();
        if let Some(task) = self.accept_task.take() {
            let _ = task.await;
        }
        for task in self.peer_tasks.drain(..) {
            let _ = task.await;
        }
    }

    fn signal_shutdown(&self) {
        self.shutdown.send_replace(true);
        for peer in self.peers.values() {
            if let Some(connection) = peer.connection() {
                connection.close();
            }
        }
    }
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        self.signal_shutdown();
        if let Some(task) = self.accept_task.take() {
            task.abort();
        }
        for task in &self.peer_tasks {
            task.abort();
        }
    }
}

/// Configuration errors that prevent a session manager from starting.
#[derive(Debug, Error)]
pub enum SessionManagerError {
    /// A timing relationship would make retry or liveness behavior invalid.
    #[error("invalid session policy: {0}")]
    InvalidPolicy(&'static str),
    /// One peer key appeared more than once.
    #[error("duplicate configured peer key {0:?}")]
    DuplicatePeer(PublicKey),
    /// The local machine key was incorrectly configured as a peer.
    #[error("local machine key cannot be configured as peer {0:?}")]
    LocalPeer(PublicKey),
    /// The manager was started outside a Tokio runtime.
    #[error("Tokio runtime is unavailable: {0}")]
    Runtime(String),
}

async fn accept_connections(
    endpoint: Arc<FlowEndpoint>,
    routes: BTreeMap<PublicKey, mpsc::Sender<FlowConnection>>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        tokio::select! {
            accepted = endpoint.accept() => match accepted {
                Ok(connection) => {
                    let Some(route) = routes.get(&connection.peer_key()) else {
                        connection.close();
                        continue;
                    };
                    if let Err(error) = route.try_send(connection) {
                        error.into_inner().close();
                    }
                }
                Err(crate::transport::TransportError::EndpointClosed) => return,
                Err(_) => {}
            },
            _ = shutdown.changed() => return,
        }
    }
}

struct PeerSignals {
    state: watch::Sender<LinkState>,
    connection: watch::Sender<Option<Arc<FlowConnection>>>,
}

async fn run_peer(
    endpoint: Arc<FlowEndpoint>,
    peer: PeerConfig,
    mut incoming: mpsc::Receiver<FlowConnection>,
    signals: PeerSignals,
    provider: Arc<dyn TrustedStateProvider>,
    policy: SessionPolicy,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut retry_attempt = 0;
    let mut pending_connection = None;
    loop {
        let connection = if let Some(connection) = pending_connection.take() {
            connection
        } else {
            match acquire_connection(&endpoint, &peer, &mut incoming, &mut shutdown).await {
                AcquireOutcome::Connection(connection) => {
                    retry_attempt = 0;
                    connection
                }
                AcquireOutcome::Unavailable => {
                    let delay = retry_delay(policy, peer.public_key, retry_attempt);
                    retry_attempt = retry_attempt.saturating_add(1);
                    match wait_before_retry(delay, &mut incoming, &mut shutdown).await {
                        WaitOutcome::Connection(connection) => connection,
                        WaitOutcome::Elapsed => continue,
                        WaitOutcome::Shutdown => return,
                    }
                }
                AcquireOutcome::Shutdown => return,
            }
        };
        signals.state.send_replace(LinkState::Connected);
        signals
            .connection
            .send_replace(Some(Arc::clone(&connection)));

        match run_connection(
            &connection,
            &mut incoming,
            &signals.state,
            provider.as_ref(),
            policy,
            &mut shutdown,
        )
        .await
        {
            LiveOutcome::Replace(replacement) => {
                pending_connection = Some(replacement);
            }
            LiveOutcome::Closed => {
                connection.close();
                signals.connection.send_replace(None);
                signals.state.send_replace(LinkState::Lost);
                retry_attempt = 0;
                match wait_before_retry(
                    retry_delay(policy, peer.public_key, retry_attempt),
                    &mut incoming,
                    &mut shutdown,
                )
                .await
                {
                    WaitOutcome::Connection(replacement) => {
                        pending_connection = Some(replacement);
                    }
                    WaitOutcome::Elapsed => {
                        retry_attempt = 1;
                    }
                    WaitOutcome::Shutdown => return,
                }
            }
            LiveOutcome::Shutdown => {
                connection.close();
                signals.connection.send_replace(None);
                signals.state.send_replace(LinkState::Lost);
                return;
            }
        }
    }
}

enum AcquireOutcome {
    Connection(Arc<FlowConnection>),
    Unavailable,
    Shutdown,
}

async fn acquire_connection(
    endpoint: &Arc<FlowEndpoint>,
    peer: &PeerConfig,
    incoming: &mut mpsc::Receiver<FlowConnection>,
    shutdown: &mut watch::Receiver<bool>,
) -> AcquireOutcome {
    if *shutdown.borrow() {
        return AcquireOutcome::Shutdown;
    }
    let dial = dial_candidates(endpoint, peer);
    tokio::pin!(dial);
    let mut pending_incoming = None;
    loop {
        tokio::select! {
            dialed = &mut dial => {
                return match (dialed, pending_incoming) {
                    (Some(outgoing), Some(incoming)) if endpoint.public_key() < peer.public_key => {
                        outgoing.close();
                        AcquireOutcome::Connection(incoming)
                    }
                    (Some(outgoing), Some(incoming)) => {
                        incoming.close();
                        AcquireOutcome::Connection(outgoing)
                    }
                    (Some(outgoing), None) => AcquireOutcome::Connection(outgoing),
                    (None, Some(incoming)) => AcquireOutcome::Connection(incoming),
                    (None, None) => AcquireOutcome::Unavailable,
                };
            }
            candidate = incoming.recv() => {
                let Some(candidate) = candidate else {
                    return AcquireOutcome::Unavailable;
                };
                let candidate = Arc::new(candidate);
                if endpoint.public_key() < peer.public_key {
                    return AcquireOutcome::Connection(candidate);
                }
                if pending_incoming.is_some() {
                    candidate.close();
                } else {
                    pending_incoming = Some(candidate);
                }
            }
            _ = shutdown.changed() => return AcquireOutcome::Shutdown,
        }
    }
}

async fn dial_candidates(
    endpoint: &Arc<FlowEndpoint>,
    peer: &PeerConfig,
) -> Option<Arc<FlowConnection>> {
    let addresses = collect_candidates(&peer.sources, peer.public_key)
        .await
        .ok()?;
    let mut attempts = JoinSet::new();
    for address in addresses {
        let endpoint = Arc::clone(endpoint);
        attempts.spawn(async move { endpoint.connect(address).await });
    }
    while let Some(result) = attempts.join_next().await {
        if let Ok(Ok(connection)) = result {
            if connection.peer_key() == peer.public_key {
                return Some(Arc::new(connection));
            }
            connection.close();
        }
    }
    None
}

enum WaitOutcome {
    Connection(Arc<FlowConnection>),
    Elapsed,
    Shutdown,
}

async fn wait_before_retry(
    delay: Duration,
    incoming: &mut mpsc::Receiver<FlowConnection>,
    shutdown: &mut watch::Receiver<bool>,
) -> WaitOutcome {
    if *shutdown.borrow() {
        return WaitOutcome::Shutdown;
    }
    tokio::select! {
        () = tokio::time::sleep(delay) => WaitOutcome::Elapsed,
        candidate = incoming.recv() => match candidate {
            Some(candidate) => WaitOutcome::Connection(Arc::new(candidate)),
            None => WaitOutcome::Elapsed,
        },
        _ = shutdown.changed() => WaitOutcome::Shutdown,
    }
}

enum LiveOutcome {
    Replace(Arc<FlowConnection>),
    Closed,
    Shutdown,
}

async fn run_connection(
    connection: &Arc<FlowConnection>,
    incoming: &mut mpsc::Receiver<FlowConnection>,
    state: &watch::Sender<LinkState>,
    provider: &dyn TrustedStateProvider,
    policy: SessionPolicy,
    shutdown: &mut watch::Receiver<bool>,
) -> LiveOutcome {
    let now = Instant::now();
    let mut last_pong = now;
    let mut highest_sent = 0_u64;
    let mut highest_pong = 0_u64;
    let mut announced = false;
    if send_initial_state(connection, provider, &mut announced)
        .await
        .is_err()
    {
        return LiveOutcome::Closed;
    }

    let mut heartbeat = tokio::time::interval_at(now + policy.ping_interval, policy.ping_interval);
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut trust = tokio::time::interval(TRUST_POLL_INTERVAL);
    trust.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            datagram = connection.read_datagram() => {
                let Ok(datagram) = datagram else {
                    return LiveOutcome::Closed;
                };
                match datagram.kind {
                    FrameKind::Ping => {
                        let Ok(message) = datagram.decode::<proto::Ping>(InboundRole::Notification) else {
                            return LiveOutcome::Closed;
                        };
                        let Ok(pong) = message_envelope(FrameKind::Pong, &proto::Pong {
                            seq: message.seq,
                            ..Default::default()
                        }) else {
                            return LiveOutcome::Closed;
                        };
                        if connection.send_datagram(&pong).is_err() {
                            return LiveOutcome::Closed;
                        }
                    }
                    FrameKind::Pong => {
                        let Ok(message) = datagram.decode::<proto::Pong>(InboundRole::Notification) else {
                            return LiveOutcome::Closed;
                        };
                        if message.seq > highest_pong && message.seq <= highest_sent {
                            highest_pong = message.seq;
                            last_pong = Instant::now();
                            state.send_replace(LinkState::Connected);
                        }
                    }
                    _ => return LiveOutcome::Closed,
                }
            }
            candidate = incoming.recv() => {
                let Some(candidate) = candidate else {
                    return LiveOutcome::Closed;
                };
                if connection.direction() == ConnectionDirection::Outgoing
                    && connection.close_if_outgoing_loses_tie()
                {
                    return LiveOutcome::Replace(Arc::new(candidate));
                }
                candidate.close();
            }
            _ = heartbeat.tick() => {
                if Instant::now().duration_since(last_pong) >= policy.degraded_after {
                    state.send_replace(LinkState::Degraded);
                }
                highest_sent = highest_sent.saturating_add(1);
                let Ok(ping) = message_envelope(FrameKind::Ping, &proto::Ping {
                    seq: highest_sent,
                    ..Default::default()
                }) else {
                    return LiveOutcome::Closed;
                };
                if connection.send_datagram(&ping).is_err() {
                    return LiveOutcome::Closed;
                }
            }
            _ = trust.tick() => {
                if send_initial_state(connection, provider, &mut announced).await.is_err() {
                    return LiveOutcome::Closed;
                }
            }
            () = connection.wait_closed() => return LiveOutcome::Closed,
            _ = shutdown.changed() => return LiveOutcome::Shutdown,
        }
    }
}

async fn send_initial_state(
    connection: &FlowConnection,
    provider: &dyn TrustedStateProvider,
    announced: &mut bool,
) -> Result<(), ()> {
    if *announced || connection.trust() != SessionTrust::Trusted {
        return Ok(());
    }
    let initial = provider.initial_state(connection.peer_key());
    let announce =
        message_envelope(FrameKind::AnnounceDevices, &initial.announce_devices).map_err(|_| ())?;
    connection.notify(announce).await.map_err(|_| ())?;
    let peer_state = message_envelope(FrameKind::PeerState, &initial.peer_state).map_err(|_| ())?;
    connection.notify(peer_state).await.map_err(|_| ())?;
    *announced = true;
    Ok(())
}

fn retry_delay(policy: SessionPolicy, peer_key: PublicKey, attempt: u32) -> Duration {
    let multiplier = 1_u32.checked_shl(attempt.min(31)).unwrap_or(u32::MAX);
    let exponential = policy.retry_base.saturating_mul(multiplier);
    let capped = exponential.min(policy.retry_max);
    let mut hasher = Sha256::new();
    hasher.update(peer_key.as_bytes());
    hasher.update(attempt.to_le_bytes());
    let digest = hasher.finalize();
    let sample = u16::from_be_bytes([digest[0], digest[1]]);
    let per_mille = 750_u128 + u128::from(sample % 501);
    let nanos = capped.as_nanos().saturating_mul(per_mille) / 1_000;
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX)).min(policy.retry_max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> SessionPolicy {
        SessionPolicy {
            retry_base: Duration::from_millis(100),
            retry_max: Duration::from_secs(2),
            ping_interval: Duration::from_secs(1),
            degraded_after: Duration::from_secs(3),
        }
    }

    #[test]
    fn retry_backoff_is_deterministic_jittered_and_capped() {
        let policy = policy();
        let key = PublicKey::new([1; 32]);
        let first = retry_delay(policy, key, 0);
        let second = retry_delay(policy, key, 1);
        let third = retry_delay(policy, key, 2);

        assert_eq!(first, retry_delay(policy, key, 0));
        assert!((Duration::from_millis(75)..=Duration::from_millis(125)).contains(&first));
        assert!(second > first);
        assert!(third > second);
        assert!(retry_delay(policy, key, u32::MAX) <= policy.retry_max);
    }

    #[test]
    fn retry_jitter_is_scoped_to_peer_key() {
        assert_ne!(
            retry_delay(policy(), PublicKey::new([1; 32]), 2),
            retry_delay(policy(), PublicKey::new([2; 32]), 2)
        );
    }

    #[test]
    fn invalid_liveness_policy_is_rejected() {
        let mut invalid = policy();
        invalid.degraded_after = invalid.ping_interval;
        assert!(matches!(
            invalid.validate(),
            Err(SessionManagerError::InvalidPolicy(_))
        ));
    }
}
