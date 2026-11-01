//! Writes to a mouse's onboard flash, run one at a time and to their end.
//!
//! A flash write is unlike every other agent write. Rewriting a G700 profile
//! page takes seconds, during which the mouse plays its factory profile, and
//! abandoning it midway leaves the mouse there or on a half-written page. So
//! a flash write never runs under a requester's deadline and is never
//! cancelled: [`FlashWrites`] owns one long-running task that takes queued
//! jobs one at a time and awaits each to its end, and the agent's shutdown
//! waits for the job in progress before the device channels close — for up
//! to [`SETTLE_BUDGET`], so a job stuck before its first byte cannot keep the
//! agent from leaving.
//!
//! Requests are debounced: what a job writes is the latest state asked of the
//! device, so a burst of edits ([`FlashWrites::request`] per change) writes
//! the mouse once, [`DEBOUNCE`] after the last of them.
//!
//! The decisions live in [`FlashQueue`], a sans-I/O object driven with an
//! explicit `now`; the task around it only sleeps, runs jobs and publishes.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use openlogi_core::binding::{FlashBindingsState, FlashPlan, FlashWrite, SlotIntent};
use openlogi_hid::DeviceRoute;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{info, warn};

use crate::hardware::DeviceAccess;
use crate::runtime::hook::SharedHookMaps;

/// How long a request waits for the next before its job starts.
pub const DEBOUNCE: Duration = Duration::from_millis(1200);

/// How long the agent's exit or replacement waits for the job in progress.
///
/// A whole sync of a G700 that answers — the played page and its siblings
/// read, one page written and read back — takes seconds, and one that stops
/// answering fails at its first unanswered request (the HID++ response
/// timeout is 5 s). Only a job that never reached the mouse outlives this:
/// one still queued for the receiver lease or the transport's turn, where
/// leaving cuts no page short.
pub const SETTLE_BUDGET: Duration = Duration::from_secs(30);

/// What a job brings the device's flash to: the latest state the
/// configuration asks of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlashJob {
    /// What the configuration wants of each button slot, in layout order.
    pub bindings: Vec<SlotIntent>,
}

/// What a finished job found on the device and left there.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FlashOutcome {
    /// The directory index of the profile the mouse plays; `None` for its
    /// factory profile, which holds no bindings.
    pub profile: Option<u8>,
    /// Where the played profile's bindings are now.
    pub plan: FlashPlan,
}

/// Runs one job against one device. The production executor writes the G700
/// over the agent's channels; tests substitute their own.
pub trait FlashExecutor: Send + Sync + 'static {
    /// Bring `route`'s flash to what `job` asks, to the end, and report what
    /// it holds now, or why that failed.
    fn execute(
        &self,
        route: &DeviceRoute,
        job: &FlashJob,
    ) -> impl Future<Output = Result<FlashOutcome, String>> + Send;
}

/// The production executor: syncs a G700's bindings over the agent's own
/// channels, under the receiver lease and the transport's turn but no
/// deadline, and hands the hook the agent buttons the flash now presses.
pub struct G700FlashExecutor {
    /// The channels every device write goes through.
    pub device: DeviceAccess,
    /// Where the hook reads the agent buttons from.
    pub hook_maps: SharedHookMaps,
}

impl FlashExecutor for G700FlashExecutor {
    async fn execute(&self, route: &DeviceRoute, job: &FlashJob) -> Result<FlashOutcome, String> {
        let synced = self
            .device
            .op(route)
            .run_to_completion(|shared| async move {
                openlogi_hid::sync_g700_bindings_on(&shared, &job.bindings).await
            })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        if synced.written {
            info!(profile = ?synced.profile, "the G700's button bindings are written");
        }
        match self.hook_maps.write() {
            Ok(mut maps) => maps.flash_routes.clone_from(&synced.plan.routed),
            Err(error) => {
                warn!(%error, lock = "hook_maps", "lock poisoned — agent buttons not published");
            }
        }
        Ok(FlashOutcome {
            profile: synced.profile,
            plan: synced.plan,
        })
    }
}

/// One queued request: the job and when it may start.
#[derive(Debug)]
struct Queued {
    job: FlashJob,
    due: Instant,
}

/// The queue's decisions: which job runs next and when, one at a time, the
/// latest request per device replacing the one before it.
#[derive(Debug, Default)]
pub struct FlashQueue {
    queued: HashMap<DeviceRoute, Queued>,
    running: Option<DeviceRoute>,
}

impl FlashQueue {
    /// Ask that `route` be brought to `job`, [`DEBOUNCE`] from `now` unless
    /// another request for it follows. The latest request replaces a queued
    /// one; one asked while the device's job runs waits for that job.
    pub fn request(&mut self, route: DeviceRoute, job: FlashJob, now: Instant) {
        self.queued.insert(
            route,
            Queued {
                job,
                due: now + DEBOUNCE,
            },
        );
    }

    /// When the next job may start: `None` while one runs or nothing waits.
    #[must_use]
    pub fn next_due(&self) -> Option<Instant> {
        if self.running.is_some() {
            return None;
        }
        self.queued.values().map(|queued| queued.due).min()
    }

    /// Start the job that is due first at `now`, if any and if none runs.
    pub fn start_due(&mut self, now: Instant) -> Option<(DeviceRoute, FlashJob)> {
        if self.running.is_some() {
            return None;
        }
        let route = self
            .queued
            .iter()
            .filter(|(_, queued)| queued.due <= now)
            .min_by_key(|(_, queued)| queued.due)
            .map(|(route, _)| route.clone())?;
        let queued = self.queued.remove(&route)?;
        self.running = Some(route.clone());
        Some((route, queued.job))
    }

    /// The running job ended.
    pub fn finish(&mut self) {
        self.running = None;
    }

    /// Where `route`'s write stands as far as the queue knows: pending,
    /// writing, or neither.
    #[must_use]
    pub fn write_of(&self, route: &DeviceRoute) -> Option<FlashWrite> {
        if self.running.as_ref() == Some(route) {
            Some(FlashWrite::Writing)
        } else if self.queued.contains_key(route) {
            Some(FlashWrite::Pending)
        } else {
            None
        }
    }

    /// Drop every request that has not started, as the agent leaves. The
    /// configuration still holds them, so the next agent writes them.
    pub fn drop_queued(&mut self) -> usize {
        let dropped = self.queued.len();
        self.queued.clear();
        dropped
    }
}

/// What a device's last job left, or why it failed.
#[derive(Debug)]
enum Settled {
    Done(FlashOutcome),
    Failed {
        error: String,
        last: Option<FlashOutcome>,
    },
}

#[derive(Debug, Default)]
struct Book {
    queue: FlashQueue,
    settled: HashMap<DeviceRoute, Settled>,
    /// No job starts while held: a replacement is draining the channels.
    held: bool,
    closing: bool,
}

#[derive(Debug)]
struct Shared {
    book: Mutex<Book>,
    wake: Notify,
    /// Whether a job runs, for [`FlashWrites::hold`] to wait on.
    running: watch::Sender<bool>,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            book: Mutex::default(),
            wake: Notify::new(),
            running: watch::Sender::new(false),
        }
    }
}

impl Shared {
    fn book(&self) -> MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The agent's flash-write task. Cloning shares it.
///
/// Requests made before [`Self::start`] wait for it, so the orchestrator can
/// request writes before (or without) a runtime.
#[derive(Clone, Debug, Default)]
pub struct FlashWrites {
    shared: Arc<Shared>,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl FlashWrites {
    /// Ask that `route`'s flash be brought to `job` (see [`FlashQueue::request`]).
    /// Refused once the agent is leaving.
    pub fn request(&self, route: DeviceRoute, job: FlashJob) {
        {
            let mut book = self.shared.book();
            if book.closing {
                return;
            }
            book.queue.request(route, job, Instant::now());
        }
        self.shared.wake.notify_one();
    }

    /// What `route`'s flash bindings were last found to be, and where its
    /// write stands. A device no job has run for yet reads as default with
    /// the write's queue state.
    #[must_use]
    pub fn state(&self, route: &DeviceRoute) -> FlashBindingsState {
        let book = self.shared.book();
        let (outcome, failure) = match book.settled.get(route) {
            Some(Settled::Done(outcome)) => (Some(outcome), None),
            Some(Settled::Failed { error, last }) => (last.as_ref(), Some(error)),
            None => (None, None),
        };
        let write = book
            .queue
            .write_of(route)
            .or_else(|| failure.map(|error| FlashWrite::Failed(error.clone())))
            .unwrap_or_default();
        let outcome = outcome.cloned().unwrap_or_default();
        FlashBindingsState {
            profile: outcome.profile,
            slots: outcome.plan.bindings,
            routed: outcome.plan.routed,
            unrouted: outcome.plan.unrouted,
            write,
        }
    }

    /// Start the task on the current runtime with `executor`. A second call
    /// is ignored.
    pub fn start<E: FlashExecutor>(&self, executor: E) {
        let mut task = self.task.lock().unwrap_or_else(PoisonError::into_inner);
        if task.is_some() {
            return;
        }
        *task = Some(tokio::spawn(run(Arc::clone(&self.shared), executor)));
    }

    /// Start no job until [`Self::resume`], and wait for the one in
    /// progress to end, for up to [`SETTLE_BUDGET`]. Requests keep queueing
    /// meanwhile. For a replacement of the agent, which drains the device
    /// channels a job writes through but may yet be called off.
    pub async fn hold(&self) {
        self.shared.book().held = true;
        let mut running = self.shared.running.subscribe();
        match tokio::time::timeout(SETTLE_BUDGET, running.wait_for(|running| !*running)).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => warn!("flash-write task gone while holding it"),
            Err(_) => warn!(
                budget = ?SETTLE_BUDGET,
                "the flash write in progress outlived the replacement's wait"
            ),
        }
    }

    /// Let jobs start again after [`Self::hold`].
    pub fn resume(&self) {
        self.shared.book().held = false;
        self.shared.wake.notify_one();
    }

    /// Refuse new requests, drop the ones not started, and wait for the job
    /// in progress to end, for up to [`SETTLE_BUDGET`]. Call while the device
    /// channels are still open.
    pub async fn shutdown(&self) {
        let dropped = {
            let mut book = self.shared.book();
            book.closing = true;
            book.queue.drop_queued()
        };
        if dropped > 0 {
            info!(
                dropped,
                "flash writes not started are left to the next agent"
            );
        }
        self.shared.wake.notify_one();
        let task = self
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(task) = task else {
            return;
        };
        match tokio::time::timeout(SETTLE_BUDGET, task).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => warn!(%error, "flash-write task ended abnormally"),
            Err(_) => warn!(
                budget = ?SETTLE_BUDGET,
                "the flash write in progress outlived the agent's exit wait"
            ),
        }
    }
}

/// The task: wait for a due job, run it to its end, publish, repeat.
async fn run<E: FlashExecutor>(shared: Arc<Shared>, executor: E) {
    loop {
        let next = {
            let mut book = shared.book();
            if book.closing {
                return;
            }
            if book.held {
                Err(None)
            } else if let Some(job) = book.queue.start_due(Instant::now()) {
                shared.running.send_replace(true);
                Ok(job)
            } else {
                Err(book.queue.next_due())
            }
        };
        match next {
            Ok((route, job)) => {
                // Awaited here, in the task's own body: nothing selects
                // against it, so nothing can drop it before it ends.
                let result = executor.execute(&route, &job).await;
                let mut book = shared.book();
                book.queue.finish();
                let last = match book.settled.remove(&route) {
                    Some(Settled::Done(outcome)) => Some(outcome),
                    Some(Settled::Failed { last, .. }) => last,
                    None => None,
                };
                let settled = match result {
                    Ok(outcome) => Settled::Done(outcome),
                    Err(error) => {
                        warn!(%error, "flash write failed");
                        Settled::Failed { error, last }
                    }
                };
                book.settled.insert(route, settled);
                shared.running.send_replace(false);
            }
            Err(Some(due)) => {
                tokio::select! {
                    () = shared.wake.notified() => {}
                    () = tokio::time::sleep_until(due) => {}
                }
            }
            Err(None) => shared.wake.notified().await,
        }
    }
}

#[cfg(test)]
mod tests;
