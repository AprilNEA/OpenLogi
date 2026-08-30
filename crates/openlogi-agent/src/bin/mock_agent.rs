//! Hardware-free mock agent for GUI development.
//!
//! Serves the same tarpc [`Agent`] service as the real agent, from a scripted
//! in-memory inventory: no HID I/O, no input hook, no Accessibility. The GUI
//! needs zero changes; it connects, handshakes the real [`PROTOCOL_VERSION`],
//! and renders whatever this binary scripts.
//!
//! ```sh
//! cargo run -p openlogi-agent --bin openlogi-agent-mock
//! cargo run -p openlogi-agent --bin openlogi-agent-mock -- --fixture profile.json
//! OPENLOGI_DEV_AGENT=0 cargo run -p openlogi-desktop   # in a second terminal
//! ```
//!
//! It defaults to the `openlogi-dev` profile — the one the dev app bundle
//! already uses — so it meets the dev GUI on the dev socket and the installed
//! production app is left alone. `OPENLOGI_PROFILE=prod` serves the production
//! socket instead, where the shared `agent.lock` keeps the mock and a real
//! agent from running at the same time in either direction.
//!
//! With no arguments it retains the animated scripted demo. A semantic device
//! profile loaded with `--fixture` runs in frozen test time instead: camera,
//! foreground application, battery, and pairing never advance from wall time.
//!
//! Built-in demo behavior:
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

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use interprocess::local_socket::traits::tokio::Listener as _;
use openlogi_core::app::ForegroundApp;
use openlogi_core::binding::ActionRingSlot;
use openlogi_core::config::{Config, Lighting};
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, Capabilities, DeviceInventory, DeviceKind,
    PairedDevice, StandaloneDevice,
};
use openlogi_core::single_instance::{self, InstanceError};
use openlogi_device::fixture::{
    DeviceProfile, FixtureError, ProfileDeviceSettings, ProfileSetting,
};
use openlogi_hid::{
    BacklightState, DeviceRoute, Dpi, DpiInfo, LightCommand, PasskeyMethod, ReceiverSelector,
    ScrollWheelMode, SmartShiftStatus, WriteError,
};
use openlogi_ipc::transport;
use openlogi_ipc::{
    ActionRingCommandError, ActionRingInvocation, Agent, AgentSnapshot, AgentStatus, ClientKind,
    ConfigReloadError, ForegroundApps, FoundDevice, Generation, Identity, InventoryHealth,
    MonitorEvent, OBSERVE_HOLD, Observation, PROTOCOL_VERSION, PairingCommandError, PairingFailure,
    PairingPhase, PairingUpdate, RingObservation,
};
use succession::Compat;
use tarpc::context::Context;
use tarpc::server::{BaseChannel, Channel as _};
use tokio::sync::Mutex;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[path = "mock_agent/profile.rs"]
mod profile;

use profile::{built_in_profile, parse_profile, unsupported_settings, validate_light_command};

/// Unique ID of the scripted Bolt receiver; Bolt routes are matched against it.
const RECEIVER_UID: &str = "MOCK-BOLT-01";
const MOUSE_SLOT: u8 = 1;
#[cfg(test)]
const OFFLINE_SLOT: u8 = 2;
#[cfg(test)]
const KEYBOARD_SLOT: u8 = 3;
/// How often the scripted `camera_active` flag flips.
const CAMERA_TOGGLE_PERIOD: Duration = Duration::from_secs(30);

/// How often the scripted foreground application changes, so a client's
/// per-app rendering has something switching under it.
const FOREGROUND_SWITCH_PERIOD: Duration = Duration::from_secs(10);

/// The applications the mock pretends the user is switching between, in the
/// order it cycles them. Real macOS bundle identifiers, so a profile authored
/// against the mock keeps working against a real agent.
const SCRIPTED_APPS: [(&str, &str); 3] = [
    ("com.apple.Safari", "Safari"),
    ("com.microsoft.VSCode", "Code"),
    ("com.apple.finder", "Finder"),
];

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

/// How often a held `observe` re-renders the scripted state looking for a
/// change. The real agent is told by its watchers and needs no tick at all; a
/// mock has nothing to be told by, so it compares instead.
const OBSERVE_TICK: Duration = Duration::from_millis(250);

fn main() -> ExitCode {
    default_to_dev_profile();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_env("OPENLOGI_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let state = match state_from_args(std::env::args_os().skip(1)) {
        Ok(state) => state,
        Err(error) => {
            warn!(%error, "could not build the mock profile");
            return ExitCode::FAILURE;
        }
    };

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

fn state_from_args(args: impl Iterator<Item = OsString>) -> Result<State, String> {
    if let Some(path) = parse_fixture_arg(args)? {
        let profile = load_fixture_profile(&path)?;
        State::new(profile, MockClock::Test(Duration::ZERO)).map_err(|error| error.to_string())
    } else {
        let profile = built_in_profile()?;
        State::new(profile, MockClock::Demo(Instant::now())).map_err(|error| error.to_string())
    }
}

fn parse_fixture_arg(mut args: impl Iterator<Item = OsString>) -> Result<Option<PathBuf>, String> {
    let Some(argument) = args.next() else {
        return Ok(None);
    };
    if argument != "--fixture" {
        return Err(format!(
            "unknown argument {}; expected --fixture <profile.json>",
            argument.to_string_lossy()
        ));
    }
    let path = args
        .next()
        .ok_or_else(|| "--fixture requires a profile JSON path".to_string())?;
    if let Some(extra) = args.next() {
        return Err(format!("unexpected argument {}", extra.to_string_lossy()));
    }
    Ok(Some(PathBuf::from(path)))
}

fn load_fixture_profile(path: &Path) -> Result<DeviceProfile, String> {
    let encoded = fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    parse_profile(&encoded, &path.display().to_string())
}

/// Claim the `openlogi-dev` profile unless the caller picked one.
///
/// A bare `cargo run` binary has no `-dev` bundle to be recognized by, so
/// without this the mock would resolve the *production* socket, lock and config
/// while the dev GUI (which does run from a `-dev` bundle) waits on the dev
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

/// Time source and behavior policy for the semantic mock.
enum MockClock {
    /// Wall-clock animation used only by the no-argument developer demo.
    Demo(Instant),
    /// Frozen logical time used by loaded fixtures.
    Test(Duration),
}

impl MockClock {
    fn elapsed(&self) -> Duration {
        match self {
            Self::Demo(started) => started.elapsed(),
            Self::Test(elapsed) => *elapsed,
        }
    }

    fn is_demo(&self) -> bool {
        matches!(self, Self::Demo(_))
    }

    #[cfg(test)]
    fn advance(&mut self, duration: Duration) {
        let Self::Test(elapsed) = self else {
            panic!("only frozen test time can be advanced logically");
        };
        *elapsed += duration;
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
    /// Validated device facts and mutable setting readback state.
    profile: DeviceProfile,
    /// Devices added by a scripted pairing session, appended to the Bolt
    /// receiver's paired list. The scripted devices themselves are rebuilt per
    /// poll, so this holds only what pairing added.
    paired_extra: Vec<PairedDevice>,
    /// Slot the next scripted pairing assigns.
    next_slot: u8,
    pairing: Option<PairingSession>,
    /// Event stream for the most recently started pairing session. It outlives
    /// the live session long enough for a client to drain its terminal update.
    pairing_updates: Option<UnboundedReceiver<PairingUpdate>>,
    /// Where pairing stands, for the observable snapshot. Outlives
    /// [`Self::pairing`]: a terminal phase is the session's result.
    phase: Option<PairingPhase>,
    /// Id handed to the next pairing session; only ever increases.
    next_pairing_id: u64,
    clock: MockClock,
}

impl State {
    fn new(profile: DeviceProfile, clock: MockClock) -> Result<Self, FixtureError> {
        profile.validate()?;
        let next_slot = profile
            .inventories
            .iter()
            .filter(|inventory| inventory.receiver.unique_id.as_deref() == Some(RECEIVER_UID))
            .flat_map(|inventory| inventory.paired.iter().map(|device| device.slot))
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        Ok(Self {
            profile,
            paired_extra: Vec::new(),
            next_slot,
            pairing: None,
            pairing_updates: None,
            phase: None,
            next_pairing_id: 0,
            clock,
        })
    }

    /// Publish where pairing stands. Separate from [`Self::pairing`] because a
    /// terminal phase is the session's *result* and outlives it.
    fn set_phase(&mut self, phase: PairingPhase) {
        self.phase = Some(phase);
    }

    /// Register a new pairing session and publish its initial phase together.
    fn begin_pairing(&mut self) -> Result<u64, PairingCommandError> {
        if self.pairing.is_some() {
            return Err(PairingCommandError::AlreadyActive);
        }
        let (updates, update_rx) = mpsc::unbounded_channel();
        let id = self.next_pairing_id;
        self.next_pairing_id += 1;
        let _ = updates.send(PairingUpdate::Searching);
        self.pairing = Some(PairingSession {
            id,
            updates,
            discovered: None,
        });
        self.pairing_updates = Some(update_rx);
        self.set_phase(PairingPhase::Searching);
        Ok(id)
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

    /// Select one device and publish `Pairing` in the same state transition.
    fn select_pairing_device(
        &mut self,
        address: [u8; 6],
    ) -> Result<(u64, UnboundedSender<PairingUpdate>, String), PairingCommandError> {
        let Some(session) = self.pairing.as_ref() else {
            return Err(PairingCommandError::NoActiveSession);
        };
        let Some(found) = session
            .discovered
            .as_ref()
            .filter(|found| found.address == address)
        else {
            return Err(PairingCommandError::UnknownDevice);
        };
        let selected = (session.id, session.updates.clone(), found.name.clone());
        self.set_phase(PairingPhase::Pairing);
        Ok(selected)
    }

    /// Cancel the live session, or dismiss a terminal result, as one transition.
    fn cancel_pairing(&mut self) {
        self.phase = None;
        if let Some(session) = self.pairing.take() {
            let _ = session
                .updates
                .send(PairingUpdate::Failed(PairingFailure::Cancelled));
        }
    }

    fn next_pairing_update(&mut self) -> Option<PairingUpdate> {
        self.pairing_updates
            .as_mut()
            .and_then(|updates| updates.try_recv().ok())
    }

    /// Whether a host camera is "in use" right now — flipped on a timer so the
    /// camera-linked light rendering has a changing input to follow.
    fn camera_active(&self) -> bool {
        self.clock.elapsed().as_secs() / CAMERA_TOGGLE_PERIOD.as_secs() % 2 == 1
    }

    /// The scripted foreground application, plus the ones "recently" in front.
    ///
    /// Cycles [`SCRIPTED_APPS`] on a timer the way `camera_active` flips, so a
    /// client can watch its per-app rendering follow an app switch with no
    /// hardware and no real window server. `recent` is the cycle unrolled
    /// backwards from the current position — the same newest-first,
    /// deduplicated shape the real agent publishes.
    fn foreground(&self) -> ForegroundApps {
        let app = |(id, name): (&str, &str)| ForegroundApp {
            id: id.to_string(),
            display_name: name.to_string(),
        };
        let elapsed = self.clock.elapsed().as_secs() / FOREGROUND_SWITCH_PERIOD.as_secs();
        let position = usize::try_from(elapsed).unwrap_or(usize::MAX) % SCRIPTED_APPS.len();
        let recent = (0..SCRIPTED_APPS.len())
            .map(|back| {
                app(SCRIPTED_APPS[(position + SCRIPTED_APPS.len() - back) % SCRIPTED_APPS.len()])
            })
            .collect();
        ForegroundApps {
            current: Some(app(SCRIPTED_APPS[position])),
            recent,
        }
    }

    /// The inventory as polled. Rebuilt per call so the online mouse's battery
    /// is re-derived from elapsed time: successive snapshots visibly differ and
    /// the GUI's poll → repaint loop can be watched working.
    fn render_inventory(&self) -> Vec<DeviceInventory> {
        let mut inventories = self.profile.inventories.clone();
        if self.clock.is_demo()
            && let Some(mouse) = inventories
                .iter_mut()
                .find(|inventory| inventory.receiver.unique_id.as_deref() == Some(RECEIVER_UID))
                .and_then(|inventory| {
                    inventory
                        .paired
                        .iter_mut()
                        .find(|device| device.slot == MOUSE_SLOT)
                })
        {
            mouse.battery = Some(draining_battery(self.clock.elapsed()));
        }
        if let Some(receiver) = inventories
            .iter_mut()
            .find(|inventory| inventory.receiver.unique_id.as_deref() == Some(RECEIVER_UID))
        {
            receiver.paired.extend_from_slice(&self.paired_extra);
        }
        inventories
    }

    fn standalone(&self) -> Vec<StandaloneDevice> {
        self.profile.standalone.clone()
    }

    fn settings_for(&self, route: &DeviceRoute) -> Result<&ProfileDeviceSettings, WriteError> {
        self.require_online(route)?;
        self.profile
            .settings
            .iter()
            .find(|settings| settings.route == *route)
            .ok_or(WriteError::DeviceNotFound)
    }

    fn settings_for_mut(
        &mut self,
        route: &DeviceRoute,
    ) -> Result<&mut ProfileDeviceSettings, WriteError> {
        self.require_online(route)?;
        self.profile
            .settings
            .iter_mut()
            .find(|settings| settings.route == *route)
            .ok_or(WriteError::DeviceNotFound)
    }

    fn require_online(&self, route: &DeviceRoute) -> Result<(), WriteError> {
        match self.route_online(route) {
            Some(true) => Ok(()),
            Some(false) => Err(WriteError::DeviceUnreachable {
                index: route.device_index(),
            }),
            None => Err(WriteError::DeviceNotFound),
        }
    }

    fn route_online(&self, route: &DeviceRoute) -> Option<bool> {
        self.profile
            .inventories
            .iter()
            .find_map(|inventory| {
                inventory.paired.iter().find_map(|device| {
                    (DeviceRoute::device_route_for(inventory, device.slot).as_ref() == Some(route))
                        .then_some(device.online)
                })
            })
            .or_else(|| {
                self.paired_extra.iter().find_map(|device| {
                    (route
                        == &DeviceRoute::Bolt {
                            receiver_uid: RECEIVER_UID.to_string(),
                            slot: device.slot,
                        })
                        .then_some(device.online)
                })
            })
            .or_else(|| {
                self.profile.standalone.iter().find_map(|device| {
                    (standalone_route(device) == *route).then_some(device.online)
                })
            })
    }

    #[cfg(test)]
    fn advance_test_time(&mut self, duration: Duration) {
        self.clock.advance(duration);
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
        self.profile
            .settings
            .push(unsupported_settings(DeviceRoute::Bolt {
                receiver_uid: RECEIVER_UID.to_string(),
                slot,
            }));
        slot
    }
}

fn profile_value<'a, T>(
    setting: &'a ProfileSetting<T>,
    route: &DeviceRoute,
    feature_hex: u16,
) -> Result<&'a T, WriteError> {
    match setting {
        ProfileSetting::Unsupported => Err(WriteError::FeatureUnsupported { feature_hex }),
        ProfileSetting::Supported(value) => Ok(value),
        ProfileSetting::Unavailable => Err(WriteError::DeviceUnreachable {
            index: route.device_index(),
        }),
    }
}

fn profile_value_mut<'a, T>(
    setting: &'a mut ProfileSetting<T>,
    route: &DeviceRoute,
    feature_hex: u16,
) -> Result<&'a mut T, WriteError> {
    match setting {
        ProfileSetting::Unsupported => Err(WriteError::FeatureUnsupported { feature_hex }),
        ProfileSetting::Supported(value) => Ok(value),
        ProfileSetting::Unavailable => Err(WriteError::DeviceUnreachable {
            index: route.device_index(),
        }),
    }
}

fn standalone_route(device: &StandaloneDevice) -> DeviceRoute {
    DeviceRoute::RawHid {
        vendor_id: device.address.vendor_id,
        product_id: device.address.product_id,
        usage_page: device.address.usage_page,
        usage_id: device.address.usage_id,
        identity: device.address.identity.clone(),
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
        input_monitoring_granted: true,
        hid_open_failures: false,
    }
}

/// The scripted [`Agent`] implementation, cloned per connection.
#[derive(Clone)]
struct MockAgent {
    state: Arc<Mutex<State>>,
    /// The last [`Observation`] handed out, so `observe` can stamp a new
    /// generation when the rendered state differs from it.
    served: Arc<Mutex<Observation>>,
}

impl MockAgent {
    fn new(state: State) -> Self {
        let served = Observation {
            generation: 1,
            snapshot: snapshot_of(&state),
        };
        Self {
            state: Arc::new(Mutex::new(state)),
            served: Arc::new(Mutex::new(served)),
        }
    }

    /// The current observation, stamped with a new generation if the scripted
    /// state has moved since the last one served.
    async fn current(&self) -> Observation {
        let snapshot = snapshot_of(&*self.state.lock().await);
        let mut served = self.served.lock().await;
        if served.snapshot != snapshot {
            served.generation += 1;
            served.snapshot = snapshot;
        }
        served.clone()
    }
}

/// Render what the GUI observes out of the scripted state.
fn snapshot_of(state: &State) -> AgentSnapshot {
    AgentSnapshot {
        status: agent_status(),
        inventory: state.render_inventory(),
        standalone: state.standalone(),
        camera_active: state.camera_active(),
        pairing: state.phase.clone(),
        foreground: state.foreground(),
    }
}

// Pairing updates are sent with `let _ =`: a send only fails when the GUI's
// long-poll receiver is gone (Add Device window closed / GUI died), and
// dropping the event is exactly right then.
#[expect(
    clippy::unused_async_trait_impl,
    reason = "scripted answers rarely await; keeping every method `async fn` mirrors \
              the real server impl, which is the point of the mock"
)]
impl Agent for MockAgent {
    async fn protocol_version(self, _: Context) -> u32 {
        PROTOCOL_VERSION
    }

    async fn declare_client(self, _: Context, _kind: ClientKind) {
        // The mock has no dormancy gate; declarations are accepted and ignored.
    }

    async fn identity(self, _: Context) -> Identity {
        Identity::mine(Compat::from(PROTOCOL_VERSION))
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
        // Superseded by `observe_action_ring`.
        None
    }

    async fn observe_action_ring(self, _: Context, _since: Generation) -> RingObservation {
        // The mock scripts no rings, so it only ever has "none" to report —
        // held for the window so an overlay polling it doesn't spin.
        tokio::time::sleep(OBSERVE_HOLD).await;
        RingObservation {
            generation: 1,
            invocation: None,
        }
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

    async fn set_dpi(self, _: Context, route: DeviceRoute, dpi: Dpi) -> Result<(), WriteError> {
        let mut state = self.state.lock().await;
        let settings = state.settings_for_mut(&route)?;
        let dpi_state = profile_value_mut(&mut settings.dpi, &route, 0x2201)?;
        dpi_state.current = dpi_state.capabilities.nearest(dpi);
        info!(%route, dpi = %dpi_state.current, "set_dpi");
        Ok(())
    }

    async fn set_lighting(
        self,
        _: Context,
        route: DeviceRoute,
        lighting: Lighting,
    ) -> Result<(), WriteError> {
        let state = self.state.lock().await;
        if !state.settings_for(&route)?.lighting.is_supported() {
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
        status: SmartShiftStatus,
    ) -> Result<(), WriteError> {
        let mut state = self.state.lock().await;
        let settings = state.settings_for_mut(&route)?;
        let smartshift = profile_value_mut(&mut settings.smartshift, &route, 0x2110)?;
        *smartshift = status;
        info!(%route, ?status, "set_smartshift");
        Ok(())
    }

    async fn read_dpi(self, _: Context, route: DeviceRoute) -> Result<DpiInfo, WriteError> {
        let state = self.state.lock().await;
        profile_value(&state.settings_for(&route)?.dpi, &route, 0x2201).cloned()
    }

    async fn read_smartshift(
        self,
        _: Context,
        route: DeviceRoute,
    ) -> Result<SmartShiftStatus, WriteError> {
        let state = self.state.lock().await;
        profile_value(&state.settings_for(&route)?.smartshift, &route, 0x2110).copied()
    }

    async fn read_wheel(
        self,
        _: Context,
        route: DeviceRoute,
    ) -> Result<ScrollWheelMode, WriteError> {
        let state = self.state.lock().await;
        profile_value(&state.settings_for(&route)?.wheel, &route, 0x2121).copied()
    }

    async fn read_backlight(
        self,
        _: Context,
        route: DeviceRoute,
    ) -> Result<BacklightState, WriteError> {
        let state = self.state.lock().await;
        profile_value(&state.settings_for(&route)?.backlight, &route, 0x1982).copied()
    }

    async fn request_accessibility_prompt(self, _: Context) {
        info!("request_accessibility_prompt (no-op in the mock)");
    }

    async fn start_pairing(
        self,
        _: Context,
        _selector: ReceiverSelector,
    ) -> Result<(), PairingCommandError> {
        let id = {
            let mut state = self.state.lock().await;
            if !state.clock.is_demo() {
                return Err(PairingCommandError::WatcherUnavailable);
            }
            state.begin_pairing()?
        };

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
                session.discovered = Some(found.clone());
                state.set_phase(PairingPhase::Found(vec![found]));
            }
        });
        Ok(())
    }

    async fn pair_device(self, _: Context, address: [u8; 6]) -> Result<(), PairingCommandError> {
        let (id, tx, name) = { self.state.lock().await.select_pairing_device(address)? };

        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            tokio::time::sleep(PASSKEY_DELAY).await;
            // A cancel in the meantime ends the flow: a plain cancel leaves the
            // GUI polling this very channel, so sending the prompt anyway would
            // show it a passkey *after* `Failed(Cancelled)`.
            let method = PasskeyMethod::Keyboard("482913".to_string());
            {
                let mut state = state.lock().await;
                if state.pairing_session(id).is_none() {
                    return;
                }
                state.set_phase(PairingPhase::Passkey(method.clone()));
                let _ = tx.send(PairingUpdate::Passkey(method));
            }
            tokio::time::sleep(PASSKEY_TYPING_DELAY).await;
            let mut state = state.lock().await;
            // No session of ours left = cancelled while the "user" was typing.
            // Ending it by id also means a session started in the meantime is
            // neither consumed here nor completed with our (dropped) channel.
            if state.end_pairing(id).is_none() {
                return;
            }
            let slot = state.pair_scripted(&name);
            state.set_phase(PairingPhase::Paired { slot });
            let _ = tx.send(PairingUpdate::Paired { slot });
        });
        Ok(())
    }

    async fn cancel_pairing(self, _: Context) -> Result<(), PairingCommandError> {
        // Cancelling with nothing active is `Ok` in the real agent, so it is
        // `Ok` here — the GUI must not see a different contract from the mock.
        let mut state = self.state.lock().await;
        state.cancel_pairing();
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
            if let Some(update) = self.state.lock().await.next_pairing_update() {
                return Some(update);
            }
            tokio::time::sleep(PAIRING_POLL_TICK).await;
        }
        None
    }

    async fn snapshot(self, _: Context) -> AgentSnapshot {
        snapshot_of(&*self.state.lock().await)
    }

    async fn observe(self, _: Context, since: Generation) -> Observation {
        let deadline = Instant::now() + OBSERVE_HOLD;
        loop {
            let current = self.current().await;
            if current.generation != since || Instant::now() >= deadline {
                return current;
            }
            tokio::time::sleep(OBSERVE_TICK).await;
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
        let state = self.state.lock().await;
        validate_light_command(&state, &route, command)?;
        info!(%route, ?command, "set_light");
        Ok(())
    }

    async fn set_light_manual_power(
        self,
        _: Context,
        route: DeviceRoute,
        enabled: bool,
    ) -> Result<(), WriteError> {
        let state = self.state.lock().await;
        validate_light_command(&state, &route, LightCommand::Power(enabled))?;
        info!(%route, enabled, "set_light_manual_power");
        Ok(())
    }
}

#[cfg(test)]
#[path = "mock_agent/tests.rs"]
mod tests;
