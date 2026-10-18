use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use openlogi_flow::clipboard::{files_sync_enabled, text_sync_enabled};
use openlogi_flow::discovery::{
    CandidateSource, DEFAULT_PORT, ManualCandidateSource, MdnsAdvertiser, MdnsCandidateSource,
    MdnsRecord,
};
use openlogi_flow::frame::FrameKind;
use openlogi_flow::generated as proto;
use openlogi_flow::sas::PublicKey;
use openlogi_flow::session::{
    LinkState, PeerConfig, PeerSessionHandle, SessionManager, SessionPolicy, TrustedInitialState,
    TrustedStateProvider,
};
use openlogi_flow::transport::{
    FlowConnection, FlowEndpoint, MachineIdentity, NotificationEvent, PeerTrust, RpcEvent,
    TransportError, message_envelope,
};
use openlogi_hid::{ChannelPool, DeviceRoute, HostSwitchTarget};
use openlogi_hook::edge::{EdgeSide, ExposedEdges};
use openlogi_ipc::{FlowLinkState, FlowPeerStatus, FlowStatus};
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::clipboard::ClipboardSync;
use super::config::CompiledFlowConfig;
use super::handoff::{HandoffBook, handle_notification, handle_rpc, inventory_changed};
use super::{FlowDeviceSnapshot, RuntimeDevice, is_pointing_device};
use crate::observable::ObservableState;
use crate::receiver_access::ReceiverAccess;

const FLOW_IDENTITY_FILE: &str = "flow-identity.pk8";
const PROTOCOL_MIN: u32 = 1;
const PROTOCOL_MAX: u32 = 1;
/// How long a stopping generation waits for its connections to drain.
const ENDPOINT_DRAIN: std::time::Duration = std::time::Duration::from_secs(2);
/// Binding the port is tried this many times, `BIND_RETRY` apart, while the
/// previous generation still holds it.
const BIND_ATTEMPTS: u32 = 30;
const BIND_RETRY: std::time::Duration = std::time::Duration::from_millis(100);

mod controller;
mod input;

pub use controller::{FlowController, FlowInputHandle};
pub(crate) use input::FlowInput;

struct FlowGeneration {
    state: Arc<GenerationState>,
    sessions: SessionManager,
    tasks: Vec<JoinHandle<()>>,
    /// Waits on keystrokes rather than on the sessions, so it is aborted
    /// rather than awaited at shutdown.
    keyboard_follower: JoinHandle<()>,
    _advertiser: Option<MdnsAdvertiser>,
    endpoint: Arc<FlowEndpoint>,
}

impl FlowGeneration {
    async fn start(
        config: Arc<CompiledFlowConfig>,
        snapshots: &[FlowDeviceSnapshot],
        observable: Arc<ObservableState>,
        channel_pool: ChannelPool,
        receiver_access: ReceiverAccess,
        input: Arc<FlowInput>,
    ) -> Result<Self, FlowRuntimeError> {
        let identity = tokio::task::spawn_blocking(load_machine_identity)
            .await
            .map_err(|error| FlowRuntimeError::IdentityTask(error.to_string()))??;
        let clipboard = ClipboardSync::system(config.clipboard);
        let hello = proto::Hello {
            proto_min: PROTOCOL_MIN,
            proto_max: PROTOCOL_MAX,
            public_key: identity.public_key().as_bytes().to_vec(),
            session_nonce: rand::random::<[u8; 16]>().to_vec(),
            machine_name: machine_name(),
            platform: platform().into(),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            capabilities: clipboard
                .capabilities()
                .into_iter()
                .map(Into::into)
                .collect(),
            ..Default::default()
        };
        let endpoint = Arc::new(
            bind_endpoint(
                SocketAddr::from((Ipv4Addr::UNSPECIFIED, DEFAULT_PORT)),
                identity,
                PeerTrust::pinned(config.peers.iter().map(|peer| peer.public_key)),
                hello,
            )
            .await?,
        );
        let record = MdnsRecord::new(endpoint.public_key(), PROTOCOL_MIN, PROTOCOL_MAX)?;
        let advertiser = match MdnsAdvertiser::start(record, endpoint.local_addr()?.port()) {
            Ok(advertiser) => Some(advertiser),
            Err(error) => {
                warn!(%error, "Flow mDNS advertisement unavailable — manual addresses remain active");
                None
            }
        };
        let browser: Option<Arc<dyn CandidateSource>> =
            match MdnsCandidateSource::browse(PROTOCOL_MIN, PROTOCOL_MAX) {
                Ok(browser) => Some(Arc::new(browser)),
                Err(error) => {
                    warn!(%error, "Flow mDNS browsing unavailable — manual addresses only");
                    None
                }
            };
        let peers = config.peers.iter().map(|peer| {
            let mut sources = Vec::<Arc<dyn CandidateSource>>::new();
            if let Some(browser) = &browser {
                sources.push(Arc::clone(browser));
            }
            if !peer.addresses.is_empty() {
                sources.push(Arc::new(ManualCandidateSource::new(peer.addresses.clone())));
            }
            PeerConfig {
                public_key: peer.public_key,
                sources,
            }
        });
        let state = Arc::new(GenerationState::new(
            Arc::clone(&config),
            snapshots,
            observable,
            channel_pool,
            receiver_access,
            input,
            clipboard,
        ));
        prepare_switch_targets(&state);
        let provider: Arc<dyn TrustedStateProvider> = state.clone();
        let sessions = SessionManager::start(
            Arc::clone(&endpoint),
            peers,
            provider,
            SessionPolicy {
                retry_max: PEER_RETRY_MAX,
                ..SessionPolicy::default()
            },
        )?;
        let handles: Vec<_> = sessions.peers().cloned().collect();
        let mut tasks = Vec::with_capacity(handles.len() * 2);
        for handle in handles {
            tasks.push(tokio::spawn(watch_link_state(
                Arc::clone(&state),
                handle.clone(),
            )));
            tasks.push(tokio::spawn(watch_connection(Arc::clone(&state), handle)));
        }
        let keyboard_follower = tokio::spawn(follow_keyboards(Arc::clone(&state)));
        info!(peers = config.peers.len(), "Flow runtime armed");
        Ok(Self {
            state,
            sessions,
            tasks,
            keyboard_follower,
            _advertiser: advertiser,
            endpoint,
        })
    }

    async fn update_devices(&self, snapshots: &[FlowDeviceSnapshot]) {
        self.state.update_devices(snapshots);
        prepare_switch_targets(&self.state);
        self.state.publish_device_state().await;
        inventory_changed(Arc::clone(&self.state)).await;
    }

    async fn shutdown(mut self) {
        {
            let _lifecycle = self.state.lifecycle.lock().await;
            self.state.active.store(false, Ordering::Release);
        }
        self.keyboard_follower.abort();
        self.sessions.shutdown().await;
        for task in self.tasks.drain(..) {
            let _ = task.await;
        }
        // The next generation binds the same port, which stays taken while
        // any connection of this one is alive: drop the ones this generation
        // holds and drain the rest. `bind_endpoint` waits out stragglers.
        if let Ok(mut connections) = self.state.connections.write() {
            connections.clear();
        }
        if tokio::time::timeout(ENDPOINT_DRAIN, self.endpoint.close())
            .await
            .is_err()
        {
            warn!("Flow endpoint still draining at shutdown");
        }
    }
}

/// Bind Flow's port, retrying while it is still taken. A reload restarts
/// the runtime, and the previous generation releases the socket only once
/// its last connection handle is gone.
async fn bind_endpoint(
    address: SocketAddr,
    identity: MachineIdentity,
    trust: PeerTrust,
    hello: proto::Hello,
) -> Result<FlowEndpoint, TransportError> {
    let mut attempt = 1;
    loop {
        match FlowEndpoint::bind(address, identity.clone(), trust.clone(), hello.clone()) {
            Err(TransportError::Io(error))
                if error.kind() == io::ErrorKind::AddrInUse && attempt < BIND_ATTEMPTS =>
            {
                debug!(attempt, "Flow port still taken, retrying");
                attempt += 1;
                tokio::time::sleep(BIND_RETRY).await;
            }
            result => return result,
        }
    }
}

pub(super) struct GenerationState {
    pub(super) config: Arc<CompiledFlowConfig>,
    pub(super) devices: RwLock<Vec<RuntimeDevice>>,
    connections: RwLock<HashMap<PublicKey, Arc<FlowConnection>>>,
    link_states: RwLock<HashMap<PublicKey, LinkState>>,
    observable: Arc<ObservableState>,
    channel_pool: ChannelPool,
    receiver_access: ReceiverAccess,
    device_revision: AtomicU64,
    peer_revision: AtomicU64,
    active: AtomicBool,
    pub(super) lifecycle: Mutex<()>,
    pub(super) handoffs: HandoffBook,
    pub(super) input: Arc<FlowInput>,
    pub(super) clipboard: ClipboardSync,
    /// Online devices already prepared for a one-write switch; `None` while
    /// a preparation is in flight.
    switch_targets: std::sync::Mutex<Vec<(DeviceRoute, Option<Arc<HostSwitchTarget>>)>>,
    /// Where Flow itself last moved each device, by config key, and when.
    presence: std::sync::Mutex<HashMap<String, (Presence, std::time::Instant)>>,
    /// The peer this machine last sent the devices to.
    handed_off_to: std::sync::Mutex<Option<String>>,
}

/// What Flow knows first-hand about a device's whereabouts.
///
/// The inventory takes seconds to notice a device arriving over or leaving a
/// receiver. Flow moved it, so for a while its own record is the better one:
/// without it a device that just arrived cannot be sent on ("no online
/// devices"), and a peer's request for one that just left is refused as
/// "already online".
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Presence {
    /// A handoff to this machine was accepted for the device.
    Arrived,
    /// This machine switched the device to a peer.
    Departed,
}

/// Longest wait between attempts to reach a lost peer. A peer is a machine on
/// the same desk: after it restarts or wakes, the link should be back within
/// seconds, not after the session layer's 30 s default backoff ceiling. One
/// QUIC handshake attempt every few seconds toward an absent peer costs nothing.
const PEER_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(5);

/// How long a switch waits for its receiver I/O lease before going ahead.
/// The shared lease is free unless a pairing session holds the receiver.
const LEASE_PATIENCE: std::time::Duration = std::time::Duration::from_millis(20);

/// How long Flow's own record outranks the inventory.
const PRESENCE_TTL: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Clone)]
pub(super) struct OutgoingDevice {
    key: String,
    route: DeviceRoute,
    host: u8,
    pointing: bool,
    pub(super) identity: proto::DeviceIdentity,
}

/// When a device that should have left is checked for still answering here.
/// Early: on two PCs a keyboard missed the switch about once in thirty
/// crossings, and each check-and-resend round is typing delay.
const DEPARTURE_CHECK_DELAY: std::time::Duration = std::time::Duration::from_millis(150);
/// How long that check waits for an answer.
const DEPARTURE_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(400);
/// How many times a device that did not leave is told again.
const DEPARTURE_RETRIES: u32 = 3;
/// When modifiers a departed keyboard left down here are released.
const MODIFIER_RELEASE_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

impl GenerationState {
    fn new(
        config: Arc<CompiledFlowConfig>,
        snapshots: &[FlowDeviceSnapshot],
        observable: Arc<ObservableState>,
        channel_pool: ChannelPool,
        receiver_access: ReceiverAccess,
        input: Arc<FlowInput>,
        clipboard: ClipboardSync,
    ) -> Self {
        let devices = runtime_devices(&config, snapshots);
        Self {
            config,
            devices: RwLock::new(devices),
            connections: RwLock::new(HashMap::new()),
            link_states: RwLock::new(HashMap::new()),
            observable,
            channel_pool,
            receiver_access,
            device_revision: AtomicU64::new(1),
            peer_revision: AtomicU64::new(1),
            active: AtomicBool::new(true),
            lifecycle: Mutex::new(()),
            handoffs: HandoffBook::default(),
            input,
            clipboard,
            switch_targets: std::sync::Mutex::new(Vec::new()),
            presence: std::sync::Mutex::new(HashMap::new()),
            handed_off_to: std::sync::Mutex::new(None),
        }
    }

    /// Record that Flow moved the devices with these config keys.
    pub(super) fn note_presence<'a>(
        &self,
        keys: impl IntoIterator<Item = &'a str>,
        presence: Presence,
    ) {
        if let Ok(mut record) = self.presence.lock() {
            let now = std::time::Instant::now();
            for key in keys {
                record.insert(key.to_owned(), (presence, now));
            }
        }
    }

    /// Drop the arrival an accepted handoff assumed for these config keys: the
    /// devices never came.
    pub(super) fn forget_arrival(&self, keys: &[String]) {
        if let Ok(mut record) = self.presence.lock() {
            for key in keys {
                if record
                    .get(key)
                    .is_some_and(|(presence, _)| *presence == Presence::Arrived)
                {
                    record.remove(key);
                }
            }
        }
    }

    /// The devices with their inventory online state corrected by what Flow
    /// itself did within [`PRESENCE_TTL`].
    pub(super) fn effective_devices(&self) -> Vec<RuntimeDevice> {
        let record = self.presence.lock().ok();
        self.devices_snapshot()
            .into_iter()
            .map(|mut device| {
                let recent = record.as_ref().and_then(|record| {
                    record
                        .get(&device.snapshot.config_key)
                        .filter(|(_, at)| at.elapsed() < PRESENCE_TTL)
                        .map(|(presence, _)| *presence)
                });
                match recent {
                    Some(Presence::Arrived) => device.snapshot.online = true,
                    Some(Presence::Departed) => device.snapshot.online = false,
                    None => {}
                }
                device
            })
            .collect()
    }

    pub(super) fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    fn update_devices(&self, snapshots: &[FlowDeviceSnapshot]) {
        if let Ok(mut devices) = self.devices.write() {
            *devices = runtime_devices(&self.config, snapshots);
            self.device_revision.fetch_add(1, Ordering::Relaxed);
            self.peer_revision.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(super) fn devices_snapshot(&self) -> Vec<RuntimeDevice> {
        self.devices
            .read()
            .map_or_else(|_| Vec::new(), |devices| devices.clone())
    }

    pub(super) fn connection(&self, peer: PublicKey) -> Option<Arc<FlowConnection>> {
        self.connections
            .read()
            .ok()
            .and_then(|connections| connections.get(&peer).cloned())
    }

    fn set_connection(&self, peer: PublicKey, connection: Option<Arc<FlowConnection>>) {
        if let Ok(mut connections) = self.connections.write() {
            match connection {
                Some(connection) => {
                    connections.insert(peer, connection);
                }
                None => {
                    connections.remove(&peer);
                }
            }
        }
    }

    async fn set_link_state(&self, peer: PublicKey, state: LinkState) {
        let _lifecycle = self.lifecycle.lock().await;
        if !self.is_active() {
            return;
        }
        if let Ok(mut states) = self.link_states.write() {
            states.insert(peer, state);
        }
        self.publish_status();
    }

    fn publish_status(&self) {
        let states = self.link_states.read().ok();
        self.observable.set_flow(FlowStatus {
            enabled: self.config.enabled,
            peers: self
                .config
                .peers
                .iter()
                .map(|peer| FlowPeerStatus {
                    name: peer.name.clone(),
                    public_key: peer.canonical_key.clone(),
                    state: states
                        .as_ref()
                        .and_then(|states| states.get(&peer.public_key))
                        .copied()
                        .map_or(FlowLinkState::Lost, ipc_link_state),
                })
                .collect(),
            local_public_key: None,
        });
    }

    pub(super) fn outgoing_devices(&self, peer: &str) -> Vec<OutgoingDevice> {
        let mut devices: Vec<_> = self
            .effective_devices()
            .into_iter()
            .filter(|device| device.snapshot.online)
            .filter_map(|device| {
                let route = device.snapshot.route.clone()?;
                let host = device.channels.get(peer).copied()?;
                Some((
                    device.snapshot.kind,
                    OutgoingDevice {
                        key: device.snapshot.config_key,
                        route,
                        host,
                        pointing: is_pointing_device(device.snapshot.kind),
                        identity: device.identity,
                    },
                ))
            })
            .collect();
        devices.sort_by_key(|(kind, _)| {
            if is_pointing_device(*kind) {
                0
            } else if *kind == openlogi_core::device::DeviceKind::Keyboard {
                2
            } else {
                1
            }
        });
        devices.into_iter().map(|(_, device)| device).collect()
    }

    pub(super) async fn switch_devices(
        &self,
        devices: &[OutgoingDevice],
    ) -> Result<bool, openlogi_hid::HostSwitchError> {
        let started = std::time::Instant::now();
        // A shared I/O lease, not the exclusive one: exclusive access waits for
        // every in-flight receiver operation and asks the button capture to
        // stand down, which it never did in time — every switch made while
        // capture was active waited out the full patience (~105 ms) for
        // nothing. The switch is one write per device and the devices are
        // leaving, so their capture ends on its own. The shared lease still
        // queues behind a pairing session; past a short patience it goes ahead.
        let lease = tokio::time::timeout(LEASE_PATIENCE, self.receiver_access.acquire_for_io())
            .await
            .ok();
        let leased = started.elapsed();
        if !self.is_active() {
            return Ok(false);
        }
        let prepared = self.prepared_targets(devices);
        let fast = match &prepared {
            Some(prepared) => {
                let mut outcome = Ok(());
                for (target, host) in prepared {
                    if let Err(error) = target.switch_to(*host).await {
                        outcome = Err(error);
                        break;
                    }
                }
                if let Err(error) = &outcome {
                    warn!(%error, "prepared Flow host switch failed — re-preparing");
                    self.forget_switch_targets();
                }
                outcome.is_ok()
            }
            None => false,
        };
        if !fast {
            let targets: Vec<_> = devices
                .iter()
                .map(|device| (device.route.clone(), device.host))
                .collect();
            openlogi_hid::switch_hosts(&targets, &self.channel_pool).await?;
        }
        if fast && let Some(prepared) = prepared {
            // The mouse is busy crossing and takes the write at once; a
            // keyboard idling in its power-saving state can miss it.
            let unconfirmed: Vec<_> = prepared
                .into_iter()
                .zip(devices)
                .filter(|(_, device)| !device.pointing)
                .map(|(target, _)| target)
                .collect();
            if !unconfirmed.is_empty() {
                tokio::spawn(confirm_departures(unconfirmed));
            }
        }
        self.note_presence(
            devices.iter().map(|device| device.key.as_str()),
            Presence::Departed,
        );
        info!(
            lease_ms = leased.as_millis(),
            switch_ms = started.elapsed().saturating_sub(leased).as_millis(),
            devices = devices.len(),
            prepared = fast,
            leased = lease.is_some(),
            "Flow host switch issued"
        );
        Ok(true)
    }

    /// The prepared switch handle and destination for every device, or `None`
    /// when any of them is not prepared yet.
    fn prepared_targets(
        &self,
        devices: &[OutgoingDevice],
    ) -> Option<Vec<(Arc<HostSwitchTarget>, u8)>> {
        let cache = self.switch_targets.lock().ok()?;
        devices
            .iter()
            .map(|device| {
                cache
                    .iter()
                    .find(|(route, _)| *route == device.route)
                    .and_then(|(_, target)| target.clone())
                    .map(|target| (target, device.host))
            })
            .collect()
    }

    fn forget_switch_targets(&self) {
        if let Ok(mut cache) = self.switch_targets.lock() {
            cache.clear();
        }
    }

    /// The configured keyboards (every non-pointing device) that can go to
    /// `peer`, whatever their online state: a keystroke here is the proof.
    fn keyboards_for(&self, peer: &str) -> Vec<OutgoingDevice> {
        self.devices_snapshot()
            .into_iter()
            .filter(|device| !is_pointing_device(device.snapshot.kind))
            .filter_map(|device| {
                let route = device.snapshot.route.clone()?;
                let host = device.channels.get(peer).copied()?;
                Some(OutgoingDevice {
                    key: device.snapshot.config_key,
                    route,
                    host,
                    pointing: false,
                    identity: device.identity,
                })
            })
            .collect()
    }

    /// Record the peer this machine just sent the devices to.
    pub(super) fn set_handed_off_to(&self, peer: &str) {
        if let Ok(mut handed_off_to) = self.handed_off_to.lock() {
            *handed_off_to = Some(peer.to_owned());
        }
        self.input.departed();
        // A keyboard that left with a modifier down (Ctrl held to cross, say)
        // never sends its release here. Give a receiver that releases keys on
        // disconnect time to do so first.
        tokio::spawn(async {
            tokio::time::sleep(MODIFIER_RELEASE_DELAY).await;
            let released = openlogi_inject::release_stuck_modifiers();
            if released > 0 {
                info!(
                    released,
                    "released modifier keys the departed keyboard left down"
                );
            }
        });
    }

    fn handed_off_to(&self) -> Option<String> {
        self.handed_off_to.lock().ok().and_then(|peer| peer.clone())
    }

    pub(super) async fn send_result(&self, peer: PublicKey, result: proto::HandoffResult) {
        let Some(connection) = self.connection(peer) else {
            return;
        };
        if let Ok(envelope) = message_envelope(FrameKind::HandoffResult, &result) {
            let _ = connection.notify(envelope).await;
        }
    }

    pub(super) async fn send_cancel(
        &self,
        peer: PublicKey,
        transfer_id: u64,
        reason: proto::HandoffCancelReason,
    ) {
        let Some(connection) = self.connection(peer) else {
            return;
        };
        let cancel = proto::HandoffCancel {
            transfer_id,
            reason: reason.into(),
            ..Default::default()
        };
        if let Ok(envelope) = message_envelope(FrameKind::HandoffCancel, &cancel) {
            let _ = connection.notify(envelope).await;
        }
    }

    async fn publish_device_state(&self) {
        let initial = self.initial_state(PublicKey::new([0; 32]));
        let connections: Vec<_> = self.connections.read().map_or_else(
            |_| Vec::new(),
            |connections| connections.values().cloned().collect(),
        );
        for connection in connections {
            if let Ok(envelope) =
                message_envelope(FrameKind::AnnounceDevices, &initial.announce_devices)
            {
                let _ = connection.notify(envelope).await;
            }
            if let Ok(envelope) = message_envelope(FrameKind::PeerState, &initial.peer_state) {
                let _ = connection.notify(envelope).await;
            }
        }
    }
}

impl TrustedStateProvider for GenerationState {
    fn initial_state(&self, _peer_key: PublicKey) -> TrustedInitialState {
        let devices = self.devices_snapshot();
        TrustedInitialState {
            announce_devices: proto::AnnounceDevices {
                devices: devices
                    .iter()
                    .map(|device| {
                        let mut view = proto::DeviceView {
                            channel_to_me: u32::from(
                                device.channels.get("self").copied().unwrap_or_default(),
                            ),
                            connected: device.snapshot.online,
                            host_count: device
                                .channels
                                .values()
                                .copied()
                                .max()
                                .map_or(0, |host| u32::from(host) + 1),
                            ..Default::default()
                        };
                        *view.identity.get_or_insert_default() = device.identity.clone();
                        view
                    })
                    .collect(),
                revision: self.device_revision.load(Ordering::Relaxed),
                ..Default::default()
            },
            peer_state: proto::PeerState {
                flow_enabled: self.config.enabled,
                held: devices
                    .iter()
                    .filter(|device| device.snapshot.online)
                    .map(|device| device.identity.clone())
                    .collect(),
                revision: self.peer_revision.load(Ordering::Relaxed),
                ..Default::default()
            },
        }
    }
}

fn runtime_devices(
    config: &CompiledFlowConfig,
    snapshots: &[FlowDeviceSnapshot],
) -> Vec<RuntimeDevice> {
    snapshots
        .iter()
        .filter_map(|snapshot| {
            let channels = config.devices.get(&snapshot.config_key)?.clone();
            let identity = snapshot.identity();
            (!identity.ids.is_empty()).then(|| RuntimeDevice {
                snapshot: snapshot.clone(),
                identity,
                channels,
            })
        })
        .collect()
}

async fn watch_link_state(state: Arc<GenerationState>, handle: PeerSessionHandle) {
    let peer = handle.public_key();
    let mut changes = handle.subscribe_state();
    let current = *changes.borrow_and_update();
    state.set_link_state(peer, current).await;
    while changes.changed().await.is_ok() {
        let current = *changes.borrow_and_update();
        state.set_link_state(peer, current).await;
    }
    state.set_link_state(peer, LinkState::Lost).await;
}

async fn watch_connection(state: Arc<GenerationState>, handle: PeerSessionHandle) {
    let peer = handle.public_key();
    let mut changes = handle.subscribe_connection();
    let mut application: Option<JoinHandle<()>> = None;
    loop {
        if let Some(task) = application.take() {
            task.abort();
            let _ = task.await;
        }
        let connection = changes.borrow_and_update().clone();
        state.set_connection(peer, connection.clone());
        if let Some(connection) = connection {
            info!(
                peer = state
                    .config
                    .peers
                    .iter()
                    .find(|configured| configured.public_key == peer)
                    .map_or("?", |configured| configured.name.as_str()),
                clipboard_text = text_sync_enabled(&connection),
                clipboard_files = files_sync_enabled(&connection),
                "Flow peer connected"
            );
            application = Some(tokio::spawn(run_application_connection(
                Arc::clone(&state),
                peer,
                connection,
            )));
        }
        if changes.changed().await.is_err() {
            break;
        }
    }
    if let Some(task) = application {
        task.abort();
        let _ = task.await;
    }
    state.set_connection(peer, None);
}

async fn run_application_connection(
    state: Arc<GenerationState>,
    peer: PublicKey,
    connection: Arc<FlowConnection>,
) {
    // One loop per stream kind, never a `select!` over both accepts: neither
    // is cancel-safe, so the losing branch would drop a stream it had
    // already accepted, and the request or notification on it.
    let rpcs = async {
        let clipboard = state.clipboard.connection(peer, Arc::clone(&connection));
        loop {
            match connection.accept_rpc().await {
                Ok(RpcEvent::Request(rpc))
                    if matches!(
                        rpc.request().kind,
                        FrameKind::ClipboardFetch | FrameKind::FileFetch
                    ) =>
                {
                    clipboard.serve(rpc);
                }
                Ok(event @ (RpcEvent::Request(_) | RpcEvent::Rejected(_))) => {
                    handle_rpc(Arc::clone(&state), peer, event).await;
                }
                Err(_) => return,
            }
        }
    };
    let notifications = async {
        let mut clipboard = state.clipboard.connection(peer, Arc::clone(&connection));
        loop {
            match connection.accept_notification().await {
                Ok(NotificationEvent::Notification(announce))
                    if announce.kind == FrameKind::ClipboardAnnounce =>
                {
                    clipboard.announced(&announce);
                }
                Ok(
                    event @ (NotificationEvent::Notification(_) | NotificationEvent::Dropped(_)),
                ) => {
                    handle_notification(Arc::clone(&state), peer, event).await;
                }
                Err(_) => return,
            }
        }
    };
    // The connection is gone once either accept fails.
    tokio::select! {
        () = rpcs => {}
        () = notifications => {}
    }
}

pub(super) fn warp_entry(entry: &proto::EntryPoint) {
    let Some(side) = entry.side.as_known() else {
        return;
    };
    let displays = openlogi_hook::display_rects();
    let edges = ExposedEdges::from_displays(&displays);
    let Some((x, y)) = point_on_edge(&edges, side, entry.t) else {
        return;
    };
    if openlogi_inject::warp_cursor(x, y) {
        openlogi_hook::reseed_motion_baseline(openlogi_hook::CursorPosition { x, y });
    } else {
        warn!("Flow device arrived, but cursor warp is unsupported on this display server");
    }
}

/// Re-send the switch to a device that still answers here after it.
///
/// `setCurrentHost` is fire-and-forget, and a keyboard idling in its
/// power-saving state can miss it: the mouse leaves and the keyboard silently
/// stays. A device that left, or sleeps, no longer answers on this receiver;
/// one that answers did not leave, so it is told again.
async fn confirm_departures(targets: Vec<(Arc<HostSwitchTarget>, u8)>) {
    for (target, host) in targets {
        for attempt in 1..=DEPARTURE_RETRIES {
            tokio::time::sleep(DEPARTURE_CHECK_DELAY).await;
            if !target.answers(DEPARTURE_CHECK_TIMEOUT).await {
                break;
            }
            warn!(
                attempt,
                host, "a Flow device still answers after its switch — sending it again"
            );
            if let Err(error) = target.switch_to(host).await {
                warn!(%error, "repeating the Flow switch failed");
                break;
            }
        }
    }
}

/// Send a keyboard that stayed behind after a handoff on to the machine the
/// pointer went to, whenever a keystroke here proves it did.
async fn follow_keyboards(state: Arc<GenerationState>) {
    loop {
        state.input.keyboard_stayed.notified().await;
        if !state.is_active() {
            return;
        }
        // The keystroke was queued before an arrival or local movement put
        // the keyboard back here: it stays.
        if !state.input.keyboard_handed_off() {
            continue;
        }
        let Some(peer_name) = state.handed_off_to() else {
            continue;
        };
        let Some(peer) = state
            .config
            .peers
            .iter()
            .find(|peer| peer.name == peer_name)
        else {
            continue;
        };
        if state.connection(peer.public_key).is_none() {
            warn!(
                peer = %peer_name,
                "keystroke here after a handoff, but the peer is unreachable — keeping the keyboard"
            );
            continue;
        }
        let keyboards = state.keyboards_for(&peer_name);
        if keyboards.is_empty() {
            continue;
        }
        warn!(
            peer = %peer_name,
            devices = keyboards.len(),
            "Flow keyboard stayed behind after the handoff — sending it on"
        );
        if let Err(error) = state.switch_devices(&keyboards).await {
            warn!(%error, "sending the keyboard on failed");
        }
    }
}

fn point_on_edge(edges: &ExposedEdges, side: proto::Side, t: f64) -> Option<(f64, f64)> {
    const INSET: f64 = 1.0;
    let side = match side {
        proto::Side::Left => EdgeSide::Left,
        proto::Side::Right => EdgeSide::Right,
        proto::Side::Top => EdgeSide::Top,
        proto::Side::Bottom => EdgeSide::Bottom,
        proto::Side::Unspecified => return None,
    };
    let segments: Vec<_> = edges.for_side(side).collect();
    let total: f64 = segments
        .iter()
        .map(|segment| segment.end() - segment.start())
        .sum();
    if total <= 0.0 {
        return None;
    }
    let mut offset = t.clamp(0.0, 1.0) * total;
    let segment = segments.iter().find(|segment| {
        let length = segment.end() - segment.start();
        if offset <= length {
            true
        } else {
            offset -= length;
            false
        }
    })?;
    let along = (segment.start() + offset).min(segment.end());
    Some(match side {
        EdgeSide::Left => (segment.coordinate() + INSET, along),
        EdgeSide::Right => (segment.coordinate() - INSET, along),
        EdgeSide::Top => (along, segment.coordinate() + INSET),
        EdgeSide::Bottom => (along, segment.coordinate() - INSET),
    })
}

const fn ipc_link_state(state: LinkState) -> FlowLinkState {
    match state {
        LinkState::Connected => FlowLinkState::Connected,
        LinkState::Degraded => FlowLinkState::Degraded,
        LinkState::Lost => FlowLinkState::Lost,
    }
}

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
    } else if cfg!(target_os = "linux") {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            proto::Platform::LinuxWayland
        } else {
            proto::Platform::LinuxX11
        }
    } else if cfg!(target_os = "windows") {
        proto::Platform::Windows
    } else {
        proto::Platform::Other
    }
}

/// Keep a one-write switch prepared for every online device Flow can move, so
/// the switch at the screen edge skips the HID++ feature lookups. Preparation
/// runs in the background under an ordinary I/O lease; a device that cannot be
/// prepared now is retried on the next inventory change.
pub(super) fn prepare_switch_targets(state: &Arc<GenerationState>) {
    let devices = state.effective_devices();
    // A prepared target survives the device leaving: it comes back to the
    // same receiver slot, where the handle is still valid, and a round trip
    // is exactly when there is no time to prepare it again. A failed write
    // clears the cache instead.
    let known: Vec<DeviceRoute> = devices
        .iter()
        .filter_map(|device| device.snapshot.route.clone())
        .collect();
    let wanted: Vec<(DeviceRoute, Vec<u8>)> = devices
        .into_iter()
        .filter(|device| device.snapshot.online)
        .filter_map(|device| {
            let route = device.snapshot.route.clone()?;
            let hosts: Vec<u8> = device.channels.values().copied().collect();
            (!hosts.is_empty()).then_some((route, hosts))
        })
        .collect();
    let missing: Vec<_> = {
        let Ok(mut cache) = state.switch_targets.lock() else {
            return;
        };
        cache.retain(|(route, _)| known.contains(route));
        let missing: Vec<_> = wanted
            .into_iter()
            .filter(|(route, _)| !cache.iter().any(|(cached, _)| cached == route))
            .collect();
        cache.extend(missing.iter().map(|(route, _)| (route.clone(), None)));
        missing
    };
    for (route, hosts) in missing {
        let state = Arc::clone(state);
        tokio::spawn(async move {
            let prepared = {
                let _lease = state.receiver_access.acquire_for_io().await;
                openlogi_hid::prepare_host_switch_target(&route, &hosts, &state.channel_pool).await
            };
            let Ok(mut cache) = state.switch_targets.lock() else {
                return;
            };
            match prepared {
                Ok(target) => {
                    if let Some(entry) = cache.iter_mut().find(|(cached, _)| *cached == route) {
                        entry.1 = Some(Arc::new(target));
                    }
                }
                Err(error) => {
                    debug!(%error, %route, "Flow could not prepare a device switch yet");
                    cache.retain(|(cached, _)| *cached != route);
                }
            }
        });
    }
}

fn load_machine_identity() -> Result<MachineIdentity, FlowRuntimeError> {
    let path = openlogi_core::paths::data_dir()?.join(FLOW_IDENTITY_FILE);
    load_machine_identity_at(&path)
}

fn load_machine_identity_at(path: &Path) -> Result<MachineIdentity, FlowRuntimeError> {
    match fs::read(path) {
        Ok(bytes) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            }
            return MachineIdentity::from_pkcs8(bytes).map_err(FlowRuntimeError::from);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let identity = MachineIdentity::generate()?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(identity.private_key_pkcs8())?;
            file.sync_all()?;
            Ok(identity)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            MachineIdentity::from_pkcs8(fs::read(path)?).map_err(FlowRuntimeError::from)
        }
        Err(error) => Err(error.into()),
    }
}

#[derive(Debug, Error)]
enum FlowRuntimeError {
    #[error(transparent)]
    Paths(#[from] openlogi_core::paths::PathsError),
    #[error("Flow identity I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Identity(#[from] openlogi_flow::transport::IdentityError),
    #[error(transparent)]
    Transport(#[from] openlogi_flow::transport::TransportError),
    #[error(transparent)]
    Discovery(#[from] openlogi_flow::discovery::DiscoveryError),
    #[error(transparent)]
    Session(#[from] openlogi_flow::session::SessionManagerError),
    #[error("Flow identity task failed: {0}")]
    IdentityTask(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_hook::edge::DisplayRect;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;
    use tempfile::tempdir;

    #[test]
    fn maps_entry_across_disconnected_exposed_segments() {
        let displays = [
            DisplayRect::new(0.0, 0.0, 100.0, 100.0).unwrap(),
            DisplayRect::new(0.0, 200.0, 100.0, 100.0).unwrap(),
        ];
        let edges = ExposedEdges::from_displays(&displays);
        assert_eq!(
            point_on_edge(&edges, proto::Side::Left, 0.25),
            Some((1.0, 50.0))
        );
        assert_eq!(
            point_on_edge(&edges, proto::Side::Left, 0.75),
            Some((1.0, 250.0))
        );
    }

    #[tokio::test]
    async fn binding_waits_for_the_previous_generation_to_release_the_port() {
        let directory = tempdir().expect("temporary directory");
        let identity = load_machine_identity_at(&directory.path().join(FLOW_IDENTITY_FILE))
            .expect("generate identity");
        let hello = proto::Hello {
            proto_min: PROTOCOL_MIN,
            proto_max: PROTOCOL_MAX,
            public_key: identity.public_key().as_bytes().to_vec(),
            session_nonce: rand::random::<[u8; 16]>().to_vec(),
            machine_name: machine_name(),
            platform: platform().into(),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            ..Default::default()
        };
        let previous =
            std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("hold a free port");
        let address = previous.local_addr().expect("held address");
        let release = tokio::spawn(async move {
            tokio::time::sleep(BIND_RETRY * 3).await;
            drop(previous);
        });

        let endpoint = bind_endpoint(address, identity, PeerTrust::pinned([]), hello)
            .await
            .expect("bound once the port was released");
        assert_eq!(endpoint.local_addr().expect("bound address"), address);
        release.await.expect("release task");
    }

    #[test]
    fn machine_identity_is_persistent_and_private() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join(FLOW_IDENTITY_FILE);
        let first = load_machine_identity_at(&path).expect("generate identity");

        #[cfg(unix)]
        {
            let mode = fs::metadata(&path)
                .expect("identity metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
                .expect("loosen permissions for reload test");
        }

        let second = load_machine_identity_at(&path).expect("reload identity");
        assert_eq!(first.public_key(), second.public_key());
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&path)
                .expect("identity metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn invalid_machine_identity_is_not_silently_replaced() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join(FLOW_IDENTITY_FILE);
        let invalid = b"not a PKCS#8 Ed25519 private key";
        fs::write(&path, invalid).expect("write invalid identity");

        assert!(matches!(
            load_machine_identity_at(&path),
            Err(FlowRuntimeError::Identity(_))
        ));
        assert_eq!(fs::read(path).expect("read invalid identity"), invalid);
    }
}
