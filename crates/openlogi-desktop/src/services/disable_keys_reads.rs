//! Generation-fenced Disable Keys reads keyed by physical device identity.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use gpui::{Context, Subscription};
use openlogi_core::hid::{DeviceRoute, DisableKeysState, WriteError};
use swr_core::{QueryOptions, QueryState, Retry, RetryPolicy, Runtime, SwrClient};
use swr_gpui::Query;
use tokio::sync::mpsc;

use super::ipc::{Command, ConfigReloadContext, ReadDisableKeys};
use crate::state::{AppState, DeviceKey, DisableKeysError, DisableKeysLoad, Load};

const ROOT: &str = "disable-keys-read";
const READ_RETRY_POLICY: RetryPolicy = RetryPolicy {
    interval: Duration::ZERO,
    max_retries: Some(2),
};

type Cached = Option<Arc<DisableKeysState>>;

struct DisableKeysRead {
    route: DeviceRoute,
    route_generation: u64,
    load: DisableKeysLoad,
    query: Query<Cached, WriteError>,
    _observer: Subscription,
}

/// Whether hardware snapshots can describe the externally adopted file.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) enum ConfigReconciliation {
    /// No external adoption is waiting for an agent receipt.
    #[default]
    Confirmed,
    /// Reload Retry can refresh and submit the file again.
    Required(DisableKeysError),
    /// Only this receipt may release the shared write fence.
    Awaiting(ConfigReloadContext),
}

/// Dedicated owner of Disable Keys reads and their route generations.
#[derive(Default)]
pub(crate) struct DisableKeysReads {
    client: Option<SwrClient>,
    runtime: Option<Arc<dyn Runtime>>,
    next_generation: u64,
    reads: BTreeMap<DeviceKey, DisableKeysRead>,
    confirmed: BTreeMap<DeviceKey, Arc<DisableKeysState>>,
    reconciliation: ConfigReconciliation,
    next_reconciliation_id: u64,
    #[cfg(test)]
    test_generations: BTreeMap<DeviceKey, u64>,
}

impl DisableKeysReads {
    /// Attach the shared SWR cache and runtime.
    pub(crate) fn connect(&mut self, client: SwrClient, runtime: Arc<dyn Runtime>) {
        self.client = Some(client);
        self.runtime = Some(runtime);
    }

    /// Start a query unless this exact route already has an active subscription.
    pub(crate) fn ensure(
        &mut self,
        key: DeviceKey,
        route: DeviceRoute,
        commands: mpsc::UnboundedSender<Command>,
        cx: &mut Context<AppState>,
    ) {
        if self.reads.get(&key).is_some_and(|read| read.route == route) {
            return;
        }
        self.remove(&key);
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let route_generation = self.take_generation();
        let fetch_route = route.clone();
        let fetcher = Retry::new(
            runtime,
            move |_| {
                let commands = commands.clone();
                let route = fetch_route.clone();
                async move {
                    let (reply, result) = tokio::sync::oneshot::channel();
                    commands
                        .send(ReadDisableKeys { route, reply }.into())
                        .map_err(|_| WriteError::AgentUnavailable)?;
                    result
                        .await
                        .map_err(|_| WriteError::AgentUnavailable)?
                        .map(|state| Some(Arc::new(state)))
                }
            },
            READ_RETRY_POLICY,
        )
        .retry_if(|error| !matches!(error, WriteError::FeatureUnsupported { .. }));
        let handle = client.subscribe(query_key(&key), fetcher, QueryOptions::immutable());
        let query = Query::new(&client, handle, cx);
        let load = project_load(query.read(cx));
        let observed_key = key.clone();
        let observer = cx.observe(query.state(), move |state, query_state, cx| {
            let load = project_load(query_state.read(cx));
            state
                .apply_disable_keys_read(&observed_key, route_generation, load)
                .emit(cx);
        });
        self.reads.insert(
            key,
            DisableKeysRead {
                route,
                route_generation,
                load,
                query,
                _observer: observer,
            },
        );
    }

    /// Retry an exhausted active query.
    pub(crate) fn retry(&mut self, key: &DeviceKey) {
        let Some(read) = self.reads.get_mut(key) else {
            return;
        };
        if !matches!(read.load, Load::Ready(_)) {
            read.load = Load::Loading;
        }
        read.query.revalidate();
    }

    /// Re-read after an unsolicited settings report. Invalidation fences any
    /// older fetch without retiring the route generation of an active write.
    pub(crate) fn revalidate_device_settings(&mut self, key: &DeviceKey) {
        self.invalidate_snapshot(key);
    }

    /// A failed write may already have changed hardware. Retire the old value
    /// and fence any in-flight read before allowing another edit.
    pub(crate) fn reconcile_after_write(&mut self, key: &DeviceKey) {
        self.invalidate_snapshot(key);
    }

    /// An external config reload may change every managed keyboard. Clear
    /// retained offline values too, and return the keys whose UI must update.
    pub(crate) fn reconcile_external_config(&mut self) -> BTreeSet<DeviceKey> {
        self.reconciliation = ConfigReconciliation::Required(DisableKeysError::RefreshFailed);
        self.invalidate_all_snapshots()
    }

    /// The refreshed file still needs its own agent acknowledgement.
    pub(crate) fn external_config_refreshed(&mut self) {
        self.reconciliation = ConfigReconciliation::Required(DisableKeysError::ReloadFailed);
    }

    /// Shared recovery state, independent of device connection lifetimes.
    pub(crate) fn reconciliation(&self) -> &ConfigReconciliation {
        &self.reconciliation
    }

    /// Allocate a route-independent receipt for an application-wide reload.
    pub(crate) fn next_config_reload(&mut self) -> u64 {
        let id = self.next_reconciliation_id;
        self.next_reconciliation_id = self.next_reconciliation_id.saturating_add(1);
        id
    }

    /// The latest save supersedes any older confirmation of the adopted file.
    pub(crate) fn track_config_reload(&mut self, context: ConfigReloadContext) {
        if self.reconciliation != ConfigReconciliation::Confirmed {
            self.reconciliation = ConfigReconciliation::Awaiting(context);
        }
    }

    /// Late receipts must not release a newer revision's write fence.
    pub(crate) fn config_reload_matches(&self, context: &ConfigReloadContext) -> bool {
        matches!(&self.reconciliation, ConfigReconciliation::Awaiting(current) if current == context)
    }

    /// Only reads started after this matching receipt can enable writes.
    pub(crate) fn confirm_config_reload(
        &mut self,
        context: &ConfigReloadContext,
    ) -> BTreeSet<DeviceKey> {
        if !self.config_reload_matches(context) {
            return BTreeSet::new();
        }
        self.reconciliation = ConfigReconciliation::Confirmed;
        self.invalidate_all_snapshots()
    }

    /// Keep the existing Reload Retry action available after a failed reload.
    pub(crate) fn fail_config_reload(
        &mut self,
        context: &ConfigReloadContext,
        error: DisableKeysError,
    ) {
        if self.config_reload_matches(context) {
            self.reconciliation = ConfigReconciliation::Required(error);
        }
    }

    fn invalidate_all_snapshots(&mut self) -> BTreeSet<DeviceKey> {
        let keys: BTreeSet<_> = self
            .reads
            .keys()
            .chain(self.confirmed.keys())
            .cloned()
            .collect();
        for key in &keys {
            self.invalidate_snapshot(key);
        }
        keys
    }

    /// Clear a stale snapshot before the active query obtains a new device
    /// reading. Both an unsolicited revision and an unconfirmed write need
    /// this same fail-closed transition.
    fn invalidate_snapshot(&mut self, key: &DeviceKey) {
        self.confirmed.remove(key);
        if let Some(read) = self.reads.get_mut(key) {
            read.load = Load::Loading;
            if let Some(client) = &self.client {
                client.set::<_, Cached, WriteError>(query_key(key), None);
                client.invalidate(query_key(key));
            }
        }
    }

    /// Drop the active subscription and fence its completion, retaining the
    /// last confirmed snapshot for offline rendering.
    pub(crate) fn remove(&mut self, key: &DeviceKey) {
        if matches!(&self.reconciliation,
            ConfigReconciliation::Awaiting(ConfigReloadContext::DisableKeys(context)) if &context.key == key)
        {
            self.reconciliation =
                ConfigReconciliation::Required(DisableKeysError::ConnectionChanged);
        }
        #[cfg(test)]
        self.test_generations.remove(key);
        if let Some(read) = self.reads.remove(key) {
            drop(read);
            if let Some(client) = &self.client {
                client.set::<_, Cached, WriteError>(query_key(key), None);
                client.invalidate(query_key(key));
            }
        }
    }

    /// Forget devices no longer represented by any catalog record.
    pub(crate) fn retain_present(&mut self, present: impl Fn(&str) -> bool) {
        let removed: BTreeSet<_> = self
            .reads
            .keys()
            .chain(self.confirmed.keys())
            .filter(|key| !present(key.as_str()))
            .cloned()
            .collect();
        for key in removed {
            self.remove(&key);
            self.confirmed.remove(&key);
        }
    }

    /// Current projected load, falling back to a retained confirmed snapshot
    /// after the active query was removed by an offline transition.
    pub(crate) fn load(&self, key: &DeviceKey) -> DisableKeysLoad {
        self.reads
            .get(key)
            .map(|read| read.load.clone())
            .or_else(|| self.confirmed.get(key).cloned().map(Load::Ready))
            .unwrap_or(Load::Unknown)
    }

    /// Last device-confirmed snapshot, independent of active subscription.
    #[cfg(test)]
    pub(crate) fn confirmed(&self, key: &DeviceKey) -> Option<Arc<DisableKeysState>> {
        self.confirmed.get(key).cloned()
    }

    /// Replace the retained and visible snapshot after a confirmed write.
    pub(crate) fn set_confirmed(&mut self, key: &DeviceKey, state: DisableKeysState) {
        let state = Arc::new(state);
        self.confirmed.insert(key.clone(), state.clone());
        if let Some(read) = self.reads.get_mut(key) {
            read.load = Load::Ready(state.clone());
        }
        if let Some(client) = &self.client {
            client.set::<_, Cached, WriteError>(query_key(key), Some(state));
        }
    }

    /// Generation of the active subscription, or `None` when detached/offline.
    pub(crate) fn generation(&self, key: &DeviceKey) -> Option<u64> {
        if let Some(read) = self.reads.get(key) {
            return Some(read.route_generation);
        }
        #[cfg(test)]
        {
            self.test_generations.get(key).copied()
        }
        #[cfg(not(test))]
        {
            None
        }
    }

    #[cfg(test)]
    pub(crate) fn install_generation_for_test(&mut self, key: DeviceKey, generation: u64) {
        self.test_generations.insert(key, generation);
    }

    fn take_generation(&mut self) -> u64 {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        generation
    }

    pub(crate) fn update(
        &mut self,
        key: &DeviceKey,
        route_generation: u64,
        load: DisableKeysLoad,
    ) -> bool {
        let Some(read) = self
            .reads
            .get_mut(key)
            .filter(|read| read.route_generation == route_generation)
        else {
            return false;
        };
        if let Load::Ready(state) = &load {
            self.confirmed.insert(key.clone(), state.clone());
        }
        if read.load == load {
            return false;
        }
        read.load = load;
        true
    }
}

fn query_key(key: &DeviceKey) -> (&'static str, String) {
    (ROOT, key.to_string())
}

fn project_load(state: &QueryState<Cached, WriteError>) -> DisableKeysLoad {
    let data = state.data.as_deref().and_then(Option::as_ref);
    if state.is_validating && data.is_none() {
        return Load::Loading;
    }
    if !state.is_validating
        && let Some(error) = state.error.as_deref()
    {
        return if matches!(error, WriteError::FeatureUnsupported { .. }) {
            Load::Unsupported(error.to_string())
        } else {
            Load::Failed(error.to_string())
        };
    }
    data.cloned().map_or(Load::Unknown, Load::Ready)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_reload_clears_retained_offline_snapshots() {
        let key = DeviceKey::from("offline-keyboard");
        let mut reads = DisableKeysReads::default();
        reads.set_confirmed(&key, DisableKeysState::default());
        assert!(matches!(reads.load(&key), Load::Ready(_)));
        assert_eq!(
            reads.reconcile_external_config(),
            BTreeSet::from([key.clone()])
        );
        assert!(matches!(reads.load(&key), Load::Unknown));
        assert!(reads.confirmed(&key).is_none());
    }

    #[test]
    fn stale_disable_keys_generation_cannot_replace_newer_snapshot() {
        let key = DeviceKey::from("keyboard");
        let mut reads = DisableKeysReads::default();
        reads.confirmed.insert(
            key.clone(),
            Arc::new(DisableKeysState {
                supported: openlogi_core::hid::DisableKeysMask::CAPS_LOCK,
                disabled: openlogi_core::hid::DisableKeysMask::EMPTY,
            }),
        );
        assert!(!reads.update(&key, 7, Load::Ready(Arc::new(DisableKeysState::default()))));
        assert_eq!(
            reads.confirmed(&key).expect("retained snapshot").supported,
            openlogi_core::hid::DisableKeysMask::CAPS_LOCK
        );
    }
}
