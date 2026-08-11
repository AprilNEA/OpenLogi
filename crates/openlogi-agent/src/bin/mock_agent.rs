//! Hardware-free mock agent for GUI development.
//!
//! Serves the same tarpc [`Agent`] service as the real agent — on the real IPC
//! socket — from a scripted in-memory inventory: no HID I/O, no input hook, no
//! Accessibility. The GUI needs zero changes; it connects, handshakes the real
//! [`PROTOCOL_VERSION`], and renders whatever this binary scripts.
//!
//! ```sh
//! pkill -x openlogi-agent            # stop the real agent if one is running
//! cargo run -p openlogi-agent --bin openlogi-agent-mock
//! cargo run -p openlogi-gui          # in a second terminal
//! ```
//!
//! The mock holds the agent's `agent.lock`, so real agents spawned meanwhile
//! (by the GUI's auto-spawn or launchd) exit as duplicates; conversely the mock
//! refuses to start while a real agent is running. Scripted behavior:
//!
//! - A Bolt receiver with an online mouse (DPI + SmartShift + battery that
//!   drains ~1%/minute), an offline mouse, and a lighting-capable keyboard,
//!   plus one directly-attached mouse — covering every panel and both route
//!   kinds without hardware.
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
use openlogi_agent_core::ipc::{
    Agent, AgentSnapshot, AgentStatus, FoundDevice, InventoryHealth, MonitorEvent,
    PROTOCOL_VERSION, PairingCommandError, PairingFailure, PairingUpdate,
};
use openlogi_agent_core::transport;
use openlogi_core::config::{Config, Lighting};
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, Capabilities, DeviceInventory, DeviceKind,
    DeviceModelInfo, DeviceTransports, PairedDevice, ReceiverInfo,
};
use openlogi_core::single_instance::{self, InstanceError};
use openlogi_hid::{
    DIRECT_DEVICE_INDEX, DeviceRoute, DpiCapabilities, DpiInfo, PasskeyMethod, ReceiverSelector,
    SmartShiftMode, SmartShiftStatus, WriteError,
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

/// Accept loop — the mock's copy of `server::run` (kept verbatim rather than
/// making the production loop generic over its service impl for a dev tool).
async fn serve(server: MockAgent) -> std::io::Result<()> {
    let listener = transport::bind()?;
    info!("mock agent listening on the real IPC socket");
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
            started: Instant::now(),
        })
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

    async fn reload_config(self, _: Context) {
        info!("reload_config (no-op in the mock)");
    }

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
        {
            let mut state = self.state.lock().await;
            if state.pairing.is_some() {
                return Err(PairingCommandError::AlreadyActive);
            }
            state.pairing = Some(PairingSession {
                updates: tx.clone(),
                discovered: None,
            });
        }
        *self.pairing_rx.lock().await = Some(rx);
        let _ = tx.send(PairingUpdate::Searching);

        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            tokio::time::sleep(DISCOVERY_DELAY).await;
            let mut state = state.lock().await;
            if let Some(session) = state.pairing.as_mut() {
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
        let (tx, name) = {
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
            (session.updates.clone(), found.name.clone())
        };

        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            tokio::time::sleep(PASSKEY_DELAY).await;
            let _ = tx.send(PairingUpdate::Passkey(PasskeyMethod::Keyboard(
                "482913".to_string(),
            )));
            tokio::time::sleep(PASSKEY_TYPING_DELAY).await;
            let mut state = state.lock().await;
            // Session gone = cancelled while the "user" was typing.
            if state.pairing.take().is_none() {
                return;
            }
            let slot = state.pair_scripted(&name);
            let _ = tx.send(PairingUpdate::Paired { slot });
        });
        Ok(())
    }

    async fn cancel_pairing(self, _: Context) -> Result<(), PairingCommandError> {
        let mut state = self.state.lock().await;
        let Some(session) = state.pairing.take() else {
            return Err(PairingCommandError::NoActiveSession);
        };
        let _ = session
            .updates
            .send(PairingUpdate::Failed(PairingFailure::Cancelled));
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
        AgentSnapshot {
            status: agent_status(),
            inventory: self.state.lock().await.render_inventory(),
        }
    }

    async fn poll_event_monitor(self, _: Context) -> Vec<MonitorEvent> {
        Vec::new()
    }
}
