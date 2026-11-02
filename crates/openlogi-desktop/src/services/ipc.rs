//! Agent IPC client.
//!
//! The agent owns all device I/O, so the GUI never opens a device — it connects
//! to the agent's local socket and (a) keeps one
//! [`Agent::observe`](openlogi_ipc::Agent::observe) request open
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
//! generation ledger and the observe call in flight; while down it owns the
//! outage clock and what the window has been told about it. What is left to
//! time is failure. `launch::spawn_agent` brings the agent up when the socket stays
//! down — gated by [`reflex::SpawnReflex`], which fires immediately for an
//! agent that was never reachable but gives a lost connection
//! [`reflex::SPAWN_AFTER_LOSS`] first (the deliberate quits and the supervised
//! restarts announce themselves within that window) and never fires at a live
//! agent newer than this GUI. A lost connection is pushed to the GUI at once
//! as [`GuiUpdate::Reconnecting`], and an outage longer than
//! `link::UNREACHABLE_AFTER` as [`GuiUpdate::Unreachable`], so the window can
//! say so instead of waiting forever; from there [`RetryConnection`] relaunches
//! without waiting out the reflex. A dead agent is noticed the moment the
//! socket closes; a *hung* one is noticed when its hold window passes without
//! an answer.
//!
//! Device commands are transient: one that finds no connection is answered as
//! unavailable by the request itself ([`request::Request::deliver`]) and the
//! next snapshot repairs the panel. A config reload is not — `config.toml` has already changed on disk and the
//! agent must re-read it — but neither is it urgent while no agent is running:
//! an agent that starts reads the file anyway. So a reload ([`ReloadConfig`],
//! [`LoadConfig`]) is held as state rather than dispatched: the loop delivers
//! it over the next live
//! connection and reports the agent's verdict then. That is what keeps an app
//! relaunch that outruns its agent (every self-update does) from latching a
//! "not applied" notice the agent's arrival could never clear.

use std::time::{Duration, Instant};

use openlogi_core::hid::{
    BacklightState, DeviceRoute, DisableKeysState, Dpi, EasySwitchHosts, FnLockState,
    ForceSensingInfo, LightCommand, PointerSpeed, ReportRateInfo, WriteError,
};
use openlogi_ipc::client::{self, ConnectError};
use openlogi_ipc::{
    AgentClient, AgentSnapshot, ClientKind, ConfigReloadError, PairingFailure, SavedConfigVerdict,
};
use tarpc::client::RpcError;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::state::DeviceKey;

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
    CancelPairing, Command, LoadConfig, PairDevice, ReadBacklight, ReadDisableKeys, ReadDpi,
    ReadEasySwitchHosts, ReadFnLock, ReadForceSensing, ReadOnboard, ReadPointerSpeed,
    ReadReportRate, ReadSmartShift, ReloadConfig, RenameEasySwitchHost, RequestAccessibilityPrompt,
    RetryConnection, SetBacklight, SetDisableKeys, SetDpi, SetFnLock, SetForceSensing, SetLight,
    SetLightManualPower, SetLighting, SetPointerSpeed, SetReportRate, SetSmartShift, StartPairing,
    UnpairDevice,
};

/// Complete identity of one Disable Keys write/reload transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DisableKeysRequestContext {
    pub(crate) key: DeviceKey,
    pub(crate) route: DeviceRoute,
    pub(crate) route_generation: u64,
    pub(crate) request_id: u64,
}

/// Correlation scope for a config reload result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConfigReloadContext {
    /// Existing application-wide reload behavior.
    General,
    /// Reload following one confirmed Disable Keys transaction.
    DisableKeys(DisableKeysRequestContext),
    /// Confirmation of an externally adopted file, independent of device routes.
    Reconciled(u64),
}

/// How long to wait before retrying a connect that failed. This is a retry
/// cadence, not a poll: once connected, nothing here runs on a timer. Short
/// enough that a just-started agent is picked up immediately.
const RECONNECT_DELAY: Duration = Duration::from_millis(250);

/// What the client thread tells the GPUI loop.
pub enum GuiUpdate {
    /// The agent's state, as of a generation this client had not seen.
    Snapshot(AgentSnapshot),
    /// An established connection dropped. The agent may only be restarting —
    /// an update's self-exec, a supervised respawn — so the window keeps its
    /// last snapshot but stops presenting it as live. Sent once per lost
    /// connection; a snapshot or [`Self::Unreachable`] supersedes it.
    Reconnecting,
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
        /// The light that issued the command.
        key: DeviceKey,
        /// Monotonic request id used to ignore stale results.
        request_id: u64,
        /// The control whose write produced this result.
        command: LightCommand,
        /// Agent acceptance or typed device failure.
        result: Result<(), WriteError>,
    },
    /// Result of a guarded Disable Keys replacement.
    DisableKeysWriteResult {
        context: DisableKeysRequestContext,
        result: Result<DisableKeysState, WriteError>,
    },
    /// Result of a plain config load, such as startup recovery.
    ConfigLoadResult {
        result: Result<(), ConfigReloadError>,
    },
    /// Whether the agent accepted the exact revision a GUI save wrote.
    ConfigReloadResult {
        context: ConfigReloadContext,
        result: Result<SavedConfigVerdict, ConfigReloadError>,
    },
    /// The device's verdict on a DPI write the GUI asked for: taken, or the
    /// typed refusal. Answers [`SetDpi`].
    DpiWritten {
        /// The device that was written.
        key: DeviceKey,
        /// Which write this answers: [`SetDpi::write`].
        write: u64,
        /// The DPI that was written.
        dpi: Dpi,
        /// `Ok` once the device took `dpi`, or why it did not.
        result: Result<(), WriteError>,
    },
    /// What a keyboard reports after an Fn-lock write the GUI asked for: the
    /// state it took, or the typed refusal. Answers [`SetFnLock`].
    FnLockWritten {
        /// The keyboard that was written.
        key: DeviceKey,
        /// The echoed state, or why the write did not land.
        result: Result<FnLockState, WriteError>,
    },
    /// What a mouse reports after a report-rate write the GUI asked for: the
    /// rate it took, or the typed refusal. Answers [`SetReportRate`].
    ReportRateWritten {
        /// The mouse that was written.
        key: DeviceKey,
        /// The connection the write went out on. The rate list belongs to
        /// that connection, so an answer is stale once the mouse moves.
        route: DeviceRoute,
        /// The read-back, or why the write did not land.
        result: Result<ReportRateInfo, WriteError>,
    },
    /// What a mouse reports after a pointer-speed write the GUI asked for:
    /// the speed it took, or the typed refusal. Answers [`SetPointerSpeed`].
    PointerSpeedWritten {
        /// The mouse that was written.
        key: DeviceKey,
        /// The read-back, or why the write did not land.
        result: Result<PointerSpeed, WriteError>,
    },
    /// What a force-sensing button reports after a press-force write the GUI
    /// asked for: the force it took, or the typed refusal. Answers
    /// [`SetForceSensing`].
    ForceSensingWritten {
        /// The device that was written.
        key: DeviceKey,
        /// Which write this answers: [`SetForceSensing::write`].
        write: u64,
        /// The force that was written: [`SetForceSensing::force`].
        force: u16,
        /// The read-back, or why the write did not land. A write the agent
        /// superseded before its turn answers with a plain read.
        result: Result<ForceSensingInfo, WriteError>,
    },
    /// What a keyboard reports after a backlight change the GUI asked for:
    /// the backlight it reads back, or the typed refusal. Answers
    /// [`SetBacklight`].
    BacklightWritten {
        /// The keyboard that was written.
        key: DeviceKey,
        /// The connection the write went out on; an answer is stale once the
        /// keyboard moves, like [`Self::ReportRateWritten`]'s.
        route: DeviceRoute,
        /// Which change this answers: [`SetBacklight::write`].
        write: u64,
        /// The read-back, or why the write did not land.
        result: Result<BacklightState, WriteError>,
    },
    /// What a device reports after an Easy-Switch host rename the GUI asked
    /// for: the slots it reads back, or the typed refusal. Answers
    /// [`RenameEasySwitchHost`].
    EasySwitchHostRenamed {
        /// The device that was written.
        key: DeviceKey,
        /// The connection the write went out on; an answer is stale once the
        /// device moves, like [`Self::ReportRateWritten`]'s.
        route: DeviceRoute,
        /// Which rename this answers: [`RenameEasySwitchHost::write`].
        write: u64,
        /// The read-back, or why the rename did not land.
        result: Result<EasySwitchHosts, WriteError>,
    },
    /// A pairing command could not be delivered, so no session will ever appear
    /// in the observed state to explain the silence. Reported locally rather
    /// than faked as a session the agent never had.
    PairingUndeliverable(PairingFailure),
    /// Cancellation completed and the receiver is available for a new choice.
    PairingCancelled,
    /// Whether the receiver let a pairing go. Answers [`UnpairDevice`].
    DeviceUnpaired {
        /// The pairing the receiver was asked to drop.
        route: DeviceRoute,
        /// What the removal was for.
        intent: UnpairIntent,
        result: Result<(), UnpairFailure>,
    },
}

/// Why the GUI asked a receiver to drop a pairing, which decides what the
/// answer changes locally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnpairIntent {
    /// The user forgot the device: its card and settings go once the
    /// receiver lets it go.
    Forget {
        /// The record the user asked to forget.
        record_key: String,
        /// Where its settings live.
        config_key: Option<String>,
        /// Pairings still to drop after this one. The settings go only once
        /// the last of them is gone.
        pending: Vec<DeviceRoute>,
    },
    /// A leftover slot of a device that stays: nothing local changes, the
    /// next inventory simply no longer lists the slot.
    StalePairing {
        /// The device's display name, for a failure message.
        name: String,
    },
}

/// Why a forgotten device kept its receiver pairing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnpairFailure {
    /// The agent refused, or the receiver failed the write.
    Refused(PairingFailure),
    /// No agent could be reached to ask.
    AgentUnreachable,
}

/// Handle the GUI holds to talk to the agent: a stream of state updates and a
/// sender for device commands. Pairing progress arrives through the same state
/// updates as everything else.
pub struct Handle {
    pub updates: mpsc::UnboundedReceiver<GuiUpdate>,
    pub commands: mpsc::UnboundedSender<Command>,
}

/// Spawn the IPC client thread. Returns immediately; the thread connects (and
/// reconnects) on its own.
#[must_use]
pub fn spawn() -> Handle {
    let (update_tx, updates) = mpsc::unbounded_channel();
    let (commands, mut cmd_rx) = mpsc::unbounded_channel::<Command>();

    let started = openlogi_core::worker::spawn("openlogi-ipc-client", move |runtime| {
        runtime.block_on(observe_loop(&mut System, &update_tx, &mut cmd_rx));
    });
    if let Err(error) = started {
        warn!(%error, "could not start the IPC client thread — agent state unavailable");
    }

    Handle { updates, commands }
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
/// the observe call stays in flight across command handling, so a device write
/// never cancels it.
async fn observe_loop(
    effects: &mut impl Effects,
    update_tx: &mpsc::UnboundedSender<GuiUpdate>,
    cmd_rx: &mut mpsc::UnboundedReceiver<Command>,
) {
    let mut link = Link::cold(Instant::now());
    let mut reflex = SpawnReflex::new();
    let mut owed = OwedReloads::default();
    let mut retry = ticker(RECONNECT_DELAY);
    loop {
        let woken = tokio::select! {
            observed = link.observed() => Woken::Observed(observed.map(|snapshot| snapshot.map(Box::new))),
            cmd = cmd_rx.recv() => Woken::Command(cmd),
            _ = retry.tick(), if link.is_down() => Woken::Reconnect,
        };
        match woken {
            Woken::Observed(Ok(Some(snapshot))) => {
                let _ = update_tx.send(GuiUpdate::Snapshot(*snapshot));
            }
            // The agent's heartbeat, or a stale reply: the window is current.
            Woken::Observed(Ok(None)) => {}
            // The connection dropped (agent self-exec on update, or a crash).
            // Reconnecting re-reads the whole state, so nothing is lost.
            Woken::Observed(Err(error)) => {
                debug!(%error, "observe failed — reconnecting");
                link.lose(Instant::now());
            }
            Woken::Command(None) => break, // GUI dropped the sender → shut down
            // Not dispatched like the device commands below: held, and
            // delivered at the end of this turn if a connection exists.
            Woken::Command(Some(Command::ReloadConfig(reload))) => owed.save(reload),
            Woken::Command(Some(Command::LoadConfig(_))) => owed.load = true,
            // The user asked to try again. A live link has nothing to retry;
            // a down one starts over as if cold — nobody else is coming — so
            // the reflex fires this turn if the connect fails, and the window
            // is owed a fresh verdict on a fresh outage clock.
            Woken::Command(Some(Command::RetryConnection(_))) => {
                if link.is_down() {
                    link = Link::cold(Instant::now());
                    reflex = SpawnReflex::new();
                    link.ensure(effects, update_tx).await;
                }
            }
            Woken::Command(Some(cmd)) => {
                let pairing_transition =
                    matches!(cmd, Command::StartPairing(_) | Command::CancelPairing(_));
                let client = link.ensure(effects, update_tx).await;
                if cmd.run(client, update_tx).await.is_err() {
                    link.lose(Instant::now());
                } else if pairing_transition {
                    link.refresh_state();
                }
            }
            Woken::Reconnect => {
                link.ensure(effects, update_tx).await;
            }
        }
        // Whatever this turn did to the link, a held reload goes out the
        // moment there is one to carry it. A transport failure here is the
        // same as anywhere: drop the link, keep the reload for the next one.
        owed.deliver(&mut link, update_tx).await;
        let now = Instant::now();
        if let Some(notice) = link.down_mut().and_then(|down| down.notice(now)) {
            let _ = update_tx.send(notice);
        }
        if reflex.should_fire(&link, now) {
            effects.spawn_agent();
            reflex.fired(now);
        }
    }
}

/// The reloads the agent has not answered yet — requested with no
/// connection, or lost with one. Each re-reads the file, so one owed reload
/// stands for any number of requests and is delivered again over the next
/// live connection.
#[derive(Debug, Default)]
struct OwedReloads {
    /// A [`LoadConfig`] is owed.
    load: bool,
    /// A [`ReloadConfig`] is owed, at the revision of the latest save it
    /// stands for and with the chosen controls of every one of them.
    save: Option<ReloadConfig>,
}

impl OwedReloads {
    /// Owe `reload`, merged into the one already owed: the file is now at
    /// the revision of this save, and its chosen controls join the others.
    /// Each transaction still expects its own revision; a retry replaces only
    /// that transaction's expectation, not another device's pending receipt.
    fn save(&mut self, reload: ReloadConfig) {
        let ReloadConfig {
            chosen,
            revision,
            contexts,
        } = reload;
        let owed = self.save.get_or_insert_with(|| ReloadConfig {
            contexts: Vec::new(),
            ..Default::default()
        });
        owed.revision = revision;
        for context in contexts {
            match owed
                .contexts
                .iter_mut()
                .find(|owed| owed.context() == context.context())
            {
                Some(owed) => *owed = context,
                None => owed.contexts.push(context),
            }
        }
        for light in chosen {
            match owed
                .chosen
                .iter_mut()
                .find(|owed| owed.device == light.device)
            {
                Some(owed) => {
                    owed.power |= light.power;
                    owed.brightness |= light.brightness;
                    owed.temperature |= light.temperature;
                }
                None => owed.chosen.push(light),
            }
        }
    }

    /// Deliver what is owed if `link` is up. The saved reload goes first: it
    /// takes the light values of the file as copies of what the lights
    /// reported, so a load of the same file after it replaces no change made
    /// on a light since. A transport failure drops the link and keeps the
    /// reload for the next one.
    async fn deliver(&mut self, link: &mut Link, update_tx: &mpsc::UnboundedSender<GuiUpdate>) {
        if let Some(reload) = self.save.clone()
            && let Some(client) = link.client()
        {
            match request::run(reload, Some(client), update_tx).await {
                Ok(()) => self.save = None,
                Err(LinkLost) => link.lose(Instant::now()),
            }
        }
        if self.load
            && let Some(client) = link.client()
        {
            match request::run(LoadConfig, Some(client), update_tx).await {
                Ok(()) => self.load = false,
                Err(LinkLost) => link.lose(Instant::now()),
            }
        }
    }
}

/// Why [`observe_loop`] woke up.
enum Woken {
    /// The observe call answered, or its connection dropped. The snapshot is
    /// boxed so the other wakes do not carry its size.
    Observed(Result<Option<Box<AgentSnapshot>>, RpcError>),
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

    use std::sync::Mutex;

    use openlogi_core::config::ConfigRevision;
    use openlogi_core::hid::DeviceRoute;
    use openlogi_ipc::testing::in_memory_agent;
    use openlogi_ipc::{
        AgentRequest, AgentResponse, AgentStatus, ChosenLight, ForegroundApps, InventoryHealth,
        Observation, PROTOCOL_VERSION, SavedConfig, SavedConfigVerdict,
    };
    use tokio::sync::oneshot;

    use super::*;

    /// How a scripted agent answers a reload.
    #[derive(Clone, Copy)]
    enum OnReload {
        /// Adopt the config.
        Accept,
        /// Reload an externally changed file instead of the GUI's saved text.
        FileChangedAfterSave,
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
                        AgentRequest::ReloadConfig {} => Ok(reload_answer(
                            on_reload,
                            &counted,
                            &signal,
                            AgentResponse::ReloadConfig(Ok(())),
                        )
                        .await),
                        AgentRequest::ReloadSavedConfigWithVerdict { .. } => {
                            let verdict = match on_reload {
                                OnReload::FileChangedAfterSave => {
                                    SavedConfigVerdict::FileChangedAfterSave
                                }
                                OnReload::Accept | OnReload::Vanish => SavedConfigVerdict::Accepted,
                            };
                            Ok(reload_answer(
                                on_reload,
                                &counted,
                                &signal,
                                AgentResponse::ReloadSavedConfigWithVerdict(Ok(verdict)),
                            )
                            .await)
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

    /// Count a reload and answer it as `on_reload` says: `accepted`, or
    /// never, with the connection closed.
    async fn reload_answer(
        on_reload: OnReload,
        counted: &AtomicUsize,
        signal: &tokio::sync::Notify,
        accepted: AgentResponse,
    ) -> AgentResponse {
        counted.fetch_add(1, Ordering::SeqCst);
        match on_reload {
            OnReload::Accept | OnReload::FileChangedAfterSave => accepted,
            OnReload::Vanish => {
                signal.notify_one();
                std::future::pending().await
            }
        }
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

    /// The first startup-load result the loop reports; the other updates do
    /// not matter to these tests.
    async fn config_load_result(
        updates: &mut mpsc::UnboundedReceiver<GuiUpdate>,
    ) -> Result<(), ConfigReloadError> {
        loop {
            match updates.recv().await {
                Some(GuiUpdate::ConfigLoadResult { result }) => return result,
                Some(_) => {}
                None => panic!("the loop dropped its update channel"),
            }
        }
    }

    /// The first saved-reload verdict the loop reports for the general context.
    async fn saved_reload_verdict(
        updates: &mut mpsc::UnboundedReceiver<GuiUpdate>,
    ) -> Result<SavedConfigVerdict, ConfigReloadError> {
        loop {
            match updates.recv().await {
                Some(GuiUpdate::ConfigReloadResult {
                    context: ConfigReloadContext::General,
                    result,
                }) => return result,
                Some(_) => {}
                None => panic!("the loop dropped its update channel"),
            }
        }
    }

    /// The saved-reload verdict for one exact transaction context.
    async fn saved_reload_verdict_for(
        updates: &mut mpsc::UnboundedReceiver<GuiUpdate>,
        expected: ConfigReloadContext,
    ) -> Result<SavedConfigVerdict, ConfigReloadError> {
        loop {
            match updates.recv().await {
                Some(GuiUpdate::ConfigReloadResult { context, result }) if context == expected => {
                    return result;
                }
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
        commands.send(LoadConfig.into()).unwrap();

        let verdict = tokio::select! {
            () = observe_loop(&mut effects, &update_tx, &mut cmd_rx) => {
                panic!("the loop ends only once the GUI hangs up")
            }
            verdict = config_load_result(&mut updates) => verdict,
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
        // owed and reaches the replacement with its owning transaction context.
        let (dying, dying_reloads) = scripted_agent(OnReload::Vanish);
        let (successor, successor_reloads) = scripted_agent(OnReload::Accept);
        let mut effects = ScriptedEffects::answering([Ok(dying), Ok(successor)]);
        let (update_tx, mut updates) = mpsc::unbounded_channel();
        let (commands, mut cmd_rx) = mpsc::unbounded_channel();
        let context = DisableKeysRequestContext {
            key: DeviceKey::from("keyboard"),
            route: some_route(),
            route_generation: 3,
            request_id: 7,
        };
        commands
            .send(ReloadConfig::disable_keys(context.clone(), ConfigRevision::of("saved")).into())
            .unwrap();

        let verdict = tokio::select! {
            () = observe_loop(&mut effects, &update_tx, &mut cmd_rx) => {
                panic!("the loop ends only once the GUI hangs up")
            }
            verdict = saved_reload_verdict_for(&mut updates, ConfigReloadContext::DisableKeys(context)) => verdict,
        };

        assert_eq!(
            verdict,
            Ok(SavedConfigVerdict::Accepted),
            "the lost attempt is never reported as a verdict"
        );
        assert_eq!(dying_reloads.load(Ordering::SeqCst), 1);
        assert_eq!(successor_reloads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn file_changed_after_save_reaches_the_general_context_over_ipc() {
        // This exercises tarpc's generated request and response enums through
        // the same in-memory transport that the production client loop uses.
        let (agent, reloads) = scripted_agent(OnReload::FileChangedAfterSave);
        let mut effects = ScriptedEffects::answering([Ok(agent)]);
        let (update_tx, mut updates) = mpsc::unbounded_channel();
        let (commands, mut cmd_rx) = mpsc::unbounded_channel();
        commands
            .send(
                ReloadConfig {
                    revision: ConfigRevision::of("saved by the GUI"),
                    ..Default::default()
                }
                .into(),
            )
            .unwrap();

        let verdict = tokio::select! {
            () = observe_loop(&mut effects, &update_tx, &mut cmd_rx) => {
                panic!("the loop ends only once the GUI hangs up")
            }
            verdict = saved_reload_verdict(&mut updates) => verdict,
        };

        assert_eq!(verdict, Ok(SavedConfigVerdict::FileChangedAfterSave));
        assert_eq!(reloads.load(Ordering::SeqCst), 1, "delivered exactly once");
    }

    #[tokio::test(start_paused = true)]
    async fn saves_made_while_the_agent_is_down_reach_it_merged_and_before_the_load() {
        // Each reload re-reads the file, so the saves made while no agent
        // answers become one saved reload with the chosen controls of all of
        // them, at the revision of the last. It goes before the load: the
        // saved reload takes the light values of the file as copies of
        // reports, which the load of the same file then finds in place.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = seen.clone();
        let agent = in_memory_agent(
            move |request| {
                let recorded = recorded.clone();
                Box::pin(async move {
                    match request {
                        AgentRequest::ReloadSavedConfigWithVerdict { saved } => {
                            recorded.lock().unwrap().push(Some(saved));
                            Ok(AgentResponse::ReloadSavedConfigWithVerdict(Ok(
                                SavedConfigVerdict::Accepted,
                            )))
                        }
                        AgentRequest::ReloadConfig {} => {
                            recorded.lock().unwrap().push(None);
                            Ok(AgentResponse::ReloadConfig(Ok(())))
                        }
                        AgentRequest::Observe { .. } => std::future::pending().await,
                        other => panic!("the client loop sent an unexpected request: {other:?}"),
                    }
                })
            },
            std::future::pending(),
        );
        let chosen = |device: &str, power, brightness, temperature| ChosenLight {
            device: device.to_owned(),
            power,
            brightness,
            temperature,
        };
        let mut effects = ScriptedEffects::answering([down(), Ok(agent)]);
        let (update_tx, mut updates) = mpsc::unbounded_channel();
        let (commands, mut cmd_rx) = mpsc::unbounded_channel();
        commands.send(LoadConfig.into()).unwrap();
        for (text, chosen) in [
            ("first", vec![chosen("glow", false, true, false)]),
            ("second", Vec::new()),
            (
                "third",
                vec![
                    chosen("glow", true, false, false),
                    chosen("beam", false, false, true),
                ],
            ),
        ] {
            let revision = ConfigRevision::of(text);
            commands
                .send(
                    ReloadConfig {
                        chosen,
                        revision,
                        ..Default::default()
                    }
                    .into(),
                )
                .unwrap();
        }

        let verdicts = tokio::select! {
            () = observe_loop(&mut effects, &update_tx, &mut cmd_rx) => {
                panic!("the loop ends only once the GUI hangs up")
            }
            verdicts = async {
                let owed = (
                    saved_reload_verdict(&mut updates).await,
                    config_load_result(&mut updates).await,
                );
                // A delivered reload is no longer owed: the next save goes
                // out alone, and nothing follows it once the loop is idle.
                commands.send(ReloadConfig::default().into()).unwrap();
                let next = saved_reload_verdict(&mut updates).await;
                (owed, next)
            } => verdicts,
        };

        assert_eq!(
            verdicts,
            (
                (Ok(SavedConfigVerdict::Accepted), Ok(())),
                Ok(SavedConfigVerdict::Accepted)
            )
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                Some(SavedConfig {
                    revision: ConfigRevision::of("third"),
                    chosen: vec![
                        chosen("glow", true, true, false),
                        chosen("beam", false, false, true),
                    ],
                }),
                None,
                Some(SavedConfig::default()),
            ],
            "one saved reload with every chosen control, then the load, then the next save"
        );
    }

    #[test]
    fn owed_saves_merge_their_chosen_controls_by_light_at_the_latest_revision() {
        let chosen = |device: &str, power, brightness, temperature| ChosenLight {
            device: device.to_owned(),
            power,
            brightness,
            temperature,
        };
        let save = |text: &str, chosen| ReloadConfig {
            chosen,
            revision: ConfigRevision::of(text),
            ..Default::default()
        };
        // A save that chose nothing is still a save to deliver.
        let mut owed = OwedReloads::default();
        owed.save(save("first", Vec::new()));
        assert_eq!(
            owed.save,
            Some(ReloadConfig {
                revision: ConfigRevision::of("first"),
                chosen: Vec::new(),
                ..Default::default()
            })
        );
        // A control any merged save chose stays chosen, and the file is at
        // the revision of the last save.
        owed.save(save("second", vec![chosen("glow", true, true, true)]));
        owed.save(save(
            "third",
            vec![
                chosen("glow", false, false, false),
                chosen("beam", false, false, true),
            ],
        ));
        assert_eq!(
            owed.save,
            Some(ReloadConfig {
                revision: ConfigRevision::of("third"),
                chosen: vec![
                    chosen("glow", true, true, true),
                    chosen("beam", false, false, true),
                ],
                ..Default::default()
            })
        );
        assert!(!owed.load);
    }

    #[test]
    fn regression_coalesced_reload_must_not_accept_an_earlier_revision() {
        let context = DisableKeysRequestContext {
            key: DeviceKey::from("keyboard-a"),
            route: some_route(),
            route_generation: 3,
            request_id: 7,
        };
        let mut owed = OwedReloads::default();
        owed.save(ReloadConfig::disable_keys(
            context.clone(),
            ConfigRevision::of("policy A saved"),
        ));
        // A later save may have refreshed an external edit that replaced A.
        // Acceptance of B says nothing about the earlier context's revision.
        owed.save(ReloadConfig {
            revision: ConfigRevision::of("policy B after external edit"),
            ..Default::default()
        });
        let reload = owed.save.unwrap();
        let (updates, mut received) = mpsc::unbounded_channel();
        request::Request::deliver(reload, Ok(Ok(SavedConfigVerdict::Accepted)), &updates);
        let mut observed = false;
        while let Ok(update) = received.try_recv() {
            if let GuiUpdate::ConfigReloadResult {
                context: ConfigReloadContext::DisableKeys(actual),
                result,
            } = update
            {
                assert_eq!(actual, context);
                observed = true;
                assert_eq!(result.unwrap(), SavedConfigVerdict::FileChangedAfterSave);
            }
        }
        assert!(
            observed,
            "earlier context must receive a reconciliation verdict"
        );
    }

    #[test]
    fn coalesced_reconciliation_only_accepts_its_own_revision() {
        let mut owed = OwedReloads::default();
        owed.save(ReloadConfig::reconciled(
            1,
            ConfigRevision::of("adopted R3"),
        ));
        owed.save(ReloadConfig::reconciled(
            2,
            ConfigRevision::of("adopted R4"),
        ));
        let reload = owed.save.expect("one batched reload");
        let (updates, mut received) = mpsc::unbounded_channel();
        request::Request::deliver(reload, Ok(Ok(SavedConfigVerdict::Accepted)), &updates);
        for (expected_id, expected_verdict) in [
            (1, SavedConfigVerdict::FileChangedAfterSave),
            (2, SavedConfigVerdict::Accepted),
        ] {
            let Ok(GuiUpdate::ConfigReloadResult { context, result }) = received.try_recv() else {
                panic!("each adoption receives its own verdict");
            };
            assert_eq!(context, ConfigReloadContext::Reconciled(expected_id));
            assert_eq!(result.unwrap(), expected_verdict);
        }
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn owed_saves_preserve_each_transaction_context_at_the_latest_file_revision() {
        let context = DisableKeysRequestContext {
            key: DeviceKey::from("keyboard"),
            route: some_route(),
            route_generation: 3,
            request_id: 7,
        };
        let mut owed = OwedReloads::default();
        owed.save(ReloadConfig::disable_keys(
            context.clone(),
            ConfigRevision::of("first"),
        ));
        let chosen = ChosenLight {
            device: "light".into(),
            power: true,
            brightness: false,
            temperature: false,
        };
        owed.save(ReloadConfig {
            chosen: vec![chosen.clone()],
            revision: ConfigRevision::of("second"),
            ..Default::default()
        });
        owed.save(ReloadConfig::disable_keys(
            context.clone(),
            ConfigRevision::of("third"),
        ));
        let reload = owed.save.expect("one coalesced reload");
        assert_eq!(reload.revision, ConfigRevision::of("third"));
        assert_eq!(reload.chosen, [chosen]);
        assert_eq!(
            reload.contexts,
            [
                request::SavedReloadContext::DisableKeys {
                    context: context.clone(),
                    revision: ConfigRevision::of("third"),
                },
                request::SavedReloadContext::General
            ]
        );
        let (updates, mut received) = mpsc::unbounded_channel();
        request::Request::deliver(reload, Ok(Ok(SavedConfigVerdict::Accepted)), &updates);
        assert!(
            matches!(received.try_recv(), Ok(GuiUpdate::ConfigReloadResult {
            context: ConfigReloadContext::DisableKeys(actual),
            result: Ok(SavedConfigVerdict::Accepted)
        }) if actual == context)
        );
        assert!(matches!(
            received.try_recv(),
            Ok(GuiUpdate::ConfigReloadResult {
                context: ConfigReloadContext::General,
                result: Ok(SavedConfigVerdict::Accepted)
            })
        ));
        assert!(
            received.try_recv().is_err(),
            "a duplicate context is answered only once"
        );
    }

    #[test]
    fn coalesced_reload_keeps_per_device_verdicts_and_preserves_failures() {
        let first = DisableKeysRequestContext {
            key: DeviceKey::from("keyboard-a"),
            route: some_route(),
            route_generation: 3,
            request_id: 7,
        };
        let second = DisableKeysRequestContext {
            key: DeviceKey::from("keyboard-b"),
            route: DeviceRoute::Bolt {
                receiver_uid: "test-receiver".into(),
                slot: 2,
            },
            ..first.clone()
        };
        for same_revision in [false, true] {
            let mut owed = OwedReloads::default();
            owed.save(ReloadConfig::disable_keys(
                first.clone(),
                ConfigRevision::of(if same_revision { "latest" } else { "earlier" }),
            ));
            owed.save(ReloadConfig::disable_keys(
                second.clone(),
                ConfigRevision::of("latest"),
            ));
            owed.save(ReloadConfig {
                revision: ConfigRevision::of("latest"),
                ..Default::default()
            });
            let reload = owed.save.expect("one coalesced reload");
            for verdict in [
                Ok(SavedConfigVerdict::Accepted),
                Ok(SavedConfigVerdict::FileChangedAfterSave),
                Err(ConfigReloadError {
                    message: "invalid config".into(),
                }),
            ] {
                let (updates, mut received) = mpsc::unbounded_channel();
                request::Request::deliver(reload.clone(), Ok(verdict.clone()), &updates);
                let first_verdict = if !same_revision && verdict == Ok(SavedConfigVerdict::Accepted)
                {
                    Ok(SavedConfigVerdict::FileChangedAfterSave)
                } else {
                    verdict.clone()
                };
                for (expected_context, expected_result) in [
                    (
                        ConfigReloadContext::DisableKeys(first.clone()),
                        first_verdict,
                    ),
                    (
                        ConfigReloadContext::DisableKeys(second.clone()),
                        verdict.clone(),
                    ),
                    (ConfigReloadContext::General, verdict.clone()),
                ] {
                    let GuiUpdate::ConfigReloadResult { context, result } =
                        received.try_recv().unwrap()
                    else {
                        panic!("expected a saved-reload verdict");
                    };
                    assert_eq!(context, expected_context);
                    assert_eq!(result, expected_result);
                }
                assert!(received.try_recv().is_err(), "one result per recipient");
            }
            let (updates, mut received) = mpsc::unbounded_channel();
            request::Request::deliver(reload, Err(request::Unavailable), &updates);
            assert!(
                received.try_recv().is_err(),
                "transport failure must not acknowledge any save"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn coalesced_device_saves_deliver_independent_receipts_after_reconnect() {
        let (agent, reloads) = scripted_agent(OnReload::Accept);
        let mut effects = ScriptedEffects::answering([down(), Ok(agent)]);
        let (update_tx, mut updates) = mpsc::unbounded_channel();
        let (commands, mut cmd_rx) = mpsc::unbounded_channel();
        let first = DisableKeysRequestContext {
            key: DeviceKey::from("keyboard-a"),
            route: some_route(),
            route_generation: 3,
            request_id: 7,
        };
        let second = DisableKeysRequestContext {
            key: DeviceKey::from("keyboard-b"),
            route: DeviceRoute::Bolt {
                receiver_uid: "test-receiver".into(),
                slot: 2,
            },
            ..first.clone()
        };
        for (context, text) in [
            (first.clone(), "first save"),
            (second.clone(), "second save"),
        ] {
            commands
                .send(ReloadConfig::disable_keys(context, ConfigRevision::of(text)).into())
                .unwrap();
        }
        let verdicts = tokio::select! {
            () = observe_loop(&mut effects, &update_tx, &mut cmd_rx) => panic!("client stopped"),
            verdicts = async {
                let first = saved_reload_verdict_for(&mut updates, ConfigReloadContext::DisableKeys(first)).await;
                let second = saved_reload_verdict_for(&mut updates, ConfigReloadContext::DisableKeys(second)).await;
                (first, second)
            } => verdicts,
        };
        assert_eq!(
            verdicts,
            (
                Ok(SavedConfigVerdict::FileChangedAfterSave),
                Ok(SavedConfigVerdict::Accepted)
            )
        );
        assert_eq!(
            reloads.load(Ordering::SeqCst),
            1,
            "batching still sends one RPC"
        );
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

    #[tokio::test(start_paused = true)]
    async fn a_retry_launches_the_agent_without_waiting_out_the_reflex() {
        // The reflex fired for this cold start and would not fire again for
        // `SPAWN_RETRY_PERIOD`. The user's Retry must not wait that out.
        let mut effects = ScriptedEffects::answering([]);
        let (update_tx, _updates) = mpsc::unbounded_channel();
        let (commands, mut cmd_rx) = mpsc::unbounded_channel();

        tokio::select! {
            () = observe_loop(&mut effects, &update_tx, &mut cmd_rx) => {
                panic!("the loop ends only once the GUI hangs up")
            }
            // Retry between two reconnect ticks, so only the retry can launch.
            () = async {
                tokio::time::sleep(RECONNECT_DELAY / 2).await;
                commands.send(RetryConnection.into()).unwrap();
                tokio::time::sleep(RECONNECT_DELAY / 4).await;
            } => {}
        }

        assert_eq!(
            effects.launches, 2,
            "one launch for the cold start, and one for the retry"
        );
    }

    /// A snapshot the tests can tell apart by its camera flag.
    fn snapshot(camera_active: bool) -> AgentSnapshot {
        AgentSnapshot {
            status: AgentStatus {
                accessibility_granted: true,
                hook_installed: true,
                launch_at_login: false,
                inventory: InventoryHealth::Ready,
                protocol_version: PROTOCOL_VERSION,
                agent_version: "observe-loop-test".to_owned(),
                input_monitoring_granted: true,
                hid_open_failures: false,
                secure_input: openlogi_ipc::SecureInput::Off,
                virtual_keyboard: openlogi_ipc::VirtualKeyboardStatus::Unsupported,
            },
            inventory: Vec::new(),
            standalone: Vec::new(),
            camera_active,
            pairing: None,
            foreground: ForegroundApps::default(),
            lights: Vec::new(),
            flow: openlogi_ipc::FlowStatus::default(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_discards_an_observation_buffered_before_cleanup() {
        let calls = Arc::new(AtomicUsize::new(0));
        let old_started = Arc::new(tokio::sync::Notify::new());
        let release_old = Arc::new(tokio::sync::Notify::new());
        let old_answered = Arc::new(tokio::sync::Notify::new());
        let started = old_started.clone();
        let agent = in_memory_agent(
            move |request| {
                let calls = calls.clone();
                let started = started.clone();
                let release = release_old.clone();
                let answered = old_answered.clone();
                Box::pin(async move {
                    match request {
                        AgentRequest::Observe { .. } => {
                            let call = calls.fetch_add(1, Ordering::SeqCst);
                            if call > 2 {
                                return std::future::pending().await;
                            }
                            let mut state = snapshot(false);
                            if call < 2 {
                                state.pairing = Some(openlogi_ipc::PairingPhase::Searching);
                            }
                            if call == 1 {
                                started.notify_one();
                                release.notified().await;
                                answered.notify_one();
                            }
                            Ok(AgentResponse::Observe(Observation {
                                generation: call as u64 + 1,
                                snapshot: state,
                            }))
                        }
                        AgentRequest::CancelPairing {} => {
                            release.notify_one();
                            answered.notified().await;
                            Ok(AgentResponse::CancelPairing(Ok(())))
                        }
                        other => panic!("unexpected request: {other:?}"),
                    }
                })
            },
            std::future::pending(),
        );
        let mut effects = ScriptedEffects::answering([Ok(agent)]);
        let (update_tx, mut updates) = mpsc::unbounded_channel();
        let (commands, mut cmd_rx) = mpsc::unbounded_channel();
        tokio::select! {
            () = observe_loop(&mut effects, &update_tx, &mut cmd_rx) => panic!("client stopped"),
            () = async {
                assert!(matches!(updates.recv().await, Some(GuiUpdate::Snapshot(_))));
                old_started.notified().await;
                commands.send(CancelPairing.into()).unwrap();
                assert!(matches!(updates.recv().await, Some(GuiUpdate::PairingCancelled)));
                let Some(GuiUpdate::Snapshot(current)) = updates.recv().await else { panic!("expected current state"); };
                assert_eq!(current.pairing, None, "a stale active phase must not replace cancellation completion");
            } => {}
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_newer_snapshot_reaches_the_window_and_a_heartbeat_does_not() {
        // The agent answers the hold elapsing with the generation the client
        // already has. That keeps the link alive and must not repaint a
        // window that is already current.
        let answers = Arc::new(Mutex::new(VecDeque::from([
            (1, snapshot(false)),
            (1, snapshot(false)),
            (2, snapshot(true)),
        ])));
        let agent = in_memory_agent(
            move |request| {
                let answers = answers.clone();
                Box::pin(async move {
                    let AgentRequest::Observe { .. } = request else {
                        panic!("the client loop sent an unexpected request: {request:?}");
                    };
                    let next = answers.lock().unwrap().pop_front();
                    match next {
                        Some((generation, snapshot)) => Ok(AgentResponse::Observe(Observation {
                            generation,
                            snapshot,
                        })),
                        None => std::future::pending().await,
                    }
                })
            },
            std::future::pending(),
        );
        let mut effects = ScriptedEffects::answering([Ok(agent)]);
        let (update_tx, mut updates) = mpsc::unbounded_channel();
        let (_commands, mut cmd_rx) = mpsc::unbounded_channel();

        let seen = tokio::select! {
            () = observe_loop(&mut effects, &update_tx, &mut cmd_rx) => {
                panic!("the loop ends only once the GUI hangs up")
            }
            seen = async {
                let mut seen = Vec::new();
                while seen.len() < 2 {
                    if let Some(GuiUpdate::Snapshot(snapshot)) = updates.recv().await {
                        seen.push(snapshot);
                    }
                }
                seen
            } => seen,
        };

        assert_eq!(seen, [snapshot(false), snapshot(true)]);
    }
}
