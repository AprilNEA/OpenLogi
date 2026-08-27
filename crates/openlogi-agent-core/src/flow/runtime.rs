use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

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
    message_envelope,
};
use openlogi_hid::{ChannelPool, DeviceRoute};
use openlogi_hook::edge::{EdgeSide, ExposedEdges};
use openlogi_ipc::{FlowLinkState, FlowPeerStatus, FlowStatus};
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use super::config::CompiledFlowConfig;
use super::handoff::{HandoffBook, handle_notification, handle_rpc, inventory_changed};
use super::{FlowDeviceSnapshot, RuntimeDevice, is_pointing_device};
use crate::observable::ObservableState;
use crate::receiver_access::{ExclusiveAccessReason, ReceiverAccess};

const FLOW_IDENTITY_FILE: &str = "flow-identity.pk8";
const PROTOCOL_MIN: u32 = 1;
const PROTOCOL_MAX: u32 = 1;

mod controller;

pub use controller::{FlowController, FlowInputHandle};

struct FlowGeneration {
    state: Arc<GenerationState>,
    sessions: SessionManager,
    tasks: Vec<JoinHandle<()>>,
    _advertiser: Option<MdnsAdvertiser>,
    _endpoint: Arc<FlowEndpoint>,
}

impl FlowGeneration {
    async fn start(
        config: Arc<CompiledFlowConfig>,
        snapshots: &[FlowDeviceSnapshot],
        observable: Arc<ObservableState>,
        channel_pool: ChannelPool,
        receiver_access: ReceiverAccess,
    ) -> Result<Self, FlowRuntimeError> {
        let identity = tokio::task::spawn_blocking(load_machine_identity)
            .await
            .map_err(|error| FlowRuntimeError::IdentityTask(error.to_string()))??;
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
        let endpoint = Arc::new(FlowEndpoint::bind(
            SocketAddr::from((Ipv4Addr::UNSPECIFIED, DEFAULT_PORT)),
            identity,
            PeerTrust::pinned(config.peers.iter().map(|peer| peer.public_key)),
            hello,
        )?);
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
        ));
        let provider: Arc<dyn TrustedStateProvider> = state.clone();
        let sessions = SessionManager::start(
            Arc::clone(&endpoint),
            peers,
            provider,
            SessionPolicy::default(),
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
        info!(peers = config.peers.len(), "Flow runtime armed");
        Ok(Self {
            state,
            sessions,
            tasks,
            _advertiser: advertiser,
            _endpoint: endpoint,
        })
    }

    async fn update_devices(&self, snapshots: &[FlowDeviceSnapshot]) {
        self.state.update_devices(snapshots);
        self.state.publish_device_state().await;
        inventory_changed(Arc::clone(&self.state)).await;
    }

    async fn shutdown(mut self) {
        {
            let _lifecycle = self.state.lifecycle.lock().await;
            self.state.active.store(false, Ordering::Release);
        }
        self.sessions.shutdown().await;
        for task in self.tasks.drain(..) {
            let _ = task.await;
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
}

#[derive(Clone)]
pub(super) struct OutgoingDevice {
    route: DeviceRoute,
    host: u8,
    pub(super) identity: proto::DeviceIdentity,
}

impl GenerationState {
    fn new(
        config: Arc<CompiledFlowConfig>,
        snapshots: &[FlowDeviceSnapshot],
        observable: Arc<ObservableState>,
        channel_pool: ChannelPool,
        receiver_access: ReceiverAccess,
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
        }
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
        });
    }

    pub(super) fn outgoing_devices(&self, peer: &str) -> Vec<OutgoingDevice> {
        let mut devices: Vec<_> = self
            .devices_snapshot()
            .into_iter()
            .filter(|device| device.snapshot.online)
            .filter_map(|device| {
                let route = device.snapshot.route.clone()?;
                let host = device.channels.get(peer).copied()?;
                Some((
                    device.snapshot.kind,
                    OutgoingDevice {
                        route,
                        host,
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
        let _lease = self
            .receiver_access
            .acquire_exclusive(ExclusiveAccessReason::HostTransition)
            .await;
        if !self.is_active() {
            return Ok(false);
        }
        let targets: Vec<_> = devices
            .iter()
            .map(|device| (device.route.clone(), device.host))
            .collect();
        openlogi_hid::switch_hosts(&targets, &self.channel_pool).await?;
        Ok(true)
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
    loop {
        tokio::select! {
            rpc = connection.accept_rpc() => match rpc {
                Ok(event @ (RpcEvent::Request(_) | RpcEvent::Rejected(_))) => {
                    handle_rpc(Arc::clone(&state), peer, event).await;
                }
                Err(_) => return,
            },
            notification = connection.accept_notification() => match notification {
                Ok(event @ (NotificationEvent::Notification(_) | NotificationEvent::Dropped(_))) => {
                    handle_notification(Arc::clone(&state), peer, event).await;
                }
                Err(_) => return,
            },
        }
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
    if !openlogi_inject::warp_cursor(x, y) {
        warn!("Flow device arrived, but cursor warp is unsupported on this display server");
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
