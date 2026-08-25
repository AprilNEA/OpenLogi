//! SWR-backed DPI and SmartShift reads keyed by device identity.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use gpui::{Context, Subscription};
use openlogi_core::hid::{DeviceRoute, DpiInfo, OnboardProfilesInfo, SmartShiftStatus, WriteError};
use swr_core::{
    MaybeSend, MaybeSync, QueryOptions, QueryState, Retry, RetryPolicy, Runtime, SwrClient,
};
use swr_gpui::Query;
use tokio::sync::mpsc;

use super::ipc::Command;
use crate::state::{
    AppState, DeviceKey, DpiStatus, Load, ProfilesLoad, SmartShiftLoad, StateEvent,
};

const ROOT: &str = "device-read";
const DPI: &str = "dpi";
const SMARTSHIFT: &str = "smartshift";
const PROFILES: &str = "profiles";

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
    generation: u64,
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
    next_generation: u64,
    dpi: BTreeMap<DeviceKey, DeviceRead<DpiInfo>>,
    smartshift: BTreeMap<DeviceKey, DeviceRead<SmartShiftStatus>>,
    profiles: BTreeMap<DeviceKey, DeviceRead<OnboardProfilesInfo>>,
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
        let generation = self.take_generation();
        let fetch_route = route.clone();
        let fetcher = Retry::new(
            runtime,
            move |_| {
                let commands = commands.clone();
                let route = fetch_route.clone();
                read_ipc(move |reply| Command::ReadDpi(route, reply), commands)
            },
            READ_RETRY_POLICY,
        )
        .retry_if(|error| !dpi_error_is_permanent(error));
        let handle = client.subscribe(query_key(DPI, &key), fetcher, QueryOptions::immutable());
        let query = Query::new(&client, handle, cx);
        let load = project_load(query.read(cx), dpi_error_is_permanent);
        let observed_key = key.clone();
        let observer = cx.observe(query.state(), move |state, query_state, cx| {
            let load = project_load(query_state.read(cx), dpi_error_is_permanent);
            if state.reads.update_dpi(&observed_key, generation, load) {
                state.apply_dpi_read(&observed_key);
                cx.emit(StateEvent::DpiChanged(observed_key.clone()));
            }
        });
        self.dpi.insert(
            key,
            DeviceRead {
                route,
                generation,
                load,
                query,
                _observer: observer,
            },
        );
    }

    /// Start the onboard-profile query unless the same device route is already subscribed.
    pub(crate) fn ensure_profiles(
        &mut self,
        key: DeviceKey,
        route: DeviceRoute,
        commands: mpsc::UnboundedSender<Command>,
        cx: &mut Context<AppState>,
    ) {
        if self
            .profiles
            .get(&key)
            .is_some_and(|read| read.route == route)
        {
            return;
        }
        self.remove_profiles(&key);
        let Some((client, runtime)) = self.cache() else {
            return;
        };
        let generation = self.take_generation();
        let fetch_route = route.clone();
        let fetcher = Retry::new(
            runtime,
            move |_| {
                let commands = commands.clone();
                let route = fetch_route.clone();
                read_ipc(
                    move |reply| Command::ReadOnboardProfiles(route, reply),
                    commands,
                )
            },
            READ_RETRY_POLICY,
        )
        .retry_if(|error| !profiles_error_is_permanent(error));
        let handle = client.subscribe(
            query_key(PROFILES, &key),
            fetcher,
            QueryOptions::immutable(),
        );
        let query = Query::new(&client, handle, cx);
        let load = project_load(query.read(cx), profiles_error_is_permanent);
        let observed_key = key.clone();
        let observer = cx.observe(query.state(), move |state, query_state, cx| {
            let load = project_load(query_state.read(cx), profiles_error_is_permanent);
            if state.reads.update_profiles(&observed_key, generation, load) {
                cx.emit(StateEvent::ProfilesChanged(observed_key.clone()));
            }
        });
        self.profiles.insert(
            key,
            DeviceRead {
                route,
                generation,
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
        if preserve_data {
            // A confirming read keeps the optimistic write visible as stale
            // data while invalidation starts the replacement query.
            client.invalidate(query_key(SMARTSHIFT, &key));
        } else if had_previous {
            self.clear::<SmartShiftStatus>(SMARTSHIFT, &key);
        }
        let generation = self.take_generation();
        let fetch_route = route.clone();
        let fetcher = Retry::new(
            runtime,
            move |_| {
                let commands = commands.clone();
                let route = fetch_route.clone();
                read_ipc(move |reply| Command::ReadSmartShift(route, reply), commands)
            },
            READ_RETRY_POLICY,
        )
        .retry_if(|error| !smartshift_error_is_permanent(error));
        let handle = client.subscribe(
            query_key(SMARTSHIFT, &key),
            fetcher,
            QueryOptions::immutable(),
        );
        let query = Query::new(&client, handle, cx);
        let load = project_load(query.read(cx), smartshift_error_is_permanent);
        let observed_key = key.clone();
        let observer = cx.observe(query.state(), move |state, query_state, cx| {
            let query_state = query_state.read(cx);
            let settled = smartshift_read_is_settled(query_state);
            let load = project_load(query_state, smartshift_error_is_permanent);
            if state
                .reads
                .update_smartshift(&observed_key, generation, load)
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
                generation,
                load,
                query,
                _observer: observer,
            },
        );
        true
    }

    #[must_use]
    pub(crate) fn dpi_status(&self, key: &DeviceKey) -> DpiStatus {
        self.dpi
            .get(key)
            .map_or(Load::Unknown, |read| read.load.clone())
    }

    #[must_use]
    pub(crate) fn dpi_load(&self, key: &DeviceKey) -> Option<&DpiStatus> {
        self.dpi.get(key).map(|read| &read.load)
    }

    #[must_use]
    pub(crate) fn smartshift_status(&self, key: &DeviceKey) -> SmartShiftLoad {
        self.smartshift
            .get(key)
            .map_or(Load::Unknown, |read| read.load.clone())
    }

    #[must_use]
    pub(crate) fn smartshift_load(&self, key: &DeviceKey) -> Option<&SmartShiftLoad> {
        self.smartshift.get(key).map(|read| &read.load)
    }

    #[must_use]
    pub(crate) fn profiles_status(&self, key: &DeviceKey) -> ProfilesLoad {
        self.profiles
            .get(key)
            .map_or(Load::Unknown, |read| read.load.clone())
    }

    #[must_use]
    pub(crate) fn profiles_load(&self, key: &DeviceKey) -> Option<&ProfilesLoad> {
        self.profiles.get(key).map(|read| &read.load)
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

    /// Retry or confirm an onboard-profile query.
    pub(crate) fn retry_profiles(&mut self, key: &DeviceKey) {
        let Some(read) = self.profiles.get_mut(key) else {
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

    /// Publish an onboard-profile write optimistically into swr and the view model.
    pub(crate) fn set_profiles_ready(&mut self, key: &DeviceKey, info: OnboardProfilesInfo) {
        let value = Arc::new(info);
        if let Some(client) = &self.client {
            client.set::<_, Cached<OnboardProfilesInfo>, WriteError>(
                query_key(PROFILES, key),
                Some(value.clone()),
            );
        }
        if let Some(read) = self.profiles.get_mut(key) {
            read.load = Load::Ready(value);
        }
    }

    /// Forget both feature queries for a device and fence their old flights.
    pub(crate) fn remove(&mut self, key: &DeviceKey) {
        self.remove_dpi(key);
        self.remove_smartshift(key);
        self.remove_profiles(key);
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

    pub(crate) fn remove_profiles(&mut self, key: &DeviceKey) {
        if let Some(read) = self.profiles.remove(key) {
            drop(read);
            self.clear::<OnboardProfilesInfo>(PROFILES, key);
        }
    }

    /// Forget every query whose device is no longer present.
    pub(crate) fn retain_present(&mut self, present: impl Fn(&str) -> bool) {
        let removed: BTreeSet<_> = self
            .dpi
            .keys()
            .chain(self.smartshift.keys())
            .chain(self.profiles.keys())
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
        // Drop subscribers before this call. `set(None)` fences the old flight;
        // invalidation then leaves the empty entry stale for the next route.
        client.set::<_, Cached<T>, WriteError>(query_key(kind, key), None);
        client.invalidate(query_key(kind, key));
    }

    fn take_generation(&mut self) -> u64 {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        generation
    }

    fn update_dpi(&mut self, key: &DeviceKey, generation: u64, load: DpiStatus) -> bool {
        let Some(read) = self
            .dpi
            .get_mut(key)
            .filter(|read| read.generation == generation)
        else {
            return false;
        };
        if read.load == load {
            return false;
        }
        read.load = load;
        true
    }

    fn update_smartshift(
        &mut self,
        key: &DeviceKey,
        generation: u64,
        load: SmartShiftLoad,
    ) -> bool {
        let Some(read) = self
            .smartshift
            .get_mut(key)
            .filter(|read| read.generation == generation)
        else {
            return false;
        };
        // A confirmation commonly resolves to the optimistic value already in
        // `load`. It must still reach `apply_smartshift_read` so Applying can
        // transition to Confirmed; the generation check is the stale guard.
        read.load = load;
        true
    }

    fn update_profiles(&mut self, key: &DeviceKey, generation: u64, load: ProfilesLoad) -> bool {
        let Some(read) = self
            .profiles
            .get_mut(key)
            .filter(|read| read.generation == generation)
        else {
            return false;
        };
        if read.load == load {
            return false;
        }
        read.load = load;
        true
    }
}

fn query_key(kind: &'static str, key: &DeviceKey) -> (&'static str, &'static str, String) {
    (ROOT, kind, key.to_string())
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

fn profiles_error_is_permanent(error: &WriteError) -> bool {
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
            attempt_count(
                WriteError::FeatureUnsupported {
                    feature_hex: 0x8100,
                },
                profiles_error_is_permanent,
            )
            .await,
            1
        );
        assert_eq!(
            attempt_count(WriteError::EmptyDpiList, dpi_error_is_permanent).await,
            1
        );
    }
}
