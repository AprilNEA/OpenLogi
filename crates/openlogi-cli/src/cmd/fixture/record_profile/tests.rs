use std::collections::BTreeSet;
use std::io;
use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use openlogi_core::app::ForegroundApp;
use openlogi_core::binding::ActionRingSlot;
use openlogi_core::config::Lighting;
use openlogi_core::device::DeviceInventory;
use openlogi_core::hid::{
    BacklightMode, BacklightState, BacklightStatus, Dpi, DpiInfo, LightCommand, PasskeyMethod,
    ReceiverSelector, ScrollWheelMode, SmartShiftStatus,
};
use openlogi_device::fixture::{
    CANONICAL_DEVICE_PROFILE_JSON, SyntheticIdentityKind, classify_synthetic_identity_bytes,
    classify_synthetic_profile_identity,
};
use openlogi_ipc::{
    ActionRingCommandError, ActionRingInvocation, Agent, AgentStatus, ClientKind,
    ConfigReloadError, ForegroundApps, Generation, Identity, InventoryHealth, MonitorEvent,
    Observation, PairingCommandError, PairingPhase, PairingUpdate, RingObservation,
};
use tarpc::context::Context as TarpcContext;
use tarpc::server::{BaseChannel, Channel as _};

use super::*;

const RAW_RECEIVER_UID: &str = "RAW-RECEIVER-IDENTITY";
const RAW_DEVICE_SERIAL: &str = "RAW-DEVICE-SERIAL";
const RAW_STANDALONE_ID: &str = "RAW-STANDALONE-IDENTITY";
const RUNTIME_APP_ID: &str = "/Users/private/Secret.app";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadFamily {
    Dpi,
    Smartshift,
    Wheel,
    Backlight,
}

#[derive(Clone)]
struct TestAgent {
    snapshot: AgentSnapshot,
    settings: Arc<Vec<ProfileDeviceSettings>>,
    calls: Arc<Mutex<Vec<(ReadFamily, DeviceRoute)>>>,
    failure: Option<(ReadFamily, DeviceRoute, WriteError)>,
    declared: Arc<Mutex<Vec<ClientKind>>>,
    snapshots: Arc<Mutex<usize>>,
}

impl TestAgent {
    fn from_profile(profile: DeviceProfile, snapshot: AgentSnapshot) -> Self {
        Self {
            snapshot,
            settings: Arc::new(profile.settings),
            calls: Arc::new(Mutex::new(Vec::new())),
            failure: None,
            declared: Arc::new(Mutex::new(Vec::new())),
            snapshots: Arc::new(Mutex::new(0)),
        }
    }

    fn fail(mut self, family: ReadFamily, route: DeviceRoute, error: WriteError) -> Self {
        self.failure = Some((family, route, error));
        self
    }

    fn read<T: Clone>(
        &self,
        family: ReadFamily,
        route: &DeviceRoute,
        setting: impl for<'a> FnOnce(&'a ProfileDeviceSettings) -> &'a ProfileSetting<T>,
        feature_hex: u16,
    ) -> Result<T, WriteError> {
        self.calls
            .lock()
            .expect("calls lock")
            .push((family, route.clone()));
        if let Some((failed_family, failed_route, error)) = &self.failure
            && *failed_family == family
            && failed_route == route
        {
            return Err(error.clone());
        }
        let settings = self
            .settings
            .iter()
            .find(|settings| settings.route == *route)
            .ok_or(WriteError::DeviceNotFound)?;
        match setting(settings) {
            ProfileSetting::Supported(value) => Ok(value.clone()),
            ProfileSetting::Unsupported => Err(WriteError::FeatureUnsupported { feature_hex }),
            ProfileSetting::Unavailable => Err(WriteError::DeviceUnreachable {
                index: route.device_index(),
            }),
        }
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the in-process IPC service intentionally returns immediate scripted values"
)]
impl Agent for TestAgent {
    async fn protocol_version(self, _: TarpcContext) -> u32 {
        PROTOCOL_VERSION
    }

    async fn status(self, _: TarpcContext) -> AgentStatus {
        unreachable!("profile capture must use one snapshot")
    }

    async fn inventory(self, _: TarpcContext) -> Vec<DeviceInventory> {
        unreachable!("profile capture must use one snapshot")
    }

    async fn reload_config(self, _: TarpcContext) -> Result<(), ConfigReloadError> {
        unreachable!("profile capture is read-only")
    }

    async fn set_dpi(
        self,
        _: TarpcContext,
        _route: DeviceRoute,
        _dpi: Dpi,
    ) -> Result<(), WriteError> {
        unreachable!("profile capture must never write DPI")
    }

    async fn set_lighting(
        self,
        _: TarpcContext,
        _route: DeviceRoute,
        _lighting: Lighting,
    ) -> Result<(), WriteError> {
        unreachable!("profile capture must never write lighting")
    }

    async fn set_smartshift(
        self,
        _: TarpcContext,
        _route: DeviceRoute,
        _status: SmartShiftStatus,
    ) -> Result<(), WriteError> {
        unreachable!("profile capture must never write SmartShift")
    }

    async fn read_dpi(self, _: TarpcContext, route: DeviceRoute) -> Result<DpiInfo, WriteError> {
        self.read(ReadFamily::Dpi, &route, |settings| &settings.dpi, 0x2201)
    }

    async fn read_smartshift(
        self,
        _: TarpcContext,
        route: DeviceRoute,
    ) -> Result<SmartShiftStatus, WriteError> {
        self.read(
            ReadFamily::Smartshift,
            &route,
            |settings| &settings.smartshift,
            0x2110,
        )
    }

    async fn request_accessibility_prompt(self, _: TarpcContext) {
        unreachable!("profile capture must not prompt")
    }

    async fn start_pairing(
        self,
        _: TarpcContext,
        _selector: ReceiverSelector,
    ) -> Result<(), PairingCommandError> {
        unreachable!("profile capture must not pair")
    }

    async fn pair_device(
        self,
        _: TarpcContext,
        _address: [u8; 6],
    ) -> Result<(), PairingCommandError> {
        unreachable!("profile capture must not pair")
    }

    async fn cancel_pairing(self, _: TarpcContext) -> Result<(), PairingCommandError> {
        unreachable!("profile capture must not change pairing")
    }

    async fn next_pairing(self, _: TarpcContext) -> Option<PairingUpdate> {
        unreachable!("profile capture must not inspect pairing")
    }

    async fn snapshot(self, _: TarpcContext) -> AgentSnapshot {
        *self.snapshots.lock().expect("snapshot lock") += 1;
        self.snapshot
    }

    async fn poll_event_monitor(self, _: TarpcContext) -> Vec<MonitorEvent> {
        unreachable!("profile capture must not poll events")
    }

    async fn set_light(
        self,
        _: TarpcContext,
        _route: DeviceRoute,
        _command: LightCommand,
    ) -> Result<(), WriteError> {
        unreachable!("profile capture must never write a light")
    }

    async fn set_light_manual_power(
        self,
        _: TarpcContext,
        _route: DeviceRoute,
        _enabled: bool,
    ) -> Result<(), WriteError> {
        unreachable!("profile capture must never write a light")
    }

    async fn next_action_ring(self, _: TarpcContext) -> Option<ActionRingInvocation> {
        unreachable!("profile capture must not inspect the Actions Ring")
    }

    async fn action_ring_hover(
        self,
        _: TarpcContext,
        _session_id: u64,
        _slot: ActionRingSlot,
    ) -> Result<(), ActionRingCommandError> {
        unreachable!("profile capture must not drive the Actions Ring")
    }

    async fn action_ring_activate(
        self,
        _: TarpcContext,
        _session_id: u64,
        _slot: ActionRingSlot,
    ) -> Result<(), ActionRingCommandError> {
        unreachable!("profile capture must not drive the Actions Ring")
    }

    async fn action_ring_cancel(self, _: TarpcContext, _session_id: u64) {
        unreachable!("profile capture must not drive the Actions Ring")
    }

    async fn identity(self, _: TarpcContext) -> Identity {
        unreachable!("profile capture does not need run identity")
    }

    async fn observe(self, _: TarpcContext, _since: Generation) -> Observation {
        unreachable!("profile capture must use one snapshot")
    }

    async fn observe_action_ring(self, _: TarpcContext, _since: Generation) -> RingObservation {
        unreachable!("profile capture must not inspect the Actions Ring")
    }

    async fn declare_client(self, _: TarpcContext, kind: ClientKind) {
        self.declared.lock().expect("declaration lock").push(kind);
    }

    async fn read_wheel(
        self,
        _: TarpcContext,
        route: DeviceRoute,
    ) -> Result<ScrollWheelMode, WriteError> {
        self.read(
            ReadFamily::Wheel,
            &route,
            |settings| &settings.wheel,
            0x2121,
        )
    }

    async fn read_backlight(
        self,
        _: TarpcContext,
        route: DeviceRoute,
    ) -> Result<BacklightState, WriteError> {
        self.read(
            ReadFamily::Backlight,
            &route,
            |settings| &settings.backlight,
            0x1982,
        )
    }
}

async fn test_connection(agent: TestAgent) -> Connection {
    let (client_transport, server_transport) = tarpc::transport::channel::unbounded();
    let client = AgentClient::new(tarpc::client::Config::default(), client_transport).spawn();
    tokio::spawn(
        BaseChannel::with_defaults(server_transport)
            .execute(agent.serve())
            .for_each(|response| async move {
                tokio::spawn(response);
            }),
    );
    let version = client
        .protocol_version(context::current())
        .await
        .expect("in-process Agent handshake");
    Connection { client, version }
}

fn fixture_agent() -> TestAgent {
    let mut profile: DeviceProfile =
        serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).expect("canonical profile parses");
    profile.inventories[0]
        .paired
        .retain(|device| device.slot != 2);
    profile
        .settings
        .retain(|settings| !matches!(settings.route, DeviceRoute::Bolt { slot: 2, .. }));
    profile.inventories[0].receiver.unique_id = Some(RAW_RECEIVER_UID.to_string());
    for device in &mut profile.inventories[0].paired {
        if let Some(model) = &mut device.model_info {
            model.serial_number = Some(format!("{RAW_DEVICE_SERIAL}-{}", device.slot));
        }
    }
    for settings in &mut profile.settings {
        if let DeviceRoute::Bolt { receiver_uid, .. } = &mut settings.route {
            *receiver_uid = RAW_RECEIVER_UID.to_string();
        }
    }
    let mouse = profile
        .settings
        .iter_mut()
        .find(|settings| matches!(settings.route, DeviceRoute::Bolt { slot: 1, .. }))
        .expect("mouse settings");
    mouse.backlight = ProfileSetting::Supported(BacklightState {
        enabled: true,
        mode: BacklightMode::Automatic,
        status: BacklightStatus::AlsAutomatic,
        current_level: 3,
        nb_levels: 8,
    });
    profile.standalone[0].address.identity = RAW_STANDALONE_ID.to_string();
    profile.standalone[0].serial_number = Some("RAW-LIGHT-SERIAL".to_string());

    let snapshot = AgentSnapshot {
        status: AgentStatus {
            accessibility_granted: false,
            hook_installed: false,
            launch_at_login: false,
            inventory: InventoryHealth::Ready,
            protocol_version: PROTOCOL_VERSION,
            agent_version: "/private/Agent.app".to_string(),
            input_monitoring_granted: false,
            hid_open_failures: true,
        },
        inventory: profile.inventories.clone(),
        standalone: profile.standalone.clone(),
        camera_active: true,
        pairing: Some(PairingPhase::Passkey(PasskeyMethod::Keyboard(
            "SECRET-PASSKEY".to_string(),
        ))),
        foreground: ForegroundApps {
            current: Some(ForegroundApp {
                id: RUNTIME_APP_ID.to_string(),
                display_name: "Secret App".to_string(),
            }),
            recent: Vec::new(),
        },
    };
    TestAgent::from_profile(profile, snapshot)
}

fn args(output: PathBuf, device: Option<&str>) -> RecordProfileArgs {
    RecordProfileArgs {
        id: "mx-master-3s-001".to_string(),
        name: "Synthetic receiver profile".to_string(),
        output,
        device: device.map(str::to_string),
        force: false,
    }
}

#[tokio::test]
async fn receiver_capture_uses_agent_reads_and_writes_validated_profile_only() {
    let directory = tempfile::tempdir().expect("tempdir");
    let output = directory.path().join("nested/profile.json");
    let agent = fixture_agent();
    let inspection = agent.clone();
    let connection = test_connection(agent).await;

    capture_connected(
        args(output.clone(), Some("synthetic performance mouse")),
        connection,
    )
    .await
    .expect("semantic capture succeeds");

    assert_eq!(
        *inspection.declared.lock().expect("declaration lock"),
        [ClientKind::Cli]
    );
    assert_eq!(*inspection.snapshots.lock().expect("snapshot lock"), 1);
    let calls = inspection.calls.lock().expect("calls lock").clone();
    assert_eq!(
        calls.iter().map(|(family, _)| *family).collect::<Vec<_>>(),
        [
            ReadFamily::Dpi,
            ReadFamily::Smartshift,
            ReadFamily::Wheel,
            ReadFamily::Backlight,
            ReadFamily::Smartshift,
        ]
    );

    let bytes = std::fs::read(&output).expect("profile output");
    assert!(bytes.ends_with(b"\n"));
    assert!(bytes.windows(2).any(|window| window == b"\n "));
    let encoded = String::from_utf8(bytes.clone()).expect("profile is UTF-8 JSON");
    for secret in [
        RAW_RECEIVER_UID,
        RAW_DEVICE_SERIAL,
        RAW_STANDALONE_ID,
        RUNTIME_APP_ID,
        "SECRET-PASSKEY",
        "/private/Agent.app",
    ] {
        assert!(!encoded.contains(secret), "output leaked {secret}");
    }

    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("profile JSON");
    let keys = value
        .as_object()
        .expect("profile object")
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        keys,
        BTreeSet::from([
            "id",
            "inventories",
            "name",
            "schema_version",
            "settings",
            "standalone",
        ])
    );
    let profile: DeviceProfile = serde_json::from_slice(&bytes).expect("typed profile");
    profile.validate().expect("published profile validates");
    assert_eq!(profile.schema_version, FIXTURE_SCHEMA_VERSION);
    assert_eq!(profile.inventories.len(), 1);
    assert_eq!(profile.inventories[0].paired.len(), 2);
    assert!(profile.standalone.is_empty());
    assert_eq!(profile.settings.len(), 2);
    assert_eq!(
        profile.inventories[0].receiver.unique_id.as_deref(),
        Some("OL-BOLT-UID-0001")
    );
    assert!(profile.settings.iter().all(|settings| {
        matches!(
            &settings.route,
            DeviceRoute::Bolt { receiver_uid, .. } if receiver_uid == "OL-BOLT-UID-0001"
        )
    }));
    classify_synthetic_profile_identity(
        SyntheticIdentityKind::BoltReceiverUid,
        profile.inventories[0]
            .receiver
            .unique_id
            .as_deref()
            .expect("captured receiver identity"),
    )
    .expect("captured receiver follows the shared fixture policy");
    for device in &profile.inventories[0].paired {
        let model = device.model_info.as_ref().expect("captured model info");
        let unit_ordinal =
            classify_synthetic_identity_bytes(SyntheticIdentityKind::DeviceUnitId, &model.unit_id)
                .expect("captured unit ID follows the shared fixture policy");
        let serial_ordinal = classify_synthetic_profile_identity(
            SyntheticIdentityKind::DeviceSerialNumber,
            model
                .serial_number
                .as_deref()
                .expect("captured synthetic serial"),
        )
        .expect("captured serial follows the shared fixture policy");
        assert_eq!(unit_ordinal, serial_ordinal);
    }
}

#[tokio::test]
async fn standalone_capture_retains_only_the_selected_semantic_device() {
    let directory = tempfile::tempdir().expect("tempdir");
    let output = directory.path().join("profile.json");
    let agent = fixture_agent();
    let inspection = agent.clone();

    capture_connected(
        args(output.clone(), Some("synthetic studio light")),
        test_connection(agent).await,
    )
    .await
    .expect("standalone capture succeeds");

    let profile: DeviceProfile =
        serde_json::from_slice(&std::fs::read(output).expect("profile output"))
            .expect("typed profile");
    profile.validate().expect("standalone profile validates");
    assert!(profile.inventories.is_empty());
    assert_eq!(profile.standalone.len(), 1);
    assert_eq!(profile.settings.len(), 1);
    assert_eq!(
        profile.standalone[0].address.identity,
        "OPENLOGI-FIXTURE-RAWHID-001"
    );
    classify_synthetic_profile_identity(
        SyntheticIdentityKind::RawHidProfileIdentity,
        &profile.standalone[0].address.identity,
    )
    .expect("captured raw-HID identity follows the shared fixture policy");
    let unit_ordinal = classify_synthetic_identity_bytes(
        SyntheticIdentityKind::DeviceUnitId,
        &profile.standalone[0].unit_id,
    )
    .expect("captured standalone unit ID follows the shared fixture policy");
    let serial_ordinal = classify_synthetic_profile_identity(
        SyntheticIdentityKind::DeviceSerialNumber,
        profile.standalone[0]
            .serial_number
            .as_deref()
            .expect("captured standalone serial"),
    )
    .expect("captured standalone serial follows the shared fixture policy");
    assert_eq!(unit_ordinal, serial_ordinal);
    assert!(profile.settings[0].light.is_supported());
    assert!(inspection.calls.lock().expect("calls lock").is_empty());
}

#[tokio::test]
async fn direct_capture_retains_only_the_selected_link() {
    let directory = tempfile::tempdir().expect("tempdir");
    let output = directory.path().join("profile.json");
    let agent = fixture_agent();

    capture_connected(
        args(output.clone(), Some("synthetic direct mouse")),
        test_connection(agent).await,
    )
    .await
    .expect("direct capture succeeds");

    let profile: DeviceProfile =
        serde_json::from_slice(&std::fs::read(output).expect("profile output"))
            .expect("typed profile");
    profile.validate().expect("direct profile validates");
    assert_eq!(profile.inventories.len(), 1);
    assert_eq!(profile.inventories[0].paired.len(), 1);
    assert!(profile.inventories[0].receiver.unique_id.is_none());
    assert!(profile.standalone.is_empty());
    assert_eq!(profile.settings.len(), 1);
    assert!(matches!(
        profile.settings[0].route,
        DeviceRoute::Direct { .. }
    ));
}

#[tokio::test]
async fn receiver_profile_is_stable_whichever_route_selects_the_receiver() {
    let directory = tempfile::tempdir().expect("tempdir");
    let mouse_output = directory.path().join("mouse.json");
    let keyboard_output = directory.path().join("keyboard.json");

    capture_connected(
        args(mouse_output.clone(), Some("synthetic performance mouse")),
        test_connection(fixture_agent()).await,
    )
    .await
    .expect("mouse route selects the receiver profile");
    capture_connected(
        args(keyboard_output.clone(), Some("synthetic rgb keyboard")),
        test_connection(fixture_agent()).await,
    )
    .await
    .expect("keyboard route selects the same receiver profile");

    let mouse: DeviceProfile = serde_json::from_slice(
        &std::fs::read(mouse_output).expect("mouse-selected profile output"),
    )
    .expect("mouse-selected typed profile");
    let keyboard: DeviceProfile = serde_json::from_slice(
        &std::fs::read(keyboard_output).expect("keyboard-selected profile output"),
    )
    .expect("keyboard-selected typed profile");
    assert_eq!(mouse, keyboard);
}

#[tokio::test]
async fn online_transient_read_aborts_without_output_or_error_detail_leak() {
    let directory = tempfile::tempdir().expect("tempdir");
    let output = directory.path().join("profile.json");
    let agent = fixture_agent();
    let route = DeviceRoute::Bolt {
        receiver_uid: RAW_RECEIVER_UID.to_string(),
        slot: 1,
    };
    let agent = agent.fail(
        ReadFamily::Dpi,
        route,
        WriteError::Hid("SECRET-TRANSPORT-PATH".to_string()),
    );

    let error = capture_connected(
        args(output.clone(), Some("Synthetic Performance Mouse")),
        test_connection(agent).await,
    )
    .await
    .expect_err("transient read must abort")
    .to_string();

    assert!(error.contains("online DPI semantic read"), "{error}");
    assert!(!error.contains("SECRET-TRANSPORT-PATH"));
    assert!(!error.contains(RAW_RECEIVER_UID));
    assert!(!output.exists());
}

#[tokio::test]
async fn offline_unknown_support_aborts_without_reads_or_output() {
    let directory = tempfile::tempdir().expect("tempdir");
    let output = directory.path().join("profile.json");
    let mut agent = fixture_agent();
    let direct = &mut agent.snapshot.inventory[1].paired[0];
    direct.online = false;
    assert!(
        direct
            .capabilities
            .expect("captured direct capabilities")
            .pointer,
        "offline DPI support is known"
    );
    let inspection = agent.clone();

    let error = capture_connected(
        args(output.clone(), Some("Synthetic Direct Mouse")),
        test_connection(agent).await,
    )
    .await
    .expect_err("unknown offline SmartShift support must abort")
    .to_string();

    assert!(
        error.contains("captured SmartShift capability fact"),
        "{error}"
    );
    assert!(error.contains("refusing to guess support"), "{error}");
    assert!(inspection.calls.lock().expect("calls lock").is_empty());
    assert!(!output.exists());
}

#[tokio::test]
async fn protocol_mismatch_aborts_before_snapshot_or_output() {
    let directory = tempfile::tempdir().expect("tempdir");
    let output = directory.path().join("profile.json");
    let agent = fixture_agent();
    let inspection = agent.clone();
    let mut connection = test_connection(agent).await;
    connection.version = PROTOCOL_VERSION - 1;

    let error = capture_connected(args(output.clone(), None), connection)
        .await
        .expect_err("protocol mismatch must abort")
        .to_string();

    assert!(error.contains("protocol v29"), "{error}");
    assert!(error.contains("requires v30"), "{error}");
    assert_eq!(*inspection.snapshots.lock().expect("snapshot lock"), 0);
    assert!(!output.exists());
}

#[test]
fn unreachable_and_handshake_failures_are_safe_and_agent_only() {
    let endpoint = ConnectError::Endpoint(io::Error::other("/Users/private/agent.sock"));
    let endpoint_error = safe_connect_error(&endpoint).to_string();
    assert!(endpoint_error.contains("running OpenLogi Agent"));
    assert!(endpoint_error.contains("no direct-hardware fallback"));
    assert!(!endpoint_error.contains("/Users/private"));

    let handshake = ConnectError::Handshake(RpcError::Shutdown);
    let handshake_error = safe_connect_error(&handshake).to_string();
    assert!(handshake_error.contains("healthy IPC handshake"));
    assert!(!handshake_error.contains("shutdown"));
}

#[test]
fn offline_known_support_is_unavailable_and_unknown_support_fails_closed() {
    assert_eq!(
        capability_setting::<DpiInfo>(false, true),
        Some(ProfileSetting::Unavailable)
    );
    assert_eq!(
        capability_setting::<ScrollWheelMode>(false, false),
        Some(ProfileSetting::Unsupported)
    );
    assert!(
        unknown_offline_support("SmartShift")
            .to_string()
            .contains("refusing to guess support")
    );
}

#[test]
fn target_selection_is_exact_unambiguous_and_privacy_safe() {
    let agent = fixture_agent();
    let candidates =
        selection::target_candidates(&agent.snapshot.inventory, &agent.snapshot.standalone);
    let error = selection::select_target(&candidates, None)
        .expect_err("multiple candidates require --device")
        .to_string();
    assert!(!error.contains(RAW_RECEIVER_UID));
    assert!(!error.contains(RAW_STANDALONE_ID));

    let route = format!("slot 1 on receiver {RAW_RECEIVER_UID}");
    assert!(matches!(
        selection::select_target(&candidates, Some(&route))
            .expect("exact rendered route selects")
            .location,
        TargetLocation::Inventory {
            inventory: 0,
            device: 0,
        }
    ));
    selection::select_target(&candidates, Some("performance"))
        .expect_err("substrings must not match");

    let mut duplicate = agent.snapshot.inventory.clone();
    duplicate[1].paired[0].codename = Some("Synthetic Performance Mouse".to_string());
    let duplicate_candidates = selection::target_candidates(&duplicate, &[]);
    selection::select_target(&duplicate_candidates, Some("synthetic performance mouse"))
        .expect_err("duplicate display names must be ambiguous");

    let direct = agent.snapshot.inventory[1].clone();
    let mut other_direct = direct.clone();
    other_direct.paired[0].codename = Some("Other direct device".to_string());
    let duplicate_route_candidates = selection::target_candidates(&[direct, other_direct], &[]);
    let error =
        selection::select_target(&duplicate_route_candidates, Some("Synthetic Direct Mouse"))
            .expect_err("a unique name cannot bless a duplicate route")
            .to_string();
    assert!(error.contains("selected route is duplicated"));
}

#[test]
fn profile_metadata_must_be_nonempty_before_agent_access() {
    let output = PathBuf::from("unused.json");
    for (id, name) in [("", "name"), ("   ", "name"), ("id", ""), ("id", "\t")] {
        validate_metadata(&RecordProfileArgs {
            id: id.to_string(),
            name: name.to_string(),
            output: output.clone(),
            device: None,
            force: false,
        })
        .expect_err("blank synthetic metadata must fail");
    }
}
