//! The supervisor: which modules run, starting and stopping them, and what
//! each one's status row says.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use openlogi_core::module::ModuleId;
use openlogi_ipc::{ModuleInfo, ModuleState, ModuleStatus};
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{debug, info, warn};

use super::child::{self, EventSink, RunEnd, RunEvent, RunHandle, RunSpec};
use super::discovery::{self, Found};
use super::restart::{AfterExit, Restarts};
use super::{
    HostCommand, ModuleEvent, ModuleKey, ModulePlan, ModulePress, ModuleTimings, SharedModulePlan,
};
use crate::watchers::retry::wait_for_deadline;

/// How many run events may wait for the host.
const RUN_EVENT_QUEUE: usize = 64;

/// The module host, built by [`ModuleHostHandle::new`](super::ModuleHostHandle::new)
/// and started with [`Self::run`].
pub struct ModuleHost {
    dir: Option<PathBuf>,
    timings: ModuleTimings,
    commands: mpsc::Receiver<HostCommand>,
    statuses: watch::Sender<Arc<Vec<ModuleStatus>>>,
    listening: watch::Receiver<bool>,
}

/// One module's life across its runs.
struct Module {
    restarts: Restarts,
    run: Option<LiveRun>,
    phase: Phase,
    /// How the last run that the host did not end ended, until a run gets
    /// ready again.
    last_error: Option<String>,
    /// The plan's [`ModulePlan::turned_off`] count for it when last seen.
    turned_off: u64,
}

/// An installed module that can run here.
struct Installed {
    id: ModuleId,
    folder: PathBuf,
    executable: PathBuf,
    fingerprint: u64,
}

/// What happens next to a module with no live run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Start it as soon as it is wanted.
    Idle,
    /// Start it at this time, if it is still wanted.
    RestartAt(Instant),
    /// It failed to start too often; it waits for a toggle or a new folder
    /// fingerprint.
    Failed { fingerprint: u64 },
}

/// A run the host started and has not seen end.
struct LiveRun {
    handle: RunHandle,
    generation: u64,
    ready: bool,
    /// The folder fingerprint and settings it started with: a change to
    /// either restarts it.
    fingerprint: u64,
    settings: toml::Table,
    /// The host told it to stop.
    stopping: bool,
}

/// The host's state while it runs.
struct Supervisor {
    dir: Option<PathBuf>,
    timings: ModuleTimings,
    found: Vec<Found>,
    plan: Arc<ModulePlan>,
    modules: BTreeMap<ModuleId, Module>,
    generation: u64,
    run_events: mpsc::Sender<(ModuleId, u64, RunEvent)>,
    events: mpsc::Sender<ModuleEvent>,
    /// Whether the agent drains `events`; while it does not, nothing is
    /// forwarded.
    listening: watch::Receiver<bool>,
    /// The modules whose end the agent was not listening for, told once it
    /// listens again.
    untold_stops: BTreeSet<ModuleId>,
    /// Every module was stopped and none starts until the agent resumes.
    held: bool,
    keypads: KeypadNames,
}

/// What modules call each keypad: `keypad-1`, `keypad-2`, and so on, in the
/// order the host first meets them, for as long as the host runs. A config
/// key can hold a receiver's UID or a device serial, which is no module's
/// business.
#[derive(Default)]
struct KeypadNames(BTreeMap<String, String>);

impl KeypadNames {
    /// The name modules know the keypad `config_key` by.
    fn name(&mut self, config_key: &str) -> String {
        let next = self.0.len() + 1;
        self.0
            .entry(config_key.to_owned())
            .or_insert_with(|| format!("keypad-{next}"))
            .clone()
    }

    /// The config key of the keypad modules call `name`.
    fn config_key(&self, name: &str) -> Option<String> {
        self.0
            .iter()
            .find(|(_, known)| *known == name)
            .map(|(config_key, _)| config_key.clone())
    }

    /// `face` with its keypad under its config key again; `None` for a
    /// name no module was given.
    fn to_config_keys(&self, face: ModuleEvent) -> Option<ModuleEvent> {
        Some(match face {
            ModuleEvent::Image {
                module,
                device,
                slot,
                image,
            } => ModuleEvent::Image {
                module,
                device: self.config_key(&device)?,
                slot,
                image,
            },
            ModuleEvent::Clear {
                module,
                device,
                slot,
            } => ModuleEvent::Clear {
                module,
                device: self.config_key(&device)?,
                slot,
            },
            face @ (ModuleEvent::ImageAll { .. } | ModuleEvent::Stopped { .. }) => face,
        })
    }
}

impl ModuleHost {
    pub(super) fn new(
        dir: Option<PathBuf>,
        timings: ModuleTimings,
        commands: mpsc::Receiver<HostCommand>,
        statuses: watch::Sender<Arc<Vec<ModuleStatus>>>,
        listening: watch::Receiver<bool>,
    ) -> Self {
        Self {
            dir,
            timings,
            commands,
            statuses,
            listening,
        }
    }

    /// Run the host until it is told to shut down or the agent drops its
    /// handles: keep the modules `plan` wants running, send their face
    /// changes to `events`, and publish where each stands.
    pub async fn run(self, mut plan: SharedModulePlan, events: mpsc::Sender<ModuleEvent>) {
        let Self {
            dir,
            timings,
            mut commands,
            statuses,
            listening,
        } = self;
        let (run_events, mut run_rx) = mpsc::channel(RUN_EVENT_QUEUE);
        let mut host = Supervisor {
            dir,
            timings,
            found: Vec::new(),
            plan: Arc::clone(&plan.borrow_and_update()),
            modules: BTreeMap::new(),
            generation: 0,
            run_events,
            events,
            listening,
            untold_stops: BTreeSet::new(),
            held: false,
            keypads: KeypadNames::default(),
        };
        let mut rescan = tokio::time::interval(timings.rescan_every);
        rescan.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            statuses.send_if_modified(|current| {
                let next = host.statuses();
                if **current == next {
                    return false;
                }
                *current = Arc::new(next);
                true
            });
            let restart = host.next_restart();
            tokio::select! {
                command = commands.recv() => match command {
                    Some(HostCommand::Press(press)) => host.press(press),
                    Some(HostCommand::Rescan) => {
                        host.rescan().await;
                        host.reconcile();
                    }
                    Some(HostCommand::StopAll(ack)) => {
                        host.stop_all(&mut run_rx).await;
                        host.held = true;
                        let _ = ack.send(());
                    }
                    Some(HostCommand::Resume) => {
                        host.resume().await;
                        host.reconcile();
                    }
                    Some(HostCommand::Shutdown(ack)) => {
                        host.shut_down(&mut run_rx).await;
                        statuses.send_replace(Arc::new(Vec::new()));
                        let _ = ack.send(());
                        return;
                    }
                    None => {
                        host.shut_down(&mut run_rx).await;
                        return;
                    }
                },
                changed = plan.changed() => {
                    if changed.is_err() {
                        host.shut_down(&mut run_rx).await;
                        return;
                    }
                    host.plan = Arc::clone(&plan.borrow_and_update());
                    host.rescan().await;
                    host.reconcile();
                }
                Some((id, generation, event)) = run_rx.recv() => {
                    host.run_event(&id, generation, event).await;
                    host.reconcile();
                }
                _ = rescan.tick() => {
                    host.rescan().await;
                    host.reconcile();
                }
                () = wait_for_deadline(restart) => host.reconcile(),
            }
        }
    }
}

impl Supervisor {
    /// Read the modules folder again. An unreadable folder keeps what was
    /// last found.
    async fn rescan(&mut self) {
        let Some(dir) = self.dir.clone() else {
            return;
        };
        match tokio::task::spawn_blocking(move || discovery::scan(&dir)).await {
            Ok(Ok(found)) => self.found = found,
            Ok(Err(error)) => {
                warn!(%error, "modules folder cannot be read: keeping the modules found before");
            }
            Err(error) => {
                warn!(%error, "modules folder scan failed: keeping the modules found before");
            }
        }
    }

    /// The installed modules that can run here, with their folders.
    fn runnable(&self) -> impl Iterator<Item = &Found> {
        self.found.iter().filter(|found| {
            found
                .manifest
                .as_ref()
                .is_ok_and(openlogi_core::module::ModuleManifest::runs_here)
        })
    }

    /// The keys `id` owns in the current plan, each keypad under the name
    /// modules know it by.
    fn keys_of(&mut self, id: &ModuleId) -> Vec<ModuleKey> {
        let keys = self.plan.keys.get(id).map_or(&[][..], Vec::as_slice);
        keys.iter()
            .map(|key| ModuleKey {
                device: self.keypads.name(&key.device),
                ..key.clone()
            })
            .collect()
    }

    /// Bring every module in line with the plan and the folder: start what
    /// is wanted and due, stop what is not wanted or changed under it, and
    /// hand each live run its current keys. A held host starts nothing.
    fn reconcile(&mut self) {
        if self.held {
            return;
        }
        let now = Instant::now();
        let installed: Vec<Installed> = self
            .runnable()
            .filter_map(|found| {
                let manifest = found.manifest.as_ref().ok()?;
                Some(Installed {
                    id: manifest.id().clone(),
                    folder: found.path.clone(),
                    executable: manifest.executable_in(&found.path),
                    fingerprint: found.fingerprint,
                })
            })
            .collect();
        self.stop_gone(&installed);
        for module in installed {
            if self.due(&module, now) {
                self.start(module);
            }
        }
    }

    /// Stop each module whose folder is gone, invalid, or no longer for
    /// this platform, and forget it once its run has ended.
    fn stop_gone(&mut self, installed: &[Installed]) {
        self.modules.retain(|id, module| {
            if installed.iter().any(|installed| installed.id == *id) {
                return true;
            }
            match module.run.as_mut() {
                Some(run) => {
                    stop(id, run);
                    true
                }
                None => false,
            }
        });
    }

    /// Settle `installed` against the plan at `now`, and say whether it is
    /// to start: stop it when turned off or changed under it, clear its
    /// failure on a toggle or a new folder, hand a live run its keys.
    fn due(&mut self, installed: &Installed, now: Instant) -> bool {
        let id = &installed.id;
        let keys = self.keys_of(id);
        let config = self.plan.config.get(id);
        let enabled = config.is_some_and(|config| config.enabled);
        let settings = config.map(|config| &config.settings);
        let turned_off = self.plan.turned_off.get(id).copied().unwrap_or(0);
        let timings = self.timings;
        let module = self.modules.entry(id.clone()).or_insert_with(|| Module {
            restarts: Restarts::new(timings),
            run: None,
            phase: Phase::Idle,
            last_error: None,
            turned_off,
        });
        // Turning a module off is the toggle that clears a failure; the
        // count says so even when this plan already turns it on again.
        if module.turned_off != turned_off {
            module.turned_off = turned_off;
            if matches!(module.phase, Phase::Failed { .. } | Phase::RestartAt(_)) {
                module.phase = Phase::Idle;
                module.restarts = Restarts::new(timings);
                module.last_error = None;
            }
        }
        if !enabled {
            if let Some(run) = module.run.as_mut() {
                stop(id, run);
            }
            return false;
        }
        if let Phase::Failed { fingerprint } = module.phase {
            if fingerprint == installed.fingerprint {
                return false;
            }
            info!(module = %id, "module folder changed: trying the failed module again");
            module.phase = Phase::Idle;
            module.restarts = Restarts::new(timings);
        }
        if let Some(run) = module.run.as_mut() {
            if run.stopping {
                return false;
            }
            if run.fingerprint != installed.fingerprint || Some(&run.settings) != settings {
                info!(module = %id, "module folder or settings changed: restarting it");
                stop(id, run);
                module.phase = Phase::Idle;
                return false;
            }
            let keys = Arc::new(keys);
            run.handle.keys.send_if_modified(|current| {
                if *current == keys {
                    return false;
                }
                *current = Arc::clone(&keys);
                true
            });
            return false;
        }
        !matches!(module.phase, Phase::RestartAt(at) if at > now)
    }

    /// Start a run of `installed` with its current settings and keys.
    fn start(&mut self, installed: Installed) {
        let Installed {
            id,
            folder,
            executable,
            fingerprint,
        } = installed;
        let settings = self
            .plan
            .config
            .get(&id)
            .map_or_else(toml::Table::new, |config| config.settings.clone());
        let keys = Arc::new(self.keys_of(&id));
        self.generation += 1;
        let generation = self.generation;
        info!(module = %id, generation, "starting module");
        let handle = child::start(
            RunSpec {
                id: id.clone(),
                folder,
                executable,
                settings: settings.clone(),
                timings: self.timings,
            },
            keys,
            EventSink {
                id: id.clone(),
                generation,
                events: self.run_events.clone(),
            },
        );
        if let Some(module) = self.modules.get_mut(&id) {
            module.phase = Phase::Idle;
            module.run = Some(LiveRun {
                handle,
                generation,
                ready: false,
                fingerprint,
                settings,
                stopping: false,
            });
        }
    }

    /// The earliest pending restart.
    fn next_restart(&self) -> Option<Instant> {
        self.modules
            .values()
            .filter(|module| module.run.is_none())
            .filter_map(|module| match module.phase {
                Phase::RestartAt(at) => Some(at),
                Phase::Idle | Phase::Failed { .. } => None,
            })
            .min()
    }

    /// Fold one event of run `generation` of `id` in. An event of a run that
    /// is no longer live is dropped.
    async fn run_event(&mut self, id: &ModuleId, generation: u64, event: RunEvent) {
        let Some(module) = self.modules.get_mut(id) else {
            return;
        };
        let Some(run) = module
            .run
            .as_mut()
            .filter(|run| run.generation == generation)
        else {
            return;
        };
        match event {
            RunEvent::Ready => {
                run.ready = true;
                module.last_error = None;
                info!(module = %id, "module ready");
            }
            RunEvent::Face(face) => {
                if run.stopping {
                    return;
                }
                if let Some(face) = self.keypads.to_config_keys(face) {
                    self.forward(face).await;
                }
            }
            RunEvent::Ended(end) => {
                let run = module.run.take();
                let stopping = run.as_ref().is_some_and(|run| run.stopping);
                let fingerprint = run.map_or(0, |run| run.fingerprint);
                module.phase = if stopping {
                    info!(module = %id, "module stopped");
                    Phase::Idle
                } else {
                    after_exit(id, module, &end, fingerprint)
                };
                self.forward(ModuleEvent::Stopped { module: id.clone() })
                    .await;
                if !self.runnable().any(|found| {
                    found
                        .manifest
                        .as_ref()
                        .is_ok_and(|manifest| manifest.id() == id)
                }) {
                    self.modules.remove(id);
                }
            }
        }
    }

    /// Hand `event` to the agent, unless it has stopped listening: then
    /// the event is dropped, and a module's end is told once it listens
    /// again.
    async fn forward(&mut self, event: ModuleEvent) {
        let stopped = match &event {
            ModuleEvent::Stopped { module } => Some(module.clone()),
            _ => None,
        };
        let sent = tokio::select! {
            biased;
            _ = self.listening.wait_for(|listening| !listening) => false,
            sent = self.events.send(event) => sent.is_ok(),
        };
        if !sent {
            debug!("agent not listening: module event dropped");
            if let Some(module) = stopped {
                self.untold_stops.insert(module);
            }
        }
    }

    /// Hand `press` to its module, when it runs and is ready.
    fn press(&mut self, press: ModulePress) {
        let ModulePress {
            module,
            device,
            slot,
        } = press;
        let device = self.keypads.name(&device);
        let run = self
            .modules
            .get(&module)
            .and_then(|module| module.run.as_ref())
            .filter(|run| run.ready && !run.stopping);
        if let Some(run) = run {
            if !run.handle.press(device, slot) {
                warn!(%module, "module is behind on presses: press dropped");
            }
        } else {
            debug!(%module, "press for a module that is not running: dropped");
        }
    }

    /// Stop every run and wait, within the grace and a second more, for
    /// each to end: told to leave, killed past its grace, and reaped.
    async fn stop_all(&mut self, run_rx: &mut mpsc::Receiver<(ModuleId, u64, RunEvent)>) {
        let mut live: Vec<(ModuleId, u64)> = Vec::new();
        for (id, module) in &mut self.modules {
            if let Some(run) = module.run.as_mut() {
                stop(id, run);
                live.push((id.clone(), run.generation));
            }
        }
        let deadline =
            Instant::now() + self.timings.shutdown_grace + std::time::Duration::from_secs(1);
        while !live.is_empty() {
            match tokio::time::timeout_at(deadline, run_rx.recv()).await {
                Ok(Some((id, generation, event))) => {
                    if matches!(event, RunEvent::Ended(_)) {
                        live.retain(|run| *run != (id.clone(), generation));
                    }
                    self.run_event(&id, generation, event).await;
                }
                Ok(None) | Err(_) => break,
            }
        }
    }

    /// Lift [`Self::stop_all`]'s hold: tell the agent which modules ended
    /// while it was not listening, so their images go, and let the modules
    /// start again.
    async fn resume(&mut self) {
        self.held = false;
        for module in std::mem::take(&mut self.untold_stops) {
            self.forward(ModuleEvent::Stopped { module }).await;
        }
    }

    /// Stop every run as [`Self::stop_all`] does, for good: the agent is
    /// leaving.
    async fn shut_down(&mut self, run_rx: &mut mpsc::Receiver<(ModuleId, u64, RunEvent)>) {
        self.stop_all(run_rx).await;
        // Dropping the handles closes every run task still around; their
        // processes are killed when dropped.
        self.modules.clear();
    }

    /// The status row of every folder, in folder order.
    fn statuses(&self) -> Vec<ModuleStatus> {
        self.found
            .iter()
            .map(|found| {
                let manifest = match &found.manifest {
                    Ok(manifest) => manifest,
                    Err(error) => {
                        return ModuleStatus {
                            folder: found.folder.clone(),
                            module: None,
                            enabled: false,
                            state: ModuleState::Invalid,
                            error: Some(error.clone()),
                        };
                    }
                };
                let id = manifest.id();
                let enabled = self
                    .plan
                    .config
                    .get(id)
                    .is_some_and(|config| config.enabled);
                let module = self.modules.get(id);
                let state = if !manifest.runs_here() {
                    ModuleState::Unsupported
                } else if !enabled {
                    ModuleState::Disabled
                } else {
                    match module {
                        Some(Module { run: Some(run), .. }) if run.stopping => {
                            ModuleState::Restarting
                        }
                        Some(Module { run: Some(run), .. }) if run.ready => ModuleState::Running,
                        Some(Module {
                            phase: Phase::RestartAt(_),
                            run: None,
                            ..
                        }) => ModuleState::Restarting,
                        Some(Module {
                            phase: Phase::Failed { .. },
                            run: None,
                            ..
                        }) => ModuleState::Failed,
                        _ => ModuleState::Starting,
                    }
                };
                ModuleStatus {
                    folder: found.folder.clone(),
                    module: Some(ModuleInfo {
                        id: id.clone(),
                        name: manifest.name().to_owned(),
                        description: manifest.description().map(str::to_owned),
                        version: manifest.version().map(str::to_owned),
                        platforms: manifest.platforms().map(str::to_owned).collect(),
                    }),
                    enabled,
                    state,
                    error: module.and_then(|module| module.last_error.clone()),
                }
            })
            .collect()
    }
}

fn stop(id: &ModuleId, run: &mut LiveRun) {
    if !run.stopping {
        debug!(module = %id, "stopping module");
        run.stopping = true;
        run.handle.stop();
    }
}

/// What follows a run of `id` that ended on its own, as `end` says.
fn after_exit(id: &ModuleId, module: &mut Module, end: &RunEnd, fingerprint: u64) -> Phase {
    let reason = end.error.clone().unwrap_or_else(|| "stopped".into());
    module.last_error = Some(reason.clone());
    match module.restarts.ended(end.ready_for) {
        AfterExit::RestartIn(delay) => {
            warn!(module = %id, %reason, delay_ms = delay.as_millis(), "module stopped: restarting");
            Phase::RestartAt(Instant::now() + delay)
        }
        AfterExit::Failed => {
            warn!(module = %id, %reason, "module failed to start five times in a row: leaving it stopped until it is turned off and on or its folder changes");
            Phase::Failed { fingerprint }
        }
    }
}
