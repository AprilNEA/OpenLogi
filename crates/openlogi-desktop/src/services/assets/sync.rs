//! Background sync against OpenLogi's asset mirrors.
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

/// One model-level asset lookup requested by the GUI.
///
/// A HID++ entry pairs the device's model info with its firmware `codename`,
/// so the depot match can fall back to the registry `displayName` for devices
/// whose live PID isn't in the registry (e.g. an MX Master 3S over BTLE).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AssetTarget {
    /// HID++ lookup, retaining the existing codename/variant behavior.
    Hidpp {
        /// HID++ model information used for registry matching.
        model: DeviceModelInfo,
        /// Firmware codename used as the final matching fallback.
        codename: Option<String>,
    },
    /// Standalone raw-HID lookup by the driver-provided registry identity.
    Standalone {
        /// Exact model-level registry id, never a physical-device key.
        registry_model_id: String,
    },
}

/// Stable session key for one model-level asset target.
#[must_use]
pub(crate) fn model_key(target: &AssetTarget) -> String {
    match target {
        AssetTarget::Hidpp { model, codename } => format!(
            "hidpp:{:02x}:{:04x}:{:04x}:{:04x}:{}",
            model.extended_model_id,
            model.model_ids[0],
            model.model_ids[1],
            model.model_ids[2],
            codename.as_deref().unwrap_or_default()
        ),
        AssetTarget::Standalone { registry_model_id } => {
            format!("standalone:model:{registry_model_id}")
        }
    }
}

/// Refresh the local cache: probe the built-in mirrors (or use the selected
/// source), persist the selected source's `index.json`, then sync the depots
/// for every entry in `targets`. An empty `targets` is a valid call — it
/// prefetches just the index so device resolution works the moment a device
/// first appears.
/// Probe the mirrors (or use the selected source), persist the winner's
/// `index.json`, and hand back the registry.
///
/// Separate from [`sync_target`] because the registry is the one resource every
/// target shares: probing three mirrors once per device would be the whole cost
/// of the sync paid N times. Cached under its own key so the depot fetches
/// depend on it instead of each redoing it.
pub fn load_registry(source: Option<AssetSource>) -> Result<AssetRegistry> {
    let cache_root = super::paths::user_cache_root();
    fs::create_dir_all(&cache_root)
        .with_context(|| format!("create cache root {}", cache_root.display()))?;
    let registry = AssetRegistry::load_source(source, &cache_root).context("fetch asset index")?;
    // `OPENLOGI_FORCE_DEPOT` names a depot with no device behind it, so it has
    // no target to ride in on. It belongs to whoever loads the registry — and
    // `ext = 0` gets its base PNG.
    if let Ok(forced) = std::env::var(openlogi_core::env::FORCE_DEPOT)
        && let Some(entry) = registry.index().devices.get(&forced).cloned()
        && let Err(e) = sync_depot(registry.client(), &cache_root, &forced, &entry, 0)
    {
        warn!(depot = %forced, error = %e, "forced depot sync failed");
    }
    Ok(registry)
}

/// Download what one device model needs, against an already-loaded registry.
///
/// A model the registry doesn't list is not an error — that device renders from
/// the fallback art. Per-depot failures stay best-effort for the same reason
/// they always did: an optional colour variant 404 must not fail the model.
pub fn sync_target(registry: &AssetRegistry, target: &AssetTarget) -> Result<()> {
    let cache_root = super::paths::user_cache_root();
    let index = registry.index();
    // Each target carries the HID++ `extended_model_id` byte so the depot sync
    // can fetch the right colour variant.
    let resolved = match target {
        AssetTarget::Hidpp { model, codename } => {
            super::resolve_in_index(index, model, codename.as_deref())
                .map(|(depot, entry)| (depot, entry, model.extended_model_id))
        }
        AssetTarget::Standalone { registry_model_id } => index
            .find_by_model_id(registry_model_id)
            .map(|(depot, entry)| (depot, entry, 0)),
    };
    let Some((depot, entry, ext)) = resolved else {
        if let AssetTarget::Standalone { registry_model_id } = target {
            info!(
                registry_model_id,
                "standalone model is not registered — using fallback art"
            );
        } else {
            debug!("sync: no matching depot for this model");
        }
        return Ok(());
    };
    sync_depot(registry.client(), &cache_root, depot, entry, ext)
        .with_context(|| format!("sync depot {depot}"))
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
    let mut fetched = entry.baseline_files();
    for name in &fetched {
        fetch_to_cache(client, &entry.asset_path, &dir, entry, name)?;
    }

    // Dedicated buttons render — only present on devices whose manifest
    // points `device_buttons_image` at a distinct side view. Fetch it only
    // when the registry lists it so front-only devices don't 404; failure
    // is non-fatal (the GUI falls back to the hero render).
    if let Some(side) = entry.preferred_file(&BUTTONS_RENDER_FILES) {
        match fetch_to_cache(client, &entry.asset_path, &dir, entry, side) {
            Ok(()) => fetched.push(side),
            Err(e) => warn!(depot, error = %e, "buttons render fetch failed"),
        }
    }

    // Optional second pass: download every file the manifest names for the
    // colour variant matching the device's `extended_model_id` — the front
    // (gallery) and side / buttons (mouse-model) views, the camera hero
    // (camera depots ship no bare `front*.png`, so the baseline fetch above
    // brings no render for them at all), and the variant's hotspot metadata
    // (the only place a depot with handed variants names its metadata file).
    // Failure is non-fatal — `AssetResolver.load_files` falls back to
    // whatever landed.
    for variant in variant_filenames(&dir.join("manifest.json"), entry, depot, ext) {
        if fetched.contains(&variant.as_str()) {
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

/// Parse a freshly-downloaded `manifest.json` and name every file the colour
/// variant `ext` draws or reads ([`DepotManifest::consumed_srcs_for`]).
/// `ext == 0` names the base model's files — needed for depots whose base
/// render isn't a baseline `front*.png` (the caller's skip list keeps
/// already-fetched baseline names from re-fetching). Empty when the manifest
/// is missing, malformed, or lacks the variant.
///
/// Every model id the depot answers to is tried as the variant base, the
/// same way the resolver does: a manifest is keyed on whichever pid Logi
/// authored it against, which isn't always the index primary.
fn variant_filenames(
    manifest_path: &Path,
    entry: &DeviceEntry,
    depot: &str,
    ext: u8,
) -> Vec<String> {
    if !manifest_path.exists() {
        return Vec::new();
    }
    let Ok(manifest) = DepotManifest::load_from(manifest_path)
        .map_err(|e| warn!(error = %e, path = %manifest_path.display(), "manifest unreadable"))
    else {
        return Vec::new();
    };
    let bases = super::images::candidate_manifest_bases(entry, depot);
    manifest
        .consumed_srcs_for(&bases, ext)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// A manual asset action requested from the Settings → Assets tab, pushed to
/// the main event loop via [`AssetControl`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// The source one sync session should use: an `OPENLOGI_ASSETS` override wins,
/// otherwise the user's saved preference.
pub(crate) fn selected_source(preference: AssetSourcePreference) -> Option<AssetSource> {
    let server = std::env::var(openlogi_core::env::ASSETS).ok();
    source_for_sync(preference, server.as_deref())
}

/// A stable name for `preference`, for callers that key something on which
/// source a fetch will use. Distinct per variant is the only requirement; the
/// env override is not folded in because it cannot change while running.
pub(crate) fn source_segment(preference: AssetSourcePreference) -> &'static str {
    match preference {
        AssetSourcePreference::Automatic => "automatic",
        AssetSourcePreference::OpenLogi => "openlogi",
        AssetSourcePreference::Cloudflare => "cloudflare",
        AssetSourcePreference::Fastly => "fastly",
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
    use super::{AssetTarget, model_key, source_for_sync};
    use openlogi_assets::AssetSource;
    use openlogi_core::config::AssetSourcePreference;

    #[test]
    fn g512_sync_fetches_metadata_not_in_the_baseline() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};

        let root = tempfile::tempdir().expect("asset root");
        let dir = root.path().join("g513");
        std::fs::create_dir(&dir).unwrap();
        let metadata = br#"{"images":[]}"#;
        let mut entry = openlogi_assets::DeviceEntry {
            model_id: "c33c".into(),
            model_ids: Vec::new(),
            display_name: "G513".into(),
            kind: "keyboard".into(),
            asset_path: "assets/g513/".into(),
            files: Vec::new(),
        };
        for (name, bytes) in [
            ("manifest.json", br#"{"devices":[{"modelId":"g512_ext6","resources":[{"key":"image_metadata","src":"metadata.json"}]}]}"#.as_slice()),
            ("metadata_full.json", br#"{"images":[]}"#.as_slice()),
            ("metadata.json", metadata.as_slice()),
        ] {
            entry.files.push(openlogi_assets::FileEntry {
                name: name.into(),
                sha256: openlogi_assets::http::sha256_hex(bytes),
                bytes: bytes.len() as u64,
            });
            if name != "metadata.json" {
                std::fs::write(dir.join(name), bytes).unwrap();
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while stream.read(&mut byte).unwrap() != 0 {
                request.push(byte[0]);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            if !request.is_empty() {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    metadata.len()
                )
                .unwrap();
                stream.write_all(metadata).unwrap();
            }
            request
        });
        let client = openlogi_assets::http::AssetClient::new(&format!("http://{address}"));
        super::sync_depot(&client, root.path(), "g513", &entry, 6).unwrap();
        // Release the server even when a regression skips the HTTP request.
        drop(TcpStream::connect(address));
        let request = server.join().unwrap();
        assert!(request.starts_with(b"GET /assets/g513/metadata.json "));
        assert_eq!(std::fs::read(dir.join("metadata.json")).unwrap(), metadata);
    }

    /// #2457: the white G502 X PLUS (`ext1`) synced only the baseline. Its
    /// manifest names the variant renders `images/device/<uuid>`, and the
    /// fetch rejected every name that was not a single path component.
    #[test]
    fn g502x_plus_sync_fetches_variant_renders_under_images_device() {
        use std::collections::HashMap;
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};

        const FRONT: &str = "images/device/e0e9dacc-b5c0-4434-8b8d-410a7aa81d2f";
        const SIDE: &str = "images/device/22c9ac7a-43ed-48b7-8771-72ca4ae2a6bb";
        let root = tempfile::tempdir().expect("asset root");
        let dir = root.path().join("g502x_plus");
        std::fs::create_dir(&dir).unwrap();
        let manifest = format!(
            r#"{{"devices":[{{"modelId":"g502x_plus_ext1","resources":[
                {{"key":"device_image","src":"{FRONT}"}},
                {{"key":"device_side","src":"{SIDE}"}}]}}]}}"#
        );
        let mut entry = openlogi_assets::DeviceEntry {
            model_id: "c095".into(),
            model_ids: vec!["c095".into(), "4099".into()],
            display_name: "G502 X PLUS".into(),
            kind: "MOUSE".into(),
            asset_path: "v1/devices/g502x_plus/".into(),
            files: Vec::new(),
        };
        // The baseline is already cached, so only the renders go on the wire.
        let mut bodies = HashMap::new();
        for (name, bytes) in [
            ("manifest.json", manifest.into_bytes()),
            ("metadata.json", br#"{"images":[]}"#.to_vec()),
            ("front.png", b"black front".to_vec()),
            (FRONT, b"white front".to_vec()),
            (SIDE, b"white side".to_vec()),
        ] {
            entry.files.push(openlogi_assets::FileEntry {
                name: name.into(),
                sha256: openlogi_assets::http::sha256_hex(&bytes),
                bytes: bytes.len() as u64,
            });
            if name.contains('/') {
                bodies.insert(format!("/v1/devices/g502x_plus/{name}"), bytes);
            } else {
                std::fs::write(dir.join(name), bytes).unwrap();
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let expected = bodies.len();
        let server = std::thread::spawn(move || {
            let mut paths = Vec::new();
            for _ in 0..expected {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while stream.read(&mut byte).unwrap() != 0 {
                    request.push(byte[0]);
                    if request.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8(request).unwrap();
                let Some(path) = request.split(' ').nth(1) else {
                    continue;
                };
                let body = bodies.get(path).cloned().unwrap_or_default();
                let status = if bodies.contains_key(path) {
                    "200 OK"
                } else {
                    "404 Not Found"
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
                paths.push(path.to_owned());
            }
            paths
        });
        let client = openlogi_assets::http::AssetClient::new(&format!("http://{address}"));
        super::sync_depot(&client, root.path(), "g502x_plus", &entry, 1).unwrap();
        // Release the server even when a regression skips the HTTP requests.
        for _ in 0..expected {
            drop(TcpStream::connect(address));
        }
        let mut paths = server.join().unwrap();
        paths.sort();

        assert_eq!(
            paths,
            [
                format!("/v1/devices/g502x_plus/{SIDE}"),
                format!("/v1/devices/g502x_plus/{FRONT}"),
            ]
        );
        assert_eq!(std::fs::read(dir.join(FRONT)).unwrap(), b"white front");
        assert_eq!(std::fs::read(dir.join(SIDE)).unwrap(), b"white side");
    }

    #[test]
    fn g512_photo_and_metadata_are_selected_for_download() {
        let root = tempfile::tempdir().expect("asset root");
        let manifest = root.path().join("manifest.json");
        std::fs::write(
            &manifest,
            r#"{"devices":[{"modelId":"g512_ext6","resources":[{"key":"device_image","src":"front_ext6.png"},{"key":"image_metadata","src":"metadata.json"}]}]}"#,
        )
        .unwrap();
        let entry = openlogi_assets::DeviceEntry {
            model_id: "c33c".into(),
            model_ids: Vec::new(),
            display_name: "G513".into(),
            kind: "keyboard".into(),
            asset_path: "assets/g513/".into(),
            files: Vec::new(),
        };
        assert_eq!(
            super::variant_filenames(&manifest, &entry, "g513", 6),
            ["front_ext6.png", "metadata.json"]
        );
        assert!(super::variant_filenames(&manifest, &entry, "g513", 5).is_empty());
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

    #[test]
    fn standalone_target_key_is_model_scoped_not_physical() {
        assert_eq!(
            model_key(&AssetTarget::Standalone {
                registry_model_id: "8c900".into(),
            }),
            "standalone:model:8c900"
        );
    }
}
