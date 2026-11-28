//! The key-module host: runs the modules the user installed and turned on,
//! and carries their keys, faces, and presses.
//!
//! A module is a program in its own folder under the modules directory,
//! described by a `module.toml` ([`openlogi_core::module`]). The host finds
//! the folders (`discovery`), starts every installed module that is turned
//! on and lists this platform as a child process, and speaks the protocol of
//! `docs/MODULES.md` over its stdin and stdout (`protocol`, `child`): it
//! tells the module which keypad keys it owns, hands it their presses, and
//! passes the images it sends on to the orchestrator, which owns what every
//! key shows. A module that exits is started again after a doubling delay
//! (`restart`); one that keeps failing to start stays stopped.
//!
//! The orchestrator decides what each module owns and publishes it as a
//! [`ModulePlan`]; the host owns the processes and publishes where each one
//! stands as [`ModuleStatus`] rows. Images flow back as [`ModuleEvent`]s
//! through the agent's control loop, which hands them to the orchestrator.

mod child;
mod discovery;
mod host;
mod protocol;
mod restart;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use openlogi_core::binding::GridSlot;
use openlogi_core::config::ModuleConfig;
use openlogi_core::module::ModuleId;
use openlogi_ipc::ModuleStatus;
use tokio::sync::{mpsc, oneshot, watch};
use tracing::debug;

use crate::watchers::key_display::KeyImage;

pub use host::ModuleHost;

/// One keypad key a module owns: what its `keys` message lists.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ModuleKey {
    /// The keypad, by its config key. A module never sees it: the host
    /// names each keypad `keypad-1`, `keypad-2`, and so on to modules.
    pub device: String,
    /// The key and the page it is on.
    pub slot: GridSlot,
    /// The key's side in pixels.
    pub size: u16,
}

/// What the orchestrator wants the host to run, and with which keys.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModulePlan {
    /// Every module `config.toml` names: whether it is on, and its settings.
    pub config: BTreeMap<ModuleId, ModuleConfig>,
    /// The keys each module owns now: the keys of every connected keypad
    /// whose binding on its current page, for the app in front, is that
    /// module. A module with no keys has no entry.
    pub keys: BTreeMap<ModuleId, Vec<ModuleKey>>,
    /// How many times the agent has seen each module turned off. A change
    /// is the toggle that clears a failed module, even when the off and the
    /// on reach the host as one plan.
    pub turned_off: BTreeMap<ModuleId, u64>,
}

/// Read-only, coalescing view of the published module plan.
pub type SharedModulePlan = watch::Receiver<Arc<ModulePlan>>;

/// A face change one module asked for, or the end of its run, for the
/// orchestrator to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleEvent {
    /// Show `image` on one key.
    Image {
        /// The module that sent it.
        module: ModuleId,
        /// The keypad, by config key.
        device: String,
        /// The key.
        slot: GridSlot,
        /// The decoded image.
        image: KeyImage,
    },
    /// Show `image` on every key the module owns.
    ImageAll {
        /// The module that sent it.
        module: ModuleId,
        /// The decoded image.
        image: KeyImage,
    },
    /// Show the module glyph on one key again.
    Clear {
        /// The module that sent it.
        module: ModuleId,
        /// The keypad, by config key.
        device: String,
        /// The key.
        slot: GridSlot,
    },
    /// The module's process ended: every image it sent is dropped.
    Stopped {
        /// The module.
        module: ModuleId,
    },
}

/// A press of a key a module owns, for the host to deliver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModulePress {
    /// The module the key is bound to.
    pub module: ModuleId,
    /// The keypad, by config key.
    pub device: String,
    /// The key, on the page that was current when it went down.
    pub slot: GridSlot,
}

/// The host's clock: how long each step of a module's life may take. The
/// defaults are the protocol's; tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleTimings {
    /// How long a started module has to answer `hello` with `ready`.
    pub ready_timeout: Duration,
    /// How long a module told to shut down has to exit before it is killed.
    pub shutdown_grace: Duration,
    /// The delay before the first restart after an exit.
    pub first_restart: Duration,
    /// The longest delay between restarts.
    pub max_restart: Duration,
    /// How long a module must run after `ready` for its restart delay to
    /// start over from [`Self::first_restart`].
    pub healthy_after: Duration,
    /// How often the modules folder is scanned again, besides on every
    /// config reload.
    pub rescan_every: Duration,
}

impl Default for ModuleTimings {
    fn default() -> Self {
        Self {
            ready_timeout: Duration::from_secs(10),
            shutdown_grace: Duration::from_secs(2),
            first_restart: Duration::from_secs(1),
            max_restart: Duration::from_secs(60),
            healthy_after: Duration::from_secs(60),
            rescan_every: Duration::from_secs(10),
        }
    }
}

/// Requests the agent makes of a running host.
#[derive(Debug)]
enum HostCommand {
    Press(ModulePress),
    Rescan,
    /// Stop every module and start none until [`HostCommand::Resume`].
    StopAll(oneshot::Sender<()>),
    Resume,
    Shutdown(oneshot::Sender<()>),
}

/// How many commands may wait for the host. Presses beyond it are dropped,
/// not queued without bound.
const COMMAND_QUEUE: usize = 64;

/// The agent's handle on the module host. Cheap to clone; usable before the
/// host runs, when commands wait for it and the status list is empty.
#[derive(Clone)]
pub struct ModuleHostHandle {
    commands: mpsc::Sender<HostCommand>,
    statuses: watch::Receiver<Arc<Vec<ModuleStatus>>>,
    /// Whether the agent drains module events. Cleared before it stops the
    /// modules, so a host waiting to hand it an event gives up and hears
    /// the command.
    listening: Arc<watch::Sender<bool>>,
}

impl ModuleHostHandle {
    /// Build a host for the modules under `dir` (none when `None`), and the
    /// handle that drives it. The host does nothing until [`ModuleHost::run`].
    #[must_use]
    pub fn new(dir: Option<PathBuf>, timings: ModuleTimings) -> (Self, ModuleHost) {
        let (commands, command_rx) = mpsc::channel(COMMAND_QUEUE);
        let (status_tx, statuses) = watch::channel(Arc::new(Vec::new()));
        let (listening, listening_rx) = watch::channel(true);
        (
            Self {
                commands,
                statuses,
                listening: Arc::new(listening),
            },
            ModuleHost::new(dir, timings, command_rx, status_tx, listening_rx),
        )
    }

    /// Deliver `press` to its module. Dropped when the module is not
    /// running, or when the host is too far behind to queue it.
    pub fn press(&self, press: ModulePress) {
        if self.commands.try_send(HostCommand::Press(press)).is_err() {
            debug!("module host busy or stopped: press dropped");
        }
    }

    /// Scan the modules folder again now. A scan already waiting covers it.
    pub fn rescan(&self) {
        let _ = self.commands.try_send(HostCommand::Rescan);
    }

    /// Every module folder and where its module stands, in folder order.
    #[must_use]
    pub fn statuses(&self) -> Vec<ModuleStatus> {
        self.statuses.borrow().as_ref().clone()
    }

    /// A receiver that wakes on every status change.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Arc<Vec<ModuleStatus>>> {
        self.statuses.clone()
    }

    /// Tell every module to shut down, kill those that do not leave within
    /// their grace, and stop the host. Returns once the host has, or after
    /// `bound` when the host does not answer (it never ran, or it is gone).
    ///
    /// The agent stops draining module events from here on.
    pub async fn shutdown(&self, bound: Duration) {
        self.listening.send_replace(false);
        let (ack, done) = oneshot::channel();
        self.ask(HostCommand::Shutdown(ack), done, bound).await;
    }

    /// Stop every module as [`Self::shutdown`] does, sending each
    /// `shutdown`, killing and reaping those that overstay their grace, but
    /// keep the host: it starts none until [`Self::resume`]. Returns once
    /// every module is gone, or after `bound`.
    ///
    /// The agent stops draining module events until it resumes.
    pub async fn stop_all(&self, bound: Duration) {
        self.listening.send_replace(false);
        let (ack, done) = oneshot::channel();
        self.ask(HostCommand::StopAll(ack), done, bound).await;
    }

    /// Drain module events again and start the modules [`Self::stop_all`]
    /// stopped.
    pub async fn resume(&self) {
        self.listening.send_replace(true);
        let _ = self.commands.send(HostCommand::Resume).await;
    }

    async fn ask(&self, command: HostCommand, done: oneshot::Receiver<()>, bound: Duration) {
        let answered = async {
            if self.commands.send(command).await.is_ok() {
                let _ = done.await;
            }
        };
        let _ = tokio::time::timeout(bound, answered).await;
    }
}

#[cfg(test)]
mod tests;
