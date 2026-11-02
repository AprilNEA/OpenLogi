//! One running Flow generation.
//!
//! [`FlowRuntime::start`] binds the endpoint, starts mDNS, the peer sessions
//! and the pointer thread, and hands every piece of mutable Flow state to one
//! actor task. Nothing else holds Flow state, so there is no lock to wait on:
//! peer tasks and the pointer thread send the actor events, and the actor only
//! queues frames for the peer writers. A configuration change ends the
//! generation and starts a new one.

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use openlogi_core::config::{FlowConfig, FlowPeer};
use openlogi_flow::discovery::{
    CandidateSource, DEFAULT_PORT, DiscoveryError, ManualCandidateSource, MdnsAdvertiser,
    MdnsCandidateSource, MdnsRecord,
};
use openlogi_flow::frame::Envelope;
use openlogi_flow::generated as proto;
use openlogi_flow::sas::PublicKey;
use openlogi_flow::session::{
    LinkState, PeerConfig, SessionManager, SessionManagerError, SessionPolicy, TrustedInitialState,
    TrustedStateProvider,
};
use openlogi_flow::transport::{
    FlowEndpoint, IncomingRpc, MachineIdentity, PeerTrust, TransportError,
};
use thiserror::Error;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::devices::InventoryDevice;
use super::identity::{self, IdentityLoadError};
use super::pointer::{PointerThread, PointerWaker};
use crate::observable::ObservableState;

mod actor;
mod link;
mod sender;
#[cfg(test)]
mod tests;

use actor::{Actor, ActorInputs, ActorPeer};
pub(crate) use sender::FlowHardware;
use sender::SendReport;

const PROTOCOL_MIN: u32 = 1;
const PROTOCOL_MAX: u32 = 1;
/// Events from the peer tasks waiting for the actor.
const EVENT_QUEUE: usize = 64;
/// Transfer frames waiting for one peer's writer.
const FRAME_QUEUE: usize = 16;
/// Edge crossings waiting for the actor. A crossing that does not fit is
/// stale by the time the actor could act on it.
const CROSSING_QUEUE: usize = 1;

/// What the actor hears from the peer tasks.
enum Event {
    /// A peer's link state, sent once at start and on every change.
    Link { peer: PublicKey, state: LinkState },
    /// A request the peer opened; the actor owns the response.
    Request { peer: PublicKey, rpc: IncomingRpc },
    /// A notification the peer sent.
    Notification { peer: PublicKey, envelope: Envelope },
    /// An outgoing transfer ended.
    Sent {
        transfer_id: u64,
        report: SendReport,
    },
}

/// What a generation needs from the agent.
pub(crate) struct FlowStart {
    /// The configuration this generation runs.
    pub(crate) config: Arc<FlowConfig>,
    /// This host's devices, republished on every change.
    pub(crate) inventory: watch::Receiver<Arc<[InventoryDevice]>>,
    /// Where the generation publishes its peer status.
    pub(crate) observable: Arc<ObservableState>,
    /// The device access outgoing transfers use.
    pub(crate) hardware: FlowHardware,
}

/// Why a generation could not start.
#[derive(Debug, Error)]
pub(crate) enum FlowRuntimeError {
    /// The machine identity is unavailable.
    #[error("the Flow identity is unavailable")]
    Identity(#[from] IdentityLoadError),
    /// The identity task panicked.
    #[error("the Flow identity task failed")]
    IdentityTask,
    /// The endpoint could not bind.
    #[error("the Flow endpoint could not start")]
    Transport(#[from] TransportError),
    /// The mDNS record could not be built.
    #[error("the Flow mDNS record is invalid")]
    Discovery(#[from] DiscoveryError),
    /// The peer sessions could not start.
    #[error("the Flow peer sessions could not start")]
    Session(#[from] SessionManagerError),
    /// The pointer thread could not start.
    #[error("the Flow pointer thread could not start")]
    Pointer(#[source] std::io::Error),
}

/// The state the session layer sends a peer the moment it becomes trusted.
struct WholeState(watch::Receiver<TrustedInitialState>);

impl TrustedStateProvider for WholeState {
    fn initial_state(&self, _peer_key: PublicKey) -> TrustedInitialState {
        self.0.borrow().clone()
    }
}

/// A running generation. Call [`Self::stop`] to end it; dropping it aborts
/// the tasks without closing the endpoint cleanly.
pub(crate) struct FlowRuntime {
    stop: watch::Sender<bool>,
    actor: JoinHandle<()>,
    tasks: Vec<JoinHandle<()>>,
    sessions: SessionManager,
    endpoint: Arc<FlowEndpoint>,
    waker: PointerWaker,
    advertiser: Option<MdnsAdvertiser>,
}

/// Start the three tasks of each configured peer: link-state forwarding,
/// stream reading, and frame writing.
fn spawn_links(
    sessions: &SessionManager,
    writers: Vec<(PublicKey, mpsc::Receiver<Envelope>)>,
    events: &mpsc::Sender<Event>,
    state: &watch::Receiver<TrustedInitialState>,
    stop: &watch::Receiver<bool>,
) -> Vec<JoinHandle<()>> {
    let mut tasks = Vec::with_capacity(writers.len() * 3);
    for (key, frames) in writers {
        let Some(handle) = sessions.peer(key).cloned() else {
            continue;
        };
        tasks.push(tokio::spawn(link::watch_link(
            handle.clone(),
            events.clone(),
            stop.clone(),
        )));
        tasks.push(tokio::spawn(link::read_peer(
            handle.clone(),
            events.clone(),
            stop.clone(),
        )));
        tasks.push(tokio::spawn(link::write_peer(
            handle,
            frames,
            state.clone(),
            stop.clone(),
        )));
    }
    tasks
}

/// Where and as whom a generation listens.
struct Binding {
    identity: MachineIdentity,
    address: SocketAddr,
    /// Advertise and browse with mDNS. Tests use manual addresses only.
    mdns: bool,
    /// Open the session's pointer probe. Without it, as on Wayland or with
    /// no display, the generation still receives handoffs.
    probe: bool,
}

impl FlowRuntime {
    /// Start a generation for `start.config`.
    pub(crate) async fn start(start: FlowStart) -> Result<Self, FlowRuntimeError> {
        let identity = tokio::task::spawn_blocking(identity::load)
            .await
            .map_err(|_| FlowRuntimeError::IdentityTask)??;
        Self::start_on(
            start,
            Binding {
                identity,
                address: SocketAddr::from((Ipv6Addr::UNSPECIFIED, DEFAULT_PORT)),
                mdns: true,
                probe: true,
            },
        )
    }

    fn start_on(start: FlowStart, binding: Binding) -> Result<Self, FlowRuntimeError> {
        let FlowStart {
            config,
            inventory,
            observable,
            hardware,
        } = start;
        let Binding {
            identity,
            address,
            mdns,
            probe,
        } = binding;
        let local_key = identity.public_key();
        let peer_keys: Vec<PublicKey> = config
            .peers()
            .iter()
            .map(|peer| PublicKey::new(*peer.public_key.as_bytes()))
            .collect();
        let endpoint = Arc::new(bind(
            address,
            identity,
            PeerTrust::pinned(peer_keys.iter().copied()),
            hello(local_key),
        )?);
        let (advertiser, browser) = if mdns {
            (advertise(&endpoint)?, browse())
        } else {
            (None, None)
        };

        let (stop, stop_rx) = watch::channel(false);
        let (events_tx, events_rx) = mpsc::channel(EVENT_QUEUE);
        let (crossings_tx, crossings_rx) = mpsc::channel(CROSSING_QUEUE);
        let (state_tx, state_rx) = watch::channel(TrustedInitialState::default());
        let pointer =
            PointerThread::start(probe, crossings_tx).map_err(FlowRuntimeError::Pointer)?;
        let waker = pointer.waker();

        let mut writers = Vec::with_capacity(peer_keys.len());
        let mut peers = Vec::with_capacity(peer_keys.len());
        for (peer, key) in config.peers().iter().zip(&peer_keys) {
            let (frames_tx, frames_rx) = mpsc::channel(FRAME_QUEUE);
            writers.push((*key, frames_rx));
            peers.push(ActorPeer::new(*key, peer, frames_tx));
        }
        // The actor publishes the initial state before any session can
        // become trusted and read it.
        let mut actor = Actor::new(
            Arc::clone(&config),
            peers,
            inventory,
            state_tx,
            pointer,
            observable,
            hardware,
            ActorInputs {
                events: events_rx,
                events_tx: events_tx.clone(),
                crossings: crossings_rx,
                stop: stop_rx.clone(),
            },
        );

        let sessions = SessionManager::start(
            Arc::clone(&endpoint),
            config
                .peers()
                .iter()
                .zip(&peer_keys)
                .map(|(peer, key)| peer_config(peer, *key, browser.as_ref())),
            Arc::new(WholeState(state_rx.clone())),
            SessionPolicy::default(),
        )?;

        actor.attach(&sessions);
        let tasks = spawn_links(&sessions, writers, &events_tx, &state_rx, &stop_rx);
        info!(peers = peer_keys.len(), "Flow runtime started");
        Ok(Self {
            stop,
            actor: tokio::spawn(actor.run()),
            tasks,
            sessions,
            endpoint,
            waker,
            advertiser,
        })
    }

    /// The address the endpoint listens on.
    #[cfg(test)]
    fn local_addr(&self) -> SocketAddr {
        self.endpoint
            .local_addr()
            .expect("a bound endpoint has an address")
    }

    /// The waker the OS hook callback calls on pointer motion.
    pub(crate) fn pointer_waker(&self) -> PointerWaker {
        self.waker.clone()
    }

    /// End the generation: stop the actor and the pointer thread, close the
    /// sessions, then the endpoint.
    pub(crate) async fn stop(self) {
        let _ = self.stop.send(true);
        for task in &self.tasks {
            task.abort();
        }
        if self.actor.await.is_err() {
            warn!("the Flow actor panicked");
        }
        self.sessions.shutdown().await;
        drop(self.advertiser);
        // Every connection is closed, so this is the last reference unless a
        // response task still holds a stream; then the socket closes when it
        // ends.
        if let Some(endpoint) = Arc::into_inner(self.endpoint) {
            endpoint.close().await;
        }
        info!("Flow runtime stopped");
    }
}

/// Milliseconds since the Unix epoch, or zero before it.
fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

/// Where the session looks for `peer`: mDNS when available, then its manual
/// addresses. Addresses are hints; the pinned key decides trust.
fn peer_config(
    peer: &FlowPeer,
    key: PublicKey,
    browser: Option<&Arc<dyn CandidateSource>>,
) -> PeerConfig {
    let mut sources = Vec::<Arc<dyn CandidateSource>>::new();
    if let Some(browser) = browser {
        sources.push(Arc::clone(browser));
    }
    if !peer.addresses.is_empty() {
        sources.push(Arc::new(ManualCandidateSource::new(
            peer.addresses.iter().cloned(),
        )));
    }
    PeerConfig {
        public_key: key,
        sources,
    }
}

/// Bind `address`. The IPv6 wildcard binds dual-stack, so a peer reachable
/// only over IPv6 connects too; on a host without IPv6 it falls back to the
/// IPv4 wildcard on the same port.
fn bind(
    address: SocketAddr,
    identity: MachineIdentity,
    trust: PeerTrust,
    hello: proto::Hello,
) -> Result<FlowEndpoint, TransportError> {
    match FlowEndpoint::bind(address, identity.clone(), trust.clone(), hello.clone()) {
        Err(TransportError::Io(error))
            if address.ip() == Ipv6Addr::UNSPECIFIED
                && error.kind() != io::ErrorKind::AddrInUse =>
        {
            debug!(%error, "Flow cannot listen on IPv6 here; listening on IPv4 only");
            FlowEndpoint::bind(
                SocketAddr::from((Ipv4Addr::UNSPECIFIED, address.port())),
                identity,
                trust,
                hello,
            )
        }
        result => result,
    }
}

fn hello(local_key: PublicKey) -> proto::Hello {
    proto::Hello {
        proto_min: PROTOCOL_MIN,
        proto_max: PROTOCOL_MAX,
        public_key: local_key.as_bytes().to_vec(),
        machine_name: machine_name(),
        platform: platform().into(),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        ..Default::default()
    }
}

fn advertise(endpoint: &FlowEndpoint) -> Result<Option<MdnsAdvertiser>, FlowRuntimeError> {
    let record = MdnsRecord::new(endpoint.public_key(), PROTOCOL_MIN, PROTOCOL_MAX)?;
    let port = endpoint.local_addr()?.port();
    match MdnsAdvertiser::start(record, port) {
        Ok(advertiser) => Ok(Some(advertiser)),
        Err(error) => {
            warn!(%error, "Flow mDNS advertisement is unavailable; manual addresses still work");
            Ok(None)
        }
    }
}

fn browse() -> Option<Arc<dyn CandidateSource>> {
    match MdnsCandidateSource::browse(PROTOCOL_MIN, PROTOCOL_MAX) {
        Ok(browser) => Some(Arc::new(browser)),
        Err(error) => {
            warn!(%error, "Flow mDNS browsing is unavailable; only manual addresses work");
            None
        }
    }
}

/// The name peers show for this machine. Presentation only.
fn machine_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "OpenLogi".to_owned())
}

fn platform() -> proto::Platform {
    if cfg!(target_os = "macos") {
        proto::Platform::Macos
    } else if cfg!(target_os = "windows") {
        proto::Platform::Windows
    } else if cfg!(target_os = "linux") {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            proto::Platform::LinuxWayland
        } else {
            proto::Platform::LinuxX11
        }
    } else {
        proto::Platform::Other
    }
}
