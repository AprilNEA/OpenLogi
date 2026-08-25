//! Generation-fenced Disable Keys reads keyed by physical device identity.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use gpui::{Context, Subscription};
use openlogi_core::hid::{DeviceRoute, DisableKeysState, WriteError};
use swr_core::{QueryOptions, QueryState, Retry, RetryPolicy, Runtime, SwrClient};
use swr_gpui::Query;
use tokio::sync::mpsc;

use super::ipc::Command;
use crate::state::{AppState, DeviceKey, DisableKeysLoad, Load, StateEvent};

const ROOT: &str = "disable-keys-read";
const READ_RETRY_POLICY: RetryPolicy = RetryPolicy {
    interval: Duration::ZERO,
    max_retries: Some(2),
};

type Cached = Option<Arc<DisableKeysState>>;

struct DisableKeysRead {
    route: DeviceRoute,
    generation: u64,
    load: DisableKeysLoad,
    query: Query<Cached, WriteError>,
    _observer: Subscription,
}

/// Dedicated owner of Disable Keys reads and their route generations.
#[derive(Default)]
pub(crate) struct DisableKeysReads {
    client: Option<SwrClient>,
    runtime: Option<Arc<dyn Runtime>>,
    next_generation: u64,
    reads: BTreeMap<DeviceKey, DisableKeysRead>,
    confirmed: BTreeMap<DeviceKey, Arc<DisableKeysState>>,
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
        let generation = self.take_generation();
        let fetch_route = route.clone();
        let fetcher = Retry::new(
            runtime,
            move |_| {
                let commands = commands.clone();
                let route = fetch_route.clone();
                async move {
                    let (reply, result) = tokio::sync::oneshot::channel();
                    commands
                        .send(Command::ReadDisableKeys(route, reply))
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
            if state
                .disable_keys_reads_mut()
                .update(&observed_key, generation, load)
            {
                cx.emit(StateEvent::DisableKeysChanged(observed_key.clone()));
            }
        });
        self.reads.insert(
            key,
            DisableKeysRead {
                route,
                generation,
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

    /// Drop the active subscription and fence its completion, retaining the
    /// last confirmed snapshot for offline rendering.
    pub(crate) fn remove(&mut self, key: &DeviceKey) {
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
        self.reads.get(key).map(|read| read.generation).or_else(|| {
            #[cfg(test)]
            {
                self.test_generations.get(key).copied()
            }
            #[cfg(not(test))]
            {
                None
            }
        })
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

    fn update(&mut self, key: &DeviceKey, generation: u64, load: DisableKeysLoad) -> bool {
        let Some(read) = self
            .reads
            .get_mut(key)
            .filter(|read| read.generation == generation)
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
