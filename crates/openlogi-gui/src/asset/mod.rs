//! Render-time device→asset resolver, backed by a two-tier filesystem cache.
//!
//! At render time [`AssetResolver::resolve`] probes (in order):
//!
//! 1. The macOS app bundle's `Contents/Resources/assets/` — populated at
//!    packaging time by `openlogi assets sync` and shipped with every
//!    release. Zero network at end-user runtime.
//! 2. The per-user cache at `~/.local/share/openlogi/assets/` —
//!    populated by [`sync::sync`] when it runs (debug builds and the
//!    bundle-missing safety net).
//!
//! Either tier missing the requested files falls through to the next, and
//! ultimately to the synthetic silhouette. The write side ([`sync::sync`])
//! always targets the user cache — the bundle is read-only.

pub mod sync;

use std::path::{Path, PathBuf};

use openlogi_assets::{DepotManifest, DeviceEntry, Index, Metadata};
use openlogi_core::device::DeviceModelInfo;
use tracing::{debug, warn};

const INDEX_FILE: &str = "index.json";

#[derive(Debug, Clone)]
pub struct ResolvedAsset {
    #[allow(
        dead_code,
        reason = "depot label will be surfaced in the carousel tooltip (P0.4+)"
    )]
    pub depot: String,
    pub display_name: String,
    pub image_path: PathBuf,
    pub metadata: Metadata,
    /// Actual pixel dimensions of `image_path`. Logi's
    /// `core_metadata.json` `origin` field tracks the *bbox of the mouse
    /// silhouette inside* the PNG — the PNG ships with extra transparent
    /// padding on the sides. Without the real PNG size we can't tell
    /// where that padding lives, and hotspot percentages drift off the
    /// real buttons.
    pub png_width: u32,
    pub png_height: u32,
}

pub struct AssetResolver {
    /// Read-time search order. Bundle root (if present) comes first so
    /// release builds never touch the user cache; the user cache comes
    /// second so `sync::sync` writes are immediately visible.
    read_roots: Vec<PathBuf>,
    /// Where [`sync::sync`] is allowed to write. Always the per-user dir
    /// — the bundle is read-only inside the signed `.app`.
    write_root: PathBuf,
    /// `true` when a populated bundle root was discovered; release builds
    /// skip the network sync in that case.
    has_bundle: bool,
    index: Option<Index>,
}

impl AssetResolver {
    pub fn new() -> Self {
        let write_root = user_cache_root();
        let bundle = bundle_assets_root();
        let has_bundle = bundle.is_some();
        let mut read_roots = Vec::with_capacity(2);
        if let Some(b) = bundle {
            debug!(path = %b.display(), "bundle assets root detected");
            read_roots.push(b);
        }
        read_roots.push(write_root.clone());
        let index = load_index(&read_roots);
        Self {
            read_roots,
            write_root,
            has_bundle,
            index,
        }
    }

    /// Where [`sync::sync`] writes. Public so the sync module can build
    /// destination paths.
    pub fn cache_root(&self) -> &Path {
        &self.write_root
    }

    /// `true` when the binary is running from a populated app bundle.
    pub fn has_bundle_root(&self) -> bool {
        self.has_bundle
    }

    pub fn resolve(&self, model: &DeviceModelInfo) -> Option<ResolvedAsset> {
        let index = self.index.as_ref()?;
        let (depot, entry) = resolve_in_index(index, model)?;
        self.load_files(depot, entry, model)
    }

    fn load_files(
        &self,
        depot: &str,
        entry: &DeviceEntry,
        model: &DeviceModelInfo,
    ) -> Option<ResolvedAsset> {
        for root in &self.read_roots {
            let dir = root.join(depot);
            let meta_path = dir.join("core_metadata.json");
            if !meta_path.exists() {
                continue;
            }

            // Pick the colour variant matching this device's HID++
            // extended_model_id byte. Logi calibrates the assignment
            // markers against the *buttons* image (typically
            // `side_*.png`), so we prefer that resource for the
            // mouse-model render — otherwise hotspot percentages drift
            // off every button. `front_*.png` is left for the carousel.
            let buttons_name = buttons_image_for(&dir, &entry.model_id, model.extended_model_id);
            let variant_front_name =
                variant_image_for(&dir, &entry.model_id, model.extended_model_id);
            let image_name = buttons_name
                .clone()
                .or_else(|| variant_front_name.clone())
                .unwrap_or_else(|| "side_core.png".to_string());
            // The chosen file may not have been synced (older bundles
            // shipped front-only); fall back through alternatives so a
            // stale cache still gets *something* rather than a synthetic
            // silhouette.
            let candidates = [
                image_name.clone(),
                "side_core.png".to_string(),
                variant_front_name.unwrap_or_default(),
                "front_core.png".to_string(),
            ];
            let Some(image_path) = candidates
                .iter()
                .filter(|n| !n.is_empty())
                .map(|n| dir.join(n))
                .find(|p| p.exists())
            else {
                continue;
            };

            let metadata = match Metadata::load_from(&meta_path) {
                Ok(m) => m,
                Err(e) => {
                    warn!(depot, root = %root.display(), error = ?e, "failed to parse core_metadata.json");
                    continue;
                }
            };
            let (png_width, png_height) = match read_png_dimensions(&image_path) {
                Ok(dims) => dims,
                Err(e) => {
                    warn!(
                        path = %image_path.display(),
                        error = %e,
                        "could not read PNG dimensions — falling back to metadata origin"
                    );
                    let origin = metadata.origin();
                    (
                        origin.map_or(0, |o| o.width),
                        origin.map_or(0, |o| o.height),
                    )
                }
            };
            debug!(
                depot,
                root = %root.display(),
                image = %image_name,
                ext = model.extended_model_id,
                png_width,
                png_height,
                "asset hit"
            );
            return Some(ResolvedAsset {
                depot: depot.to_string(),
                display_name: entry.display_name.clone(),
                image_path,
                metadata,
                png_width,
                png_height,
            });
        }
        debug!(depot, "asset cache miss across all roots");
        None
    }
}

/// Read width + height from a PNG's `IHDR` chunk.
///
/// PNG layout: 8-byte signature, then chunks. The first chunk is always
/// `IHDR` per the spec, located at bytes 12–24: 4 bytes length, 4 bytes
/// type tag, then the data. The first 8 data bytes are width + height as
/// big-endian u32s. We only need those 24 leading bytes — much cheaper
/// than decoding the whole image.
fn read_png_dimensions(path: &Path) -> std::io::Result<(u32, u32)> {
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};
    const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut file = File::open(path)?;
    let mut header = [0u8; 24];
    file.read_exact(&mut header)?;
    if header[0..8] != PNG_SIGNATURE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "missing PNG signature",
        ));
    }
    if &header[12..16] != b"IHDR" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "missing IHDR chunk",
        ));
    }
    let width = u32::from_be_bytes([header[16], header[17], header[18], header[19]]);
    let height = u32::from_be_bytes([header[20], header[21], header[22], header[23]]);
    // Re-seek to start so any later reader sees the full file.
    file.seek(SeekFrom::Start(0))?;
    Ok((width, height))
}

/// Walk the depot's `manifest.json` (if present) for the colour
/// variant matching `ext`. Returns the `device_image` src filename or
/// `None` when the manifest is missing / malformed / lacks the variant.
fn variant_image_for(dir: &Path, base_model_id: &str, ext: u8) -> Option<String> {
    let manifest = load_manifest(dir)?;
    manifest
        .resource_for_variant(base_model_id, ext, "device_image")
        .map(str::to_string)
}

/// Like [`variant_image_for`] but returns the `device_buttons_image`
/// resource (typically `side_*.png`) — that's the view Logi calibrates
/// the assignment markers against, so the mouse-model render uses it.
fn buttons_image_for(dir: &Path, base_model_id: &str, ext: u8) -> Option<String> {
    let manifest = load_manifest(dir)?;
    manifest
        .resource_for_variant(base_model_id, ext, "device_buttons_image")
        .map(str::to_string)
}

fn load_manifest(dir: &Path) -> Option<DepotManifest> {
    let manifest_path = dir.join("manifest.json");
    if !manifest_path.exists() {
        return None;
    }
    DepotManifest::load_from(&manifest_path)
        .map_err(
            |e| warn!(error = ?e, path = %manifest_path.display(), "depot manifest unreadable"),
        )
        .ok()
}

impl Default for AssetResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-user writable cache root: `openlogi_core::paths::data_dir()` plus an
/// `assets/` subdir, keeping the render cache out of the config dir. Falls
/// back to `./assets` only when no home directory can be resolved.
fn user_cache_root() -> PathBuf {
    openlogi_core::paths::data_dir()
        .map_or_else(|_| PathBuf::from("./assets"), |d| d.join("assets"))
}

/// Read-only root pointing inside the macOS `.app` bundle when the binary
/// is launched from one: `<exe_dir>/../Resources/assets/`. The probe also
/// requires an `index.json` inside — an empty dir (e.g. `cargo bundle`
/// run without first syncing) is treated as not-present so the runtime
/// HTTP fallback can still recover.
fn bundle_assets_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.parent()?.join("Resources").join("assets");
    candidate.join(INDEX_FILE).is_file().then_some(candidate)
}

/// Walk read roots looking for the first parseable `index.json`. Bundle
/// wins over user cache so a release-time snapshot stays authoritative.
fn load_index(roots: &[PathBuf]) -> Option<Index> {
    for root in roots {
        let path = root.join(INDEX_FILE);
        if !path.exists() {
            continue;
        }
        match Index::load_from(&path) {
            Ok(idx) => {
                debug!(
                    devices = idx.devices.len(),
                    root = %root.display(),
                    "asset index loaded"
                );
                return Some(idx);
            }
            Err(e) => {
                warn!(error = ?e, root = %root.display(), "failed to parse asset index");
            }
        }
    }
    debug!("no asset index found — using synthetic silhouette for all devices");
    None
}

/// Match a connected device's HID++ model info against a loaded index,
/// returning the depot name + entry without touching the filesystem.
///
/// Match order:
/// 1. `OPENLOGI_FORCE_DEPOT` env override (dev convenience).
/// 2. Strict `{ext:x}{bolt_pid:04x}` against registry `modelId`.
/// 3. Suffix match on the bare bolt PID — covers devices like MX
///    Master 4 where Logi's registry prefix doesn't line up with HID++
///    `extended_model_id` (registry: `"2b042"`, device reports
///    `ext=01 + b042`). Safe in practice because Logitech reserves PID
///    ranges per product family.
pub(crate) fn resolve_in_index<'a>(
    index: &'a Index,
    model: &DeviceModelInfo,
) -> Option<(&'a str, &'a DeviceEntry)> {
    if let Ok(forced) = std::env::var("OPENLOGI_FORCE_DEPOT")
        && let Some((depot, entry)) = index
            .devices
            .iter()
            .find(|(d, _)| *d == &forced)
            .map(|(d, e)| (d.as_str(), e))
    {
        debug!(depot, "OPENLOGI_FORCE_DEPOT override active");
        return Some((depot, entry));
    }
    let strict = strict_candidates(model);
    if let Some((depot, entry)) = strict.iter().find_map(|m| index.find_by_model_id(m)) {
        return Some((depot, entry));
    }
    let suffix = suffix_candidates(model);
    let hit = suffix
        .iter()
        .find_map(|m| index.find_by_model_id_suffix(m))?;
    debug!(depot = hit.0, "asset matched via bolt-pid suffix fallback");
    Some(hit)
}

fn strict_candidates(model: &DeviceModelInfo) -> Vec<String> {
    model
        .model_ids
        .iter()
        .filter(|id| **id != 0)
        .map(|id| format!("{:x}{:04x}", model.extended_model_id, id))
        .collect()
}

fn suffix_candidates(model: &DeviceModelInfo) -> Vec<String> {
    model
        .model_ids
        .iter()
        .filter(|id| **id != 0)
        .map(|id| format!("{id:04x}"))
        .collect()
}
