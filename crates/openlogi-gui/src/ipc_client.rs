//! Client side of the agent IPC.
//!
//! The agent owns all device I/O, so the GUI never opens a device — it connects
//! to the agent's Unix socket and (a) polls status + inventory on a timer to
//! drive the device list and the Accessibility gate, and (b) forwards "apply
//! now" / "read" device commands. Both run on one dedicated OS thread with a
//! tokio runtime (the GPUI thread owns no async runtime), mirroring the old
//! watcher pattern: results cross back over `mpsc` to the GPUI loop.
//!
//! The single client connection is re-established by this loop itself: polling
//! runs at [`STARTUP_POLL_PERIOD`] until the agent's first completed
//! enumeration and again after any disconnect (an agent self-exec on update, a
//! crash), and [`spawn_agent`] relaunches the binary when the socket stays
//! down — there is no launchd dependency here (`KeepAlive` only acts when the
//! agent *exits*, and autostart may be off entirely). When the agent stays
//! unreachable or answers with a newer protocol, that is pushed to the GUI as
//! a [`GuiUpdate`] so the window can say so instead of spinning forever.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use openlogi_agent_core::ipc::{
    AgentClient, AgentStatus, InventoryHealth, PROTOCOL_VERSION, PairingUpdate,
};
use openlogi_core::config::Lighting;
use openlogi_core::device::DeviceInventory;
use openlogi_hid::{
    DeviceRoute, DpiInfo, ReceiverSelector, SmartShiftMode, SmartShiftStatus, WriteError,
};
use tarpc::client;
use tarpc::context;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

/// Minimum gap between agent-launch attempts while the socket is unreachable.
/// Long enough that a missing or crash-looping binary can't be respawned in a
/// tight loop, short enough that a quit / crashed agent is recovered promptly.
const SPAWN_RETRY_PERIOD: Duration = Duration::from_secs(30);

/// Poll cadence until the agent reports its first completed enumeration
/// ([`InventoryHealth::Ready`]). The steady `poll_period` is tuned for quiet
/// background refresh; at startup it would leave the window on its loading
/// frame for up to a full period *after* the agent already knows the devices.
/// A status+inventory round every 250 ms is noise for the agent and gets the
/// gallery up moments after enumeration lands.
const STARTUP_POLL_PERIOD: Duration = Duration::from_millis(250);

/// How long the fast phase may run without readiness before falling back to
/// the steady cadence (agent start plus a worst-case first enumeration is
/// ~6 s) — an agent that never becomes ready must not hold the loop at 4 Hz
/// for the GUI's lifetime. Doubles as the threshold after which a snapshot-
/// less connection is reported to the GUI as [`GuiUpdate::Unreachable`].
const FAST_PHASE_MAX: Duration = Duration::from_secs(15);

/// What the client thread tells the GPUI loop.
pub enum GuiUpdate {
    /// A delivered status + inventory snapshot.
    Snapshot(PollUpdate),
    /// No snapshot for [`FAST_PHASE_MAX`] while disconnected: the agent is
    /// genuinely unreachable (not just starting up). Sent once per outage;
    /// the next snapshot supersedes it.
    Unreachable,
    /// The agent answered the handshake with a *newer* protocol — the app was
    /// updated on disk while this GUI kept running, and only a relaunch
    /// helps. Sent once per episode.
    OutdatedGui,
}

/// A poll snapshot pushed to the GPUI loop on every successful poll round.
pub struct PollUpdate {
    pub inventory: Vec<DeviceInventory>,
    pub status: AgentStatus,
}

/// A device command sent from the GPUI thread to the client thread. Reads carry
/// a `oneshot` for the reply; "apply now" writes are fire-and-forget (the GUI
/// updates its display optimistically and the client logs any device failure).
pub enum Command {
    SetDpi(DeviceRoute, u32),
    SetLighting(DeviceRoute, Lighting),
    SetSmartShift(DeviceRoute, SmartShiftMode, u8, u8),
    ReadDpi(DeviceRoute, oneshot::Sender<Result<DpiInfo, WriteError>>),
    ReadSmartShift(
        DeviceRoute,
        oneshot::Sender<Result<SmartShiftStatus, WriteError>>,
    ),
    ReloadConfig,
    /// Ask the agent to fire the macOS Accessibility prompt. The agent owns the
    /// CGEventTap, so the system dialog must name (and authorize) the *agent*
    /// binary, not the GUI — prompting locally would grant the wrong process.
    RequestAccessibilityPrompt,
    /// Pairing (agent-owned, since it opens the receiver): begin a session,
    /// pair a discovered device by address, or cancel. Events stream back via
    /// the separate [`IpcClient::pairing`] long-poll, not these commands.
    StartPairing(ReceiverSelector),
    PairDevice([u8; 6]),
    CancelPairing,
}

/// Handle the GUI holds to talk to the agent: a stream of poll updates, a
/// sender for device commands, and a stream of pairing events (long-polled on a
/// separate connection so a held pairing poll never stalls inventory).
pub struct IpcClient {
    pub updates: mpsc::UnboundedReceiver<GuiUpdate>,
    pub commands: mpsc::UnboundedSender<Command>,
    pub pairing: mpsc::UnboundedReceiver<PairingUpdate>,
}

/// Spawn the IPC client thread. Returns immediately; the thread connects (and
/// reconnects) on its own.
#[must_use]
pub fn spawn(poll_period: Duration) -> IpcClient {
    let (update_tx, updates) = mpsc::unbounded_channel();
    let (commands, mut cmd_rx) = mpsc::unbounded_channel::<Command>();
    let (pairing_tx, pairing) = mpsc::unbounded_channel();

    let spawn_result = std::thread::Builder::new()
        .name("openlogi-ipc-client".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    warn!(error = %e, "tokio runtime init failed; IPC client exiting");
                    return;
                }
            };
            rt.block_on(async move {
                // Pairing events stream on their own connection + long-poll so
                // a held next_pairing never delays the status/inventory poll.
                tokio::spawn(pairing_poll(pairing_tx));
                poll_loop(poll_period, &update_tx, &mut cmd_rx).await;
            });
        });
    if let Err(e) = spawn_result {
        warn!(error = %e, "could not spawn IPC client thread — agent state unavailable");
    }

    IpcClient {
        updates,
        commands,
        pairing,
    }
}

/// The poll/command select loop. Cadence policy lives in [`pacing::Pacing`];
/// this function maps its decisions onto the tokio interval and owns the
/// connection, the spawn retry, and the once-per-episode GUI notices.
async fn poll_loop(
    poll_period: Duration,
    update_tx: &mpsc::UnboundedSender<GuiUpdate>,
    cmd_rx: &mut mpsc::UnboundedReceiver<Command>,
) {
    let mut client: Option<AgentClient> = None;
    // The agent is normally started by launchd, but the GUI launches it if the
    // socket is down — invaluable in dev (one `cargo run` of the GUI brings
    // the whole system up) and a prod fallback. Retry while the socket stays
    // down, but rate-limited (see SPAWN_RETRY_PERIOD) so a missing / failing
    // binary can't become a tight respawn loop.
    let mut last_spawn_attempt: Option<Instant> = None;
    let started = Instant::now();
    let mut last_delivery: Option<Instant> = None;
    let mut notified_unreachable = false;
    let mut notified_outdated = false;
    let mut pacing = pacing::Pacing::new(poll_period, FAST_PHASE_MAX, started);
    let mut interval = ticker(None, STARTUP_POLL_PERIOD);
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let now = Instant::now();
                // Skip the spawn gate on the tick that *detected* a drop: an
                // agent self-exec rebinds the socket within a tick, and
                // spawning a duplicate right away would race it for the
                // singleton lock (and could knock the launchd-tracked copy
                // out of supervision via a clean duplicate-exit).
                let mut just_disconnected = false;
                let cadence = match poll(&mut client, update_tx).await {
                    Ok(PollOutcome::Delivered { ready }) => {
                        last_delivery = Some(now);
                        notified_unreachable = false;
                        notified_outdated = false;
                        pacing.on_delivered(ready, now)
                    }
                    Ok(PollOutcome::NoAgent) => pacing.on_unreachable(now),
                    Ok(PollOutcome::NewerAgent) => {
                        if !notified_outdated {
                            notified_outdated = true;
                            let _ = update_tx.send(GuiUpdate::OutdatedGui);
                        }
                        pacing.on_newer_agent(now)
                    }
                    Err(()) => {
                        client = None; // drop the dead connection; reconnect next tick
                        just_disconnected = true;
                        pacing.on_disconnect(now)
                    }
                };
                if let Some(cadence) = cadence {
                    interval = apply_cadence(cadence, &pacing);
                }
                if client.is_none()
                    && !notified_unreachable
                    && now.duration_since(last_delivery.unwrap_or(started)) >= FAST_PHASE_MAX
                {
                    notified_unreachable = true;
                    let _ = update_tx.send(GuiUpdate::Unreachable);
                }
                if client.is_none()
                    && !just_disconnected
                    && last_spawn_attempt.is_none_or(|t| t.elapsed() >= SPAWN_RETRY_PERIOD)
                {
                    spawn_agent();
                    last_spawn_attempt = Some(Instant::now());
                }
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break }; // GUI dropped the sender → shut down
                if handle(&mut client, cmd).await.is_err() {
                    // Same as a poll-detected drop: back to the fast cadence
                    // so the reconnect (agent self-exec, crash) re-converges
                    // just as quickly as at startup.
                    client = None;
                    if let Some(cadence) = pacing.on_disconnect(Instant::now()) {
                        interval = apply_cadence(cadence, &pacing);
                    }
                }
            }
        }
    }
}

/// Build the interval for a cadence decision. The fast interval ticks
/// immediately — after a disconnect that means one instant reconnect probe,
/// which is deliberate. The steady interval starts a full period out: the
/// poll that triggered the switch already ran, and a fresh `interval` would
/// fire a redundant back-to-back poll.
fn apply_cadence(cadence: pacing::Cadence, pacing: &pacing::Pacing) -> tokio::time::Interval {
    match cadence {
        pacing::Cadence::Fast => ticker(None, STARTUP_POLL_PERIOD),
        pacing::Cadence::Steady => ticker(Some(pacing.steady_period()), pacing.steady_period()),
    }
}

/// A tokio interval that *delays* missed ticks instead of bursting them: a
/// stalled poll (the RPC deadline is ~10 s) would otherwise accrue dozens of
/// 250 ms ticks and replay them back-to-back once it returns.
fn ticker(first_in: Option<Duration>, period: Duration) -> tokio::time::Interval {
    let mut interval = match first_in {
        Some(delay) => tokio::time::interval_at(tokio::time::Instant::now() + delay, period),
        None => tokio::time::interval(period),
    };
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval
}

/// Poll-cadence policy: fast until the agent's first completed enumeration,
/// steady afterwards; fast again on every disconnect; and a cap so the states
/// where fast polling buys nothing (an agent that never becomes ready, a
/// protocol mismatch) fall back to steady instead of running at 4 Hz forever.
///
/// Pure bookkeeping — the caller maps [`Cadence`] switches onto its timer —
/// so the transitions are unit-testable.
mod pacing {
    use std::time::{Duration, Instant};

    /// Which poll period the loop should run on.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Cadence {
        /// `STARTUP_POLL_PERIOD` — converging on a fresh agent.
        Fast,
        /// The configured steady `poll_period`.
        Steady,
    }

    pub struct Pacing {
        steady_period: Duration,
        fast_cap: Duration,
        mode: Cadence,
        /// When the current fast phase began (valid while `mode == Fast`).
        fast_since: Instant,
        /// The fast phase expired without readiness. Cleared by readiness, a
        /// disconnect, or the first delivery after an outage — each starts a
        /// genuinely new episode that deserves a fresh fast phase.
        capped: bool,
        /// Whether the previous tick delivered a snapshot, so the first
        /// delivery after an outage is recognizable.
        was_delivering: bool,
    }

    impl Pacing {
        pub fn new(steady_period: Duration, fast_cap: Duration, now: Instant) -> Self {
            Self {
                steady_period,
                fast_cap,
                mode: Cadence::Fast,
                fast_since: now,
                capped: false,
                was_delivering: false,
            }
        }

        pub fn steady_period(&self) -> Duration {
            self.steady_period
        }

        /// A snapshot was delivered. Ready → steady; not ready → fast until
        /// the cap, then steady.
        pub fn on_delivered(&mut self, ready: bool, now: Instant) -> Option<Cadence> {
            if !self.was_delivering {
                // First delivery after an outage: a just-(re)started agent
                // deserves a fresh fast phase regardless of how the outage
                // episode ended.
                self.capped = false;
                self.fast_since = now;
            }
            self.was_delivering = true;
            if ready {
                self.capped = false;
                return self.switch(Cadence::Steady, now);
            }
            if self.capped || self.expired(now) {
                self.capped = true;
                return self.switch(Cadence::Steady, now);
            }
            self.switch(Cadence::Fast, now)
        }

        /// No agent reachable this tick (and no live connection to lose).
        pub fn on_unreachable(&mut self, now: Instant) -> Option<Cadence> {
            self.was_delivering = false;
            if self.capped || self.expired(now) {
                self.capped = true;
                return self.switch(Cadence::Steady, now);
            }
            None
        }

        /// A live connection dropped — re-converge fast, fresh phase.
        pub fn on_disconnect(&mut self, now: Instant) -> Option<Cadence> {
            self.was_delivering = false;
            self.capped = false;
            self.switch(Cadence::Fast, now)
        }

        /// The agent speaks a newer protocol: only a GUI relaunch resolves
        /// it, so fast polling buys nothing.
        pub fn on_newer_agent(&mut self, now: Instant) -> Option<Cadence> {
            self.was_delivering = false;
            self.capped = true;
            self.switch(Cadence::Steady, now)
        }

        fn expired(&self, now: Instant) -> bool {
            self.mode == Cadence::Fast && now.duration_since(self.fast_since) >= self.fast_cap
        }

        fn switch(&mut self, to: Cadence, now: Instant) -> Option<Cadence> {
            if self.mode == to {
                return None;
            }
            if to == Cadence::Fast {
                self.fast_since = now;
            }
            self.mode = to;
            Some(to)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{Cadence, Pacing};
        use std::time::{Duration, Instant};

        const STEADY: Duration = Duration::from_secs(2);
        const CAP: Duration = Duration::from_secs(15);

        fn pacing(now: Instant) -> Pacing {
            Pacing::new(STEADY, CAP, now)
        }

        #[test]
        fn readiness_settles_to_steady_and_disconnect_rearms_fast() {
            let t0 = Instant::now();
            let mut p = pacing(t0);
            assert_eq!(p.on_delivered(false, t0), None); // already fast
            assert_eq!(p.on_delivered(true, t0), Some(Cadence::Steady));
            assert_eq!(p.on_delivered(true, t0 + STEADY), None);
            assert_eq!(p.on_disconnect(t0 + STEADY * 2), Some(Cadence::Fast));
        }

        #[test]
        fn never_ready_falls_back_to_steady_after_the_cap() {
            let t0 = Instant::now();
            let mut p = pacing(t0);
            // The first delivery opens the fast phase; the cap counts from it.
            assert_eq!(p.on_delivered(false, t0), None);
            assert_eq!(p.on_delivered(false, t0 + CAP / 2), None);
            assert_eq!(p.on_delivered(false, t0 + CAP), Some(Cadence::Steady));
            // Capped: further not-ready deliveries stay steady.
            assert_eq!(p.on_delivered(false, t0 + CAP + STEADY), None);
            // …but readiness still lands (and stays steady).
            assert_eq!(p.on_delivered(true, t0 + CAP + STEADY * 2), None);
        }

        #[test]
        fn unreachable_episode_caps_and_a_new_agent_gets_a_fresh_fast_phase() {
            let t0 = Instant::now();
            let mut p = pacing(t0);
            assert_eq!(p.on_unreachable(t0 + Duration::from_secs(1)), None);
            assert_eq!(p.on_unreachable(t0 + CAP), Some(Cadence::Steady));
            // An agent finally comes up, still scanning: fresh fast phase
            // despite the cap from the outage episode.
            assert_eq!(
                p.on_delivered(false, t0 + CAP + STEADY),
                Some(Cadence::Fast)
            );
            assert_eq!(
                p.on_delivered(true, t0 + CAP + STEADY * 2),
                Some(Cadence::Steady)
            );
        }

        #[test]
        fn newer_agent_goes_steady_immediately() {
            let t0 = Instant::now();
            let mut p = pacing(t0);
            assert_eq!(p.on_newer_agent(t0), Some(Cadence::Steady));
            assert_eq!(p.on_unreachable(t0 + STEADY), None); // stays steady
        }
    }
}

/// Long-poll the agent's pairing event stream on a dedicated connection, pushing
/// each [`PairingUpdate`] to the GUI. Runs for the client's lifetime; when no
/// session is active the agent returns `None` at its hold window and we re-poll.
async fn pairing_poll(tx: mpsc::UnboundedSender<PairingUpdate>) {
    let mut client: Option<AgentClient> = None;
    // Whether the last forwarded update was non-terminal, i.e. the Add Device
    // window believes a session is live. The agent guarantees a terminal event
    // for every session end — but that guarantee dies with the process (a
    // self-exec on update, a crash), and the replacement agent knows nothing
    // of the session. Synthesize the failure then, or the window would sit in
    // "Searching…" until the user cancels by hand.
    let mut session_active = false;
    loop {
        match poll_pairing_once(&mut client, &tx, &mut session_active).await {
            Ok(true) => {}       // delivered an event / hold elapsed; keep polling
            Ok(false) => return, // GUI dropped the pairing receiver → stop
            Err(()) => {
                client = None; // connection dropped (agent restart) — reconnect
                if session_active {
                    session_active = false;
                    let _ = tx.send(PairingUpdate::Failed(
                        tr!("The background service restarted — try pairing again.").to_string(),
                    ));
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/// One pairing long-poll. `Ok(false)` means the GUI receiver is gone; `Err` a
/// dropped connection the caller should reconnect.
async fn poll_pairing_once(
    client: &mut Option<AgentClient>,
    tx: &mpsc::UnboundedSender<PairingUpdate>,
    session_active: &mut bool,
) -> Result<bool, ()> {
    let Ok(client) = ensure(client).await else {
        tokio::time::sleep(Duration::from_secs(1)).await; // agent not up yet
        return Ok(true);
    };
    // The agent holds the poll ~20s; give the request a bit longer so the agent
    // answers (with an event or None) before the client deadline fires.
    let mut ctx = context::current();
    ctx.deadline = Instant::now() + Duration::from_secs(25);
    match client.next_pairing(ctx).await {
        Ok(Some(update)) => {
            *session_active = !matches!(
                update,
                PairingUpdate::Paired { .. } | PairingUpdate::Failed(_)
            );
            Ok(tx.send(update).is_ok())
        }
        Ok(None) => Ok(true),
        Err(_) => Err(()),
    }
}

/// Launch the agent once when the socket is unreachable. Detached so it
/// outlives the GUI (the agent is the always-on process); logs and moves on if
/// the binary can't be found / started — the user may start it via launchd or by
/// hand, and the poll loop keeps retrying the connection regardless.
fn spawn_agent() {
    let Some(path) = agent_binary_path() else {
        warn!(
            "agent not reachable and its binary wasn't found next to the GUI — \
             start it via launchd or by hand"
        );
        return;
    };
    // Spawn the agent under its *own* macOS TCC identity, not the GUI's:
    // otherwise it inherits the GUI's responsibility and the Accessibility /
    // Input-Monitoring grants the user gave the agent look missing (#192, #214).
    // The packaged helper goes through LaunchServices so it is its own TCC
    // responsible process; everything else is a `disclaim` exec (a no-op
    // pass-through to `std::process::Command` off macOS).
    match launch_agent(&path) {
        Ok(()) => info!(path = %path.display(), "agent not running — launched it"),
        Err(e) => warn!(error = %e, path = %path.display(), "could not launch the agent"),
    }
}

/// Launch the agent binary at `path` under its own TCC identity.
fn launch_agent(path: &std::path::Path) -> std::io::Result<()> {
    // The packaged helper goes through LaunchServices so the agent is its own
    // TCC responsible process; a direct exec attributes its Accessibility
    // check to the parent GUI and the grant flips with the launch path (#192).
    #[cfg(target_os = "macos")]
    if let Some(bundle) = helper_bundle(path) {
        return std::process::Command::new("/usr/bin/open")
            .arg("-g")
            .arg("-n")
            .arg(bundle)
            .spawn()
            .map(|_| ());
    }
    // Any other layout (bare dev binary, Windows, Linux): exec the binary
    // directly while disclaiming the GUI's TCC responsibility (#214).
    disclaim::Command::new(path).spawn().map(|_| ())
}

/// The `OpenLogiAgent.app` root of a packaged helper binary, `None` for a bare dev binary.
#[cfg(target_os = "macos")]
fn helper_bundle(path: &std::path::Path) -> Option<&std::path::Path> {
    let bundle = path.ancestors().nth(3)?;
    (bundle.extension()? == "app").then_some(bundle)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::path::Path;

    use super::helper_bundle;

    #[test]
    fn helper_bundle_resolves_only_the_packaged_layout() {
        let packaged = Path::new(
            "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogiAgent.app/Contents/MacOS/openlogi-agent",
        );
        assert_eq!(
            helper_bundle(packaged),
            Some(Path::new(
                "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogiAgent.app"
            ))
        );
        assert_eq!(
            helper_bundle(Path::new("target/debug/openlogi-agent")),
            None
        );
    }
}

/// Resolve the agent executable relative to the running GUI: a sibling in the
/// cargo target dir (dev, and the flat Windows install layout), else the
/// embedded `OpenLogiAgent.app` login-item helper (packaged macOS build).
fn agent_binary_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    // EXE_SUFFIX, or the Windows lookup misses `openlogi-agent.exe` and the
    // spawn retry — the only thing that restarts an updated agent there, since
    // Windows has no exec and the Run-key autostart only fires at login —
    // silently never works.
    let sibling = dir.join(format!("openlogi-agent{}", std::env::consts::EXE_SUFFIX));
    if sibling.exists() {
        return Some(sibling);
    }
    // Packaged: …/OpenLogi.app/Contents/MacOS/openlogi-gui → the helper at
    // …/OpenLogi.app/Contents/Library/LoginItems/OpenLogiAgent.app/Contents/MacOS/openlogi-agent
    #[cfg(target_os = "macos")]
    {
        let helper = dir
            .parent()?
            .join("Library/LoginItems/OpenLogiAgent.app/Contents/MacOS/openlogi-agent");
        helper.exists().then_some(helper)
    }
    #[cfg(not(target_os = "macos"))]
    None
}

/// Why [`ensure`] couldn't produce a usable client.
enum ConnectFailure {
    /// Socket down, handshake failed, or the agent is *older* than us — in
    /// every case the fix is an agent (re)start, which the spawn retry and
    /// the agent-side takeover drive; keep retrying.
    Unreachable,
    /// The agent is *newer* than us: this GUI process is the stale side and
    /// only a relaunch helps. Surfaced to the user as [`GuiUpdate::OutdatedGui`].
    NewerAgent,
}

/// Ensure a live client, connecting on demand.
async fn ensure(client: &mut Option<AgentClient>) -> Result<&AgentClient, ConnectFailure> {
    if client.is_none() {
        let stream = openlogi_agent_core::transport::connect()
            .await
            .map_err(|_| ConnectFailure::Unreachable)?;
        let transport = openlogi_agent_core::transport::wrap(stream);
        let fresh = AgentClient::new(client::Config::default(), transport).spawn();
        // Protocol handshake before any real RPC: mismatched bincode layouts
        // would otherwise surface only as opaque RpcErrors and a silently
        // empty device list. Refuse the connection with a clear log instead
        // and report the direction — who is stale decides who must restart.
        match fresh.protocol_version(context::current()).await {
            Ok(version) if version == PROTOCOL_VERSION => {
                *client = Some(fresh);
                debug!("connected to agent IPC socket");
            }
            Ok(version) if version < PROTOCOL_VERSION => {
                warn!(
                    agent = version,
                    gui = PROTOCOL_VERSION,
                    "agent IPC protocol is older — waiting for the agent to be replaced"
                );
                return Err(ConnectFailure::Unreachable);
            }
            Ok(version) => {
                warn!(
                    agent = version,
                    gui = PROTOCOL_VERSION,
                    "agent IPC protocol is newer — this GUI needs a relaunch"
                );
                return Err(ConnectFailure::NewerAgent);
            }
            Err(_) => {
                return Err(ConnectFailure::Unreachable);
            }
        }
    }
    // `client` is `Some` here (just set, or already was); the `None` arm is
    // unreachable but keeps this `expect`-free.
    client.as_ref().ok_or(ConnectFailure::Unreachable)
}

/// One poll round's outcome, driving the cadence policy.
enum PollOutcome {
    /// A snapshot was pushed; `ready` is whether enumeration has completed.
    Delivered { ready: bool },
    /// The agent isn't reachable (or usable) yet; nothing was pushed.
    NoAgent,
    /// The agent speaks a newer protocol than this GUI.
    NewerAgent,
}

/// Poll status + inventory and push a snapshot. `Err` means a live connection
/// dropped (the caller reconnects fast); the no-agent cases come back as
/// [`PollOutcome`] so the caller can tell them apart from a delivery.
async fn poll(
    client: &mut Option<AgentClient>,
    update_tx: &mpsc::UnboundedSender<GuiUpdate>,
) -> Result<PollOutcome, ()> {
    let client = match ensure(client).await {
        Ok(client) => client,
        Err(ConnectFailure::Unreachable) => return Ok(PollOutcome::NoAgent),
        Err(ConnectFailure::NewerAgent) => return Ok(PollOutcome::NewerAgent),
    };
    // Status strictly before inventory: status carries the readiness the
    // inventory is interpreted under. Fetched the other way around, the
    // agent's first enumeration could land *between* the two RPCs and pair an
    // empty pre-enumeration inventory with "ready" — exactly the "No devices"
    // flash this poll exists to prevent. The inverse pairing (fresh inventory
    // under a stale not-ready status) is benign: devices render regardless of
    // the scanning state, and readiness lands next tick.
    let status = client.status(context::current()).await.map_err(|_| ())?;
    let inventory = client.inventory(context::current()).await.map_err(|_| ())?;
    let ready = status.inventory == InventoryHealth::Ready;
    let _ = update_tx.send(GuiUpdate::Snapshot(PollUpdate { inventory, status }));
    Ok(PollOutcome::Delivered { ready })
}

/// Run one device command. `Err` signals a dropped connection so the caller
/// reconnects; the command's own failure is reported back over its oneshot.
async fn handle(client: &mut Option<AgentClient>, cmd: Command) -> Result<(), ()> {
    // keep `client` None on connect failure; that's not a dropped live connection
    let Ok(client) = ensure(client).await else {
        reply_disconnected(cmd);
        return Ok(());
    };
    let ctx = context::current();
    match cmd {
        Command::SetDpi(route, dpi) => log_apply(client.set_dpi(ctx, route, dpi).await)?,
        Command::SetLighting(route, lighting) => {
            log_apply(client.set_lighting(ctx, route, lighting).await)?;
        }
        Command::SetSmartShift(route, mode, auto, torque) => {
            log_apply(client.set_smartshift(ctx, route, mode, auto, torque).await)?;
        }
        Command::ReadDpi(route, reply) => {
            let _ = reply.send(rpc_result(client.read_dpi(ctx, route).await)?);
        }
        Command::ReadSmartShift(route, reply) => {
            let _ = reply.send(rpc_result(client.read_smartshift(ctx, route).await)?);
        }
        Command::ReloadConfig => client.reload_config(ctx).await.map_err(|_| ())?,
        Command::RequestAccessibilityPrompt => client
            .request_accessibility_prompt(ctx)
            .await
            .map_err(|_| ())?,
        Command::StartPairing(selector) => {
            client.start_pairing(ctx, selector).await.map_err(|_| ())?;
        }
        Command::PairDevice(address) => client.pair_device(ctx, address).await.map_err(|_| ())?,
        Command::CancelPairing => client.cancel_pairing(ctx).await.map_err(|_| ())?,
    }
    Ok(())
}

/// A fire-and-forget "apply now": `Err(())` (transport drop) propagates so the
/// caller reconnects; a device-side failure is logged, not surfaced.
fn log_apply(r: Result<Result<(), WriteError>, tarpc::client::RpcError>) -> Result<(), ()> {
    match r {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            warn!(error = %e, "agent rejected device command");
            Ok(())
        }
        Err(_) => Err(()),
    }
}

/// Unwrap a tarpc transport result: `Err(())` (connection dropped) propagates so
/// the caller reconnects; the inner application `Result` is returned for the reply.
fn rpc_result<T>(r: Result<T, tarpc::client::RpcError>) -> Result<T, ()> {
    r.map_err(|_| ())
}

/// Reply to a read command that the agent is unreachable; writes are
/// fire-and-forget so they have nothing to reply to.
#[allow(
    clippy::match_same_arms,
    reason = "the two read arms send the same disconnect error to differently-typed reply channels, so they can't be merged"
)]
fn reply_disconnected(cmd: Command) {
    // Transient (Hidpp), not a permanent feature error: the agent is just
    // restarting, so the panel should keep retrying, not latch "unsupported".
    let unreachable = || WriteError::Hidpp("background agent not running".to_string());
    match cmd {
        Command::ReadDpi(_, reply) => {
            let _ = reply.send(Err(unreachable()));
        }
        Command::ReadSmartShift(_, reply) => {
            let _ = reply.send(Err(unreachable()));
        }
        _ => {}
    }
}
