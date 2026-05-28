//! Startup-time HTTP sync against `assets.openlogi.org`.
//!
//! Runs **before** the GUI opens. For each connected device with a
//! [`DeviceModelInfo`], resolves the matching depot from the freshly-
//! fetched `index.json`, then downloads any per-device files we don't
//! already have cached (or whose sha256 differs). Failures are logged
//! and swallowed — the GUI falls back to whatever's currently on disk
//! and ultimately to the synthetic silhouette.

use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result};
use openlogi_assets::http;
use openlogi_assets::{DepotManifest, DeviceEntry, variant_model_id};
use openlogi_core::device::DeviceModelInfo;
use tracing::{debug, info, warn};

use super::AssetCache;

/// Default origin for asset fetches. Overridable via `OPENLOGI_ASSETS`
/// so dev / staging deployments can point elsewhere without a rebuild.
pub const DEFAULT_BASE: &str = "https://assets.openlogi.org";

/// Baseline files always fetched per depot. `AssetCache` opens
/// `front_core.png` + `core_metadata.json`; `manifest.json` drives the
/// HID++ `extended_model_id` → colour variant lookup. The variant PNG
/// itself is fetched in a second pass after the manifest is on disk.
const FETCH_FILES: &[&str] = &["core_metadata.json", "manifest.json", "front_core.png"];

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

/// Refresh the local cache for every model the host can plausibly want.
pub fn sync(server: &str, models: &[DeviceModelInfo]) -> Result<()> {
    let cache_root = AssetCache::new().cache_root().to_path_buf();
    fs::create_dir_all(&cache_root)
        .with_context(|| format!("create cache root {}", cache_root.display()))?;

    let index = match http::fetch_index_to_dir(server, &cache_root) {
        Ok(idx) => idx,
        Err(e) => {
            warn!(error = ?e, "index.json fetch failed — proceeding with cached files");
            return Ok(());
        }
    };

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
    for model in models {
        if let Some((depot, entry)) = super::resolve_in_index(&index, model) {
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
        if let Err(e) = sync_depot(server, &cache_root, depot, entry, *ext) {
            warn!(depot, error = %e, "depot sync failed");
        }
    }
    info!(devices = targets.len(), "asset sync complete");
    Ok(())
}

fn sync_depot(
    server: &str,
    cache_root: &Path,
    depot: &str,
    entry: &DeviceEntry,
    ext: u8,
) -> Result<()> {
    let dir = cache_root.join(depot);
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

    // Baseline: metadata + manifest + base PNG. Manifest is mandatory
    // so the variant lookup below has something to consult.
    for name in FETCH_FILES {
        fetch_to_cache(server, &entry.asset_path, &dir, entry, name)?;
    }

    // Optional second pass: download the colour variant PNG matching the
    // connected device's `extended_model_id`. Failure is non-fatal —
    // `AssetCache.load_files` falls back to `front_core.png`.
    if let Some(variant) = pick_variant_filename(&dir.join("manifest.json"), &entry.model_id, ext)
        && variant != "front_core.png"
    {
        if let Err(e) = fetch_to_cache(server, &entry.asset_path, &dir, entry, &variant) {
            warn!(depot, variant = %variant, error = %e, "variant fetch failed");
        }
    }
    Ok(())
}

/// Fetch a single named file from `<server>/<asset_path>/<name>` into
/// `<dir>/<name>`. SHA-checked against `entry.files`; missing registry
/// entry trips a warn but doesn't bail (some depots ship one-off files
/// not yet rolled into the registry).
fn fetch_to_cache(
    server: &str,
    asset_path: &str,
    dir: &Path,
    entry: &DeviceEntry,
    name: &str,
) -> Result<()> {
    let dst = dir.join(name);
    if let Some(file_entry) = entry.files.iter().find(|f| f.name == name) {
        if http::cached_matches(&dst, &file_entry.sha256) {
            debug!(file = name, "cache hit");
            return Ok(());
        }
    } else {
        warn!(
            file = name,
            "registry lists no entry — fetching without sha verify"
        );
    }
    let bytes = http::fetch_file_to_dir(server, asset_path, dir, name)?;
    info!(file = name, bytes, "downloaded");
    Ok(())
}

/// Parse a freshly-downloaded `manifest.json` and resolve the colour
/// variant filename. `None` when the manifest is missing, malformed,
/// or doesn't list the device's `ext` byte.
fn pick_variant_filename(manifest_path: &Path, base_model_id: &str, ext: u8) -> Option<String> {
    if ext == 0 || !manifest_path.exists() {
        return None;
    }
    let manifest = DepotManifest::load_from(manifest_path)
        .map_err(|e| warn!(error = %e, path = %manifest_path.display(), "manifest unreadable"))
        .ok()?;
    let variant = variant_model_id(base_model_id, ext);
    manifest.device_image_for(&variant).map(str::to_string)
}
