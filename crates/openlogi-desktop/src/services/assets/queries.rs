//! The asset cache expressed as swr entries.
//!
//! Two keys, and the dependency between them is the whole design:
//!
//! - `("assets", "index")` holds the [`AssetRegistry`] — the mirror probe plus
//!   the parsed `index.json`. One entry, so probing three mirrors happens once
//!   per session however many devices are attached.
//! - `("assets", "model", <model key>)` is one device model's download, and its
//!   fetcher reads the index entry first. That read is what keeps the probe
//!   shared instead of paid per device.
//!
//! What this replaces is bookkeeping, not behaviour. The retry backoff, the
//! already-synced set, the "index fetched once" flag, the single-flight
//! exclusion and the queue holding a manual command behind an in-flight fetch
//! were all hand-rolled around one global sync. Per key, swr gives the same
//! properties: concurrent reads join one request, `stale_time` *is* the
//! already-synced set, and a failure retries on its own schedule.

use std::sync::Arc;
use std::time::Duration;

use gpui::{AsyncApp, Task};
use openlogi_assets::AssetRegistry;
use openlogi_core::config::AssetSourcePreference;
use swr_core::{
    FetchError, MaybeSend, MaybeSync, QueryHandle, QueryOptions, ReadPolicy, SwrClient,
};
use tokio::sync::mpsc::UnboundedSender;
use tracing::warn;

use super::sync::{AssetTarget, load_registry, model_key, selected_source, sync_target};

/// First segment of every asset key, so one prefix `invalidate` clears the tier.
const ROOT: &str = "assets";

/// Errors surface as `anyhow` so the mirror and HTTP context survives to the log.
type SyncError = anyhow::Error;

/// The policy for every asset entry.
///
/// `fetch` reads the client's defaults rather than per-query options, and one
/// policy fits both keys anyway: device art is versioned by the asset release
/// rather than edited in place, so re-checking mid-session buys nothing. This
/// is the old "synced once per session" set expressed as a duration. `gc_time`
/// outlives a device unplugging, so a reconnect is a cache hit.
pub(crate) fn default_options() -> QueryOptions {
    QueryOptions {
        stale_time: Duration::from_mins(30),
        gc_time: Duration::from_hours(1),
        refresh_interval: None,
        ..QueryOptions::default()
    }
}

/// Ensure the registry is loaded, and hand it out.
///
/// Every model fetch goes through this key, which is what deduplicates the
/// mirror probe across devices.
async fn registry(
    client: &SwrClient,
    preference: AssetSourcePreference,
) -> Result<Arc<AssetRegistry>, FetchError<SyncError>> {
    client
        .fetch(
            (ROOT, "index"),
            move |_| async move {
                // Blocking HTTP on a background-executor thread — the same
                // place the old dedicated sync thread ran.
                load_registry(selected_source(preference))
            },
            ReadPolicy::StaleWhileRevalidate,
        )
        .await
}

/// Watch one device model's download, reporting each settled outcome on `tx`.
///
/// A subscription rather than a one-shot read, because the caller offers every
/// known device on every snapshot and only wants to hear about real changes: an
/// entry that is already fresh transitions to nothing, so it notifies nobody and
/// the expensive resolver rebuild behind `tx` never fires for it. Subscribing is
/// also what keeps the entry alive and what makes `invalidate` refetch
/// immediately instead of waiting for a next read.
///
/// The returned task owns the subscription. Dropping it unsubscribes, after
/// which the entry follows normal GC.
pub(crate) fn watch_model(
    client: &SwrClient,
    preference: AssetSourcePreference,
    target: AssetTarget,
    tx: UnboundedSender<bool>,
    cx: &AsyncApp,
) -> Task<()> {
    let weak = client.downgrade();
    let handle = client.subscribe(
        (ROOT, "model", model_key(&target)),
        move |_| {
            let weak = weak.clone();
            let target = target.clone();
            async move {
                let Some(client) = weak.upgrade() else {
                    anyhow::bail!("client dropped before the registry resolved");
                };
                let registry = registry(&client, preference)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                sync_target(&registry, &target)
            }
        },
        default_options(),
    );
    settled_outcomes(handle, tx, "asset sync", cx)
}

/// Watch the registry on its own, for the window before any device has appeared.
///
/// The old scheduler special-cased this ("fetch the index even with no
/// devices") so resolution works the moment a first device shows up. Once
/// devices exist their own entries depend on this one, and this subscription
/// just keeps it warm.
pub(crate) fn watch_index(
    client: &SwrClient,
    preference: AssetSourcePreference,
    tx: UnboundedSender<bool>,
    cx: &AsyncApp,
) -> Task<()> {
    let handle = client.subscribe(
        (ROOT, "index"),
        move |_| async move { load_registry(selected_source(preference)) },
        default_options(),
    );
    settled_outcomes(handle, tx, "asset index", cx)
}

/// Forward every *settled* state of `handle` to `tx` as "did it succeed".
///
/// `changed` also fires when a request starts; those carry no new bytes, so
/// reporting them would rebuild the resolver for nothing.
fn settled_outcomes<T>(
    mut handle: QueryHandle<T, SyncError>,
    tx: UnboundedSender<bool>,
    what: &'static str,
    cx: &AsyncApp,
) -> Task<()>
where
    T: MaybeSend + MaybeSync + 'static,
{
    cx.background_executor().spawn(async move {
        while handle.changed().await.is_ok() {
            let state = handle.snapshot();
            if state.is_validating {
                continue;
            }
            if let Some(error) = state.error.as_ref() {
                warn!(%error, "{what} failed — swr will retry");
            }
            if tx.send(state.error.is_none()).is_err() {
                break; // the event loop is gone
            }
        }
    })
}

/// Mark the whole asset tier stale. Subscribed entries refetch immediately, so
/// this is all either Settings → Assets action needs to do to force fresh art.
pub(crate) fn invalidate_all(client: &SwrClient) {
    client.invalidate(ROOT);
}
