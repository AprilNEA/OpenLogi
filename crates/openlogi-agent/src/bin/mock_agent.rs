//! Hardware-free mock agent for GUI development.
//!
//! Serves the same tarpc [`Agent`] service as the real agent, from a scripted
//! in-memory inventory: no HID I/O, no input hook, no Accessibility. The GUI
//! needs zero changes; it connects, handshakes the real [`PROTOCOL_VERSION`],
//! and renders whatever this binary scripts.
//!
//! ```sh
//! cargo run -p openlogi-agent --bin openlogi-agent-mock
//! OPENLOGI_DEV_AGENT=0 cargo run -p openlogi-gui   # in a second terminal
//! ```
//!
//! It defaults to the `openlogi-dev` profile — the one the dev app bundle
//! already uses — so it meets the dev GUI on the dev socket and the installed
//! production app is left alone. `OPENLOGI_PROFILE=prod` serves the production
//! socket instead, where the shared `agent.lock` keeps the mock and a real
//! agent from running at the same time in either direction.
//!
//! Scripted behavior:
//!
//! - A Bolt receiver with an online mouse (DPI + SmartShift + battery that
//!   drains ~1%/minute), an offline mouse, and a lighting-capable keyboard,
//!   plus one directly-attached mouse — covering every panel and both route
//!   kinds without hardware.
//! - A standalone Litra light whose power / brightness / temperature writes
//!   persist, and a `camera_active` flag that flips every 30s so the
//!   camera-linked light rendering has something to follow.
//! - DPI / SmartShift writes persist in memory and read back, so sliders and
//!   toggles behave like a live device.
//! - `start_pairing` runs a scripted Bolt flow: discovery → passkey → paired,
//!   and the paired keyboard joins the inventory.

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use interprocess::local_socket::traits::tokio::Listener as _;
use openlogi_core::binding::ActionRingSlot;
use openlogi_core::config::{Config, Lighting};
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, Capabilities, DeviceInventory, DeviceKind,
    DeviceModelInfo, DeviceTransports, LightCapabilities, LightValueRange, LightValueUnit,
    PairedDevice, RawDeviceAddress, ReceiverInfo, StandaloneDevice,
};
use openlogi_core::single_instance::{self, InstanceError};
use openlogi_hid::{
    DIRECT_DEVICE_INDEX, DeviceRoute, DpiCapabilities, DpiInfo, LightCommand, PasskeyMethod,
    ReceiverSelector, SmartShiftMode, SmartShiftStatus, WriteError,
};
use openlogi_ipc::transport;
use openlogi_ipc::{
    ActionRingCommandError, ActionRingInvocation, Agent, AgentSnapshot, AgentStatus,
    ConfigReloadError, FoundDevice, InventoryHealth, MonitorEvent, PROTOCOL_VERSION,
    PairingCommandError, PairingFailure, PairingUpdate,
};
use tarpc::context::Context;
use tarpc::server::{BaseChannel, Channel as _};
use tokio::sync::Mutex;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

const LOGITECH_VID: u16 = 0x046d;
/// Unique ID of the scripted Bolt receiver; Bolt routes are matched against it.
const RECEIVER_UID: &str = "MOCK-BOLT-01";
const MOUSE_SLOT: u8 = 1;
const OFFLINE_SLOT: u8 = 2;
const KEYBOARD_SLOT: u8 = 3;
/// Product ID of the scripted directly-attached mouse; `DeviceRoute::Direct`
/// is matched against it.
const DIRECT_PID: u16 = 0xb020;
/// Product ID of the scripted standalone Litra light (Litra Glow).
const LITRA_PID: u16 = 0xc900;
/// How often the scripted `camera_active` flag flips.
const CAMERA_TOGGLE_PERIOD: Duration = Duration::from_secs(30);

/// BTLE address of the scripted pairing candidate.
const CANDIDATE_ADDRESS: [u8; 6] = [0xe0, 0x15, 0x27, 0x42, 0x91, 0x3a];
/// How long "discovery" runs before the candidate appears.
const DISCOVERY_DELAY: Duration = Duration::from_millis(1500);
/// Pause between accepting `pair_device` and asking for the passkey.
const PASSKEY_DELAY: Duration = Duration::from_millis(800);
/// How long the "user" takes to type the passkey before pairing completes.
const PASSKEY_TYPING_DELAY: Duration = Duration::from_secs(3);
/// How long `next_pairing` holds an empty long-poll before answering `None`.
const PAIRING_HOLD: Duration = Duration::from_secs(2);
/// How often that hold checks for an event. Short enough that a scripted step
/// reaches the GUI promptly; see [`MockAgent::next_pairing`] for why the hold
/// polls instead of awaiting the receiver.
const PAIRING_POLL_TICK: Duration = Duration::from_millis(100);

fn main() -> ExitCode {
    default_to_dev_profile();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_env("OPENLOGI_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Impersonate the agent role fully: holding `agent.lock` makes every real
    // agent spawned meanwhile (GUI auto-spawn, launchd KeepAlive) exit as a
    // duplicate — its takeover handshake sees us answer the current
    // PROTOCOL_VERSION and stands down.
    let _guard = match single_instance::acquire("agent.lock") {
        Ok(guard) => guard,
        Err(InstanceError::AlreadyRunning { path }) => {
            warn!(
                path = %path.display(),
                "an openlogi-agent is already running — quit it first (pkill -x openlogi-agent)"
            );
            return ExitCode::FAILURE;
        }
        Err(e) => {
            warn!(error = %e, "single-instance check failed");
            return ExitCode::FAILURE;
        }
    };

    let state = match State::new() {
        Ok(state) => state,
        Err(e) => {
            warn!(error = %e, "could not build the scripted inventory");
            return ExitCode::FAILURE;
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            warn!(error = %e, "tokio runtime init failed");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(serve(MockAgent::new(state))) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            warn!(error = %e, "could not bind the IPC socket");
            ExitCode::FAILURE
        }
    }
}

/// Claim the `openlogi-dev` profile unless the caller picked one.
///
/// A bare `cargo run` binary has no `.dev` bundle to be recognized by, so
/// without this the mock would resolve the *production* socket, lock and config
/// while the dev GUI (which does run from a `.dev` bundle) waits on the dev
/// socket — the two would never meet, and the mock would sit on the installed
/// app's paths instead.
fn default_to_dev_profile() {
    if std::env::var_os("OPENLOGI_PROFILE").is_some() {
        return;
    }
    #[expect(
        unsafe_code,
        reason = "the profile must be chosen before openlogi_core::paths caches it, and only a process-wide env var selects it"
    )]
    // SAFETY: `set_var` is unsound only against concurrent env access. This is
    // the first statement of `main`: no runtime, no tracing subscriber, no
    // other thread exists yet, and nothing has read the environment.
    unsafe {
        std::env::set_var("OPENLOGI_PROFILE", "dev");
    }
}

/// Accept loop — the mock's copy of `server::run` (kept verbatim rather than
/// making the production loop generic over its service impl for a dev tool).
async fn serve(server: MockAgent) -> std::io::Result<()> {
    let listener = transport::bind()?;
    info!(
        profile = std::env::var("OPENLOGI_PROFILE").unwrap_or_default(),
        "mock agent listening"
    );
    loop {
        let stream = match listener.accept().await {
            Ok(stream) => stream,
            Err(e) => {
                warn!(error = %e, "IPC accept failed");
                continue;
            }
        };
        let server = server.clone();
        let channel = BaseChannel::with_defaults(transport::wrap(stream));
        tokio::spawn(
            channel
                .execute(server.serve())
                .for_each(|response| async move {
                    tokio::spawn(response);
                }),
        );
    }
}

/// Mutable DPI state for one scripted device.
struct DpiState {
    current: u16,
    capabilities: DpiCapabilities,
}

/// What one scripted device answers to the settings RPCs. `None` / `false`
/// answer [`WriteError::FeatureUnsupported`], exercising the GUI's permanent-
/// error path (it must stop re-probing).
struct DeviceSettings {
    dpi: Option<DpiState>,
    smartshift: Option<SmartShiftStatus>,
    lighting: bool,
}

impl DeviceSettings {
    fn unsupported() -> Self {
        Self {
            dpi: None,
            smartshift: None,
            lighting: false,
        }
    }
}

/// An in-flight scripted pairing session.
struct PairingSession {
    /// Identifies this session to the tasks it spawned. Their sleeps outlive a
    /// cancel, so every one of them must prove it is still the live session
    /// before touching state — otherwise a session started right after a cancel
    /// inherits the previous one's discovery or gets consumed by it.
    id: u64,
    updates: UnboundedSender<PairingUpdate>,
    /// The candidate surfaced by discovery, once `DISCOVERY_DELAY` elapsed;
    /// `pair_device` only accepts its address.
    discovered: Option<FoundDevice>,
}

/// Everything the RPCs read or mutate. Guarded by one async mutex; locks stay
/// short and never span an await.
struct State {
    /// Devices added by a scripted pairing session, appended to the Bolt
    /// receiver's paired list. The scripted devices themselves are rebuilt per
    /// poll, so this holds only what pairing added.
    paired_extra: Vec<PairedDevice>,
    /// Slot the next scripted pairing assigns.
    next_slot: u8,
    /// Keyed by HID++ device index (Bolt slot / [`DIRECT_DEVICE_INDEX`]),
    /// unique here because the script has a single receiver.
    settings: HashMap<u8, DeviceSettings>,
    pairing: Option<PairingSession>,
    /// Id handed to the next pairing session; only ever increases.
    next_pairing_id: u64,
    started: Instant,
}

impl State {
    fn new() -> Result<Self, WriteError> {
        let mut settings = HashMap::new();
        settings.insert(
            MOUSE_SLOT,
            DeviceSettings {
                dpi: Some(DpiState {
                    current: 1600,
                    capabilities: DpiCapabilities::new((200u16..=8000).step_by(50).collect())?,
                }),
                smartshift: Some(SmartShiftStatus {
                    mode: SmartShiftMode::Ratchet,
                    auto_disengage: 16,
                    tunable_torque: 50,
                }),
                lighting: false,
            },
        );
        settings.insert(OFFLINE_SLOT, DeviceSettings::unsupported());
        settings.insert(
            KEYBOARD_SLOT,
            DeviceSettings {
                dpi: None,
                smartshift: None,
                lighting: true,
            },
        );
        settings.insert(
            DIRECT_DEVICE_INDEX,
            DeviceSettings {
                dpi: Some(DpiState {
                    current: 1000,
                    capabilities: DpiCapabilities::new((400u16..=4000).step_by(100).collect())?,
                }),
                smartshift: None,
                lighting: false,
            },
        );
        Ok(Self {
            paired_extra: Vec::new(),
            next_slot: KEYBOARD_SLOT + 1,
            settings,
            pairing: None,
            next_pairing_id: 0,
            started: Instant::now(),
        })
    }

    /// Register a new pairing session and return its id.
    fn begin_pairing(&mut self, updates: UnboundedSender<PairingUpdate>) -> u64 {
        let id = self.next_pairing_id;
        self.next_pairing_id += 1;
        self.pairing = Some(PairingSession {
            id,
            updates,
            discovered: None,
        });
        id
    }

    /// The live session, but only if it is still the one `id` started.
    fn pairing_session(&mut self, id: u64) -> Option<&mut PairingSession> {
        self.pairing.as_mut().filter(|session| session.id == id)
    }

    /// End the live session, but only if it is still the one `id` started.
    fn end_pairing(&mut self, id: u64) -> Option<PairingSession> {
        if self.pairing.as_ref().is_some_and(|s| s.id == id) {
            self.pairing.take()
        } else {
            None
        }
    }

    /// Whether a host camera is "in use" right now — flipped on a timer so the
    /// camera-linked light rendering has a changing input to follow.
    fn camera_active(&self) -> bool {
        self.started.elapsed().as_secs() / CAMERA_TOGGLE_PERIOD.as_secs() % 2 == 1
    }

    /// The inventory as polled. Rebuilt per call so the online mouse's battery
    /// is re-derived from elapsed time: successive snapshots visibly differ and
    /// the GUI's poll → repaint loop can be watched working.
    fn render_inventory(&self) -> Vec<DeviceInventory> {
        let mut bolt = bolt_inventory(draining_battery(self.started.elapsed()));
        bolt.paired.extend_from_slice(&self.paired_extra);
        vec![bolt, direct_inventory()]
    }

    fn settings_for(&self, route: &DeviceRoute) -> Result<&DeviceSettings, WriteError> {
        settings_key(route)
            .and_then(|key| self.settings.get(&key))
            .ok_or(WriteError::DeviceNotFound)
    }

    fn settings_for_mut(&mut self, route: &DeviceRoute) -> Result<&mut DeviceSettings, WriteError> {
        settings_key(route)
            .and_then(|key| self.settings.get_mut(&key))
            .ok_or(WriteError::DeviceNotFound)
    }

    /// Append the scripted pairing candidate to the Bolt receiver's inventory
    /// and return its assigned slot.
    fn pair_scripted(&mut self, name: &str) -> u8 {
        let slot = self.next_slot;
        self.next_slot = self.next_slot.saturating_add(1);
        self.paired_extra.push(PairedDevice {
            slot,
            codename: Some(name.to_string()),
            wpid: Some(0x408a),
            kind: DeviceKind::Keyboard,
            online: true,
            battery: Some(BatteryInfo {
                percentage: 90,
                level: BatteryLevel::Full,
                status: BatteryStatus::Discharging,
            }),
            model_info: None,
            capabilities: Some(Capabilities::default()),
        });
        self.settings.insert(slot, DeviceSettings::unsupported());
        slot
    }
}

/// The scripted standalone Litra light. The wire form carries the light's
/// *capabilities*, not its current values — the panel reads those from config —
/// so this is constant, and writes are answered by [`MockAgent::set_light`].
fn standalone_light() -> StandaloneDevice {
    StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: LOGITECH_VID,
            product_id: LITRA_PID,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "MOCK-LITRA-01".to_string(),
        },
        display_name: "Litra Glow".to_string(),
        manufacturer: Some("Logitech".to_string()),
        serial_number: Some("MOCKLITRA1".to_string()),
        unit_id: [0x0d, 0x0e, 0x0f, 0x10],
        kind: DeviceKind::Unknown,
        online: true,
        capabilities: None,
        light_capabilities: Some(LightCapabilities {
            power: true,
            brightness: LightValueRange::new(0, 100, 1, LightValueUnit::Percent).ok(),
            temperature: LightValueRange::new(2700, 6500, 100, LightValueUnit::Kelvin).ok(),
            color: false,
            zones: false,
        }),
        driver_id: "litra".to_string(),
        // Must stay `Some` until #571 is fixed: `registry_model_id` is
        // `skip_serializing_if`, which truncates the bincode stream when it is
        // `None` and makes the whole snapshot undecodable. `8c900` is also the
        // real registry id for a Litra Glow, so the asset lookup resolves.
        registry_model_id: Some("8c900".to_string()),
    }
}

/// The route the GUI addresses the scripted light by.
fn light_route() -> DeviceRoute {
    DeviceRoute::Direct {
        vendor_id: LOGITECH_VID,
        product_id: LITRA_PID,
    }
}

/// Resolve a wire route to the scripted settings key. `None` = no such device.
fn settings_key(route: &DeviceRoute) -> Option<u8> {
    match route {
        DeviceRoute::Bolt { receiver_uid, slot } if receiver_uid == RECEIVER_UID => Some(*slot),
        DeviceRoute::Direct {
            vendor_id: LOGITECH_VID,
            product_id: DIRECT_PID,
        } => Some(DIRECT_DEVICE_INDEX),
        _ => None,
    }
}

/// Sawtooth battery for the online mouse: 80% draining ~1%/minute down to
/// 20%, then back to 80%, with the coarse level tracking the percentage.
fn draining_battery(elapsed: Duration) -> BatteryInfo {
    let drained = u8::try_from(elapsed.as_secs() / 60 % 61).unwrap_or(0);
    let percentage = 80 - drained;
    BatteryInfo {
        percentage,
        level: match percentage {
            0..=10 => BatteryLevel::Critical,
            11..=25 => BatteryLevel::Low,
            _ => BatteryLevel::Good,
        },
        status: BatteryStatus::Discharging,
    }
}

/// The scripted Bolt receiver and its devices. `mouse_battery` is passed in
/// because it is the one field that moves between polls.
fn bolt_inventory(mouse_battery: BatteryInfo) -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "Logi Bolt Receiver".to_string(),
            vendor_id: LOGITECH_VID,
            product_id: 0xc548,
            unique_id: Some(RECEIVER_UID.to_string()),
        },
        paired: vec![
            PairedDevice {
                slot: MOUSE_SLOT,
                codename: Some("MX Master 3S".to_string()),
                wpid: Some(0xb034),
                kind: DeviceKind::Mouse,
                online: true,
                battery: Some(mouse_battery),
                model_info: Some(DeviceModelInfo {
                    entity_count: 3,
                    serial_number: Some("2140LZ00MOCK".to_string()),
                    unit_id: [0x01, 0x02, 0x03, 0x04],
                    transports: DeviceTransports {
                        usb: false,
                        equad: true,
                        btle: true,
                        bluetooth: false,
                    },
                    model_ids: [0xb034, 0x4082, 0],
                    extended_model_id: 0x0b,
                }),
                capabilities: Some(Capabilities {
                    buttons: true,
                    pointer: true,
                    lighting: false,
                    scroll_inversion: true,
                    hires_wheel: true,
                    thumbwheel: true,
                    haptic_feedback: true,
                    haptic_panel: true,
                }),
            },
            PairedDevice {
                slot: OFFLINE_SLOT,
                codename: Some("MX Anywhere 3".to_string()),
                wpid: Some(0x4090),
                kind: DeviceKind::Mouse,
                online: false,
                battery: None,
                model_info: None,
                capabilities: None,
            },
            // Lighting is scripted `true` (unlike a real MX Keys) so the
            // Lighting panel is reachable without G-series hardware.
            PairedDevice {
                slot: KEYBOARD_SLOT,
                codename: Some("MX Keys".to_string()),
                wpid: Some(0x408a),
                kind: DeviceKind::Keyboard,
                online: true,
                battery: Some(BatteryInfo {
                    percentage: 100,
                    level: BatteryLevel::Full,
                    status: BatteryStatus::Full,
                }),
                model_info: Some(DeviceModelInfo {
                    entity_count: 2,
                    serial_number: None,
                    unit_id: [0x05, 0x06, 0x07, 0x08],
                    transports: DeviceTransports {
                        usb: false,
                        equad: true,
                        btle: true,
                        bluetooth: false,
                    },
                    model_ids: [0x408a, 0xb35b, 0],
                    extended_model_id: 0,
                }),
                capabilities: Some(Capabilities {
                    buttons: false,
                    pointer: false,
                    lighting: true,
                    scroll_inversion: false,
                    hires_wheel: false,
                    thumbwheel: false,
                    haptic_feedback: false,
                    haptic_panel: false,
                }),
            },
        ],
    }
}

/// A directly-attached (Bluetooth) mouse: its synthetic receiver entry mirrors
/// the device itself, and its route is [`DeviceRoute::Direct`].
fn direct_inventory() -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "MX Vertical".to_string(),
            vendor_id: LOGITECH_VID,
            product_id: DIRECT_PID,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: DIRECT_DEVICE_INDEX,
            codename: Some("MX Vertical".to_string()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: Some(BatteryInfo {
                percentage: 55,
                level: BatteryLevel::Good,
                status: BatteryStatus::Discharging,
            }),
            model_info: Some(DeviceModelInfo {
                entity_count: 2,
                serial_number: None,
                unit_id: [0x09, 0x0a, 0x0b, 0x0c],
                transports: DeviceTransports {
                    usb: true,
                    equad: false,
                    btle: true,
                    bluetooth: false,
                },
                model_ids: [DIRECT_PID, 0, 0],
                extended_model_id: 0,
            }),
            capabilities: Some(Capabilities {
                buttons: true,
                pointer: true,
                lighting: false,
                scroll_inversion: false,
                hires_wheel: false,
                thumbwheel: false,
                haptic_feedback: false,
                haptic_panel: false,
            }),
        }],
    }
}

/// `launch_at_login` mirrors the config file so the Settings toggle round-trips
/// (the GUI saves config.toml, calls `reload_config`, then expects the next
/// snapshot to agree). Everything else is scripted green.
fn agent_status() -> AgentStatus {
    let launch_at_login =
        Config::load_or_default().is_ok_and(|config| config.app_settings.launch_at_login);
    AgentStatus {
        accessibility_granted: true,
        hook_installed: true,
        launch_at_login,
        inventory: InventoryHealth::Ready,
        protocol_version: PROTOCOL_VERSION,
        // The "-mock" marker shows up anywhere the GUI displays the agent
        // version, so a mock session can't be mistaken for a live one.
        agent_version: concat!(env!("CARGO_PKG_VERSION"), "-mock").to_string(),
    }
}

/// The scripted [`Agent`] implementation, cloned per connection.
#[derive(Clone)]
struct MockAgent {
    state: Arc<Mutex<State>>,
    /// Long-poll side of the pairing channel, outside [`MockAgent::state`] so a
    /// held `next_pairing` can't block `snapshot`.
    pairing_rx: Arc<Mutex<Option<UnboundedReceiver<PairingUpdate>>>>,
}

impl MockAgent {
    fn new(state: State) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
            pairing_rx: Arc::new(Mutex::new(None)),
        }
    }
}

// Pairing updates are sent with `let _ =`: a send only fails when the GUI's
// long-poll receiver is gone (Add Device window closed / GUI died), and
// dropping the event is exactly right then.
impl Agent for MockAgent {
    async fn protocol_version(self, _: Context) -> u32 {
        PROTOCOL_VERSION
    }

    async fn status(self, _: Context) -> AgentStatus {
        agent_status()
    }

    async fn inventory(self, _: Context) -> Vec<DeviceInventory> {
        self.state.lock().await.render_inventory()
    }

    async fn reload_config(self, _: Context) -> Result<(), ConfigReloadError> {
        info!("reload_config (no-op in the mock)");
        Ok(())
    }

    // The mock has no Actions Ring hardware: long-polls idle until the
    // overlay's request deadline (returning immediately would hot-loop it),
    // and interaction commands answer like an expired session.
    async fn next_action_ring(self, _: Context) -> Option<ActionRingInvocation> {
        tokio::time::sleep(Duration::from_secs(20)).await;
        None
    }

    async fn action_ring_hover(
        self,
        _: Context,
        _session_id: u64,
        _slot: ActionRingSlot,
    ) -> Result<(), ActionRingCommandError> {
        Err(ActionRingCommandError::SessionNotFound)
    }

    async fn action_ring_activate(
        self,
        _: Context,
        _session_id: u64,
        _slot: ActionRingSlot,
    ) -> Result<(), ActionRingCommandError> {
        Err(ActionRingCommandError::SessionNotFound)
    }

    async fn action_ring_cancel(self, _: Context, _session_id: u64) {}

    async fn set_dpi(self, _: Context, route: DeviceRoute, dpi: u32) -> Result<(), WriteError> {
        let mut state = self.state.lock().await;
        let settings = state.settings_for_mut(&route)?;
        let dpi_state = settings
            .dpi
            .as_mut()
            .ok_or(WriteError::FeatureUnsupported {
                feature_hex: 0x2201,
            })?;
        dpi_state.current = dpi_state.capabilities.nearest(dpi);
        info!(%route, dpi = dpi_state.current, "set_dpi");
        Ok(())
    }

    async fn set_lighting(
        self,
        _: Context,
        route: DeviceRoute,
        lighting: Lighting,
    ) -> Result<(), WriteError> {
        let state = self.state.lock().await;
        if !state.settings_for(&route)?.lighting {
            return Err(WriteError::FeatureUnsupported {
                feature_hex: 0x8070,
            });
        }
        info!(%route, ?lighting, "set_lighting");
        Ok(())
    }

    async fn set_smartshift(
        self,
        _: Context,
        route: DeviceRoute,
        mode: SmartShiftMode,
        auto_disengage: u8,
        tunable_torque: u8,
    ) -> Result<(), WriteError> {
        let mut state = self.state.lock().await;
        let settings = state.settings_for_mut(&route)?;
        let smartshift = settings
            .smartshift
            .as_mut()
            .ok_or(WriteError::FeatureUnsupported {
                feature_hex: 0x2110,
            })?;
        *smartshift = SmartShiftStatus {
            mode,
            auto_disengage,
            tunable_torque,
        };
        info!(%route, ?mode, auto_disengage, tunable_torque, "set_smartshift");
        Ok(())
    }

    async fn read_dpi(self, _: Context, route: DeviceRoute) -> Result<DpiInfo, WriteError> {
        let state = self.state.lock().await;
        state
            .settings_for(&route)?
            .dpi
            .as_ref()
            .map(|dpi| DpiInfo {
                current: dpi.current,
                capabilities: dpi.capabilities.clone(),
            })
            .ok_or(WriteError::FeatureUnsupported {
                feature_hex: 0x2201,
            })
    }

    async fn read_smartshift(
        self,
        _: Context,
        route: DeviceRoute,
    ) -> Result<SmartShiftStatus, WriteError> {
        let state = self.state.lock().await;
        state
            .settings_for(&route)?
            .smartshift
            .ok_or(WriteError::FeatureUnsupported {
                feature_hex: 0x2110,
            })
    }

    async fn request_accessibility_prompt(self, _: Context) {
        info!("request_accessibility_prompt (no-op in the mock)");
    }

    async fn start_pairing(
        self,
        _: Context,
        _selector: ReceiverSelector,
    ) -> Result<(), PairingCommandError> {
        let (tx, rx) = mpsc::unbounded_channel();
        let id = {
            let mut state = self.state.lock().await;
            if state.pairing.is_some() {
                return Err(PairingCommandError::AlreadyActive);
            }
            state.begin_pairing(tx.clone())
        };
        *self.pairing_rx.lock().await = Some(rx);
        let _ = tx.send(PairingUpdate::Searching);

        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            tokio::time::sleep(DISCOVERY_DELAY).await;
            let mut state = state.lock().await;
            // Only *this* session's discovery: a cancel and restart inside the
            // delay must not hand the replacement a device it never searched for.
            if let Some(session) = state.pairing_session(id) {
                let found = FoundDevice {
                    address: CANDIDATE_ADDRESS,
                    name: "ERGO K860".to_string(),
                };
                let _ = session
                    .updates
                    .send(PairingUpdate::DeviceFound(found.clone()));
                session.discovered = Some(found);
            }
        });
        Ok(())
    }

    async fn pair_device(self, _: Context, address: [u8; 6]) -> Result<(), PairingCommandError> {
        let (id, tx, name) = {
            let state = self.state.lock().await;
            let Some(session) = state.pairing.as_ref() else {
                return Err(PairingCommandError::NoActiveSession);
            };
            let Some(found) = session
                .discovered
                .as_ref()
                .filter(|found| found.address == address)
            else {
                return Err(PairingCommandError::UnknownDevice);
            };
            (session.id, session.updates.clone(), found.name.clone())
        };

        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            tokio::time::sleep(PASSKEY_DELAY).await;
            // A cancel in the meantime ends the flow: a plain cancel leaves the
            // GUI polling this very channel, so sending the prompt anyway would
            // show it a passkey *after* `Failed(Cancelled)`.
            if state.lock().await.pairing_session(id).is_none() {
                return;
            }
            let _ = tx.send(PairingUpdate::Passkey(PasskeyMethod::Keyboard(
                "482913".to_string(),
            )));
            tokio::time::sleep(PASSKEY_TYPING_DELAY).await;
            let mut state = state.lock().await;
            // No session of ours left = cancelled while the "user" was typing.
            // Ending it by id also means a session started in the meantime is
            // neither consumed here nor completed with our (dropped) channel.
            if state.end_pairing(id).is_none() {
                return;
            }
            let slot = state.pair_scripted(&name);
            let _ = tx.send(PairingUpdate::Paired { slot });
        });
        Ok(())
    }

    async fn cancel_pairing(self, _: Context) -> Result<(), PairingCommandError> {
        // Cancelling with nothing active is `Ok` in the real agent, so it is
        // `Ok` here — the GUI must not see a different contract from the mock.
        if let Some(session) = self.state.lock().await.pairing.take() {
            let _ = session
                .updates
                .send(PairingUpdate::Failed(PairingFailure::Cancelled));
        }
        Ok(())
    }

    async fn next_pairing(self, _: Context) -> Option<PairingUpdate> {
        // Polled rather than awaiting the receiver directly: the lock is then
        // never held across an await, so a `start_pairing` arriving mid-hold
        // isn't stuck behind this poll. A drained-but-open channel and a
        // finished session look the same here — both simply wait out the hold,
        // which is what keeps the GUI's poll loop from spinning.
        let started = Instant::now();
        while started.elapsed() < PAIRING_HOLD {
            if let Some(update) = self
                .pairing_rx
                .lock()
                .await
                .as_mut()
                .and_then(|rx| rx.try_recv().ok())
            {
                return Some(update);
            }
            tokio::time::sleep(PAIRING_POLL_TICK).await;
        }
        None
    }

    async fn snapshot(self, _: Context) -> AgentSnapshot {
        let state = self.state.lock().await;
        AgentSnapshot {
            status: agent_status(),
            inventory: state.render_inventory(),
            standalone: vec![standalone_light()],
            camera_active: state.camera_active(),
        }
    }

    async fn poll_event_monitor(self, _: Context) -> Vec<MonitorEvent> {
        Vec::new()
    }

    async fn set_light(
        self,
        _: Context,
        route: DeviceRoute,
        command: LightCommand,
    ) -> Result<(), WriteError> {
        if route != light_route() {
            return Err(WriteError::DeviceNotFound);
        }
        info!(%route, ?command, "set_light");
        Ok(())
    }

    async fn set_light_manual_power(
        self,
        _: Context,
        route: DeviceRoute,
        enabled: bool,
    ) -> Result<(), WriteError> {
        if route != light_route() {
            return Err(WriteError::DeviceNotFound);
        }
        info!(%route, enabled, "set_light_manual_power");
        Ok(())
    }
}
