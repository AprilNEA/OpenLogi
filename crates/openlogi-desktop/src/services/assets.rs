//! Device asset resolution and cache management.
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

mod glow;
mod images;
mod paths;
pub(crate) mod queries;
pub mod sync;

pub(crate) use self::glow::GlowGeometry;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use openlogi_assets::http::safe_component_path;
use openlogi_assets::{
    BUTTONS_RENDER_FILES, DeviceEntry, FRONT_RENDER_FILES, Index, METADATA_FILES, Metadata,
};
use openlogi_core::device::{DeviceKind, DeviceModelInfo};
use tracing::{debug, warn};
use walkdir::WalkDir;

use self::images::{buttons_image_for, load_manifest, read_png_dimensions, variant_image_for};
use self::paths::{bundle_assets_root, load_index, user_cache_root};

/// Total bytes of the per-user asset cache — the tier [`sync`] writes and
/// [`clear_cache`] removes. The read-only app bundle (release builds) is a
/// separate tier and isn't counted.
#[must_use]
pub fn cache_size_bytes() -> u64 {
    WalkDir::new(user_cache_root())
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.metadata().map_or(0, |m| m.len()))
        .sum()
}

/// Delete the per-user asset cache. The next sync re-fetches what the
/// connected devices need; on a release build the bundled art keeps serving
/// in the meantime. A missing cache is treated as already clear.
pub fn clear_cache() -> std::io::Result<()> {
    match std::fs::remove_dir_all(user_cache_root()) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Remove the legacy pre-rendered keyboard glow overlays (`glow-<hex>.png`, plus
/// any `.tmp` left by an interrupted write) the old overlay path baked into each
/// depot's user-cache dir. The glow is painted live from the depot's run-mask
/// now, so these are dead bytes; sweep them once at startup. Best-effort — an
/// unreadable dir or undeletable file is skipped silently.
pub fn cleanup_legacy_glow_pngs() {
    cleanup_glow_pngs_in(&user_cache_root());
}

fn cleanup_glow_pngs_in(root: &Path) {
    for file in WalkDir::new(root)
        .min_depth(2)
        .max_depth(2)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let name = file.file_name().to_string_lossy();
        if name.starts_with("glow-") && (name.ends_with(".png") || name.ends_with(".png.tmp")) {
            let _ = std::fs::remove_file(file.path());
        }
    }
}

/// Reveal the asset cache directory in the OS file manager (Finder on macOS),
/// creating it first so there's something to open.
pub fn reveal_cache_in_file_manager() {
    let root = user_cache_root();
    if let Err(e) = std::fs::create_dir_all(&root) {
        warn!(error = %e, path = %root.display(), "could not create cache dir to reveal");
        return;
    }
    open_in_file_manager(&root);
}

/// Open `path` in the platform file manager. `opener` dispatches per OS
/// (Finder / Explorer / xdg-open), so no `#[cfg]` split — the old macOS-only
/// gating left the Settings → Assets "Open" button silently dead elsewhere.
fn open_in_file_manager(path: &Path) {
    if let Err(e) = opener::open(path) {
        warn!(error = %e, "could not open cache dir in the file manager");
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAsset {
    pub depot: String,
    pub display_name: String,
    /// The registry's curated device type for this model, normalized from the
    /// asset index `type` string. Per-model and human-maintained, so it's the
    /// most authoritative kind signal we have — the UI prefers it over the
    /// runtime HID++ classification when a device matched a known depot.
    /// `None` when the registry type was missing/unmodelled: no asset opinion.
    pub kind: Option<DeviceKind>,
    pub image_path: PathBuf,
    /// The front/hero render (`device_image`, typically `front_*.png`) used for
    /// the device gallery cards — distinct from [`Self::image_path`], which is
    /// the side/buttons view the mouse model aligns hotspots against. `None`
    /// when the depot ships no front render.
    pub hero_image_path: Option<PathBuf>,
    /// Precomputed inter-key lighting holes for a light-up keyboard, decoded
    /// from the depot's baked RLE mask and painted live over the device image
    /// (see [`crate::app::glow_canvas`]). `None` for depots without a mask.
    pub glow: Option<Arc<GlowGeometry>>,
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

    /// `true` when the asset index loaded; `false` means devices show the silhouette.
    pub fn index_loaded(&self) -> bool {
        self.index.is_some()
    }

    /// Number of device models in the loaded index, or `None` if no index loaded.
    pub fn index_entry_count(&self) -> Option<usize> {
        self.index.as_ref().map(|index| index.devices.len())
    }

    pub fn resolve(
        &self,
        model: &DeviceModelInfo,
        codename: Option<&str>,
    ) -> Option<ResolvedAsset> {
        let index = self.index.as_ref()?;
        let (depot, entry) = resolve_in_index(index, model, codename)?;
        self.load_files(depot, entry, model)
    }

    /// Resolve a standalone device directly by its registry model id.
    ///
    /// Standalone raw-HID devices do not expose a HID++ `DeviceModelInfo`, so
    /// constructing one just to reuse [`Self::resolve`] would conflate a
    /// physical protocol identity with a model-level asset identity. The
    /// registry lookup remains exact and case-insensitive, while all local
    /// filenames still pass through the same safe component checks.
    pub fn resolve_registry_model(&self, registry_model_id: &str) -> Option<ResolvedAsset> {
        let index = self.index.as_ref()?;
        let (depot, entry) = index.find_by_model_id(registry_model_id)?;
        self.load_standalone_files(depot, entry, registry_model_id)
    }

    fn load_files(
        &self,
        depot: &str,
        entry: &DeviceEntry,
        model: &DeviceModelInfo,
    ) -> Option<ResolvedAsset> {
        for root in &self.read_roots {
            let Ok(dir) = safe_component_path(root, depot, "asset depot") else {
                warn!(
                    depot,
                    "unsafe asset depot component — ignoring registry entry"
                );
                continue;
            };
            // Hotspot metadata in whichever schema this depot cached:
            // `core_metadata.json` (newer) or `metadata.json` (older).
            //
            // Two different situations look the same on disk, and only the
            // registry tells them apart. A depot that *publishes* hotspot
            // metadata but has none in this root is a stale or half-synced
            // cache: skip the root so the next one (or the synthetic fallback)
            // serves the device, rather than a render with no hotspots. A depot
            // that publishes none at all — cameras, whose `image_metadata` is a
            // per-PID `metadata_<pid>.json` of marker-less settings slots that
            // no hotspot consumer reads — legitimately resolves from its render
            // alone.
            let meta_name = METADATA_FILES.iter().find(|n| dir.join(n).exists());
            if meta_name.is_none() && entry.preferred_file(&METADATA_FILES).is_some() {
                continue;
            }

            // Pick the colour variant matching this device's HID++
            // extended_model_id byte. Logi calibrates the assignment
            // markers against the *buttons* image (typically
            // `side_*.png`), so we prefer that resource for the
            // mouse-model render — otherwise hotspot percentages drift
            // off every button. `front_*.png` is left for the gallery.
            //
            // The depot's manifest keys variants on one of its model ids,
            // which isn't always the index primary — the MX Master 3S
            // manifest is keyed on `2b034` while the index lists `2b043`
            // first. Try each listed id as the variant base so the right
            // colour render resolves regardless of which pid Logi keyed on.
            // Parse the manifest once and consult it for every candidate.
            let manifest = load_manifest(&dir);
            let buttons_name = manifest.as_ref().and_then(|m| {
                entry
                    .model_id_candidates()
                    .find_map(|base| buttons_image_for(m, base, model.extended_model_id))
            });
            let variant_front_name = manifest.as_ref().and_then(|m| {
                entry
                    .model_id_candidates()
                    .find_map(|base| variant_image_for(m, base, model.extended_model_id))
            });
            // Front/hero render for the gallery: the colour variant's
            // `device_image`, falling back to the generic front renders. Resolved
            // against this same root so it sits beside the buttons image.
            let hero_image_path = variant_front_name
                .clone()
                .into_iter()
                .chain(FRONT_RENDER_FILES.map(str::to_string))
                .filter_map(|n| safe_component_path(&dir, &n, "asset file").ok())
                .find(|p| p.exists());
            let image_name = buttons_name
                .clone()
                .or_else(|| variant_front_name.clone())
                .unwrap_or_else(|| "side_core.png".to_string());
            // The chosen file may not have been synced (older bundles
            // shipped front-only); fall back through alternatives so a
            // stale cache still gets *something* rather than a synthetic
            // silhouette. Both filename schemas (`*_core` and bare) are
            // tried for each of the buttons and hero renders.
            let mut candidates = vec![image_name.clone()];
            candidates.extend(BUTTONS_RENDER_FILES.map(str::to_string));
            candidates.extend(variant_front_name);
            candidates.extend(FRONT_RENDER_FILES.map(str::to_string));
            let Some(image_path) = candidates
                .iter()
                .filter_map(|n| safe_component_path(&dir, n, "asset file").ok())
                .find(|p| p.exists())
            else {
                continue;
            };

            let metadata = if let Some(&meta_name) = meta_name {
                Metadata::load_from(&dir.join(meta_name)).unwrap_or_else(|e| {
                    warn!(depot, root = %root.display(), file = meta_name, error = ?e, "device metadata unparseable — rendering image without hotspots");
                    Metadata::default()
                })
            } else {
                debug!(depot, root = %root.display(), "depot ships no hotspot metadata — rendering image without hotspots");
                Metadata::default()
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
            let kind = DeviceKind::from_registry_type(&entry.kind);
            // Only keyboards paint the inter-key glow, and the runtime
            // fallback decodes the full render — don't pay that for mice.
            let glow = (kind == Some(DeviceKind::Keyboard))
                .then(|| self::glow::resolve_glow_geometry(&dir, &image_path))
                .flatten()
                .map(Arc::new);
            return Some(ResolvedAsset {
                depot: depot.to_string(),
                display_name: entry.display_name.clone(),
                kind,
                image_path,
                hero_image_path,
                glow,
                metadata,
                png_width,
                png_height,
            });
        }
        debug!(depot, "asset cache miss across all roots");
        None
    }

    fn load_standalone_files(
        &self,
        depot: &str,
        entry: &DeviceEntry,
        registry_model_id: &str,
    ) -> Option<ResolvedAsset> {
        for root in &self.read_roots {
            let Ok(dir) = safe_component_path(root, depot, "asset depot") else {
                continue;
            };
            let manifest = load_manifest(&dir);
            let Some(image_name) = manifest
                .as_ref()
                .and_then(|manifest| manifest.device_image_for(registry_model_id))
                .or_else(|| entry.preferred_file(&FRONT_RENDER_FILES))
            else {
                continue;
            };
            let Ok(image_path) = safe_component_path(&dir, image_name, "asset file") else {
                continue;
            };
            if !image_path.is_file() {
                continue;
            }
            let Ok((png_width, png_height)) = read_png_dimensions(&image_path) else {
                continue;
            };
            debug!(
                depot,
                root = %root.display(),
                image = image_name,
                "standalone asset hit"
            );
            return Some(ResolvedAsset {
                depot: depot.to_owned(),
                display_name: entry.display_name.clone(),
                kind: DeviceKind::from_registry_type(&entry.kind),
                image_path: image_path.clone(),
                hero_image_path: Some(image_path),
                glow: None,
                // Standalone-light rendering intentionally consumes only the
                // verified front image; shared metadata remains for HID++
                // button hotspots in `load_files`.
                metadata: Metadata::default(),
                png_width,
                png_height,
            });
        }
        debug!(depot, "standalone asset cache miss across all roots");
        None
    }
}

impl Default for AssetResolver {
    fn default() -> Self {
        Self::new()
    }
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
/// 4. Firmware `codename` ↔ registry `displayName` (exact, case-insensitive).
///    Last resort for devices whose live PID is absent from the registry
///    under every transport — e.g. an MX Master 3S over BTLE reports model
///    id `b034`, but the registry keys the 3S as `2b043`; only the name
///    ("MX Master 3S") still lines up.
pub(crate) fn resolve_in_index<'a>(
    index: &'a Index,
    model: &DeviceModelInfo,
    codename: Option<&str>,
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
    if let Some(hit) = suffix.iter().find_map(|m| index.find_by_model_id_suffix(m)) {
        debug!(depot = hit.0, "asset matched via bolt-pid suffix fallback");
        return Some(hit);
    }

    // Last resort: bridge by firmware codename ↔ registry displayName.
    let name = codename?;
    let hit = index.find_by_display_name(name)?;
    debug!(
        depot = hit.0,
        codename = name,
        "asset matched via codename↔displayName fallback"
    );
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

#[cfg(test)]
mod tests;
