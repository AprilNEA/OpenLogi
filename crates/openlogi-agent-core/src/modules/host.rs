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

use self::life::{LifeEvent, LiveRun, Module, ModuleLife};
use super::child::{self, EventSink, RunEvent, RunSpec};
use super::discovery::{self, Found};
use super::{
    HostCommand, ModuleEvent, ModuleKey, ModulePlan, ModulePress, ModuleTimings, SharedModulePlan,
};
use crate::watchers::retry::wait_for_deadline;

mod life;

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

/// An installed module that can run here.
struct Installed {
    id: ModuleId,
    folder: PathBuf,
    executable: PathBuf,
    fingerprint: u64,
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
        let mut host = Supervisor::new(
            dir,
            timings,
            Arc::clone(&plan.borrow_and_update()),
            run_events,
            events,
            listening,
        );
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
                        host.hold(&mut run_rx).await;
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
    fn new(
        dir: Option<PathBuf>,
        timings: ModuleTimings,
        plan: Arc<ModulePlan>,
        run_events: mpsc::Sender<(ModuleId, u64, RunEvent)>,
        events: mpsc::Sender<ModuleEvent>,
        listening: watch::Receiver<bool>,
    ) -> Self {
        Self {
            dir,
            timings,
            found: Vec::new(),
            plan,
            modules: BTreeMap::new(),
            generation: 0,
            run_events,
            events,
            listening,
            untold_stops: BTreeSet::new(),
            held: false,
            keypads: KeypadNames::default(),
        }
    }

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
        self.stop_gone(&installed, now);
        for module in installed {
            if self.due(&module, now) {
                self.start(module, now);
            }
        }
    }

    /// Stop each module whose folder is gone, invalid, or no longer for
    /// this platform, and forget it once its run has ended.
    fn stop_gone(&mut self, installed: &[Installed], now: Instant) {
        self.modules.retain(|id, module| {
            if installed.iter().any(|installed| installed.id == *id) {
                return true;
            }
            module.step(id, LifeEvent::Stop, now).is_ok()
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
        let module = self
            .modules
            .entry(id.clone())
            .or_insert_with(|| Module::new(timings, turned_off));
        if module.saw_turned_off(turned_off) {
            settle(id, module, LifeEvent::TurnedOff, now);
        }
        if !enabled {
            if module.run().is_some() {
                settle(id, module, LifeEvent::Stop, now);
            }
            return false;
        }
        settle(
            id,
            module,
            LifeEvent::Folder {
                fingerprint: installed.fingerprint,
            },
            now,
        );
        if module.stopping() {
            return false;
        }
        if let Some(run) = module.run() {
            if run.fingerprint != installed.fingerprint || Some(&run.settings) != settings {
                info!(module = %id, "module folder or settings changed: restarting it");
                settle(id, module, LifeEvent::Stop, now);
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
        module.due(now)
    }

    /// Start a run of `installed` with its current settings and keys.
    fn start(&mut self, installed: Installed, now: Instant) {
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
            let run = LiveRun {
                handle,
                generation,
                fingerprint,
                settings,
            };
            settle(&id, module, LifeEvent::Started(run), now);
        }
    }

    /// The earliest pending restart. A held host has none: it starts
    /// nothing until it resumes.
    fn next_restart(&self) -> Option<Instant> {
        if self.held {
            return None;
        }
        self.modules.values().filter_map(Module::restart_at).min()
    }

    /// Fold one event of run `generation` of `id` in. An event of a run that
    /// is no longer live is dropped.
    async fn run_event(&mut self, id: &ModuleId, generation: u64, event: RunEvent) {
        let Some(module) = self.modules.get_mut(id) else {
            return;
        };
        match event {
            RunEvent::Ready => settle(id, module, LifeEvent::Ready { generation }, Instant::now()),
            RunEvent::Face(face) => {
                if !module.takes_faces(generation) {
                    return;
                }
                if let Some(face) = self.keypads.to_config_keys(face) {
                    self.forward(face).await;
                }
            }
            RunEvent::Ended(end) => {
                if let Err(refused) =
                    module.step(id, LifeEvent::Ended { generation, end }, Instant::now())
                {
                    debug!(module = %id, %refused, "end of a run that is no longer live: dropped");
                    return;
                }
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
        let run = self.modules.get(&module).and_then(Module::running);
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
        let now = Instant::now();
        let mut live: Vec<(ModuleId, u64)> = Vec::new();
        for (id, module) in &mut self.modules {
            if let Some(generation) = module.run().map(|run| run.generation) {
                settle(id, module, LifeEvent::Stop, now);
                live.push((id.clone(), generation));
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

    /// Stop every run as [`Self::stop_all`] does, and start none until
    /// [`Self::resume`].
    async fn hold(&mut self, run_rx: &mut mpsc::Receiver<(ModuleId, u64, RunEvent)>) {
        self.stop_all(run_rx).await;
        self.held = true;
    }

    /// Lift [`Self::hold`]'s hold: tell the agent which modules ended
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
                    match module.map(Module::life) {
                        Some(ModuleLife::Running { .. }) => ModuleState::Running,
                        Some(ModuleLife::Stopping { .. } | ModuleLife::Waiting { .. }) => {
                            ModuleState::Restarting
                        }
                        Some(ModuleLife::Failed { .. }) => ModuleState::Failed,
                        Some(ModuleLife::Idle { .. } | ModuleLife::Starting { .. }) | None => {
                            ModuleState::Starting
                        }
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
                    error: module.and_then(Module::error).map(str::to_owned),
                }
            })
            .collect()
    }
}

/// Apply `event` to `module`, which the caller has checked takes it; a
/// refusal is logged and changes nothing.
fn settle(id: &ModuleId, module: &mut Module, event: LifeEvent, now: Instant) {
    if let Err(refused) = module.step(id, event, now) {
        debug!(module = %id, %refused, "module event refused");
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::modules::child::{RunEnd, RunHandle};

    /// A host stopped while a module waits out its restart delay has no
    /// deadline to wake for, even once the delay has passed: it would
    /// otherwise wake at once, start nothing, and wake again without end.
    #[tokio::test(start_paused = true)]
    async fn a_held_host_waits_for_no_restart() {
        let timings = ModuleTimings::default();
        let (run_events, mut run_rx) = mpsc::channel(1);
        let (events, _events_rx) = mpsc::channel(1);
        let (_listening, listening_rx) = watch::channel(true);
        let mut host = Supervisor::new(
            None,
            timings,
            Arc::default(),
            run_events,
            events,
            listening_rx,
        );
        let id = ModuleId::try_new("com.example.test".to_owned()).unwrap();
        let mut module = Module::new(timings, 0);
        let started = Instant::now();
        let run = LiveRun {
            handle: RunHandle::detached(),
            generation: 1,
            fingerprint: 0,
            settings: toml::Table::new(),
        };
        module.step(&id, LifeEvent::Started(run), started).unwrap();
        let end = RunEnd {
            ready_for: None,
            error: Some("exited with status 3".into()),
        };
        module
            .step(&id, LifeEvent::Ended { generation: 1, end }, started)
            .unwrap();
        host.modules.insert(id, module);
        let restart = started + timings.first_restart;
        assert_eq!(host.next_restart(), Some(restart));

        tokio::time::timeout(Duration::from_secs(5), host.hold(&mut run_rx))
            .await
            .expect("the host holds in time");
        tokio::time::advance(timings.first_restart + Duration::from_secs(1)).await;
        assert!(Instant::now() > restart, "the old deadline has passed");
        assert_eq!(host.next_restart(), None, "a held host wakes for nothing");

        tokio::time::timeout(Duration::from_secs(5), host.resume())
            .await
            .expect("the host resumes in time");
        assert_eq!(
            host.next_restart(),
            Some(restart),
            "it is due again on resume"
        );
    }
}
