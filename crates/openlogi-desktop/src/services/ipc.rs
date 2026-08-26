//! Agent IPC client.
//!
//! The agent owns all device I/O, so the GUI never opens a device — it connects
//! to the agent's local socket and (a) keeps one [`Agent::observe`] request open
//! for the agent's state, and (b) forwards "apply now" / "read" device commands.
//! Both run on one dedicated OS thread with a tokio runtime (the GPUI thread owns
//! no async runtime): results cross back over `mpsc` to the GPUI loop.
//!
//! There is no poll cadence to tune. `observe` carries a generation, and the
//! agent answers the moment its state differs from the one this client last saw,
//! so the GUI is told *when* to look instead of asking on a timer — and because
//! every answer is the complete state, a reconnect needs no resynchronisation:
//! ask again with generation 0 and the next answer is the whole truth.
//!
//! What is left to time is failure. [`spawn_agent`] relaunches the binary when
//! the socket stays down (no launchd dependency: `KeepAlive` only acts when the
//! agent *exits*, and autostart may be off entirely), and a stretch without a
//! usable connection longer than [`UNREACHABLE_AFTER`] is pushed to the GUI as
//! [`GuiUpdate::Unreachable`] so the window can say so instead of waiting
//! forever. A dead agent is noticed the moment the socket closes; a *hung* one
//! is noticed when its hold window passes without an answer.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::{Duration, Instant};

use openlogi_core::config::Lighting;
use openlogi_core::hid::{
    DeviceRoute, Dpi, DpiInfo, LightCommand, ReceiverSelector, SmartShiftStatus, WriteError,
};
use openlogi_ipc::{
    AgentClient, AgentSnapshot, ConfigReloadError, Generation, OBSERVE_HOLD, Observation,
    PROTOCOL_VERSION, PairingCommandError, PairingFailure,
};
use tarpc::context;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

/// Minimum gap between agent-launch attempts while the socket is unreachable.
/// Long enough that a missing or crash-looping binary can't be respawned in a
/// tight loop, short enough that a quit / crashed agent is recovered promptly.
const SPAWN_RETRY_PERIOD: Duration = Duration::from_secs(30);

/// How long to wait before retrying a connect that failed. This is a retry
/// cadence, not a poll: once connected, nothing here runs on a timer. Short
/// enough that a just-started agent is picked up immediately.
const RECONNECT_DELAY: Duration = Duration::from_millis(250);

/// How long the client may go without a usable connection before the GUI is
/// told the agent is genuinely unreachable rather than still starting (agent
/// start plus a worst-case first enumeration is ~6 s).
const UNREACHABLE_AFTER: Duration = Duration::from_secs(15);

/// Request deadline for a held `observe`, above the agent's own
/// [`OBSERVE_HOLD`]: tarpc cancels a handler whose deadline passes, so a
/// shorter one would kill the hold instead of waiting it out.
const OBSERVE_DEADLINE: Duration = OBSERVE_HOLD.saturating_add(Duration::from_secs(5));

/// What the client thread tells the GPUI loop.
pub enum GuiUpdate {
    /// The agent's state, as of a generation this client had not seen.
    Snapshot(AgentSnapshot),
    /// No usable connection for [`UNREACHABLE_AFTER`]: the agent is genuinely
    /// unreachable (not just starting up). Sent once per outage; the next
    /// snapshot supersedes it.
    Unreachable,
    /// The agent answered the handshake with a *newer* protocol — the app was
    /// updated on disk while this GUI kept running, and only a relaunch
    /// helps. Sent once per episode.
    OutdatedGui,
    /// Result of an agent-owned standalone-light command. The typed failure
    /// reaches the GPUI state model instead of being reduced to a log line.
    LightCommandResult {
        /// Runtime/config key of the light that issued the command.
        key: String,
        /// Monotonic request id used to ignore stale results.
        request_id: u64,
        /// The control whose write produced this result.
        command: LightCommand,
        /// Agent acceptance or typed device failure.
        result: Result<(), WriteError>,
    },
    /// Whether the agent adopted the config currently on disk.
    ConfigReloadResult(Result<(), ConfigReloadError>),
    /// A pairing command could not be delivered, so no session will ever appear
    /// in the observed state to explain the silence. Reported locally rather
    /// than faked as a session the agent never had.
    PairingUndeliverable(PairingFailure),
}

/// A device command sent from the GPUI thread to the client thread. Reads carry
/// a `oneshot` for the reply; standalone-light writes return a result event so
/// the GUI can surface device failures after an optimistic update.
pub enum Command {
    SetDpi(DeviceRoute, Dpi),
    SetLighting(DeviceRoute, Lighting),
    SetLight(DeviceRoute, LightCommand, String, u64),
    SetLightManualPower(DeviceRoute, bool, String, u64),
    SetSmartShift(DeviceRoute, SmartShiftStatus),
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
    /// Drain the agent's live event-monitor buffer for the debug Diagnostics
    /// monitor. The first poll enables monitoring agent-side; the agent
    /// auto-disables it once polls stop.
    #[cfg(all(target_os = "macos", debug_assertions))]
    PollEventMonitor(oneshot::Sender<Vec<openlogi_ipc::MonitorEvent>>),
}

/// Handle the GUI holds to talk to the agent: a stream of state updates and a
/// sender for device commands. Pairing progress arrives through the same state
/// updates as everything else.
pub struct IpcClient {
    pub updates: mpsc::UnboundedReceiver<GuiUpdate>,
    pub commands: mpsc::UnboundedSender<Command>,
}

/// Spawn the IPC client thread. Returns immediately; the thread connects (and
/// reconnects) on its own.
#[must_use]
pub fn spawn() -> IpcClient {
    let (update_tx, updates) = mpsc::unbounded_channel();
    let (commands, mut cmd_rx) = mpsc::unbounded_channel::<Command>();

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
                observe_loop(&update_tx, &mut cmd_rx).await;
            });
        });
    if let Err(e) = spawn_result {
        warn!(error = %e, "could not spawn IPC client thread — agent state unavailable");
    }

    IpcClient { updates, commands }
}

/// The state/command loop.
///
/// One `observe` request is kept in flight at all times, carrying the last
/// generation this client saw; the agent answers when its state differs from
/// that, or after its hold window with the same state as a heartbeat. Commands
/// share the connection — tarpc multiplexes requests, and the in-flight poll is
/// held across command handling so a device write never cancels it.
async fn observe_loop(
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
    let mut connected_since: Option<Instant> = None;
    let mut notified_unreachable = false;
    let mut notified_outdated = false;
    // Generation 0 means "I have seen nothing", so the first answer is the
    // agent's whole state rather than a wait for the next change. Reset on
    // every disconnect: the replacement agent numbers its own generations.
    let mut seen: Generation = 0;
    let mut inflight: Option<ObserveFuture> = None;
    let mut retry = ticker(RECONNECT_DELAY);
    loop {
        // Taken for the duration of the select so the completed arm can consume
        // it while the others hand it back untouched.
        let mut pending = inflight.take();
        let woken = tokio::select! {
            observed = maybe(pending.as_mut()) => Woken::Observed(observed),
            cmd = cmd_rx.recv() => Woken::Command(cmd),
            _ = retry.tick(), if pending.is_none() => Woken::Reconnect,
        };
        match woken {
            // The poll answered: apply it and arm the next one. `pending` is
            // finished, so it is deliberately not handed back.
            Woken::Observed(Ok(observation)) => {
                connected_since = Some(Instant::now());
                notified_unreachable = false;
                notified_outdated = false;
                if observation.generation != seen {
                    seen = observation.generation;
                    let _ = update_tx.send(GuiUpdate::Snapshot(observation.snapshot));
                }
                if let Some(client) = client.as_ref() {
                    inflight = Some(observe(client, seen));
                }
            }
            // The connection dropped (agent self-exec on update, or a crash).
            // Reconnecting re-reads the whole state, so nothing is lost.
            Woken::Observed(Err(())) => {
                client = None;
                seen = 0;
                connected_since = None;
            }
            Woken::Command(None) => break, // GUI dropped the sender → shut down
            Woken::Command(Some(cmd)) => {
                inflight = pending;
                if handle(&mut client, update_tx, cmd).await.is_err() {
                    client = None;
                    seen = 0;
                    connected_since = None;
                }
            }
            Woken::Reconnect => match ensure(&mut client).await {
                Ok(client) => inflight = Some(observe(client, seen)),
                Err(ConnectFailure::Unreachable) => {}
                Err(ConnectFailure::NewerAgent) => {
                    if !notified_outdated {
                        notified_outdated = true;
                        let _ = update_tx.send(GuiUpdate::OutdatedGui);
                    }
                }
            },
        }
        if client.is_none() {
            let down_since = connected_since.unwrap_or(started);
            if !notified_unreachable && down_since.elapsed() >= UNREACHABLE_AFTER {
                notified_unreachable = true;
                let _ = update_tx.send(GuiUpdate::Unreachable);
            }
            if last_spawn_attempt.is_none_or(|t| t.elapsed() >= SPAWN_RETRY_PERIOD) {
                spawn_agent();
                last_spawn_attempt = Some(Instant::now());
            }
        }
    }
}

/// Why [`observe_loop`] woke up. Named so the in-flight poll can be handed back
/// after the select ends rather than mutated from inside a borrowed arm.
enum Woken {
    /// The long-poll answered, or its connection dropped.
    Observed(Result<Observation, ()>),
    /// A device command, or `None` once the GUI drops the sender.
    Command(Option<Command>),
    /// Time to try connecting again.
    Reconnect,
}

/// A long-poll in flight. Boxed because it is stored across loop turns, and it
/// owns a clone of the client so the loop can still replace its own `client`
/// while the poll is outstanding.
type ObserveFuture = Pin<Box<dyn Future<Output = Result<Observation, ()>> + Send>>;

/// Ask for the next state newer than `seen`.
fn observe(client: &AgentClient, seen: Generation) -> ObserveFuture {
    let client = client.clone();
    Box::pin(async move {
        let mut ctx = context::current();
        ctx.deadline = Instant::now() + OBSERVE_DEADLINE;
        client.observe(ctx, seen).await.map_err(|error| {
            debug!(%error, "observe failed — reconnecting");
        })
    })
}

/// Await a future that may not exist, never resolving when there is none. The
/// caller pairs it with a precondition, so "none" is a disabled select arm
/// rather than a stall.
async fn maybe<F: Future>(future: Option<F>) -> F::Output {
    match future {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
}

/// A tokio interval that *delays* missed ticks instead of bursting them: while
/// a connection is live this arm is disabled for hours, and a fresh burst of
/// backdated ticks on reconnect would buy nothing.
fn ticker(period: Duration) -> tokio::time::Interval {
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval
}

/// Set when the agent announced a deliberate suite shutdown (the tray's Quit
/// arrives as the `openlogi://quit` deep link before the agent exits). The
/// spawn fallback below must not resurrect a process the user just quit: the
/// agent's socket can close while this GUI is still tearing down, and the
/// unreachable→spawn reflex has been observed winning that race and bringing
/// the agent back seconds after Quit. Never cleared — this process is
/// quitting too. Automatic recovery is for faults, not for user intent.
static SUITE_QUITTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record that the user quit the whole suite from the agent's tray, so the
/// IPC client stops respawning the agent during this GUI's teardown.
pub fn mark_suite_quitting() {
    SUITE_QUITTING.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Launch the agent once when the socket is unreachable. Detached so it
/// outlives the GUI (the agent is the always-on process); logs and moves on if
/// the binary can't be found / started — the user may start it via launchd or by
/// hand, and the poll loop keeps retrying the connection regardless.
fn spawn_agent() {
    if SUITE_QUITTING.load(std::sync::atomic::Ordering::Relaxed) {
        info!("suite is quitting — leaving the agent down");
        return;
    }
    // A registered login-item service is launchd's to start: kickstart is
    // idempotent (a no-op on a running service), the process comes up
    // *supervised* (crash respawn per the service plist), and launchd makes it
    // its own TCC responsible process. Every failure falls through to the
    // legacy direct-launch paths below, so a broken registration degrades to
    // today's behavior instead of wedging the retry loop.
    #[cfg(target_os = "macos")]
    if kickstart_registered_agent() {
        return;
    }
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
    // "started", not "launched": on the packaged path success here only means
    // `open` was handed the bundle — the waiter inside `launch_agent` reports
    // the definitive outcome, so a LaunchServices rejection is not preceded by
    // a success claim it then contradicts.
    match launch_agent(&path) {
        Ok(()) => info!(path = %path.display(), "agent not running — launch started"),
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
        let mut child = std::process::Command::new("/usr/bin/open")
            .arg("-g")
            .arg("-n")
            .arg(bundle)
            .spawn()?;
        // `open` exits as soon as it hands the bundle to LaunchServices, and
        // its exit status is the only signal that the handoff failed (damaged
        // bundle, LaunchServices refusal) — a successful spawn alone proves
        // nothing. Reap it off-thread and log the failure the spawn hides.
        std::thread::spawn(move || match child.wait() {
            Ok(status) if !status.success() => {
                warn!(%status, "`open` could not launch the agent bundle");
            }
            Err(e) => warn!(error = %e, "could not reap the `open` helper"),
            Ok(_) => {}
        });
        return Ok(());
    }
    // Any other layout (bare dev binary, Windows, Linux): exec the binary
    // directly while disclaiming the GUI's TCC responsibility (#214).
    disclaim::Command::new(path).spawn().map(|_| ())
}

/// `launchctl kickstart` the agent's registered login-item service. Returns
/// whether the start was handed to launchd — `false` (not registered, user
/// switched it off in Login Items, or launchctl itself failed) sends the
/// caller down the direct-launch paths.
#[cfg(target_os = "macos")]
fn kickstart_registered_agent() -> bool {
    use crate::platform::login_item;

    if login_item::status() != login_item::ServiceStatus::Enabled {
        return false;
    }
    let Some(uid) = current_uid() else {
        return false;
    };
    let target = format!("gui/{uid}/{}", login_item::agent_service_label());
    match std::process::Command::new("/bin/launchctl")
        .arg("kickstart")
        .arg(&target)
        .output()
    {
        Ok(out) if out.status.success() => {
            info!(%target, "agent not running — kickstarted the registered service");
            true
        }
        Ok(out) => {
            warn!(
                %target,
                status = %out.status,
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "launchctl kickstart failed — falling back to a direct launch"
            );
            false
        }
        Err(e) => {
            warn!(error = %e, "could not run launchctl — falling back to a direct launch");
            false
        }
    }
}

/// The current user's uid, read from the home directory's owner: `launchctl`
/// addresses the per-user launchd domain as `gui/<uid>`, and std exposes no
/// direct getuid.
#[cfg(target_os = "macos")]
fn current_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt as _;

    let home = openlogi_core::paths::home_dir().ok()?;
    std::fs::metadata(home).ok().map(|meta| meta.uid())
}

/// The `.app` root of a packaged helper binary, `None` for a bare dev binary.
#[cfg(target_os = "macos")]
fn helper_bundle(path: &std::path::Path) -> Option<&std::path::Path> {
    let bundle = path.ancestors().nth(3)?;
    (bundle.extension()? == "app").then_some(bundle)
}

/// Resolve the agent executable relative to the running GUI: a sibling in the
/// cargo target dir (dev, and the flat Windows install layout), else the
/// embedded `OpenLogi Agent.app` login-item helper (packaged macOS build).
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
    // Packaged: …/OpenLogi.app/Contents/MacOS/openlogi-desktop → the helper at
    // …/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/MacOS/openlogi-agent
    // Every family names its directory after the display name, so the privacy
    // panes' filename fallback (used when bundle metadata is stale) shows the
    // real name. The last entry keeps finding helpers in bundles built before
    // the rename.
    #[cfg(target_os = "macos")]
    {
        let contents = dir.parent()?;
        for relative in [
            "Library/LoginItems/OpenLogi Agent Dev.app/Contents/MacOS/openlogi-agent",
            "Library/LoginItems/OpenLogi Agent.app/Contents/MacOS/openlogi-agent",
            "Library/LoginItems/OpenLogiAgent.app/Contents/MacOS/openlogi-agent",
        ] {
            let helper = contents.join(relative);
            if helper.exists() {
                return Some(helper);
            }
        }
        None
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
        // The handshake happens before any real RPC: mismatched bincode layouts
        // would otherwise surface only as opaque RpcErrors and a silently empty
        // device list. Refuse with a clear log instead, and report the
        // direction — who is stale decides who must restart.
        let connection = openlogi_ipc::client::connect().await.map_err(|error| {
            debug!(%error, "no usable agent");
            ConnectFailure::Unreachable
        })?;
        match connection.version {
            version if version == PROTOCOL_VERSION => {
                *client = Some(connection.client);
                debug!("connected to agent IPC socket");
            }
            version if version < PROTOCOL_VERSION => {
                warn!(
                    agent = version,
                    gui = PROTOCOL_VERSION,
                    "agent IPC protocol is older — waiting for the agent to be replaced"
                );
                return Err(ConnectFailure::Unreachable);
            }
            version => {
                warn!(
                    agent = version,
                    gui = PROTOCOL_VERSION,
                    "agent IPC protocol is newer — this GUI needs a relaunch"
                );
                return Err(ConnectFailure::NewerAgent);
            }
        }
    }
    // `client` is `Some` here (just set, or already was); the `None` arm is
    // unreachable but keeps this `expect`-free.
    client.as_ref().ok_or(ConnectFailure::Unreachable)
}

/// Run one device command. `Err` signals a dropped connection so the caller
/// reconnects; the command's own failure is reported back over its oneshot.
async fn handle(
    client: &mut Option<AgentClient>,
    update_tx: &mpsc::UnboundedSender<GuiUpdate>,
    cmd: Command,
) -> Result<(), ()> {
    // keep `client` None on connect failure; that's not a dropped live connection
    let Ok(client) = ensure(client).await else {
        reply_disconnected(update_tx, cmd);
        return Ok(());
    };
    let ctx = context::current();
    match cmd {
        Command::SetDpi(route, dpi) => log_apply(client.set_dpi(ctx, route, dpi).await)?,
        Command::SetLighting(route, lighting) => {
            log_apply(client.set_lighting(ctx, route, lighting).await)?;
        }
        Command::SetLight(route, command, key, request_id) => {
            send_light_result(
                update_tx,
                key,
                request_id,
                command,
                client.set_light(ctx, route, command).await,
            )?;
        }
        Command::SetLightManualPower(route, enabled, key, request_id) => {
            send_light_result(
                update_tx,
                key,
                request_id,
                LightCommand::Power(enabled),
                client.set_light_manual_power(ctx, route, enabled).await,
            )?;
        }
        Command::SetSmartShift(route, status) => {
            log_apply(client.set_smartshift(ctx, route, status).await)?;
        }
        Command::ReadDpi(route, reply) => {
            let _ = reply.send(rpc_result(client.read_dpi(ctx, route).await)?);
        }
        Command::ReadSmartShift(route, reply) => {
            let _ = reply.send(rpc_result(client.read_smartshift(ctx, route).await)?);
        }
        Command::ReloadConfig => {
            // A transport failure is not the agent rejecting the config, but it
            // is still a reload that did not happen — and the file on disk has
            // already changed. Staying silent here would leave the window
            // showing the new settings while the agent keeps running the old
            // ones, which is exactly the divergence this fails closed on.
            match client.reload_config(ctx).await {
                Ok(result) => {
                    let _ = update_tx.send(GuiUpdate::ConfigReloadResult(result));
                }
                Err(error) => {
                    let _ = update_tx.send(GuiUpdate::ConfigReloadResult(Err(ConfigReloadError {
                        message: format!(
                            "saved, but the agent could not be reached to apply it: {error}"
                        ),
                    })));
                    return Err(());
                }
            }
        }
        Command::RequestAccessibilityPrompt => client
            .request_accessibility_prompt(ctx)
            .await
            .map_err(|_| ())?,
        Command::StartPairing(selector) => {
            pairing_command_result(update_tx, client.start_pairing(ctx, selector).await)?;
        }
        Command::PairDevice(address) => {
            pairing_command_result(update_tx, client.pair_device(ctx, address).await)?;
        }
        Command::CancelPairing => {
            pairing_command_result(update_tx, client.cancel_pairing(ctx).await)?;
        }
        #[cfg(all(target_os = "macos", debug_assertions))]
        Command::PollEventMonitor(reply) => {
            let _ = reply.send(rpc_result(client.poll_event_monitor(ctx).await)?);
        }
    }
    Ok(())
}

/// An accepted pairing command needs no reply — its progress shows up in the
/// observed state. A *rejected* one never becomes a session, so the refusal is
/// reported here or the window would wait for something that will never come.
fn pairing_command_result(
    update_tx: &mpsc::UnboundedSender<GuiUpdate>,
    result: Result<Result<(), PairingCommandError>, tarpc::client::RpcError>,
) -> Result<(), ()> {
    match result.map_err(|_| ())? {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = update_tx.send(GuiUpdate::PairingUndeliverable(PairingFailure::from(error)));
            Ok(())
        }
    }
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

fn send_light_result(
    update_tx: &mpsc::UnboundedSender<GuiUpdate>,
    key: String,
    request_id: u64,
    command: LightCommand,
    result: Result<Result<(), WriteError>, tarpc::client::RpcError>,
) -> Result<(), ()> {
    if let Ok(result) = result {
        let _ = update_tx.send(GuiUpdate::LightCommandResult {
            key,
            request_id,
            command,
            result,
        });
        Ok(())
    } else {
        let _ = update_tx.send(GuiUpdate::LightCommandResult {
            key,
            request_id,
            command,
            result: Err(WriteError::AgentUnavailable),
        });
        Err(())
    }
}

/// Unwrap a tarpc transport result: `Err(())` (connection dropped) propagates so
/// the caller reconnects; the inner application `Result` is returned for the reply.
fn rpc_result<T>(r: Result<T, tarpc::client::RpcError>) -> Result<T, ()> {
    r.map_err(|_| ())
}

/// Reply to a read command that the agent is unreachable; writes are
/// fire-and-forget so they have nothing to reply to.
#[expect(
    clippy::match_same_arms,
    reason = "the two read arms send the same disconnect error to differently-typed reply channels, so they can't be merged"
)]
fn reply_disconnected(update_tx: &mpsc::UnboundedSender<GuiUpdate>, cmd: Command) {
    // Transient, not a permanent feature error: the agent is just restarting,
    // so the panel should keep retrying, not latch "unsupported".
    match cmd {
        Command::ReadDpi(_, reply) => {
            let _ = reply.send(Err(WriteError::AgentUnavailable));
        }
        Command::ReadSmartShift(_, reply) => {
            let _ = reply.send(Err(WriteError::AgentUnavailable));
        }
        Command::SetLight(_, command, key, request_id) => {
            let _ = update_tx.send(GuiUpdate::LightCommandResult {
                key,
                request_id,
                command,
                result: Err(WriteError::AgentUnavailable),
            });
        }
        Command::SetLightManualPower(_, enabled, key, request_id) => {
            let _ = update_tx.send(GuiUpdate::LightCommandResult {
                key,
                request_id,
                command: LightCommand::Power(enabled),
                result: Err(WriteError::AgentUnavailable),
            });
        }
        Command::StartPairing(_) | Command::PairDevice(_) => {
            let _ = update_tx.send(GuiUpdate::PairingUndeliverable(
                PairingFailure::AgentRestarted,
            ));
        }
        Command::CancelPairing => {}
        // Unlike the device commands above, a missed reload is not something a
        // later poll repairs on its own: the config file has already changed,
        // so the agent stays on the old one until another reload succeeds. Say
        // so rather than let the window imply the change took effect.
        Command::ReloadConfig => {
            let _ = update_tx.send(GuiUpdate::ConfigReloadResult(Err(ConfigReloadError {
                message: "saved, but the agent is not running, so it has not been applied yet"
                    .to_string(),
            })));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use std::path::Path;

    use super::*;

    #[test]
    fn a_reload_that_never_reached_the_agent_is_reported() {
        // The config file is already written by the time the reload is
        // dispatched, so dropping this result silently would leave the window
        // showing settings the agent is not running.
        let (update_tx, mut update_rx) = mpsc::unbounded_channel();

        reply_disconnected(&update_tx, Command::ReloadConfig);

        let Ok(GuiUpdate::ConfigReloadResult(Err(error))) = update_rx.try_recv() else {
            panic!("a reload that never reached the agent must be reported as failed");
        };
        assert!(!error.message.is_empty(), "the notice needs a reason");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn helper_bundle_resolves_only_the_packaged_layout() {
        let packaged = Path::new(
            "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/MacOS/openlogi-agent",
        );
        assert_eq!(
            helper_bundle(packaged),
            Some(Path::new(
                "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app"
            ))
        );
        let dev = Path::new(
            "/Users/me/OpenLogi/target/dev/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent Dev.app/Contents/MacOS/openlogi-agent",
        );
        assert_eq!(
            helper_bundle(dev),
            Some(Path::new(
                "/Users/me/OpenLogi/target/dev/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent Dev.app"
            ))
        );
        assert_eq!(
            helper_bundle(Path::new("target/debug/openlogi-agent")),
            None
        );
    }
}
