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

mod glow;
mod images;
mod paths;
pub mod sync;

pub(crate) use self::glow::GlowGeometry;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use openlogi_assets::{
    BUTTONS_RENDER_FILES, DeviceEntry, FRONT_RENDER_FILES, Index, METADATA_FILES, Metadata,
};
use openlogi_core::device::{DeviceKind, DeviceModelInfo};
use tracing::{debug, warn};

use self::images::{buttons_image_for, load_manifest, read_png_dimensions, variant_image_for};
use self::paths::{bundle_assets_root, load_index, user_cache_root};

/// Total bytes of the per-user asset cache — the tier [`sync`] writes and
/// [`clear_cache`] removes. The read-only app bundle (release builds) is a
/// separate tier and isn't counted.
#[must_use]
pub fn cache_size_bytes() -> u64 {
    fn dir_size(dir: &Path) -> u64 {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        entries
            .flatten()
            .map(|entry| match entry.file_type() {
                Ok(ft) if ft.is_dir() => dir_size(&entry.path()),
                Ok(_) => entry.metadata().map_or(0, |m| m.len()),
                Err(_) => 0,
            })
            .sum()
    }
    dir_size(&user_cache_root())
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
    let Ok(depots) = std::fs::read_dir(root) else {
        return;
    };
    for depot in depots.flatten() {
        if !depot.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(depot.path()) else {
            continue;
        };
        for file in files.flatten() {
            let name = file.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("glow-") && (name.ends_with(".png") || name.ends_with(".png.tmp")) {
                let _ = std::fs::remove_file(file.path());
            }
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

/// Open `path` in the platform file manager. Only macOS has a reveal command
/// wired up today; elsewhere this is a no-op. (Split out so the early return
/// above isn't the function's last statement on non-macOS — `needless_return`.)
#[cfg(target_os = "macos")]
fn open_in_file_manager(path: &Path) {
    if let Err(e) = std::process::Command::new("open").arg(path).spawn() {
        warn!(error = %e, "could not open cache dir in Finder");
    }
}

#[cfg(not(target_os = "macos"))]
fn open_in_file_manager(_path: &Path) {}

#[derive(Debug, Clone)]
pub struct ResolvedAsset {
    #[allow(
        dead_code,
        reason = "depot label will be surfaced in the carousel tooltip (P0.4+)"
    )]
    pub depot: String,
    pub display_name: String,
    /// The registry's curated device type for this model, normalized from the
    /// asset index `type` string. Per-model and human-maintained, so it's the
    /// most authoritative kind signal we have — the UI prefers it over the
    /// runtime HID++ classification when a device matched a known depot.
    /// [`DeviceKind::Unknown`] when the registry type was missing/unmodelled.
    pub kind: DeviceKind,
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

    fn load_files(
        &self,
        depot: &str,
        entry: &DeviceEntry,
        model: &DeviceModelInfo,
    ) -> Option<ResolvedAsset> {
        for root in &self.read_roots {
            let dir = root.join(depot);
            // Hotspot metadata in whichever schema this depot cached:
            // `core_metadata.json` (newer) or `metadata.json` (older).
            let Some(&meta_name) = METADATA_FILES.iter().find(|n| dir.join(n).exists()) else {
                continue;
            };
            let meta_path = dir.join(meta_name);

            // Pick the colour variant matching this device's HID++
            // extended_model_id byte. Logi calibrates the assignment
            // markers against the *buttons* image (typically
            // `side_*.png`), so we prefer that resource for the
            // mouse-model render — otherwise hotspot percentages drift
            // off every button. `front_*.png` is left for the carousel.
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
                .map(|n| dir.join(n))
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
            let Some(image_path) = candidates.iter().map(|n| dir.join(n)).find(|p| p.exists())
            else {
                continue;
            };

            let metadata = match Metadata::load_from(&meta_path) {
                Ok(m) => m,
                Err(e) => {
                    warn!(depot, root = %root.display(), file = meta_name, error = ?e, "device metadata unparseable — rendering image without hotspots");
                    Metadata::default()
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
                kind: DeviceKind::from_registry_type(&entry.kind),
                image_path,
                hero_image_path,
                glow: self::glow::load_glow_geometry(&dir).map(Arc::new),
                metadata,
                png_width,
                png_height,
            });
        }
        debug!(depot, "asset cache miss across all roots");
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
#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
mod tests {
    use super::*;
    use openlogi_assets::DeviceEntry;
    use openlogi_core::device::DeviceTransports;
    use std::collections::HashMap;

    fn mx_master_3s_entry(model_ids: Vec<String>) -> DeviceEntry {
        DeviceEntry {
            model_id: "2b043".to_string(),
            model_ids,
            display_name: "MX Master 3S".to_string(),
            kind: "mouse".to_string(),
            asset_path: "assets/mx_master_3s/".to_string(),
            files: Vec::new(),
        }
    }

    fn index_of(depot: &str, entry: DeviceEntry) -> Index {
        let mut devices = HashMap::new();
        devices.insert(depot.to_string(), entry);
        Index {
            schema_version: 1,
            devices,
        }
    }

    /// The current registry: the 3S depot lists both bolt pids Logi ships for
    /// it (`b043` via a Bolt receiver, `b034` over BTLE).
    fn mx_master_3s_index() -> Index {
        index_of(
            "mx_master_3s",
            mx_master_3s_entry(vec!["2b043".into(), "2b034".into()]),
        )
    }

    /// A legacy index generated before `modelIds` existed: only the primary
    /// pid `2b043` is listed, so the BTLE pid `b034` matches nothing.
    fn legacy_mx_master_3s_index() -> Index {
        index_of("mx_master_3s", mx_master_3s_entry(Vec::new()))
    }

    /// An MX Master 3S connected over BTLE reports bolt pid `b034` / ext 1.
    /// The strict `{ext}{pid}` key (`1b034`) matches no registry entry — the
    /// depot lists `2b034`/`2b043` (ext 2) — so the suffix `b034` is what
    /// bridges it.
    fn btle_3s_model() -> DeviceModelInfo {
        DeviceModelInfo {
            entity_count: 0,
            serial_number: None,
            unit_id: [0; 4],
            transports: DeviceTransports {
                btle: true,
                ..Default::default()
            },
            model_ids: [0xb034, 0, 0],
            extended_model_id: 0x01,
        }
    }

    #[test]
    fn secondary_pid_resolves_btle_3s_without_codename() {
        // The fix: the depot lists `2b034` alongside `2b043`, so the suffix
        // match on `b034` resolves the BTLE 3S by pid — no codename needed.
        let index = mx_master_3s_index();
        let hit = resolve_in_index(&index, &btle_3s_model(), None);
        assert_eq!(hit.map(|(depot, _)| depot), Some("mx_master_3s"));
    }

    #[test]
    fn legacy_index_misses_btle_3s_by_pid() {
        // Before `modelIds`: only `2b043` is listed, so neither strict nor
        // suffix pid matching finds the BTLE 3S (`b034`).
        let index = legacy_mx_master_3s_index();
        assert!(resolve_in_index(&index, &btle_3s_model(), None).is_none());
    }

    #[test]
    fn codename_bridges_btle_3s_on_legacy_index() {
        // Back-compat: on a legacy index the firmware codename still bridges
        // to the depot via displayName.
        let index = legacy_mx_master_3s_index();
        let hit = resolve_in_index(&index, &btle_3s_model(), Some("MX Master 3S"));
        assert_eq!(hit.map(|(depot, _)| depot), Some("mx_master_3s"));
    }

    fn bare_model() -> DeviceModelInfo {
        DeviceModelInfo {
            entity_count: 0,
            serial_number: None,
            unit_id: [0; 4],
            transports: DeviceTransports::default(),
            model_ids: [0; 3],
            extended_model_id: 0,
        }
    }

    /// A 24-byte PNG: signature + an `IHDR` chunk header carrying only the
    /// width/height — all `read_png_dimensions` actually reads.
    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes
    }

    /// An old-schema depot (`metadata.json` + `front.png`, no `*_core`
    /// names, no manifest) must still resolve — this is what makes the
    /// MX Vertical and the older mice render.
    #[test]
    fn resolves_old_schema_depot_on_disk() {
        let root = std::env::temp_dir().join(format!("openlogi-asset-test-{}", std::process::id()));
        let depot = "mx_vertical";
        let dir = root.join(depot);
        std::fs::create_dir_all(&dir).expect("create depot dir");
        std::fs::write(
            dir.join("metadata.json"),
            r#"{"images":[
                {"key":"device_image","origin":{"width":100,"height":200}},
                {"key":"device_buttons_image","origin":{"width":100,"height":200},
                 "assignments":[{"slotName":"SLOT_NAME_MIDDLE_BUTTON",
                                 "marker":{"x":50,"y":50},"label":{"x":0,"y":0}}]}
            ]}"#,
        )
        .expect("write metadata.json");
        std::fs::write(dir.join("front.png"), png_header(100, 200)).expect("write front.png");

        let resolver = AssetResolver {
            read_roots: vec![root.clone()],
            write_root: root.clone(),
            has_bundle: false,
            index: None,
        };
        let entry = DeviceEntry {
            model_id: "eb020".to_string(),
            model_ids: Vec::new(),
            display_name: "MX Vertical".to_string(),
            kind: "MOUSE".to_string(),
            asset_path: format!("v1/devices/{depot}/"),
            files: Vec::new(),
        };

        let result = resolver.load_files(depot, &entry, &bare_model());
        std::fs::remove_dir_all(&root).ok();

        let asset = result.expect("old-schema depot should resolve");
        assert_eq!(
            asset.image_path.file_name().expect("image has a file name"),
            "front.png"
        );
        assert_eq!((asset.png_width, asset.png_height), (100, 200));
        assert_eq!(asset.metadata.assignments().count(), 1);
    }

    #[test]
    fn cleanup_removes_only_legacy_glow_pngs() {
        let root =
            std::env::temp_dir().join(format!("openlogi-glow-cleanup-{}", std::process::id()));
        let depot = root.join("g513");
        std::fs::create_dir_all(&depot).expect("create depot dir");
        std::fs::write(depot.join("glow-ff9500.png"), b"x").expect("write glow png");
        std::fs::write(depot.join("glow-af52de.png.tmp"), b"x").expect("write glow tmp");
        std::fs::write(depot.join("front.png"), b"x").expect("write front render");
        std::fs::write(depot.join("metadata.json"), b"{}").expect("write metadata");

        cleanup_glow_pngs_in(&root);

        let kept = depot.join("front.png").exists() && depot.join("metadata.json").exists();
        let swept =
            !depot.join("glow-ff9500.png").exists() && !depot.join("glow-af52de.png.tmp").exists();
        // Clean up before asserting so a failing assert doesn't leave the temp dir behind.
        std::fs::remove_dir_all(&root).ok();

        assert!(swept, "legacy glow files must be deleted");
        assert!(kept, "real assets must be left untouched");
    }
}
