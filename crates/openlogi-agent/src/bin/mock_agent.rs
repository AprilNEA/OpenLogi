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
//!
//! While it runs, the mock reads one console command per stdin line (`help`
//! lists them) and changes what it reports: a device's battery, link, or
//! failing reads and writes; a lock-key or backlight indicator; frozen test
//! time; or every client connection, dropped as an agent restart drops them.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use interprocess::local_socket::traits::tokio::Listener as _;
use openlogi_agent_core::backlight_overlay::BacklightOverlayManager;
use openlogi_agent_core::lock_keys::LockKeyIndicator;
use openlogi_agent_core::notices::{LowBatteryWatch, NoticeBoard};
use openlogi_core::app::ForegroundApp;
use openlogi_core::binding::ActionRingSlot;
use openlogi_core::config::{AppSettings, Config, Lighting};
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, Capabilities, DeviceInventory, DeviceKind, LockKey,
    PairedDevice, StandaloneDevice, prettify_codename,
};
use openlogi_core::hid::{DisableKeysMask, DisableKeysState, HidppOperation};
use openlogi_core::single_instance::{self, InstanceError, Role};
use openlogi_fixture::{DeviceProfile, FixtureError, ProfileDeviceSettings, ProfileSetting};
use openlogi_hid::{
    BacklightChange, BacklightState, DeviceRoute, Dpi, DpiInfo, EasySwitchHosts, FnLockState,
    ForceSensingInfo, LightCommand, PasskeyMethod, PointerSpeed, ReceiverSelector, ReportRate,
    ReportRateInfo, ScrollWheelMode, SmartShiftStatus, WriteError,
};
use openlogi_ipc::transport;
use openlogi_ipc::{
    ActionRingCommandError, ActionRingInvocation, Agent, AgentSnapshot, AgentStatus,
    BacklightObservation, ClientKind, ConfigReloadError, ForegroundApps, FoundDevice, Generation,
    Identity, InventoryHealth, LockKeyChange, LockKeyObservation, MonitorEvent, NoticeKind,
    NoticeObservation, OBSERVE_HOLD, Observation, PROTOCOL_VERSION, PairingCommandError,
    PairingFailure, PairingPhase, PairingUpdate, RingObservation, SavedConfig, SavedConfigVerdict,
};
use succession::Compat;
use tarpc::context::Context;
use tarpc::server::{BaseChannel, Channel as _};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::{Mutex, watch};
use tracing::{info, warn};

#[path = "mock_agent/control.rs"]
mod control;
#[path = "mock_agent/profile.rs"]
mod profile;

use control::Fault;
use profile::{built_in_profile, parse_profile, unsupported_settings, validate_light_command};

/// Unique ID of the scripted Bolt receiver; Bolt routes are matched against it.
const RECEIVER_UID: &str = "OL-BOLT-UID-0001";
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
const PAIRING_POLL_PERIOD: Duration = Duration::from_millis(100);

/// How often a held `observe` re-renders the scripted state looking for a
/// change. The real agent is told by its watchers and needs no tick at all; a
/// mock has nothing to be told by, so it compares instead.
const OBSERVE_POLL_PERIOD: Duration = Duration::from_millis(250);

fn main() -> ExitCode {
    default_to_dev_profile();
    openlogi_core::logging::init_stderr();

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
    let _guard = match single_instance::acquire(Role::Agent) {
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
    let served = runtime.block_on(async {
        let agent = MockAgent::new(state);
        control::spawn_console(agent.clone());
        serve(agent).await
    });
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            warn!(error = %e, "could not bind the IPC socket");
            ExitCode::FAILURE
        }
    }
}

fn state_from_args(args: impl Iterator<Item = OsString>) -> Result<State, String> {
    let scenario = DisableKeysScenario::from_env()?;
    if let Some(path) = parse_fixture_arg(args)? {
        let profile = load_fixture_profile(&path)?;
        State::with_disable_keys_scenario(profile, MockClock::Test(Duration::ZERO), scenario)
            .map_err(|error| error.to_string())
    } else {
        let profile = built_in_profile()?;
        State::with_disable_keys_scenario(profile, MockClock::Demo(Instant::now()), scenario)
            .map_err(|error| error.to_string())
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
    if std::env::var_os(openlogi_core::env::PROFILE).is_some() {
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
        std::env::set_var(
            openlogi_core::env::PROFILE,
            openlogi_core::paths::Profile::Dev.env_value(),
        );
    }
}

/// Accept loop — the mock's copy of `server::run` (kept verbatim rather than
/// making the production loop generic over its service impl for a dev tool).
async fn serve(server: MockAgent) -> std::io::Result<()> {
    serve_on(transport::bind()?, server).await
}

async fn serve_on(
    listener: interprocess::local_socket::tokio::Listener,
    server: MockAgent,
) -> std::io::Result<()> {
    info!(
        profile = std::env::var(openlogi_core::env::PROFILE).unwrap_or_default(),
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
        tokio::spawn(async move {
            let requests =
                channel
                    .execute(server.clone().serve())
                    .for_each(|response| async move {
                        tokio::spawn(response);
                    });
            server.hold_connection(requests).await;
        });
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

    /// Move frozen test time forward. Demo time follows the wall clock and
    /// cannot be moved, and a move past the clock's range leaves it as it is.
    fn advance(&mut self, duration: Duration) -> Result<(), String> {
        let Self::Test(elapsed) = self else {
            return Err(
                "demo time follows the wall clock; start the mock with --fixture to move time"
                    .to_owned(),
            );
        };
        *elapsed = elapsed
            .checked_add(duration)
            .ok_or_else(|| "test time would pass the clock's range".to_owned())?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DisableKeysScenario {
    #[default]
    Ready,
    ReadFailsUntilRetry,
    WriteMismatch,
    WriteDelaySwitch,
    SameRouteReconnect,
    ReloadFailsOnce,
}

impl DisableKeysScenario {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "ready" => Ok(Self::Ready),
            "read-fails-until-retry" => Ok(Self::ReadFailsUntilRetry),
            "write-mismatch" => Ok(Self::WriteMismatch),
            "write-delay-switch" => Ok(Self::WriteDelaySwitch),
            "same-route-reconnect" => Ok(Self::SameRouteReconnect),
            "reload-fails-once" => Ok(Self::ReloadFailsOnce),
            _ => Err(format!(
                "unknown OPENLOGI_MOCK_DISABLE_KEYS_SCENARIO={value}"
            )),
        }
    }

    fn from_env() -> Result<Self, String> {
        match std::env::var("OPENLOGI_MOCK_DISABLE_KEYS_SCENARIO") {
            Ok(value) => Self::parse(&value),
            Err(std::env::VarError::NotPresent) => Ok(Self::Ready),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err("OPENLOGI_MOCK_DISABLE_KEYS_SCENARIO is not valid Unicode".into())
            }
        }
    }

    fn keyboard_online(self, elapsed: Duration) -> bool {
        match self {
            Self::SameRouteReconnect => !(2..4).contains(&elapsed.as_secs()),
            _ => true,
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
    /// Battery readings the console set, reported over the profile's.
    battery_overrides: Vec<(DeviceRoute, BatteryInfo)>,
    /// Devices the console made fail every read and write while online.
    faults: Vec<(DeviceRoute, Fault)>,
    /// Which batteries the console's readings have announced, as the agent
    /// keeps it.
    low_battery: LowBatteryWatch,
    disable_keys_scenario: DisableKeysScenario,
    disable_keys_reads: u64,
    disable_keys_writes: u64,
    reload_calls: u64,
}

impl State {
    #[cfg(test)]
    fn new(profile: DeviceProfile, clock: MockClock) -> Result<Self, FixtureError> {
        Self::with_disable_keys_scenario(profile, clock, DisableKeysScenario::Ready)
    }

    fn with_disable_keys_scenario(
        profile: DeviceProfile,
        clock: MockClock,
        disable_keys_scenario: DisableKeysScenario,
    ) -> Result<Self, FixtureError> {
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
            battery_overrides: Vec::new(),
            faults: Vec::new(),
            low_battery: LowBatteryWatch::default(),
            disable_keys_scenario,
            disable_keys_reads: 0,
            disable_keys_writes: 0,
            reload_calls: 0,
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

    /// The low battery an overlay is shown when it first looks, so the
    /// notice can be seen without waiting out the sawtooth. Demo time only:
    /// a `--fixture` run stays as recorded.
    fn demo_low_battery(&self) -> Option<NoticeKind> {
        if !self.clock.is_demo() {
            return None;
        }
        let mouse = self
            .profile
            .inventories
            .iter()
            .find(|inventory| inventory.receiver.unique_id.as_deref() == Some(RECEIVER_UID))?
            .paired
            .iter()
            .find(|device| device.slot == MOUSE_SLOT)?;
        Some(NoticeKind::LowBattery {
            name: mouse.codename.as_deref().map(prettify_codename),
            kind: mouse.kind,
            battery: BatteryInfo {
                percentage: Some(15),
                level: BatteryLevel::Low,
                status: BatteryStatus::Discharging,
            },
        })
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
        for inventory in &mut inventories {
            let routes = slot_routes(inventory);
            for (device, route) in inventory.paired.iter_mut().zip(routes) {
                if let Some((_, battery)) = self
                    .battery_overrides
                    .iter()
                    .find(|(overridden, _)| Some(overridden) == route.as_ref())
                {
                    device.battery = Some(battery.clone());
                }
            }
        }
        let keyboard_online = self
            .disable_keys_scenario
            .keyboard_online(self.clock.elapsed());
        for device in inventories
            .iter_mut()
            .flat_map(|inventory| &mut inventory.paired)
            .filter(|device| device.capabilities.is_some_and(|caps| caps.disable_keys))
        {
            device.online &= keyboard_online;
        }
        inventories
    }

    fn standalone(&self) -> Vec<StandaloneDevice> {
        self.profile.standalone.clone()
    }

    fn settings_for(&self, route: &DeviceRoute) -> Result<&ProfileDeviceSettings, WriteError> {
        self.require_reachable(route)?;
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
        self.require_reachable(route)?;
        self.profile
            .settings
            .iter_mut()
            .find(|settings| settings.route == *route)
            .ok_or(WriteError::DeviceNotFound)
    }

    /// Whether `route` answers: it is online, and the console has not made it
    /// fail.
    fn require_reachable(&self, route: &DeviceRoute) -> Result<(), WriteError> {
        match self
            .targets()
            .into_iter()
            .find(|target| target.route == *route)
        {
            Some(target) if !target.online => Err(WriteError::DeviceUnreachable {
                index: route.device_index(),
            }),
            Some(_) => match self.faults.iter().find(|(failing, _)| failing == route) {
                Some((_, fault)) => Err(fault.error(route)),
                None => Ok(()),
            },
            None => Err(WriteError::DeviceNotFound),
        }
    }

    /// Every device a client sees, paired ones first, in the order the
    /// console numbers them.
    fn targets(&self) -> Vec<Target> {
        let mut targets = Vec::new();
        for inventory in self.render_inventory() {
            for device in &inventory.paired {
                let Some(route) = DeviceRoute::for_slot(&inventory, device.slot) else {
                    continue;
                };
                targets.push(Target {
                    route,
                    name: device.codename.as_deref().map(prettify_codename),
                    kind: device.kind,
                    online: device.online,
                    battery: device.battery.clone(),
                    paired: true,
                });
            }
        }
        targets.extend(self.profile.standalone.iter().map(|device| Target {
            route: device.route(),
            name: Some(device.display_name.clone()),
            kind: device.kind,
            online: device.online,
            battery: None,
            paired: false,
        }));
        targets
    }

    /// The online flag of the device `route` names, wherever the profile or
    /// pairing keeps it.
    fn online_mut(&mut self, route: &DeviceRoute) -> Option<&mut bool> {
        if let Some(device) = self.paired_extra.iter_mut().find(|device| {
            *route
                == DeviceRoute::Bolt {
                    receiver_uid: RECEIVER_UID.to_string(),
                    slot: device.slot,
                }
        }) {
            return Some(&mut device.online);
        }
        for inventory in &mut self.profile.inventories {
            let routes = slot_routes(inventory);
            if let Some((device, _)) = inventory
                .paired
                .iter_mut()
                .zip(routes)
                .find(|(_, paired)| paired.as_ref() == Some(route))
            {
                return Some(&mut device.online);
            }
        }
        self.profile
            .standalone
            .iter_mut()
            .find(|device| device.route() == *route)
            .map(|device| &mut device.online)
    }

    #[cfg(test)]
    fn advance_test_time(&mut self, duration: Duration) {
        self.clock
            .advance(duration)
            .expect("tests advance frozen test time only");
    }

    /// Drop the paired device `route` names, as its receiver does on unpair.
    fn unpair(&mut self, route: &DeviceRoute) -> Result<(), PairingFailure> {
        let (DeviceRoute::Bolt { receiver_uid, slot }
        | DeviceRoute::Unifying { receiver_uid, slot }) = route
        else {
            return Err(PairingFailure::ReceiverNotFound);
        };
        let names =
            |uid: Option<&str>| uid.is_some_and(|uid| uid.eq_ignore_ascii_case(receiver_uid));
        let inventory = self
            .profile
            .inventories
            .iter_mut()
            .find(|inventory| names(inventory.receiver.unique_id.as_deref()))
            .ok_or(PairingFailure::ReceiverNotFound)?;
        inventory.paired.retain(|device| device.slot != *slot);
        if names(Some(RECEIVER_UID)) {
            self.paired_extra.retain(|device| device.slot != *slot);
        }
        self.battery_overrides
            .retain(|(overridden, _)| overridden != route);
        self.faults.retain(|(failing, _)| failing != route);
        Ok(())
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
                percentage: Some(90),
                level: BatteryLevel::Full,
                status: BatteryStatus::Discharging,
            }),
            model_info: None,
            capabilities: Some(Capabilities::default()),
            settings_revision: 0,
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

/// The route of each device `inventory` pairs, in its order.
fn slot_routes(inventory: &DeviceInventory) -> Vec<Option<DeviceRoute>> {
    inventory
        .paired
        .iter()
        .map(|device| DeviceRoute::for_slot(inventory, device.slot))
        .collect()
}

/// A device as a client sees it, which console commands address by number.
struct Target {
    route: DeviceRoute,
    name: Option<String>,
    kind: DeviceKind,
    online: bool,
    battery: Option<BatteryInfo>,
    /// Whether a receiver pairs it, so that it reports a battery.
    paired: bool,
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

/// Sawtooth battery for the online mouse: 80% draining ~1%/minute down to
/// 20%, then back to 80%, with the coarse level tracking the percentage.
fn draining_battery(elapsed: Duration) -> BatteryInfo {
    let drained = u8::try_from(elapsed.as_secs() / 60 % 61).unwrap_or(0);
    battery_at(80 - drained, BatteryStatus::Discharging)
}

/// A battery reading `percentage`, with the coarse level a device reports
/// beside it.
fn battery_at(percentage: u8, status: BatteryStatus) -> BatteryInfo {
    BatteryInfo {
        percentage: Some(percentage),
        level: match percentage {
            0..=10 => BatteryLevel::Critical,
            11..=25 => BatteryLevel::Low,
            _ => BatteryLevel::Good,
        },
        status,
    }
}

/// The user's settings from the config file, as the agent applies them; the
/// defaults when the file cannot be read.
fn app_settings() -> AppSettings {
    Config::load_or_default().map_or_else(
        |error| {
            warn!(%error, "could not read the config; using the default settings");
            AppSettings::default()
        },
        |config| config.app_settings,
    )
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
        secure_input: openlogi_ipc::SecureInput::Off,
        virtual_keyboard: openlogi_ipc::VirtualKeyboardStatus::Unsupported,
    }
}

/// The scripted [`Agent`] implementation, cloned per connection.
#[derive(Clone)]
struct MockAgent {
    state: Arc<Mutex<State>>,
    /// The last [`Observation`] handed out, so `observe` can stamp a new
    /// generation when the rendered state differs from it.
    served: Arc<Mutex<Observation>>,
    /// The notices the overlay shows, posted and ended as the agent does.
    notices: Arc<NoticeBoard>,
    /// The lock-key indicator the overlay shows, ended as the agent ends it.
    lock_keys: LockKeyIndicator,
    /// The backlight indicator the overlay shows, ended as the agent ends it.
    backlight: BacklightOverlayManager,
    /// Signalled by the console to close every open client connection. Each
    /// connection holds one receiver.
    disconnects: Arc<watch::Sender<()>>,
    /// Where the console and the demo read the user's settings: the config
    /// file, as the agent reads it. Tests set their own.
    settings: fn() -> AppSettings,
}

impl MockAgent {
    fn new(state: State) -> Self {
        let served = Observation {
            generation: 1,
            snapshot: snapshot_of(&state),
        };
        let (disconnects, _) = watch::channel(());
        Self {
            state: Arc::new(Mutex::new(state)),
            served: Arc::new(Mutex::new(served)),
            notices: Arc::new(NoticeBoard::default()),
            lock_keys: LockKeyIndicator::default(),
            backlight: BacklightOverlayManager::default(),
            disconnects: Arc::new(disconnects),
            settings: app_settings,
        }
    }

    /// Serve one client connection until the client closes it, or until the
    /// console disconnects every client, as an agent restart does. Dropping
    /// `connection` closes its transport.
    async fn hold_connection(&self, connection: impl Future<Output = ()>) {
        let mut disconnects = self.disconnects.subscribe();
        tokio::select! {
            () = connection => {}
            // The sender lives as long as `self`, so `changed` cannot fail.
            _ = disconnects.changed() => info!("closed a client connection"),
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
        lights: Vec::new(),
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
        let mut state = self.state.lock().await;
        state.reload_calls += 1;
        if state.disable_keys_scenario == DisableKeysScenario::ReloadFailsOnce
            && state.reload_calls == 2
        {
            return Err(ConfigReloadError {
                message: "scripted Disable Keys reload failure".into(),
            });
        }
        info!(
            call = state.reload_calls,
            "reload_config (no-op in the mock)"
        );
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

    async fn observe_backlight(self, _: Context, since: Generation) -> BacklightObservation {
        self.backlight.observe(since).await
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

    async fn read_fn_lock(self, _: Context, route: DeviceRoute) -> Result<FnLockState, WriteError> {
        let state = self.state.lock().await;
        profile_value(&state.settings_for(&route)?.fn_lock, &route, 0x40a3).copied()
    }

    async fn set_fn_lock(
        self,
        _: Context,
        route: DeviceRoute,
        fn_lock: bool,
    ) -> Result<FnLockState, WriteError> {
        let mut state = self.state.lock().await;
        let settings = state.settings_for_mut(&route)?;
        let stored = profile_value_mut(&mut settings.fn_lock, &route, 0x40a3)?;
        stored.fn_lock = fn_lock;
        info!(%route, fn_lock, "set_fn_lock");
        Ok(*stored)
    }

    async fn unpair_device(self, _: Context, route: DeviceRoute) -> Result<(), PairingFailure> {
        let result = self.state.lock().await.unpair(&route);
        info!(%route, ok = result.is_ok(), "unpair_device");
        result
    }
    async fn read_report_rate(
        self,
        _: Context,
        route: DeviceRoute,
    ) -> Result<ReportRateInfo, WriteError> {
        let state = self.state.lock().await;
        profile_value(&state.settings_for(&route)?.report_rate, &route, 0x8061).cloned()
    }
    async fn set_report_rate(
        self,
        _: Context,
        route: DeviceRoute,
        rate: ReportRate,
    ) -> Result<ReportRateInfo, WriteError> {
        let mut state = self.state.lock().await;
        let settings = state.settings_for_mut(&route)?;
        let stored = profile_value_mut(&mut settings.report_rate, &route, 0x8061)?;
        if !stored.supports(rate) {
            return Err(WriteError::ReportRateUnsupported { requested: rate });
        }
        stored.current = rate;
        info!(%route, %rate, "set_report_rate");
        Ok(stored.clone())
    }
    async fn read_pointer_speed(
        self,
        _: Context,
        route: DeviceRoute,
    ) -> Result<PointerSpeed, WriteError> {
        let state = self.state.lock().await;
        profile_value(&state.settings_for(&route)?.pointer_speed, &route, 0x2205).copied()
    }
    async fn set_pointer_speed(
        self,
        _: Context,
        route: DeviceRoute,
        speed: PointerSpeed,
    ) -> Result<PointerSpeed, WriteError> {
        let mut state = self.state.lock().await;
        let settings = state.settings_for_mut(&route)?;
        let stored = profile_value_mut(&mut settings.pointer_speed, &route, 0x2205)?;
        *stored = speed;
        info!(%route, %speed, "set_pointer_speed");
        Ok(speed)
    }
    async fn read_force_sensing(
        self,
        _: Context,
        route: DeviceRoute,
    ) -> Result<ForceSensingInfo, WriteError> {
        let state = self.state.lock().await;
        profile_value(&state.settings_for(&route)?.force_sensing, &route, 0x19c0).copied()
    }
    async fn set_force_sensing(
        self,
        _: Context,
        route: DeviceRoute,
        force: u16,
    ) -> Result<ForceSensingInfo, WriteError> {
        let mut state = self.state.lock().await;
        let settings = state.settings_for_mut(&route)?;
        let stored = profile_value_mut(&mut settings.force_sensing, &route, 0x19c0)?;
        if !stored.changeable {
            return Err(WriteError::ForceSensingUnchangeable);
        }
        stored.current = stored.clamp(force);
        info!(%route, force, applied = stored.current, "set_force_sensing");
        Ok(*stored)
    }
    async fn set_backlight(
        self,
        _: Context,
        route: DeviceRoute,
        change: BacklightChange,
    ) -> Result<BacklightState, WriteError> {
        let mut state = self.state.lock().await;
        let settings = state.settings_for_mut(&route)?;
        let stored = profile_value_mut(&mut settings.backlight, &route, 0x1982)?;
        if !stored.accepts(change) {
            return Err(WriteError::BacklightChangeUnsupported { change });
        }
        *stored = stored.changed(change);
        info!(%route, %change, "set_backlight");
        Ok(*stored)
    }

    async fn read_easy_switch_hosts(
        self,
        _: Context,
        route: DeviceRoute,
    ) -> Result<EasySwitchHosts, WriteError> {
        let state = self.state.lock().await;
        profile_value(&state.settings_for(&route)?.easy_switch, &route, 0x1815).cloned()
    }

    async fn rename_easy_switch_host(
        self,
        _: Context,
        route: DeviceRoute,
        slot: u8,
        name: String,
    ) -> Result<EasySwitchHosts, WriteError> {
        let mut state = self.state.lock().await;
        let settings = state.settings_for_mut(&route)?;
        let stored = profile_value_mut(&mut settings.easy_switch, &route, 0x1815)?;
        let name = stored.name_for(slot, &name)?.to_owned();
        if let Some(host) = stored.slots.iter_mut().find(|host| host.index == slot) {
            host.name = Some(name);
        }
        info!(%route, slot, "rename_easy_switch_host");
        Ok(stored.clone())
    }
    /// The mock reports no changes on its lights, so it has none to keep.
    async fn reload_saved_config(
        self,
        _: Context,
        _saved: SavedConfig,
    ) -> Result<(), ConfigReloadError> {
        info!("reload_saved_config (no-op in the mock)");
        Ok(())
    }

    async fn reload_saved_config_with_verdict(
        self,
        _: Context,
        _saved: SavedConfig,
    ) -> Result<SavedConfigVerdict, ConfigReloadError> {
        info!("reload_saved_config_with_verdict (accepted no-op in the mock)");
        Ok(SavedConfigVerdict::Accepted)
    }
    async fn observe_notices(self, _: Context, since: Generation) -> NoticeObservation {
        if since == 0
            && let Some(notice) = self.state.lock().await.demo_low_battery()
        {
            self.notices.post(notice, None);
        }
        self.notices.observe(since).await
    }

    async fn observe_lock_keys(self, _: Context, since: Generation) -> LockKeyObservation {
        // A demo turns Caps Lock on for the overlay's first look, so the
        // indicator can be seen without a keyboard.
        if since == 0 && self.state.lock().await.clock.is_demo() {
            let shown = self.lock_keys.announce(
                LockKeyChange {
                    key: LockKey::CapsLock,
                    engaged: true,
                },
                &(self.settings)(),
            );
            info!(shown, "demo Caps Lock for the overlay's first look");
        }
        self.lock_keys.observe(since).await
    }

    async fn read_disable_keys(
        self,
        _: Context,
        route: DeviceRoute,
    ) -> Result<DisableKeysState, WriteError> {
        let mut state = self.state.lock().await;
        state.disable_keys_reads += 1;
        if state.disable_keys_scenario == DisableKeysScenario::ReadFailsUntilRetry
            && state.disable_keys_reads <= 3
        {
            return Err(WriteError::RequestTimedOut {
                operation: HidppOperation::ReadDisableKeys,
            });
        }
        profile_value(&state.settings_for(&route)?.disable_keys, &route, 0x4521).copied()
    }

    async fn set_disable_keys(
        self,
        _: Context,
        route: DeviceRoute,
        desired: DisableKeysMask,
    ) -> Result<DisableKeysState, WriteError> {
        let delay = {
            let state = self.state.lock().await;
            state.disable_keys_scenario == DisableKeysScenario::WriteDelaySwitch
        };
        if delay {
            tokio::time::sleep(Duration::from_secs(3)).await;
        }

        let mut state = self.state.lock().await;
        state.disable_keys_writes += 1;
        let mismatch = state.disable_keys_scenario == DisableKeysScenario::WriteMismatch;
        let settings = state.settings_for_mut(&route)?;
        let current = *profile_value_mut(&mut settings.disable_keys, &route, 0x4521)?;
        let replacement = current.replacement_for(desired)?;
        let actual = if mismatch {
            replacement & !DisableKeysMask::CAPS_LOCK
        } else {
            replacement
        };
        let confirmed = DisableKeysState {
            supported: current.supported,
            disabled: actual,
        };
        *profile_value_mut(&mut settings.disable_keys, &route, 0x4521)? = confirmed;
        let expected_supported = replacement & current.supported;
        let actual_supported = actual & current.supported;
        if expected_supported != actual_supported {
            return Err(WriteError::WriteNotApplied {
                operation: HidppOperation::WriteDisableKeys,
                feature_hex: 0x4521,
                expected: u64::from(expected_supported.bits()),
                actual: u64::from(actual_supported.bits()),
            });
        }
        info!(%route, desired = desired.bits(), actual = actual.bits(), "set_disable_keys");
        Ok(confirmed)
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
            tokio::time::sleep(PAIRING_POLL_PERIOD).await;
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
            tokio::time::sleep(OBSERVE_POLL_PERIOD).await;
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
