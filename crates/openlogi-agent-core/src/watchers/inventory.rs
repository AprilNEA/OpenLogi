//! Event-first HID inventory reconciliation.
//!
//! A dedicated thread owns the persistent enumerator and every HID++ channel
//! it opens. OS hotplug, receiver/device lifecycle broadcasts, native/system
//! resume, bounded repair, and settings confirmation are typed reasons to run
//! one full authoritative reconciliation. A named half-minute recovery scan is
//! retained because some firmware/hosts miss lifecycle events and legacy or
//! voltage battery features have no broadcast API.

use std::collections::{BTreeMap, HashSet};
use std::future::pending;
use std::thread;
use std::time::{Instant, SystemTime};

use futures_lite::StreamExt as _;
use openlogi_core::device::{DeviceInventory, StandaloneDevice};
use openlogi_hid::ChannelRegistry;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use schedule::{
    DeadlinePurpose, HID_EVENT_SETTLE, HOTPLUG_SETTLE, ReconcileTrigger, SYSTEM_RESUME_SETTLE,
    Schedule, WakeDetector,
};

mod schedule;

/// Consecutive *initial* enumerate failures before the watcher declares
/// enumeration [`InventoryEvent::Unavailable`]. Only counts before the first
/// success: a mid-session failure keeps the last good snapshot instead (see
/// the error arm below), and a later success upgrades `Unavailable` back to a
/// live inventory.
const INITIAL_FAILURE_LIMIT: u8 = 3;

/// Number of successful raw-HID snapshots an omitted node may miss before it
/// is treated as a real detach. OS enumeration can briefly omit a registered
/// interface during unplug/replug and hotplug bursts, so a successful empty
/// snapshot is not immediately destructive for standalone lights.
const RAW_NODE_MISS_GRACE: u8 = 2;

/// What the watcher tells the agent.
#[derive(Debug)]
pub enum InventoryEvent {
    /// A completed enumeration — empty means "checked, no devices".
    Snapshot {
        /// HID++ receiver/direct inventory.
        inventories: Vec<DeviceInventory>,
        /// Recognized standalone raw-HID devices.
        standalone: Vec<StandaloneDevice>,
        /// Whether this pass failed to open at least one HID++ node — on
        /// macOS the observable signature of a missing or stale Input
        /// Monitoring grant (the open denial itself is silent).
        hid_open_failures: bool,
    },
    /// Enumeration has never succeeded and won't be treated as "still
    /// starting" any longer; without this the GUI would show its scanning
    /// state forever on a broken HID backend.
    Unavailable,
    /// A native resume notification or wall/monotonic clock gap says the
    /// system slept and woke. Devices may have power-cycled while their
    /// set/route/online state looks unchanged across the gap, so the agent
    /// re-applies volatile settings on the next snapshot (#189).
    SystemWake,
}

/// Non-blocking producer for native OS resume callbacks.
#[derive(Clone)]
pub struct ResumeSignal {
    sender: mpsc::Sender<()>,
}

/// Single-consumer stream of coalesced native OS resume transitions.
pub struct ResumeEvents {
    receiver: mpsc::Receiver<()>,
}

/// Create the directional native-resume channel.
///
/// Capacity one intentionally coalesces the equivalent wake, screen-wake, and
/// session-active bursts some operating systems emit for one resume.
#[must_use]
pub fn resume_channel() -> (ResumeSignal, ResumeEvents) {
    let (sender, receiver) = mpsc::channel(1);
    (ResumeSignal { sender }, ResumeEvents { receiver })
}

impl ResumeSignal {
    /// Publish one native resume transition without blocking the OS callback.
    pub fn notify(&self) {
        let _ = self.sender.try_send(());
    }
}

impl ResumeEvents {
    /// Wait for one resume transition. `None` means every producer was dropped.
    pub async fn recv(&mut self) -> Option<()> {
        self.receiver.recv().await
    }
}

/// The watcher's cross-pass memory, factored out of the I/O loop so the
/// result → event decision is unit-testable without spawning the thread or
/// touching real HID.
#[derive(Default)]
struct WatchState {
    /// Set once any enumeration has completed. After that, a failed pass keeps
    /// the last good snapshot forever instead of ever reporting `Unavailable`.
    succeeded: bool,
    /// Consecutive failures, counted only before the first success.
    initial_failures: u8,
    raw_nodes: RawNodeLedger,
}

#[derive(Default)]
struct RawNodeLedger {
    entries: BTreeMap<String, RawNodeEntry>,
}

struct RawNodeEntry {
    device: StandaloneDevice,
    misses: u8,
}

impl RawNodeLedger {
    /// Reconcile one successful raw enumeration with the last good per-node
    /// records. Omitted nodes stay offline for a bounded grace; a node seen
    /// again is replaced by its fresh descriptor and its miss count resets.
    fn reconcile(&mut self, live: Vec<StandaloneDevice>) -> Vec<StandaloneDevice> {
        let live_keys: HashSet<String> = live.iter().map(raw_node_key).collect();
        for device in live {
            self.entries
                .insert(raw_node_key(&device), RawNodeEntry { device, misses: 0 });
        }

        let missing: Vec<String> = self
            .entries
            .keys()
            .filter(|key| !live_keys.contains(*key))
            .cloned()
            .collect();
        for key in missing {
            let Some(entry) = self.entries.get_mut(&key) else {
                continue;
            };
            entry.misses = entry.misses.saturating_add(1);
            if entry.misses > RAW_NODE_MISS_GRACE {
                self.entries.remove(&key);
            } else {
                entry.device.online = false;
            }
        }

        self.entries
            .values()
            .map(|entry| entry.device.clone())
            .collect()
    }

    /// Return the last completed raw-HID snapshot without counting a miss.
    /// Used when standalone enumeration itself fails: absence was not
    /// observed, so advancing detach grace would manufacture a disconnect.
    fn snapshot(&self) -> Vec<StandaloneDevice> {
        self.entries
            .values()
            .map(|entry| entry.device.clone())
            .collect()
    }

    fn has_pending_misses(&self) -> bool {
        self.entries.values().any(|entry| entry.misses > 0)
    }
}

fn raw_node_key(device: &StandaloneDevice) -> String {
    let address = &device.address;
    format!(
        "{:04x}:{:04x}:{:04x}:{:04x}:{}",
        address.vendor_id,
        address.product_id,
        address.usage_page,
        address.usage_id,
        address.identity
    )
}

impl WatchState {
    /// Combine a successful HID++ enumeration with the independently fallible
    /// raw-HID pass. A raw backend failure must not suppress fresh mouse and
    /// keyboard inventory, nor count every remembered light as detached.
    fn classify_parts(
        &mut self,
        inventories: Vec<DeviceInventory>,
        standalone: Result<Vec<StandaloneDevice>, openlogi_hid::InventoryError>,
        hid_open_failures: bool,
    ) -> InventoryEvent {
        self.succeeded = true;
        let standalone = match standalone {
            Ok(devices) => self.raw_nodes.reconcile(devices),
            Err(e) => {
                warn!(
                    error = ?e,
                    "standalone enumerate failed during reconciliation — keeping last raw snapshot"
                );
                self.raw_nodes.snapshot()
            }
        };
        InventoryEvent::Snapshot {
            inventories,
            standalone,
            hid_open_failures,
        }
    }

    /// Decide what (if anything) a reconciliation pass emits.
    ///
    /// - `Ok(snapshot)` — a completed enumeration (an empty one included: that's
    ///   a genuine disconnect) — is forwarded so the agent's device set tracks
    ///   reality. A transient per-node probe miss never reaches here as an empty
    ///   `Ok`: `openlogi_hid`'s `NodeLedger` replays the node's last inventory
    ///   (#218/#222).
    /// - `Err(..)` means enumeration itself failed (OS-level HID enumerate
    ///   error): emit nothing, so the agent keeps its last good device set and
    ///   live bindings instead of wiping them. Before the *first*
    ///   success there is no good set to keep, so persistent initial failure is
    ///   reported once as [`InventoryEvent::Unavailable`]; the loop keeps
    ///   retrying and a later success recovers.
    fn classify(
        &mut self,
        result: Result<(Vec<DeviceInventory>, Vec<StandaloneDevice>), openlogi_hid::InventoryError>,
    ) -> Option<InventoryEvent> {
        match result {
            Ok((inventories, standalone)) => {
                Some(self.classify_parts(inventories, Ok(standalone), false))
            }
            Err(e) => {
                warn!(error = ?e, "enumerate failed during reconciliation — keeping last snapshot");
                if self.succeeded {
                    return None;
                }
                self.initial_failures = self.initial_failures.saturating_add(1);
                (self.initial_failures == INITIAL_FAILURE_LIMIT)
                    .then_some(InventoryEvent::Unavailable)
            }
        }
    }
}

/// A handle for requesting inventory work whose purpose belongs to the
/// authoritative orchestrator rather than a HID event source.
#[derive(Clone)]
pub struct InventoryRefresh {
    sender: mpsc::Sender<RefreshRequest>,
}

impl InventoryRefresh {
    /// Request the next delayed confirmation pass for volatile settings.
    /// Repeated requests coalesce into the bounded one-slot channel.
    pub fn request_settings_confirmation(&self) {
        let _ = self.sender.try_send(RefreshRequest::SettingsConfirmation);
    }
}

/// The watcher's event stream plus its settings-confirmation request handle.
pub struct InventoryWatcher {
    /// Completed snapshots and watcher-health events.
    pub events: mpsc::UnboundedReceiver<InventoryEvent>,
    /// Requests driven by state known only after the orchestrator applies a
    /// snapshot.
    pub refresh: InventoryRefresh,
}

#[derive(Clone, Copy)]
enum RefreshRequest {
    SettingsConfirmation,
}

/// Spawn a watcher without publishing channels into a registry.
#[must_use]
pub fn spawn() -> InventoryWatcher {
    spawn_inner(None, None)
}

/// Spawn the persistent watcher, publish its already-open HID++ channels into
/// `registry`, and consume a coalesced native-resume signal.
#[must_use]
pub fn spawn_with_registry(
    registry: ChannelRegistry,
    resume_events: ResumeEvents,
) -> InventoryWatcher {
    spawn_inner(Some(registry), Some(resume_events))
}

fn spawn_inner(
    registry: Option<ChannelRegistry>,
    resume_events: Option<ResumeEvents>,
) -> InventoryWatcher {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let worker_tx = event_tx.clone();
    let (refresh_tx, refresh_rx) = mpsc::channel(1);
    let spawn_result = thread::Builder::new()
        .name("openlogi-inventory-watcher".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    warn!(error = %e, "tokio runtime init failed; watcher exiting");
                    return;
                }
            };
            rt.block_on(run_watcher(worker_tx, refresh_rx, registry, resume_events));
        });
    if let Err(e) = spawn_result {
        // OS thread / fork limits are non-fatal for the agent as a whole, but
        // enumeration will never run. Say so — sending an empty *snapshot*
        // here would forge a "checked, no devices" answer for a check that
        // never happened.
        warn!(error = %e, "could not spawn inventory watcher — device scanning unavailable");
        let _ = event_tx.send(InventoryEvent::Unavailable);
    }
    InventoryWatcher {
        events: event_rx,
        refresh: InventoryRefresh { sender: refresh_tx },
    }
}

async fn run_watcher(
    events: mpsc::UnboundedSender<InventoryEvent>,
    refresh_requests: mpsc::Receiver<RefreshRequest>,
    registry: Option<ChannelRegistry>,
    resume_events: Option<ResumeEvents>,
) {
    // The listener is attached to each inventory-owned channel before its
    // first probe, and its bounded queue is subscribed before every snapshot.
    let (event_notifier, hid_events) = openlogi_hid::inventory::events::event_channel();
    let mut enumerator =
        openlogi_hid::host::persisted_enumerator().with_event_notifier(event_notifier);
    if let Some(registry) = registry {
        enumerator = enumerator.with_registry(registry);
    }
    let hotplug = match openlogi_hid::watch_hotplug() {
        Ok(stream) => Some(stream),
        Err(error) => {
            warn!(
                ?error,
                "hotplug watch unavailable — recovery scan remains active"
            );
            None
        }
    };
    let now = Instant::now();
    InventoryWorker {
        events,
        refresh_requests,
        enumerator,
        state: WatchState::default(),
        hotplug,
        hid_events,
        schedule: Schedule::new(now),
        wake_detector: WakeDetector::new(SystemTime::now(), now),
        resume_events,
        refresh_open: true,
    }
    .run()
    .await;
}

struct InventoryWorker {
    events: mpsc::UnboundedSender<InventoryEvent>,
    refresh_requests: mpsc::Receiver<RefreshRequest>,
    enumerator: openlogi_hid::inventory::Enumerator,
    state: WatchState,
    hotplug: Option<openlogi_hid::backend::HotplugStream>,
    hid_events: openlogi_hid::inventory::events::EventReceiver,
    schedule: Schedule,
    wake_detector: WakeDetector,
    resume_events: Option<ResumeEvents>,
    refresh_open: bool,
}

impl InventoryWorker {
    async fn run(&mut self) {
        let mut trigger = ReconcileTrigger::Initial;
        loop {
            if !self.settle_trigger(trigger).await || !self.reconcile(trigger).await {
                return;
            }
            trigger = self.next_trigger().await;
        }
    }

    async fn settle_trigger(&mut self, trigger: ReconcileTrigger) -> bool {
        match trigger {
            ReconcileTrigger::Hotplug => {
                tokio::time::sleep(HOTPLUG_SETTLE).await;
                if let Some(stream) = self.hotplug.as_mut() {
                    while let Some(drained) = futures_lite::future::poll_once(stream.next()).await {
                        if drained.is_none() {
                            self.hotplug = None;
                            warn!("hotplug stream ended — recovery scan remains active");
                            break;
                        }
                    }
                }
            }
            ReconcileTrigger::HidEvent(source) => {
                debug!(?source, "HID++ lifecycle event — reconciling inventory");
                tokio::time::sleep(HID_EVENT_SETTLE).await;
                while self.hid_events.try_recv().is_ok() {}
            }
            ReconcileTrigger::SystemResume => {
                info!("system resume — replaying settings on a settled inventory");
                if self.events.send(InventoryEvent::SystemWake).is_err() {
                    return false;
                }
                tokio::time::sleep(SYSTEM_RESUME_SETTLE).await;
            }
            ReconcileTrigger::Initial
            | ReconcileTrigger::RepairRetry
            | ReconcileTrigger::SettingsConfirmation
            | ReconcileTrigger::RecoveryScan => {}
        }
        true
    }

    async fn reconcile(&mut self, trigger: ReconcileTrigger) -> bool {
        let (event, needs_repair) = match self.enumerator.enumerate().await {
            Ok(inventories) => {
                let standalone = openlogi_hid::enumerate_standalone().await;
                let standalone_failed = standalone.is_err();
                let open_failures = self.enumerator.open_failures_last_tick();
                let event = self
                    .state
                    .classify_parts(inventories, standalone, open_failures);
                let needs_repair = self.enumerator.retry_needed_last_tick()
                    || standalone_failed
                    || self.state.raw_nodes.has_pending_misses();
                (Some(event), needs_repair)
            }
            Err(error) => (self.state.classify(Err(error)), true),
        };
        if let Some(event) = event
            && self.events.send(event).is_err()
        {
            debug!("inventory watcher receiver dropped — exiting");
            return false;
        }
        self.schedule
            .scan_finished(trigger, needs_repair, Instant::now());
        true
    }

    async fn next_trigger(&mut self) -> ReconcileTrigger {
        let trigger = loop {
            let (deadline, purpose) = self.schedule.next_deadline();
            let sleep = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline));
            tokio::pin!(sleep);
            tokio::select! {
                hotplug_event = async {
                    match self.hotplug.as_mut() {
                        Some(stream) => stream.next().await,
                        None => pending().await,
                    }
                } => if let Some(event) = hotplug_event {
                    debug!(?event, "hotplug event — scheduling settled reconciliation");
                    break ReconcileTrigger::Hotplug;
                } else {
                    self.hotplug = None;
                    warn!("hotplug stream ended — recovery scan remains active");
                },
                Some(source) = self.hid_events.recv() => {
                    break ReconcileTrigger::HidEvent(source);
                }
                resumed = async {
                    match self.resume_events.as_mut() {
                        Some(events) => events.recv().await,
                        None => pending().await,
                    }
                } => {
                    if resumed.is_some() {
                        break ReconcileTrigger::SystemResume;
                    }
                    self.resume_events = None;
                }
                request = async {
                    if self.refresh_open {
                        self.refresh_requests.recv().await
                    } else {
                        pending().await
                    }
                } => match request {
                    Some(RefreshRequest::SettingsConfirmation) => {
                        self.schedule.request_settings_confirmation(Instant::now());
                    }
                    None => self.refresh_open = false,
                },
                () = &mut sleep => {
                    match purpose {
                        DeadlinePurpose::RepairRetry => {
                            break ReconcileTrigger::RepairRetry;
                        }
                        DeadlinePurpose::SettingsConfirmation => {
                            break ReconcileTrigger::SettingsConfirmation;
                        }
                        DeadlinePurpose::RecoveryScan => {
                            break ReconcileTrigger::RecoveryScan;
                        }
                    }
                }
            }
        };

        // Any event that wakes this task is also an opportunity to compare the
        // clocks. This preserves resume recovery when the native source is
        // unavailable without restoring a dedicated wake-check timer. A
        // SystemResume reconciliation is already authoritative, so replacing
        // another trigger loses no inventory work.
        if trigger != ReconcileTrigger::SystemResume
            && self
                .wake_detector
                .observe(SystemTime::now(), Instant::now())
        {
            ReconcileTrigger::SystemResume
        } else {
            trigger
        }
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;
    use std::time::Duration;

    use openlogi_core::device::{DeviceKind, RawDeviceAddress, StandaloneDevice};
    use openlogi_hid::{BackendError, InventoryError};

    use super::{INITIAL_FAILURE_LIMIT, InventoryEvent, WatchState, resume_channel};

    #[tokio::test]
    async fn resume_signal_retains_and_coalesces_native_callbacks() {
        let (signal, mut events) = resume_channel();
        signal.notify();
        signal.notify();
        tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("a callback before the waiter is retained")
            .expect("the signal producer remains alive");
        tokio::time::timeout(Duration::from_millis(10), events.recv())
            .await
            .expect_err("equivalent callbacks coalesce into one resume");
    }

    /// A transport-level enumerate failure — what the watcher's `Err` arm now
    /// sees (a partial per-node read is replayed by the hid ledger as `Ok`).
    fn enumerate_failed() -> InventoryError {
        InventoryError::Hid(BackendError::Disconnected)
    }

    #[test]
    fn completed_enumeration_is_forwarded_even_when_empty() {
        let mut state = WatchState::default();
        // A genuine "checked, nothing there" still propagates as a disconnect —
        // the resilience must not swallow a real empty.
        assert_matches!(
            state.classify(Ok((vec![], vec![]))),
            Some(InventoryEvent::Snapshot { inventories, standalone, .. }) if inventories.is_empty() && standalone.is_empty()
        );
        assert!(state.succeeded);
    }

    #[test]
    fn failure_after_a_success_keeps_the_last_snapshot() {
        let mut state = WatchState::default();
        // A good tick first, so there is a last-known-good set to preserve.
        assert_matches!(
            state.classify(Ok((vec![], vec![]))),
            Some(InventoryEvent::Snapshot { .. })
        );
        // Then transient enumerate failures emit nothing — the agent keeps the
        // last snapshot instead of flapping to "No devices" (#218).
        assert!(state.classify(Err(enumerate_failed())).is_none());
        assert!(state.classify(Err(enumerate_failed())).is_none());
    }

    #[test]
    fn standalone_failure_keeps_raw_nodes_without_suppressing_the_snapshot() {
        let mut state = WatchState::default();
        let _ = state.classify(Ok((vec![], vec![raw_light("serial:glow-1")])));

        assert_matches!(
            state.classify_parts(vec![], Err(enumerate_failed()), false),
            InventoryEvent::Snapshot { inventories, standalone, .. }
                if inventories.is_empty()
                    && standalone.len() == 1
                    && standalone[0].online
        );
    }

    #[test]
    fn persistent_initial_failure_reports_unavailable_once_then_recovers() {
        let mut state = WatchState::default();
        // No snapshot has ever landed, so repeated failure must eventually stop
        // looking like "still scanning".
        for _ in 0..INITIAL_FAILURE_LIMIT - 1 {
            assert!(state.classify(Err(enumerate_failed())).is_none());
        }
        assert_matches!(
            state.classify(Err(enumerate_failed())),
            Some(InventoryEvent::Unavailable)
        );
        // Reported once, not on every later failure.
        assert!(state.classify(Err(enumerate_failed())).is_none());
        // …and a later success recovers with a live snapshot.
        assert_matches!(
            state.classify(Ok((vec![], vec![]))),
            Some(InventoryEvent::Snapshot { .. })
        );
    }

    fn raw_light(identity: &str) -> StandaloneDevice {
        StandaloneDevice {
            address: RawDeviceAddress {
                vendor_id: 0x046d,
                product_id: 0xc900,
                usage_page: 0xff43,
                usage_id: 0x0202,
                identity: identity.into(),
            },
            display_name: "Litra Glow".into(),
            manufacturer: Some("Logi".into()),
            serial_number: None,
            unit_id: [0; 4],
            kind: DeviceKind::Light,
            online: true,
            capabilities: None,
            light_capabilities: None,
            driver_id: "litra".into(),
            registry_model_id: None,
        }
    }

    #[test]
    fn raw_node_omission_is_graced_and_recovers() {
        let mut state = WatchState::default();
        assert_matches!(
            state.classify(Ok((vec![], vec![raw_light("id:node")]))) ,
            Some(InventoryEvent::Snapshot { standalone, .. }) if standalone.len() == 1 && standalone[0].online
        );
        assert_matches!(
            state.classify(Ok((vec![], vec![]))),
            Some(InventoryEvent::Snapshot { standalone, .. }) if standalone.len() == 1 && !standalone[0].online
        );
        assert_matches!(
            state.classify(Ok((vec![], vec![raw_light("id:node")]))) ,
            Some(InventoryEvent::Snapshot { standalone, .. }) if standalone.len() == 1 && standalone[0].online
        );
    }

    #[test]
    fn raw_node_is_removed_after_grace_is_exhausted() {
        let mut state = WatchState::default();
        let _ = state.classify(Ok((vec![], vec![raw_light("id:node")])));
        let _ = state.classify(Ok((vec![], vec![])));
        let _ = state.classify(Ok((vec![], vec![])));
        assert_matches!(
            state.classify(Ok((vec![], vec![]))),
            Some(InventoryEvent::Snapshot { standalone, .. }) if standalone.is_empty()
        );
    }
}
