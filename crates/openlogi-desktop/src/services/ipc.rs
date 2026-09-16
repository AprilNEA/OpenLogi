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
//! The connection is one [`link::Link`]: while up it owns the client, the
//! generation ledger and the poll in flight; while down it owns the outage
//! clock and what the window has been told about it. What is left to time is
//! failure. `launch::spawn_agent` brings the agent up when the socket stays
//! down — gated by [`reflex::SpawnReflex`], which fires immediately for an
//! agent that was never reachable but gives a lost connection
//! [`reflex::SPAWN_AFTER_LOSS`] first (the deliberate quits and the supervised
//! restarts announce themselves within that window) and never fires at a live
//! agent newer than this GUI. An outage longer than `link::UNREACHABLE_AFTER`
//! is pushed to the GUI as [`GuiUpdate::Unreachable`] so the window can say so
//! instead of waiting forever. A dead agent is noticed the moment the socket
//! closes; a *hung* one is noticed when its hold window passes without an
//! answer.
//!
//! Device commands are transient: one that finds no connection is answered as
//! unavailable by the request itself ([`request::Request::deliver`]) and the
//! next snapshot repairs the panel. A config reload is not — `config.toml` has already changed on disk and the
//! agent must re-read it — but neither is it urgent while no agent is running:
//! an agent that starts reads the file anyway. So `ReloadConfig` is held as
//! state rather than dispatched: the loop delivers it over the next live
//! connection and reports the agent's verdict then. That is what keeps an app
//! relaunch that outruns its agent (every self-update does) from latching a
//! "not applied" notice the agent's arrival could never clear.

use std::time::{Duration, Instant};

use openlogi_core::hid::{LightCommand, WriteError};
use openlogi_ipc::client::{self, ConnectError};
use openlogi_ipc::{
    AgentClient, AgentSnapshot, ClientKind, ConfigReloadError, Observation, PairingFailure,
};
use tarpc::client::RpcError;
use tokio::sync::mpsc;
use tracing::{debug, warn};

mod launch;
mod link;
mod reflex;
mod request;

pub use launch::mark_suite_quitting;
use launch::spawn_agent;
use link::Link;
use reflex::SpawnReflex;
use request::LinkLost;
#[cfg(all(target_os = "macos", debug_assertions))]
pub use request::PollEventMonitor;
pub use request::{
    CancelPairing, Command, PairDevice, ReadDpi, ReadSmartShift, ReloadConfig,
    RequestAccessibilityPrompt, SetDpi, SetLight, SetLightManualPower, SetLighting, SetSmartShift,
    StartPairing,
};

/// How long to wait before retrying a connect that failed. This is a retry
/// cadence, not a poll: once connected, nothing here runs on a timer. Short
/// enough that a just-started agent is picked up immediately.
const RECONNECT_DELAY: Duration = Duration::from_millis(250);

/// What the client thread tells the GPUI loop.
pub enum GuiUpdate {
    /// The agent's state, as of a generation this client had not seen.
    Snapshot(AgentSnapshot),
    /// No usable connection for `link::UNREACHABLE_AFTER`: the agent is
    /// genuinely unreachable (not just starting up). Never sent while a newer
    /// agent is the reason; a snapshot or [`Self::OutdatedGui`] supersedes it.
    Unreachable,
    /// The agent answered the handshake with a *newer* protocol — the app was
    /// updated on disk while this GUI kept running, and only a relaunch
    /// helps. Sent when an attempt finds one and not repeated until
    /// [`Self::Unreachable`] has superseded it.
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

    let started = openlogi_core::runtime::spawn_thread("openlogi-ipc-client", move |runtime| {
        runtime.block_on(observe_loop(&mut System, &update_tx, &mut cmd_rx));
    });
    if let Err(error) = started {
        warn!(%error, "could not start the IPC client thread — agent state unavailable");
    }

    IpcClient { updates, commands }
}

/// Where the agent is reached and how it is brought up — the loop's only two
/// effects on the world, behind one seam so the tests can script them.
trait Effects {
    /// Connect to the agent and complete the handshake as the GUI.
    async fn connect(&mut self) -> Result<AgentClient, ConnectError>;
    /// Start the agent when the socket stays down; see `launch::spawn_agent`.
    fn spawn_agent(&mut self);
}

/// The real thing: the agent's local socket and its supervised launch paths.
struct System;

impl Effects for System {
    async fn connect(&mut self) -> Result<AgentClient, ConnectError> {
        client::connect_as(ClientKind::Gui).await
    }

    fn spawn_agent(&mut self) {
        spawn_agent();
    }
}

/// The state/command loop.
///
/// One `observe` request is kept in flight whenever there is a connection,
/// carrying the last generation this client saw; the agent answers when its
/// state differs from that, or after its hold window with the same state as a
/// heartbeat. Commands share the connection — tarpc multiplexes requests, and
/// the poll stays in flight across command handling, so a device write never
/// cancels it.
async fn observe_loop(
    effects: &mut impl Effects,
    update_tx: &mpsc::UnboundedSender<GuiUpdate>,
    cmd_rx: &mut mpsc::UnboundedReceiver<Command>,
) {
    let mut link = Link::cold(Instant::now());
    let mut reflex = SpawnReflex::new();
    // A `ReloadConfig` the agent has not answered yet — requested with no
    // connection, or lost with one. Idempotent (the agent re-reads the file),
    // so it is simply delivered again over the next live connection.
    let mut reload_owed = false;
    let mut retry = ticker(RECONNECT_DELAY);
    loop {
        let woken = tokio::select! {
            observed = link.observed() => Woken::Observed(observed),
            cmd = cmd_rx.recv() => Woken::Command(cmd),
            _ = retry.tick(), if link.is_down() => Woken::Reconnect,
        };
        match woken {
            Woken::Observed(Ok(observation)) => {
                if let Some(snapshot) = link.answered(observation) {
                    let _ = update_tx.send(GuiUpdate::Snapshot(snapshot));
                }
            }
            // The connection dropped (agent self-exec on update, or a crash).
            // Reconnecting re-reads the whole state, so nothing is lost.
            Woken::Observed(Err(error)) => {
                debug!(%error, "observe failed — reconnecting");
                link.lose(Instant::now());
            }
            Woken::Command(None) => break, // GUI dropped the sender → shut down
            // Not dispatched like the device commands below: held, and
            // delivered at the end of this turn if a connection exists.
            Woken::Command(Some(Command::ReloadConfig(_))) => reload_owed = true,
            Woken::Command(Some(cmd)) => {
                let client = link.ensure(effects, update_tx).await;
                if cmd.run(client, update_tx).await.is_err() {
                    link.lose(Instant::now());
                }
            }
            Woken::Reconnect => {
                link.ensure(effects, update_tx).await;
            }
        }
        // Whatever this turn did to the link, a held reload goes out the
        // moment there is one to carry it. A transport failure here is the
        // same as anywhere: drop the link, keep the reload for the next one.
        if reload_owed && let Some(client) = link.client() {
            match request::run(ReloadConfig, Some(client), update_tx).await {
                Ok(()) => reload_owed = false,
                Err(LinkLost) => link.lose(Instant::now()),
            }
        }
        let now = Instant::now();
        if let Some(notice) = link
            .down_mut()
            .and_then(|down| down.unreachable_notice(now))
        {
            let _ = update_tx.send(notice);
        }
        if reflex.should_fire(&link, now) {
            effects.spawn_agent();
            reflex.fired(now);
        }
    }
}

/// Why [`observe_loop`] woke up.
enum Woken {
    /// The long-poll answered, or its connection dropped.
    Observed(Result<Observation, RpcError>),
    /// A device command, or `None` once the GUI drops the sender.
    Command(Option<Command>),
    /// Time to try connecting again.
    Reconnect,
}

/// A tokio interval that *delays* missed ticks instead of bursting them: while
/// a connection is live this arm is disabled for hours, and a fresh burst of
/// backdated ticks on reconnect would buy nothing.
fn ticker(period: Duration) -> tokio::time::Interval {
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use openlogi_core::hid::DeviceRoute;
    use openlogi_ipc::testing::in_memory_agent;
    use openlogi_ipc::{AgentRequest, AgentResponse};
    use tokio::sync::oneshot;

    use super::*;

    /// How a scripted agent answers a reload.
    #[derive(Clone, Copy)]
    enum OnReload {
        /// Adopt the config.
        Accept,
        /// Close the connection without answering, as a dying agent does.
        Vanish,
    }

    /// An in-memory agent past the handshake: reloads as told, refuses every
    /// DPI read with a device error, holds `observe` open forever (a quiet
    /// agent), and counts the reloads it saw. Anything else is out of these
    /// tests' scope.
    fn scripted_agent(on_reload: OnReload) -> (AgentClient, Arc<AtomicUsize>) {
        let reloads = Arc::new(AtomicUsize::new(0));
        let vanish = Arc::new(tokio::sync::Notify::new());
        let counted = reloads.clone();
        let signal = vanish.clone();
        let client = in_memory_agent(
            move |request| {
                let counted = counted.clone();
                let signal = signal.clone();
                Box::pin(async move {
                    match request {
                        AgentRequest::ReloadConfig {} => {
                            counted.fetch_add(1, Ordering::SeqCst);
                            match on_reload {
                                OnReload::Accept => Ok(AgentResponse::ReloadConfig(Ok(()))),
                                OnReload::Vanish => {
                                    signal.notify_one();
                                    std::future::pending().await
                                }
                            }
                        }
                        AgentRequest::ReadDpi { .. } => {
                            Ok(AgentResponse::ReadDpi(Err(WriteError::AmbiguousRawDevice)))
                        }
                        AgentRequest::Observe { .. } => std::future::pending().await,
                        other => panic!("the client loop sent an unexpected request: {other:?}"),
                    }
                })
            },
            async move { vanish.notified().await },
        );
        (client, reloads)
    }

    /// Scripted effects: connect attempts pop the script front to back and
    /// find the socket down once it runs out; launches are only counted.
    struct ScriptedEffects {
        attempts: VecDeque<Result<AgentClient, ConnectError>>,
        launches: usize,
    }

    impl ScriptedEffects {
        fn answering(
            attempts: impl IntoIterator<Item = Result<AgentClient, ConnectError>>,
        ) -> Self {
            Self {
                attempts: attempts.into_iter().collect(),
                launches: 0,
            }
        }
    }

    impl Effects for ScriptedEffects {
        #[expect(
            clippy::unused_async_trait_impl,
            reason = "the trait is async for the real socket; the script answers from memory"
        )]
        async fn connect(&mut self) -> Result<AgentClient, ConnectError> {
            self.attempts.pop_front().unwrap_or_else(down)
        }

        fn spawn_agent(&mut self) {
            self.launches += 1;
        }
    }

    fn down() -> Result<AgentClient, ConnectError> {
        Err(std::io::Error::from(std::io::ErrorKind::ConnectionRefused).into())
    }

    fn some_route() -> DeviceRoute {
        DeviceRoute::Bolt {
            receiver_uid: "test-receiver".to_owned(),
            slot: 1,
        }
    }

    /// The first reload verdict the loop reports; the other updates do not
    /// matter to these tests.
    async fn reload_verdict(
        updates: &mut mpsc::UnboundedReceiver<GuiUpdate>,
    ) -> Result<(), ConfigReloadError> {
        loop {
            match updates.recv().await {
                Some(GuiUpdate::ConfigReloadResult(verdict)) => return verdict,
                Some(_) => {}
                None => panic!("the loop dropped its update channel"),
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_reload_requested_before_the_agent_is_up_waits_for_it() {
        // The relaunch after a self-update outruns its agent, and the state
        // constructor asks for a reload right away. That reload has to wait
        // for the agent — not be reported as a failure that the agent's
        // arrival could never clear.
        let (agent, reloads) = scripted_agent(OnReload::Accept);
        let mut effects = ScriptedEffects::answering([down(), down(), Ok(agent)]);
        let (update_tx, mut updates) = mpsc::unbounded_channel();
        let (commands, mut cmd_rx) = mpsc::unbounded_channel();
        commands.send(ReloadConfig.into()).unwrap();

        let verdict = tokio::select! {
            () = observe_loop(&mut effects, &update_tx, &mut cmd_rx) => {
                panic!("the loop ends only once the GUI hangs up")
            }
            verdict = reload_verdict(&mut updates) => verdict,
        };

        assert_eq!(
            verdict,
            Ok(()),
            "the agent's own verdict is what reaches the GUI"
        );
        assert_eq!(reloads.load(Ordering::SeqCst), 1, "delivered exactly once");
        assert_eq!(
            effects.launches, 1,
            "holding the reload does not stall the spawn reflex"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_reload_the_agent_took_down_with_it_reaches_its_successor() {
        // An agent that dies mid-reload has applied nothing. The reload stays
        // owed and reaches the replacement, which answers for itself.
        let (dying, dying_reloads) = scripted_agent(OnReload::Vanish);
        let (successor, successor_reloads) = scripted_agent(OnReload::Accept);
        let mut effects = ScriptedEffects::answering([Ok(dying), Ok(successor)]);
        let (update_tx, mut updates) = mpsc::unbounded_channel();
        let (commands, mut cmd_rx) = mpsc::unbounded_channel();
        commands.send(ReloadConfig.into()).unwrap();

        let verdict = tokio::select! {
            () = observe_loop(&mut effects, &update_tx, &mut cmd_rx) => {
                panic!("the loop ends only once the GUI hangs up")
            }
            verdict = reload_verdict(&mut updates) => verdict,
        };

        assert_eq!(
            verdict,
            Ok(()),
            "the lost attempt is never reported as a verdict"
        );
        assert_eq!(dying_reloads.load(Ordering::SeqCst), 1);
        assert_eq!(successor_reloads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_read_while_the_agent_is_down_is_answered_unavailable() {
        // A read cannot wait for an agent the way a reload does: the panel is
        // showing a spinner for it. Transient, so the panel keeps retrying
        // instead of latching "unsupported".
        let mut effects = ScriptedEffects::answering([]);
        let (update_tx, _updates) = mpsc::unbounded_channel();
        let (commands, mut cmd_rx) = mpsc::unbounded_channel();
        let (reply, answer) = oneshot::channel();
        commands
            .send(
                ReadDpi {
                    route: some_route(),
                    reply,
                }
                .into(),
            )
            .unwrap();

        let answer = tokio::select! {
            () = observe_loop(&mut effects, &update_tx, &mut cmd_rx) => {
                panic!("the loop ends only once the GUI hangs up")
            }
            answer = answer => answer.expect("every read is answered"),
        };

        assert!(matches!(answer, Err(WriteError::AgentUnavailable)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_command_brings_the_link_up_and_carries_the_agents_answer() {
        // A command does not wait for the reconnect tick: it connects on the
        // spot, and the agent's own verdict — not a local one — is what the
        // caller hears.
        let (agent, _) = scripted_agent(OnReload::Accept);
        let mut effects = ScriptedEffects::answering([Ok(agent)]);
        let (update_tx, _updates) = mpsc::unbounded_channel();
        let (commands, mut cmd_rx) = mpsc::unbounded_channel();
        let (reply, answer) = oneshot::channel();
        commands
            .send(
                ReadDpi {
                    route: some_route(),
                    reply,
                }
                .into(),
            )
            .unwrap();

        let answer = tokio::select! {
            () = observe_loop(&mut effects, &update_tx, &mut cmd_rx) => {
                panic!("the loop ends only once the GUI hangs up")
            }
            answer = answer => answer.expect("every read is answered"),
        };

        assert!(matches!(answer, Err(WriteError::AmbiguousRawDevice)));
        assert_eq!(effects.launches, 0, "a reachable agent is never spawned at");
    }
}
