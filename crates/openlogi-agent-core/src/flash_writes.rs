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
//! Each onboard profile keeps its own bindings: a job writes only the
//! profile the mouse plays, and only when that profile's configuration
//! changed since the agent last applied it ([`records`]). A profile switch
//! writes nothing; it only tells the hook which agent buttons the played
//! profile holds.
//!
//! The decisions live in [`FlashQueue`], a sans-I/O object driven with an
//! explicit `now`; the task around it only sleeps, runs jobs and publishes.

pub mod records;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use openlogi_core::binding::{
    FlashBindingsState, FlashPlan, FlashSync, FlashWrite, MouseButtonNumber, ProfileRequest,
    SlotIntent,
};
use openlogi_core::hid::{FlashProfileEdit, ReportRate};
use openlogi_hid::{DeviceRoute, G700DpiSave, G700Job};
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{info, warn};

use self::records::FlashRecords;
use crate::hardware::DeviceAccess;
use crate::runtime::hook::{FlashRoute, SharedHookMaps};

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

/// How long a failed job waits before its one retry. A G700 behind a busy
/// or flaky receiver fails a request now and then; a second try a moment
/// later usually goes through, and a second failure is left to the next
/// configuration change or reconnect (see [`FlashWrites::failed`]).
pub const RETRY_DELAY: Duration = Duration::from_secs(2);

/// What a flash mouse's configuration asks of the buttons of each of its
/// onboard profiles.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlashBindingsRequest {
    /// The configuration entry the mouse is stored under, which the record
    /// of the agent's own writes is kept by.
    pub owner: String,
    /// What each profile's configuration wants of its slots, in layout
    /// order, by the flash page the profile is stored on.
    pub pages: BTreeMap<u8, Vec<SlotIntent>>,
}

/// What one request asks of a device's flash. Requests for one device merge
/// while they wait ([`Self::merge`]), and the job that runs brings the flash
/// to all of them at once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlashJob {
    /// What the configuration wants of each profile's button slots; `None`
    /// keeps what the device was last asked.
    pub bindings: Option<FlashBindingsRequest>,
    /// Profile edits, in the order asked.
    pub edits: VecDeque<FlashProfileEdit>,
    /// A live DPI change to save to the played profile.
    pub dpi: Option<G700DpiSave>,
    /// A live report rate to save to the played profile.
    pub rate: Option<ReportRate>,
}

impl FlashJob {
    /// Bring each profile's button slots to what `request` asks.
    #[must_use]
    pub fn bindings(request: FlashBindingsRequest) -> Self {
        Self {
            bindings: Some(request),
            ..Self::default()
        }
    }

    /// Apply one profile edit.
    #[must_use]
    pub fn edit(edit: FlashProfileEdit) -> Self {
        Self {
            edits: VecDeque::from([edit]),
            ..Self::default()
        }
    }

    /// Save a live DPI change.
    #[must_use]
    pub fn dpi(save: G700DpiSave) -> Self {
        Self {
            dpi: Some(save),
            ..Self::default()
        }
    }

    /// Save a live report rate.
    #[must_use]
    pub fn rate(rate: ReportRate) -> Self {
        Self {
            rate: Some(rate),
            ..Self::default()
        }
    }

    /// Fold `later` into this request: its bindings and rate replace these,
    /// its edits follow these, and a DPI save keeps this one's starting
    /// value with its own result — the mode the mouse was in before the
    /// first change is the one that takes the last.
    pub fn merge(&mut self, later: Self) {
        if later.bindings.is_some() {
            self.bindings = later.bindings;
        }
        self.edits.extend(later.edits);
        self.dpi = match (self.dpi, later.dpi) {
            (Some(first), Some(last)) => Some(G700DpiSave {
                from: first.from,
                to: last.to,
            }),
            (first, last) => last.or(first),
        };
        self.rate = later.rate.or(self.rate);
    }
}

/// What a finished job found on the device and left there.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FlashOutcome {
    /// The directory index of the profile the mouse plays; `None` for its
    /// factory profile, which holds no bindings.
    pub profile: Option<u8>,
    /// The flash page of that profile; `None` exactly when
    /// [`Self::profile`] is.
    pub page: Option<u8>,
    /// Where the played profile's bindings are now.
    pub plan: FlashPlan,
}

/// Runs one job against one device. The production executor writes the G700
/// over the agent's channels; tests substitute their own.
pub trait FlashExecutor: Send + Sync + 'static {
    /// Bring `route`'s flash to what `job` asks, to the end, and report what
    /// it holds now, or why that failed.
    /// Profile edits are removed from `job` as they complete, so a job that
    /// failed holds the edit that failed first.
    fn execute(
        &self,
        route: &DeviceRoute,
        job: &mut G700Job,
    ) -> impl Future<Output = Result<FlashOutcome, String>> + Send;
}

/// The production executor: syncs a G700's bindings over the agent's own
/// channels, under the receiver lease and the transport's turn but no
/// deadline, and hands the hook the agent buttons the flash now presses —
/// the played profile's, and those the agent gave every other profile, so a
/// profile switch on the mouse that nothing has noticed yet still finds its
/// buttons routed. Agent buttons are distinct across profiles
/// ([`openlogi_core::binding::flash::plan_flash_bindings`]), so each names
/// one slot of one profile.
pub struct G700FlashExecutor {
    /// The channels every device write goes through.
    pub device: DeviceAccess,
    /// Where the hook reads the agent buttons from.
    pub hook_maps: SharedHookMaps,
}

impl FlashExecutor for G700FlashExecutor {
    async fn execute(
        &self,
        route: &DeviceRoute,
        job: &mut G700Job,
    ) -> Result<FlashOutcome, String> {
        let requests = job.bindings.clone();
        // Boxed: a sync reads and writes several pages, and its state
        // machine is too large to keep on the caller's stack.
        let synced = self
            .device
            .op(route)
            .run_to_completion(|shared| async move {
                Box::pin(openlogi_hid::sync_g700_on(&shared, job)).await
            })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        if synced.written {
            info!(profile = ?synced.profile, page = ?synced.page, "the G700's flash is written");
        }
        let routes = hook_routes(&requests, synced.page, &synced.plan);
        match self.hook_maps.write() {
            Ok(mut maps) => maps.flash_routes = routes,
            Err(error) => {
                warn!(%error, lock = "hook_maps", "lock poisoned — agent buttons not published");
            }
        }
        Ok(FlashOutcome {
            profile: synced.profile,
            page: synced.page,
            plan: synced.plan,
        })
    }
}

/// The agent buttons the hook routes after a job that found the mouse on
/// `played`: those the agent gave the other profiles, then the played
/// profile's as it holds them now, which win a button both press — a copy
/// of a profile not yet written on its own.
fn hook_routes(
    requests: &BTreeMap<u8, ProfileRequest>,
    played: Option<u8>,
    plan: &FlashPlan,
) -> BTreeMap<MouseButtonNumber, FlashRoute> {
    let mut routes: BTreeMap<_, _> = requests
        .iter()
        .filter(|(page, _)| Some(**page) != played)
        .flat_map(|(&page, request)| {
            request
                .owned
                .iter()
                .map(move |(&button, &slot)| (button, FlashRoute { page, slot }))
        })
        .collect();
    if let Some(page) = played {
        routes.extend(
            plan.routed
                .iter()
                .map(|(&button, &slot)| (button, FlashRoute { page, slot })),
        );
    }
    routes
}

/// One queued request: the job and when it may start.
#[derive(Debug)]
struct Queued {
    job: FlashJob,
    due: Instant,
}

/// The queue's decisions: which job runs next and when, one at a time, the
/// requests per device merging into one.
#[derive(Debug, Default)]
pub struct FlashQueue {
    queued: HashMap<DeviceRoute, Queued>,
    running: Option<DeviceRoute>,
    /// The bindings each device was last asked for, which a job of a
    /// request that names none syncs to.
    asked: HashMap<DeviceRoute, FlashBindingsRequest>,
    /// What the agent last applied to each profile, which decides what a
    /// job may write.
    records: FlashRecords,
}

impl FlashQueue {
    /// Ask that `route` be brought to `job`, [`DEBOUNCE`] from `now` unless
    /// another request for it follows. A request merges into a queued one
    /// ([`FlashJob::merge`]); one asked while the device's job runs waits
    /// for that job.
    pub fn request(&mut self, route: DeviceRoute, job: FlashJob, now: Instant) {
        if let Some(request) = &job.bindings {
            self.asked.insert(route.clone(), request.clone());
        }
        let due = now + DEBOUNCE;
        match self.queued.get_mut(&route) {
            Some(queued) => {
                queued.job.merge(job);
                queued.due = due;
            }
            None => {
                self.queued.insert(route, Queued { job, due });
            }
        }
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
    pub fn start_due(&mut self, now: Instant) -> Option<(DeviceRoute, G700Job)> {
        if self.running.is_some() {
            return None;
        }
        let route = self
            .queued
            .iter()
            .filter(|(_, queued)| queued.due <= now)
            .min_by_key(|(_, queued)| queued.due)
            .map(|(route, _)| route.clone())?;
        let FlashJob {
            bindings,
            edits,
            dpi,
            rate,
        } = self.queued.remove(&route)?.job;
        let bindings = bindings
            .or_else(|| self.asked.get(&route).cloned())
            .map(|asked| self.records.requests(&asked.owner, &asked.pages))
            .unwrap_or_default();
        self.running = Some(route.clone());
        Some((
            route,
            G700Job {
                edits,
                dpi,
                rate,
                bindings,
            },
        ))
    }

    /// The running job ended.
    pub fn finish(&mut self) {
        self.running = None;
    }

    /// Record what `job`, run for `route`, applied: the played profile it
    /// found (`outcome`) holds what its configuration asked, if the job was
    /// to write it. Returns whether the record changed.
    pub fn record(&mut self, route: &DeviceRoute, job: &G700Job, outcome: &FlashOutcome) -> bool {
        let Some(owner) = self.asked.get(route).map(|asked| asked.owner.clone()) else {
            return false;
        };
        let Some(page) = outcome.page else {
            return false;
        };
        match job.bindings.get(&page) {
            Some(request) if request.sync == FlashSync::Write => self.records.apply(
                &owner,
                page,
                request.intents.clone(),
                outcome.plan.routed.clone(),
            ),
            _ => false,
        }
    }

    /// Start from the record `records`, as read back from its file.
    pub fn restore_records(&mut self, records: FlashRecords) {
        self.records = records;
    }

    /// What the agent last applied to each profile.
    #[must_use]
    pub fn records(&self) -> &FlashRecords {
        &self.records
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
    /// Where the queue's record of applied profiles is kept between runs.
    records_file: Option<PathBuf>,
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
    /// Keep the record of what each profile was last brought to in `path`,
    /// starting from what it holds. A record that does not read is
    /// replaced: the next change of each profile's configuration writes it
    /// again, and its agent buttons are only taken back once known.
    pub fn keep_records_in(&self, path: PathBuf) {
        let records = FlashRecords::load(&path).unwrap_or_else(|error| {
            warn!(%error, path = %path.display(), "the flash write record did not read — starting afresh");
            FlashRecords::default()
        });
        let mut book = self.shared.book();
        book.queue.restore_records(records);
        book.records_file = Some(path);
    }

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
            page: outcome.page,
        }
    }

    /// Whether `route`'s last job failed, its retry too: the next request
    /// of the configuration's bindings should be made even if they did not
    /// change.
    #[must_use]
    pub fn failed(&self, route: &DeviceRoute) -> bool {
        matches!(
            self.shared.book().settled.get(route),
            Some(Settled::Failed { .. })
        )
    }

    /// The directory index of the profile `route` played when its last job
    /// ended — `Some(None)` for its factory profile — or `None` while no job
    /// has found out, or one is queued or running that will.
    #[must_use]
    pub fn played(&self, route: &DeviceRoute) -> Option<Option<u8>> {
        let book = self.shared.book();
        if book.queue.write_of(route).is_some() {
            return None;
        }
        match book.settled.get(route)? {
            Settled::Done(outcome) => Some(outcome.profile),
            Settled::Failed { last, .. } => last.as_ref().map(|outcome| outcome.profile),
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

/// Runs `job` and, should it fail, once more after [`RETRY_DELAY`] unless
/// the agent is leaving. A profile edit that failed is not tried again — it
/// may have half happened, and the edits after it were asked of a state it
/// did not reach — so the retry carries the job's remaining state: the
/// pointer saves and the bindings. Its error is still the one reported.
///
/// The record is [`Settled`]: a failure keeps what the last attempt found,
/// if it got that far.
async fn run_with_retry<E: FlashExecutor>(
    shared: &Shared,
    executor: &E,
    route: &DeviceRoute,
    job: &mut G700Job,
) -> Settled {
    let error = match executor.execute(route, job).await {
        Ok(outcome) => return Settled::Done(outcome),
        Err(error) => error,
    };
    let failed_edit = !job.edits.is_empty();
    if failed_edit {
        warn!(%error, dropped = job.edits.len(), "a profile edit failed — not retried");
        job.edits.clear();
    } else {
        warn!(%error, "flash write failed — retrying once");
    }
    tokio::time::sleep(RETRY_DELAY).await;
    if shared.book().closing {
        return Settled::Failed { error, last: None };
    }
    match executor.execute(route, job).await {
        Ok(outcome) if failed_edit => Settled::Failed {
            error,
            last: Some(outcome),
        },
        Ok(outcome) => Settled::Done(outcome),
        Err(error) => Settled::Failed { error, last: None },
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
            Ok((route, mut job)) => {
                // Awaited here, in the task's own body: nothing selects
                // against it, so nothing can drop it before it ends.
                let attempt = run_with_retry(&shared, &executor, &route, &mut job).await;
                let mut book = shared.book();
                book.queue.finish();
                let recorded = match &attempt {
                    Settled::Done(outcome) => book.queue.record(&route, &job, outcome),
                    Settled::Failed { .. } => false,
                };
                if recorded
                    && let Some(path) = &book.records_file
                    && let Err(error) = book.queue.records().save(path)
                {
                    warn!(%error, path = %path.display(), "the flash write record was not saved");
                }
                let previous = match book.settled.remove(&route) {
                    Some(Settled::Done(outcome)) => Some(outcome),
                    Some(Settled::Failed { last, .. }) => last,
                    None => None,
                };
                let settled = match attempt {
                    Settled::Failed { error, last } => {
                        warn!(%error, "flash write failed");
                        Settled::Failed {
                            error,
                            last: last.or(previous),
                        }
                    }
                    done @ Settled::Done(_) => done,
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
