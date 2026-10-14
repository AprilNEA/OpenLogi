//! SWR-backed device-setting reads (DPI, SmartShift, Fn lock, report rate)
//! keyed by device identity.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use gpui::{Context, Subscription};
use openlogi_core::hid::{
    DeviceRoute, DpiInfo, FnLockState, ReportRateInfo, SmartShiftStatus, WriteError,
};
use swr_core::{
    MaybeSend, MaybeSync, QueryHandle, QueryOptions, QueryState, Retry, RetryPolicy, Runtime,
    SwrClient,
};
use swr_gpui::Query;
use tokio::sync::mpsc;

use super::ipc::{Command, ReadDpi, ReadFnLock, ReadReportRate, ReadSmartShift};
use crate::state::{
    AppState, DeviceKey, DpiLoad, FnLockLoad, Load, ReportRateLoad, SmartShiftLoad, StateEvent,
};

const ROOT: &str = "device-read";
const DPI: &str = "dpi";
const SMARTSHIFT: &str = "smartshift";
const FN_LOCK: &str = "fn-lock";
const REPORT_RATE: &str = "report-rate";

/// Preserve the old budget: one initial attempt and two retries.
const READ_RETRY_POLICY: RetryPolicy = RetryPolicy {
    // The previous cache retried immediately. Keeping a zero interval changes
    // the owner of retry policy without adding latency to device tabs.
    interval: Duration::ZERO,
    max_retries: Some(2),
};

type Cached<T> = Option<Arc<T>>;

struct DeviceRead<T: 'static> {
    route: DeviceRoute,
    /// The query flight this read belongs to: a callback from an older flight
    /// is stale and must not touch the entry that replaced it.
    flight: u64,
    load: Load<Arc<T>>,
    query: Query<Cached<T>, WriteError>,
    _observer: Subscription,
}

/// The state entity's live device-read queries.
///
/// The maps own subscriptions, not retry counters or result caches: swr owns
/// those. `Load<T>` is only the synchronous view-model projection consumed by
/// render paths.
#[derive(Default)]
pub(crate) struct DeviceReads {
    client: Option<SwrClient>,
    runtime: Option<Arc<dyn Runtime>>,
    next_flight: u64,
    dpi: BTreeMap<DeviceKey, DeviceRead<DpiInfo>>,
    smartshift: BTreeMap<DeviceKey, DeviceRead<SmartShiftStatus>>,
    fn_lock: BTreeMap<DeviceKey, DeviceRead<FnLockState>>,
    report_rate: BTreeMap<DeviceKey, DeviceRead<ReportRateInfo>>,
}

impl DeviceReads {
    /// Attach the shared cache and runtime after the GPUI app exists.
    pub(crate) fn connect(&mut self, client: SwrClient, runtime: Arc<dyn Runtime>) {
        self.client = Some(client);
        self.runtime = Some(runtime);
    }

    /// Start the DPI query unless the same device route is already subscribed.
    pub(crate) fn ensure_dpi(
        &mut self,
        key: DeviceKey,
        route: DeviceRoute,
        commands: mpsc::UnboundedSender<Command>,
        cx: &mut Context<AppState>,
    ) {
        if self.dpi.get(&key).is_some_and(|read| read.route == route) {
            return;
        }
        self.remove_dpi(&key);
        let Some((client, runtime)) = self.cache() else {
            return;
        };
        let flight = self.take_flight();
        let fetch_route = route.clone();
        let fetcher = Retry::new(
            runtime,
            move |_| {
                let commands = commands.clone();
                let route = fetch_route.clone();
                read_ipc(move |reply| ReadDpi { route, reply }.into(), commands)
            },
            READ_RETRY_POLICY,
        )
        .retry_if(|error| !dpi_error_is_permanent(error));
        let handle = client.subscribe(query_key(DPI, &key), fetcher, QueryOptions::immutable());
        read_through(&client, DPI, &key, &handle);
        let query = Query::new(&client, handle, cx);
        let load = project_load(query.read(cx), dpi_error_is_permanent);
        let observed_key = key.clone();
        let observer = cx.observe(query.state(), move |state, query_state, cx| {
            let load = project_load(query_state.read(cx), dpi_error_is_permanent);
            if state
                .device_reads_mut()
                .update_dpi(&observed_key, flight, load)
            {
                state.apply_dpi_read(&observed_key);
                cx.emit(StateEvent::DpiChanged(observed_key.clone()));
            }
        });
        self.dpi.insert(
            key,
            DeviceRead {
                route,
                flight,
                load,
                query,
                _observer: observer,
            },
        );
    }

    /// Start an initial SmartShift query unless this route is already watched.
    pub(crate) fn ensure_smartshift(
        &mut self,
        key: DeviceKey,
        route: DeviceRoute,
        commands: mpsc::UnboundedSender<Command>,
        cx: &mut Context<AppState>,
    ) {
        if self
            .smartshift
            .get(&key)
            .is_some_and(|read| read.route == route)
        {
            return;
        }
        self.subscribe_smartshift(key, route, None, false, commands, cx);
    }

    /// Replace the active SmartShift query with a write-confirmation read.
    pub(crate) fn confirm_smartshift(
        &mut self,
        key: DeviceKey,
        route: DeviceRoute,
        write_id: u64,
        commands: mpsc::UnboundedSender<Command>,
        cx: &mut Context<AppState>,
    ) -> bool {
        self.subscribe_smartshift(key, route, Some(write_id), true, commands, cx)
    }

    fn subscribe_smartshift(
        &mut self,
        key: DeviceKey,
        route: DeviceRoute,
        write_id: Option<u64>,
        preserve_data: bool,
        commands: mpsc::UnboundedSender<Command>,
        cx: &mut Context<AppState>,
    ) -> bool {
        let Some((client, runtime)) = self.cache() else {
            return false;
        };
        let previous = self.smartshift.remove(&key);
        let had_previous = previous.is_some();
        drop(previous);
        if had_previous && !preserve_data {
            self.clear::<SmartShiftStatus>(SMARTSHIFT, &key);
        }
        let flight = self.take_flight();
        let fetch_route = route.clone();
        let fetcher = Retry::new(
            runtime,
            move |_| {
                let commands = commands.clone();
                let route = fetch_route.clone();
                read_ipc(
                    move |reply| ReadSmartShift { route, reply }.into(),
                    commands,
                )
            },
            READ_RETRY_POLICY,
        )
        .retry_if(|error| !smartshift_error_is_permanent(error));
        let handle = client.subscribe(
            query_key(SMARTSHIFT, &key),
            fetcher,
            QueryOptions::immutable(),
        );
        if preserve_data {
            // A confirming read keeps the optimistic write visible as stale
            // data while invalidation starts the replacement query. A read
            // already in flight may predate the write, so it is discarded
            // rather than waited for.
            client.invalidate(query_key(SMARTSHIFT, &key));
        } else {
            read_through(&client, SMARTSHIFT, &key, &handle);
        }
        let query = Query::new(&client, handle, cx);
        let load = project_load(query.read(cx), smartshift_error_is_permanent);
        let observed_key = key.clone();
        let observer = cx.observe(query.state(), move |state, query_state, cx| {
            let query_state = query_state.read(cx);
            let settled = smartshift_read_is_settled(query_state);
            let load = project_load(query_state, smartshift_error_is_permanent);
            if state
                .device_reads_mut()
                .update_smartshift(&observed_key, flight, load)
            {
                if settled {
                    state.apply_smartshift_read(&observed_key, write_id);
                }
                cx.emit(StateEvent::SmartShiftChanged(observed_key.clone()));
            }
        });
        self.smartshift.insert(
            key,
            DeviceRead {
                route,
                flight,
                load,
                query,
                _observer: observer,
            },
        );
        true
    }

    /// Start the Fn-lock query unless the same keyboard route is already
    /// subscribed. A config write invalidates it through
    /// [`Self::refresh_fn_lock`] so the row shows what the keyboard took.
    pub(crate) fn ensure_fn_lock(
        &mut self,
        key: DeviceKey,
        route: DeviceRoute,
        commands: mpsc::UnboundedSender<Command>,
        cx: &mut Context<AppState>,
    ) {
        if self
            .fn_lock
            .get(&key)
            .is_some_and(|read| read.route == route)
        {
            return;
        }
        self.remove_fn_lock(&key);
        let Some((client, runtime)) = self.cache() else {
            return;
        };
        let flight = self.take_flight();
        let fetch_route = route.clone();
        let fetcher = Retry::new(
            runtime,
            move |_| {
                let commands = commands.clone();
                let route = fetch_route.clone();
                read_ipc(move |reply| ReadFnLock { route, reply }.into(), commands)
            },
            READ_RETRY_POLICY,
        )
        .retry_if(|error| !fn_lock_error_is_permanent(error));
        let handle = client.subscribe(query_key(FN_LOCK, &key), fetcher, QueryOptions::immutable());
        read_through(&client, FN_LOCK, &key, &handle);
        let query = Query::new(&client, handle, cx);
        let load = project_load(query.read(cx), fn_lock_error_is_permanent);
        let observed_key = key.clone();
        let observer = cx.observe(query.state(), move |state, query_state, cx| {
            let load = project_load(query_state.read(cx), fn_lock_error_is_permanent);
            if state
                .device_reads_mut()
                .update_fn_lock(&observed_key, flight, load)
            {
                cx.emit(StateEvent::FnLockChanged(observed_key.clone()));
            }
        });
        self.fn_lock.insert(
            key,
            DeviceRead {
                route,
                flight,
                load,
                query,
                _observer: observer,
            },
        );
    }

    /// Show `state` as `key`'s Fn-lock reading now. Used for the value the
    /// GUI just asked the keyboard to take and again for the value the
    /// keyboard echoed back. A local write supersedes a fetch still in
    /// flight, so a pre-write reading cannot land on top of it.
    pub(crate) fn set_fn_lock_ready(&mut self, key: &DeviceKey, state: FnLockState) {
        let value = Arc::new(state);
        if let Some(client) = &self.client {
            client.set::<_, Cached<FnLockState>, WriteError>(
                query_key(FN_LOCK, key),
                Some(value.clone()),
            );
        }
        if let Some(read) = self.fn_lock.get_mut(key) {
            read.load = Load::Ready(value);
        }
    }

    /// Re-read `key`'s Fn-lock from the keyboard, keeping the last value on
    /// screen while it answers. The fallback after a write the keyboard
    /// refused, when the shown value is no longer known to be its own.
    pub(crate) fn refresh_fn_lock(&mut self, key: &DeviceKey) {
        if let Some(read) = self.fn_lock.get_mut(key) {
            read.query.revalidate();
        }
    }

    /// Start the report-rate query unless the same mouse route is already
    /// subscribed. A write answers through [`Self::set_report_rate_ready`], or
    /// [`Self::refresh_report_rate`] when the mouse refused it.
    pub(crate) fn ensure_report_rate(
        &mut self,
        key: DeviceKey,
        route: DeviceRoute,
        commands: mpsc::UnboundedSender<Command>,
        cx: &mut Context<AppState>,
    ) {
        if self
            .report_rate
            .get(&key)
            .is_some_and(|read| read.route == route)
        {
            return;
        }
        self.remove_report_rate(&key);
        let Some((client, runtime)) = self.cache() else {
            return;
        };
        let flight = self.take_flight();
        let fetch_route = route.clone();
        let fetcher = Retry::new(
            runtime,
            move |_| {
                let commands = commands.clone();
                let route = fetch_route.clone();
                read_ipc(
                    move |reply| ReadReportRate { route, reply }.into(),
                    commands,
                )
            },
            READ_RETRY_POLICY,
        )
        .retry_if(|error| !report_rate_error_is_permanent(error));
        let handle = client.subscribe(
            query_key(REPORT_RATE, &key),
            fetcher,
            QueryOptions::immutable(),
        );
        read_through(&client, REPORT_RATE, &key, &handle);
        let query = Query::new(&client, handle, cx);
        let load = project_load(query.read(cx), report_rate_error_is_permanent);
        let observed_key = key.clone();
        let observer = cx.observe(query.state(), move |state, query_state, cx| {
            let load = project_load(query_state.read(cx), report_rate_error_is_permanent);
            if state
                .device_reads_mut()
                .update_report_rate(&observed_key, flight, load)
            {
                cx.emit(StateEvent::ReportRateChanged(observed_key.clone()));
            }
        });
        self.report_rate.insert(
            key,
            DeviceRead {
                route,
                flight,
                load,
                query,
                _observer: observer,
            },
        );
    }

    /// Show `info` as `key`'s report-rate reading now: the rate the GUI just
    /// asked for, then what the mouse read back. Like
    /// [`Self::set_fn_lock_ready`], a local write supersedes a fetch still in
    /// flight.
    ///
    /// `route` is the connection `info` was read on. The cache entry is keyed
    /// by the mouse alone, so a write answered over a connection the mouse has
    /// since left would list the wrong connection's rates: only the read
    /// subscribed on `route` takes it. Returns whether one did.
    pub(crate) fn set_report_rate_ready(
        &mut self,
        key: &DeviceKey,
        route: &DeviceRoute,
        info: ReportRateInfo,
    ) -> bool {
        let Some(read) = self.report_rate_on(key, route) else {
            return false;
        };
        let value = Arc::new(info);
        read.load = Load::Ready(value.clone());
        if let Some(client) = &self.client {
            client.set::<_, Cached<ReportRateInfo>, WriteError>(
                query_key(REPORT_RATE, key),
                Some(value),
            );
        }
        true
    }

    /// Re-read `key`'s report rate from the mouse, keeping the last value on
    /// screen while it answers — when the read is still on `route`, the
    /// connection a refused write went out on. Returns whether it was.
    pub(crate) fn refresh_report_rate(&mut self, key: &DeviceKey, route: &DeviceRoute) -> bool {
        let Some(read) = self.report_rate_on(key, route) else {
            return false;
        };
        read.query.revalidate();
        true
    }

    /// `key`'s report-rate read, if it is subscribed on `route`.
    fn report_rate_on(
        &mut self,
        key: &DeviceKey,
        route: &DeviceRoute,
    ) -> Option<&mut DeviceRead<ReportRateInfo>> {
        self.report_rate
            .get_mut(key)
            .filter(|read| read.route == *route)
    }

    /// Retry an exhausted report-rate query.
    pub(crate) fn retry_report_rate(&mut self, key: &DeviceKey) {
        let Some(read) = self.report_rate.get_mut(key) else {
            return;
        };
        if !matches!(read.load, Load::Ready(_)) {
            read.load = Load::Loading;
        }
        read.query.revalidate();
    }

    /// `key`'s report-rate load, or `None` while nothing has subscribed to it.
    #[must_use]
    pub(crate) fn report_rate_load(&self, key: &DeviceKey) -> Option<&ReportRateLoad> {
        self.report_rate.get(key).map(|read| &read.load)
    }

    /// Re-read `key`'s settings after the device reported changing one
    /// itself, keeping the shown values while it answers. Covers the settings
    /// such a report can concern — DPI, Fn lock and report rate (see
    /// [`PairedDevice::settings_revision`]); a read nothing subscribed to
    /// stays unsubscribed.
    ///
    /// [`PairedDevice::settings_revision`]: openlogi_core::device::PairedDevice::settings_revision
    pub(crate) fn revalidate_device_settings(&mut self, key: &DeviceKey) {
        if let Some(read) = self.dpi.get_mut(key) {
            read.query.revalidate();
        }
        self.refresh_fn_lock(key);
        if let Some(read) = self.report_rate.get_mut(key) {
            read.query.revalidate();
        }
    }

    /// `key`'s Fn-lock load, or `None` while nothing has subscribed to it.
    #[must_use]
    pub(crate) fn fn_lock_load(&self, key: &DeviceKey) -> Option<&FnLockLoad> {
        self.fn_lock.get(key).map(|read| &read.load)
    }

    /// `key`'s DPI load, or `None` while nothing has subscribed to it.
    #[must_use]
    pub(crate) fn dpi_load(&self, key: &DeviceKey) -> Option<&DpiLoad> {
        self.dpi.get(key).map(|read| &read.load)
    }

    /// `key`'s SmartShift load, or `None` while nothing has subscribed to it.
    #[must_use]
    pub(crate) fn smartshift_load(&self, key: &DeviceKey) -> Option<&SmartShiftLoad> {
        self.smartshift.get(key).map(|read| &read.load)
    }

    /// Retry an exhausted DPI query without changing its registered fetcher.
    pub(crate) fn retry_dpi(&mut self, key: &DeviceKey) {
        let Some(read) = self.dpi.get_mut(key) else {
            return;
        };
        if !matches!(read.load, Load::Ready(_)) {
            read.load = Load::Loading;
        }
        read.query.revalidate();
    }

    /// Retry an exhausted initial SmartShift query.
    pub(crate) fn retry_smartshift(&mut self, key: &DeviceKey) {
        let Some(read) = self.smartshift.get_mut(key) else {
            return;
        };
        if !matches!(read.load, Load::Ready(_)) {
            read.load = Load::Loading;
        }
        read.query.revalidate();
    }

    /// Publish a SmartShift write optimistically into swr and the view model.
    pub(crate) fn set_smartshift_ready(&mut self, key: &DeviceKey, status: SmartShiftStatus) {
        let value = Arc::new(status);
        if let Some(client) = &self.client {
            client.set::<_, Cached<SmartShiftStatus>, WriteError>(
                query_key(SMARTSHIFT, key),
                Some(value.clone()),
            );
        }
        if let Some(read) = self.smartshift.get_mut(key) {
            read.load = Load::Ready(value);
        }
    }

    /// Forget every feature query for a device and fence their old flights.
    pub(crate) fn remove(&mut self, key: &DeviceKey) {
        self.remove_dpi(key);
        self.remove_smartshift(key);
        self.remove_fn_lock(key);
        self.remove_report_rate(key);
    }

    pub(crate) fn remove_dpi(&mut self, key: &DeviceKey) {
        if let Some(read) = self.dpi.remove(key) {
            drop(read);
            self.clear::<DpiInfo>(DPI, key);
        }
    }

    pub(crate) fn remove_smartshift(&mut self, key: &DeviceKey) {
        if let Some(read) = self.smartshift.remove(key) {
            drop(read);
            self.clear::<SmartShiftStatus>(SMARTSHIFT, key);
        }
    }

    fn remove_fn_lock(&mut self, key: &DeviceKey) {
        if let Some(read) = self.fn_lock.remove(key) {
            drop(read);
            self.clear::<FnLockState>(FN_LOCK, key);
        }
    }

    fn remove_report_rate(&mut self, key: &DeviceKey) {
        if let Some(read) = self.report_rate.remove(key) {
            drop(read);
            self.clear::<ReportRateInfo>(REPORT_RATE, key);
        }
    }

    /// Forget every query whose device is no longer present.
    pub(crate) fn retain_present(&mut self, present: impl Fn(&str) -> bool) {
        let removed: BTreeSet<_> = self
            .dpi
            .keys()
            .chain(self.smartshift.keys())
            .chain(self.fn_lock.keys())
            .chain(self.report_rate.keys())
            .filter(|key| !present(key.as_str()))
            .cloned()
            .collect();
        for key in removed {
            self.remove(&key);
        }
    }

    fn cache(&self) -> Option<(SwrClient, Arc<dyn Runtime>)> {
        Some((
            self.client.as_ref()?.clone(),
            self.runtime.as_ref()?.clone(),
        ))
    }

    fn clear<T>(&self, kind: &'static str, key: &DeviceKey)
    where
        T: MaybeSend + MaybeSync + 'static,
    {
        let Some(client) = &self.client else {
            return;
        };
        // `set(None)` fences the old flight: whatever it answers is dropped.
        // The next subscription reads afresh through [`read_through`].
        // Invalidating here would refetch at once with the dropped read's
        // fetcher — and so over the route the device just left — because a
        // dropped `Query` unsubscribes only when its watcher task next runs.
        client.set::<_, Cached<T>, WriteError>(query_key(kind, key), None);
    }

    fn take_flight(&mut self) -> u64 {
        let flight = self.next_flight;
        self.next_flight = self.next_flight.saturating_add(1);
        flight
    }

    fn update_dpi(&mut self, key: &DeviceKey, flight: u64, load: DpiLoad) -> bool {
        let Some(read) = self.dpi.get_mut(key).filter(|read| read.flight == flight) else {
            return false;
        };
        if read.load == load {
            return false;
        }
        read.load = load;
        true
    }

    fn update_fn_lock(&mut self, key: &DeviceKey, flight: u64, load: FnLockLoad) -> bool {
        let Some(read) = self
            .fn_lock
            .get_mut(key)
            .filter(|read| read.flight == flight)
        else {
            return false;
        };
        if read.load == load {
            return false;
        }
        read.load = load;
        true
    }

    fn update_report_rate(&mut self, key: &DeviceKey, flight: u64, load: ReportRateLoad) -> bool {
        let Some(read) = self
            .report_rate
            .get_mut(key)
            .filter(|read| read.flight == flight)
        else {
            return false;
        };
        if read.load == load {
            return false;
        }
        read.load = load;
        true
    }

    fn update_smartshift(&mut self, key: &DeviceKey, flight: u64, load: SmartShiftLoad) -> bool {
        let Some(read) = self
            .smartshift
            .get_mut(key)
            .filter(|read| read.flight == flight)
        else {
            return false;
        };
        // A confirmation commonly resolves to the optimistic value already in
        // `load`. It must still reach `apply_smartshift_read` so Applying can
        // transition to Confirmed; the flight check is the stale guard.
        read.load = load;
        true
    }
}

fn query_key(kind: &'static str, key: &DeviceKey) -> (&'static str, &'static str, String) {
    (ROOT, kind, key.to_string())
}

/// Have a just-made subscription read the device now, through its own
/// fetcher and so over its own route.
///
/// A subscription is made for a device new to this window, one back from
/// absence, or one on a new route. A fresh entry starts that read on its own;
/// an entry [`DeviceReads::clear`] emptied, or one holding a write's
/// optimistic value, does not, because these queries are immutable. Only now
/// that subscribing has made this fetcher the entry's — fetchers are
/// last-wins — does invalidating read over `route` rather than the last one.
fn read_through<T>(
    client: &SwrClient,
    kind: &'static str,
    key: &DeviceKey,
    handle: &QueryHandle<Cached<T>, WriteError>,
) where
    T: MaybeSend + MaybeSync + 'static,
{
    if !handle.snapshot().is_validating {
        client.invalidate(query_key(kind, key));
    }
}

async fn read_ipc<T>(
    command: impl FnOnce(tokio::sync::oneshot::Sender<Result<T, WriteError>>) -> Command,
    commands: mpsc::UnboundedSender<Command>,
) -> Result<Cached<T>, WriteError>
where
    T: MaybeSend + MaybeSync + 'static,
{
    let (reply, result) = tokio::sync::oneshot::channel();
    commands
        .send(command(reply))
        .map_err(|_| WriteError::AgentUnavailable)?;
    result
        .await
        .map_err(|_| WriteError::AgentUnavailable)?
        .map(|value| Some(Arc::new(value)))
}

fn project_load<T>(
    state: &QueryState<Cached<T>, WriteError>,
    is_permanent: impl Fn(&WriteError) -> bool,
) -> Load<Arc<T>> {
    let data = state.data.as_deref().and_then(Option::as_ref);
    if state.is_validating && data.is_none() {
        return Load::Loading;
    }
    if !state.is_validating
        && let Some(error) = state.error.as_deref()
    {
        return if is_permanent(error) {
            Load::Unsupported(error.to_string())
        } else {
            Load::Failed(error.to_string())
        };
    }
    data.cloned().map_or(Load::Unknown, Load::Ready)
}

fn dpi_error_is_permanent(error: &WriteError) -> bool {
    matches!(
        error,
        WriteError::FeatureUnsupported { .. } | WriteError::EmptyDpiList
    )
}

fn smartshift_error_is_permanent(error: &WriteError) -> bool {
    matches!(error, WriteError::FeatureUnsupported { .. })
}

fn fn_lock_error_is_permanent(error: &WriteError) -> bool {
    matches!(error, WriteError::FeatureUnsupported { .. })
}

fn report_rate_error_is_permanent(error: &WriteError) -> bool {
    matches!(error, WriteError::FeatureUnsupported { .. })
}

/// Stale data remains renderable while SWR revalidates, but only the settled
/// snapshot represents the device-facing result of a confirmation read.
fn smartshift_read_is_settled(state: &QueryState<Cached<SmartShiftStatus>, WriteError>) -> bool {
    !state.is_validating
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use openlogi_core::hid::{Dpi, DpiCapabilities, SmartShiftAutoDisengage, SmartShiftMode};
    use swr_core::{Fetcher as _, Instant, RuntimeFuture};

    use super::*;

    struct TokioRuntime;

    impl Runtime for TokioRuntime {
        fn now(&self) -> Instant {
            Instant::now()
        }

        fn spawn(&self, future: RuntimeFuture) {
            tokio::spawn(future);
        }

        fn sleep_until(&self, at: Instant) -> RuntimeFuture {
            Box::pin(async move {
                tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await;
            })
        }
    }

    fn state<T>(
        data: Option<Arc<Cached<T>>>,
        error: Option<Arc<WriteError>>,
        is_loading: bool,
        is_validating: bool,
    ) -> QueryState<Cached<T>, WriteError> {
        QueryState {
            data,
            error,
            is_loading,
            is_validating,
            updated_at: None,
        }
    }

    async fn attempt_count(error: WriteError, is_permanent: fn(&WriteError) -> bool) -> u32 {
        let calls = Arc::new(AtomicU32::new(0));
        let fetcher = {
            let calls = calls.clone();
            move |_key: &'static str| {
                calls.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Err::<(), _>(error.clone()))
            }
        };
        let retry = Retry::new(Arc::new(TokioRuntime), fetcher, READ_RETRY_POLICY)
            .retry_if(move |error| !is_permanent(error));

        assert!(retry.fetch("device-read").await.is_err());
        calls.load(Ordering::SeqCst)
    }

    #[test]
    fn swr_state_projects_to_the_five_load_states() {
        let info = Arc::new(DpiInfo {
            current: Dpi::new(1600),
            capabilities: DpiCapabilities::new(vec![800, 1600]).expect("valid DPI list"),
        });
        assert_eq!(
            project_load(
                &state::<DpiInfo>(None, None, false, false),
                dpi_error_is_permanent
            ),
            Load::Unknown
        );
        assert_eq!(
            project_load(
                &state::<DpiInfo>(None, None, true, true),
                dpi_error_is_permanent
            ),
            Load::Loading
        );
        assert_eq!(
            project_load(
                &state(Some(Arc::new(Some(info.clone()))), None, false, false),
                dpi_error_is_permanent,
            ),
            Load::Ready(info.clone())
        );
        assert_eq!(
            project_load(
                &state(Some(Arc::new(Some(info.clone()))), None, false, true),
                dpi_error_is_permanent,
            ),
            Load::Ready(info.clone())
        );
        assert!(matches!(
            project_load(
                &state(
                    Some(Arc::new(Some(info))),
                    Some(Arc::new(WriteError::AgentUnavailable)),
                    false,
                    false,
                ),
                dpi_error_is_permanent,
            ),
            Load::Failed(_)
        ));
        assert!(matches!(
            project_load(
                &state::<DpiInfo>(None, Some(Arc::new(WriteError::EmptyDpiList)), false, false,),
                dpi_error_is_permanent,
            ),
            Load::Unsupported(_)
        ));
    }

    #[test]
    fn validating_smartshift_data_is_visible_but_not_confirmed() {
        let optimistic = Arc::new(SmartShiftStatus {
            mode: SmartShiftMode::Ratchet,
            auto_disengage: SmartShiftAutoDisengage::Permanent,
            tunable_torque: None,
        });
        let validating = state(Some(Arc::new(Some(optimistic.clone()))), None, false, true);

        assert_eq!(
            project_load(&validating, smartshift_error_is_permanent),
            Load::Ready(optimistic.clone()),
            "the optimistic value stays visible while confirmation is in flight"
        );
        assert!(
            !smartshift_read_is_settled(&validating),
            "validating stale data is not a device confirmation"
        );

        let settled = state(Some(Arc::new(Some(optimistic))), None, false, false);
        assert!(smartshift_read_is_settled(&settled));
    }

    #[tokio::test]
    async fn transient_reads_keep_the_three_attempt_budget() {
        assert_eq!(
            attempt_count(WriteError::AgentUnavailable, smartshift_error_is_permanent).await,
            3
        );
    }

    #[tokio::test]
    async fn permanent_errors_are_not_retried() {
        assert_eq!(
            attempt_count(
                WriteError::FeatureUnsupported {
                    feature_hex: 0x2111,
                },
                smartshift_error_is_permanent,
            )
            .await,
            1
        );
        assert_eq!(
            attempt_count(WriteError::EmptyDpiList, dpi_error_is_permanent).await,
            1
        );
    }

    #[tokio::test]
    async fn a_mouse_without_a_report_rate_feature_is_asked_once() {
        assert_eq!(
            attempt_count(
                WriteError::FeatureUnsupported {
                    feature_hex: 0x8061,
                },
                report_rate_error_is_permanent,
            )
            .await,
            1
        );
        assert_eq!(
            attempt_count(WriteError::AgentUnavailable, report_rate_error_is_permanent).await,
            3
        );
    }
}
