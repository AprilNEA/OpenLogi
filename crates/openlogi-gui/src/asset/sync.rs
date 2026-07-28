//! Background HTTP sync against OpenLogi's asset mirrors.
//!
//! Always fetches `index.json` first — even with no devices connected, so
//! the registry is on disk before the first device needs resolving. Then,
//! for each connected device with a [`DeviceModelInfo`], resolves the
//! matching depot from that fresh index and downloads any per-device files
//! we don't already have cached (or whose sha256 differs). Failures bubble
//! up to the caller's retry/backoff; the GUI falls back to whatever's
//! currently on disk and ultimately to the synthetic silhouette.

use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result};
use backon::{BackoffBuilder, ExponentialBuilder};
use openlogi_assets::http;
use openlogi_assets::{
    AssetRegistry, AssetSource, BUTTONS_RENDER_FILES, DepotManifest, DeviceEntry, FetchOutcome,
};
use openlogi_core::config::AssetSourcePreference;
use openlogi_core::device::DeviceModelInfo;
use tracing::{debug, info, warn};

/// Whether the startup HTTP sync should run on this launch.
///
/// Policy:
/// - `OPENLOGI_SYNC=off` → never run.
/// - `OPENLOGI_SYNC=on` → always run.
/// - Debug builds → run (so devs see registry updates immediately).
/// - Release builds → run only when the app bundle didn't ship assets
///   (safety net for malformed bundles or hand-built binaries).
pub fn should_run(has_bundle: bool) -> bool {
    match std::env::var("OPENLOGI_SYNC").ok().as_deref() {
        Some("off" | "false" | "0") => return false,
        Some("on" | "true" | "1") => return true,
        _ => {}
    }
    if cfg!(debug_assertions) {
        return true;
    }
    !has_bundle
}

/// Refresh the local cache: probe the built-in mirrors (or use the selected
/// source), persist the selected source's `index.json`, then sync
/// the depots for every model in `models`. An empty `models` is a valid
/// call — it prefetches just the index so device resolution works the
/// moment a device first appears.
///
/// Each entry pairs a device's HID++ model info with its firmware `codename`,
/// so the depot match can fall back to the registry `displayName` for devices
/// whose live PID isn't in the registry (e.g. an MX Master 3S over BTLE).
pub fn sync(
    source: Option<AssetSource>,
    models: &[(DeviceModelInfo, Option<String>)],
) -> Result<()> {
    let cache_root = super::paths::user_cache_root();
    fs::create_dir_all(&cache_root)
        .with_context(|| format!("create cache root {}", cache_root.display()))?;

    let registry = AssetRegistry::load_source(source, &cache_root).context("fetch asset index")?;
    let client = registry.client();
    let index = registry.index();
    // The index is the critical shared resource — if it can't be fetched
    // (after the HTTP layer's own retries) bail with an error so the caller
    // retries the whole sync on a later device snapshot, rather than latching
    // success off a run that downloaded nothing. Per-depot failures below stay
    // best-effort: an optional colour variant 404 shouldn't block everything.
    // Each target carries the HID++ `extended_model_id` byte so the
    // depot sync can fetch the right colour variant. `OPENLOGI_FORCE_DEPOT`
    // doesn't correspond to a physical device, so we pass `ext = 0`
    // and end up with the base PNG.
    let mut targets: Vec<(String, DeviceEntry, u8)> = Vec::new();
    if let Ok(forced) = std::env::var("OPENLOGI_FORCE_DEPOT")
        && let Some(entry) = index.devices.get(&forced)
    {
        targets.push((forced, entry.clone(), 0));
    }
    for (model, codename) in models {
        if let Some((depot, entry)) = super::resolve_in_index(index, model, codename.as_deref()) {
            targets.push((depot.to_string(), entry.clone(), model.extended_model_id));
        }
    }
    targets.sort_by(|a, b| a.0.cmp(&b.0));
    targets.dedup_by(|a, b| a.0 == b.0);

    if targets.is_empty() {
        debug!("sync: no matching depots for known devices");
        return Ok(());
    }

    for (depot, entry, ext) in &targets {
        if let Err(e) = sync_depot(client, &cache_root, depot, entry, *ext) {
            warn!(depot, error = %e, "depot sync failed");
        }
    }
    info!(devices = targets.len(), "asset sync complete");
    Ok(())
}

fn sync_depot(
    client: &http::AssetClient,
    cache_root: &Path,
    depot: &str,
    entry: &DeviceEntry,
    ext: u8,
) -> Result<()> {
    let dir = http::safe_component_path(cache_root, depot, "asset depot")?;
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

    // Baseline: hotspot metadata + manifest + hero render, in whichever
    // schema this depot ships (`*_core` or the bare names). Manifest is
    // fetched here so the variant lookup below has something to consult.
    for name in entry.baseline_files() {
        fetch_to_cache(client, &entry.asset_path, &dir, entry, name)?;
    }

    // Dedicated buttons render — only present on devices whose manifest
    // points `device_buttons_image` at a distinct side view. Fetch it only
    // when the registry lists it so front-only devices don't 404; failure
    // is non-fatal (the GUI falls back to the hero render).
    if let Some(side) = entry.preferred_file(&BUTTONS_RENDER_FILES)
        && let Err(e) = fetch_to_cache(client, &entry.asset_path, &dir, entry, side)
    {
        warn!(depot, error = %e, "buttons render fetch failed");
    }

    // Optional second pass: download the colour variant PNGs matching
    // the connected device's `extended_model_id`, for both the front
    // (carousel) and the side / buttons (mouse-model) views. Failure is
    // non-fatal — `AssetResolver.load_files` falls back to the bare hero
    // render that came in with the baseline fetch above.
    let manifest_path = dir.join("manifest.json");
    for resource_key in ["device_image", "device_buttons_image"] {
        let Some(variant) =
            pick_variant_filename(&manifest_path, &entry.model_id, ext, resource_key)
        else {
            continue;
        };
        if matches!(
            variant.as_str(),
            "front_core.png" | "front.png" | "side_core.png" | "side.png"
        ) {
            continue;
        }
        if let Err(e) = fetch_to_cache(client, &entry.asset_path, &dir, entry, &variant) {
            warn!(depot, variant = %variant, error = %e, "variant fetch failed");
        }
    }
    Ok(())
}

/// Fetch a single named file from `<server>/<asset_path>/<name>` into
/// `<dir>/<name>`. SHA-checked against `entry.files`; a name the registry
/// doesn't list is skipped (warn) — everything written to the cache must
/// carry a registry hash, so a tampered host can't plant unverified files.
fn fetch_to_cache(
    client: &http::AssetClient,
    asset_path: &str,
    dir: &Path,
    entry: &DeviceEntry,
    name: &str,
) -> Result<()> {
    if let Some(file_entry) = entry.files.iter().find(|f| f.name == name) {
        match client.fetch_entry_if_stale(asset_path, dir, file_entry)? {
            FetchOutcome::CacheHit => debug!(file = name, "cache hit"),
            FetchOutcome::Fetched { bytes } => info!(file = name, bytes, "downloaded"),
        }
    } else {
        warn!(
            file = name,
            "registry lists no entry — skipping unverified asset"
        );
    }
    Ok(())
}

/// Parse a freshly-downloaded `manifest.json` and resolve the colour
/// variant filename for `resource_key` (e.g. `"device_image"` or
/// `"device_buttons_image"`). `None` when the manifest is missing,
/// malformed, or doesn't list the device's `ext` byte.
fn pick_variant_filename(
    manifest_path: &Path,
    base_model_id: &str,
    ext: u8,
    resource_key: &str,
) -> Option<String> {
    if ext == 0 || !manifest_path.exists() {
        return None;
    }
    let manifest = DepotManifest::load_from(manifest_path)
        .map_err(|e| warn!(error = %e, path = %manifest_path.display(), "manifest unreadable"))
        .ok()?;
    manifest
        .resource_for_variant(base_model_id, ext, resource_key)
        .map(str::to_string)
}

/// Result of one background asset-sync run, reported back to the select
/// loop: whether the run succeeded, and which model keys it covered (folded
/// into the synced set on success so the same device doesn't re-sync every
/// snapshot).
pub(crate) struct SyncOutcome {
    pub(crate) ok: bool,
    pub(crate) keys: Vec<String>,
}

/// Session-stable identity for a synced model: the HID++ model ids plus the
/// extended-model byte (the colour-variant selector) and the codename the
/// depot match falls back on. Models that collapse to one key would resolve
/// to the same depot files anyway.
pub(crate) fn model_key((model, codename): &(DeviceModelInfo, Option<String>)) -> String {
    format!(
        "{:02x}:{:04x}:{:04x}:{:04x}:{}",
        model.extended_model_id,
        model.model_ids[0],
        model.model_ids[1],
        model.model_ids[2],
        codename.as_deref().unwrap_or_default()
    )
}

/// A manual asset action requested from the Settings → Assets tab, pushed to
/// the main event loop via [`AssetControl`].
pub enum AssetCommand {
    /// Force-fetch assets for known devices now, bypassing the
    /// automatic download policy.
    Refresh,
    /// Delete the per-user cache, then re-fetch.
    ClearCache,
}

/// Global handle the Settings window uses to push [`AssetCommand`]s into the
/// main loop, mirroring how the Add Device window drives pairing.
pub struct AssetControl(pub tokio::sync::mpsc::UnboundedSender<AssetCommand>);

impl gpui::Global for AssetControl {}

/// Minimum gap before re-attempting a failed sync, doubling with each
/// consecutive attempt and capped at a minute. The first attempt is
/// immediate (`last_sync_at` is `None`); after that a permanently-down host
/// is polled ever more slowly (1s, 2s, 4s … 60s) instead of on every tick,
/// while a recovered host still self-heals on the next attempt.
pub(crate) fn sync_retry_delay(attempts: u32) -> Duration {
    ExponentialBuilder::default()
        .without_max_times()
        .build()
        .nth(attempts.saturating_sub(1).min(6) as usize)
        .unwrap_or(Duration::from_mins(1))
}

/// Refresh the asset cache: the shared index always, plus the depots for
/// `models`. Returns `true` when the sync completed and `false` when it
/// failed and should be retried. Runs on a dedicated background thread —
/// the HTTP layer's blocking retries are fine here. (Whether sync runs at
/// all is the caller's gate: the automatic path checks `should_run` once at
/// startup plus the auto-download setting; the Settings → Assets manual
/// actions always fetch, even in a release build that would otherwise serve
/// only bundled art.)
pub(crate) fn run_asset_sync(
    preference: AssetSourcePreference,
    models: &[(DeviceModelInfo, Option<String>)],
) -> bool {
    let server = std::env::var("OPENLOGI_ASSETS").ok();
    let source = source_for_sync(preference, server.as_deref());
    match sync(source, models) {
        Ok(()) => true,
        Err(e) => {
            warn!(error = ?e, "asset sync failed — will retry with backoff");
            false
        }
    }
}

fn source_for_sync(
    preference: AssetSourcePreference,
    override_base: Option<&str>,
) -> Option<AssetSource> {
    if let Some(base) = override_base {
        return Some(AssetSource::Override(base.to_owned()));
    }
    match preference {
        AssetSourcePreference::Automatic => None,
        AssetSourcePreference::OpenLogi => Some(AssetSource::Production),
        AssetSourcePreference::Cloudflare => Some(AssetSource::Pages),
        AssetSourcePreference::Fastly => Some(AssetSource::JsDelivr),
    }
}

#[cfg(test)]
mod tests {
    use super::{source_for_sync, sync_retry_delay};
    use openlogi_assets::AssetSource;
    use openlogi_core::config::AssetSourcePreference;
    use std::time::Duration;

    #[test]
    fn retry_delay_doubles_then_caps() {
        assert_eq!(sync_retry_delay(1), Duration::from_secs(1));
        assert_eq!(sync_retry_delay(2), Duration::from_secs(2));
        assert_eq!(sync_retry_delay(3), Duration::from_secs(4));
        assert_eq!(sync_retry_delay(5), Duration::from_secs(16));
        // Caps at 60s and never overflows the shift for large attempt counts.
        assert_eq!(sync_retry_delay(7), Duration::from_mins(1));
        assert_eq!(sync_retry_delay(u32::MAX), Duration::from_mins(1));
    }

    #[test]
    fn automatic_preference_races_the_built_in_sources() {
        assert_eq!(
            source_for_sync(AssetSourcePreference::Automatic, None),
            None
        );
    }

    #[test]
    fn openlogi_preference_uses_the_official_source() {
        assert_eq!(
            source_for_sync(AssetSourcePreference::OpenLogi, None),
            Some(AssetSource::Production)
        );
    }

    #[test]
    fn fastly_preference_uses_the_shard_aware_jsdelivr_source() {
        assert_eq!(
            source_for_sync(AssetSourcePreference::Fastly, None),
            Some(AssetSource::JsDelivr)
        );
    }

    #[test]
    fn environment_override_takes_precedence_over_the_saved_source() {
        assert_eq!(
            source_for_sync(
                AssetSourcePreference::Cloudflare,
                Some("https://assets.example.test")
            ),
            Some(AssetSource::Override(
                "https://assets.example.test".to_owned()
            ))
        );
    }
}
