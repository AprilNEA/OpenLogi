//! Background HTTP sync against `assets.openlogi.org`.
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

use anyhow::{Context as _, Result};
use openlogi_assets::http;
use openlogi_assets::{BUTTONS_RENDER_FILES, DepotManifest, DeviceEntry, FetchOutcome};
use openlogi_core::device::DeviceModelInfo;
use tracing::{debug, info, warn};

/// Default origin for asset fetches. Overridable via `OPENLOGI_ASSETS`
/// so dev / staging deployments can point elsewhere without a rebuild.
pub const DEFAULT_BASE: &str = "https://assets.openlogi.org";

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

/// Refresh the local cache: the shared `index.json` unconditionally, then
/// the depots for every model in `models`. An empty `models` is a valid
/// call — it prefetches just the index so device resolution works the
/// moment a device first appears.
///
/// Each entry pairs a device's HID++ model info with its firmware `codename`,
/// so the depot match can fall back to the registry `displayName` for devices
/// whose live PID isn't in the registry (e.g. an MX Master 3S over BTLE).
pub fn sync(server: &str, models: &[(DeviceModelInfo, Option<String>)]) -> Result<()> {
    let cache_root = super::paths::user_cache_root();
    fs::create_dir_all(&cache_root)
        .with_context(|| format!("create cache root {}", cache_root.display()))?;

    let client = http::AssetClient::new(server);
    // The index is the critical shared resource — if it can't be fetched
    // (after the HTTP layer's own retries) bail with an error so the caller
    // retries the whole sync on a later device snapshot, rather than latching
    // success off a run that downloaded nothing. Per-depot failures below stay
    // best-effort: an optional colour variant 404 shouldn't block everything.
    let index = client
        .fetch_index_to_dir(&cache_root)
        .context("fetch asset index")?;

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
        if let Some((depot, entry)) = super::resolve_in_index(&index, model, codename.as_deref()) {
            targets.push((depot.to_string(), entry.clone(), model.extended_model_id));
        }
    }
    targets.sort_by(|a, b| a.0.cmp(&b.0));
    targets.dedup_by(|a, b| a.0 == b.0);

    if targets.is_empty() {
        debug!("sync: no matching depots for connected devices");
        return Ok(());
    }

    for (depot, entry, ext) in &targets {
        if let Err(e) = sync_depot(&client, &cache_root, depot, entry, *ext) {
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
