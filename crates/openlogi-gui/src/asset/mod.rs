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

mod images;
mod paths;
pub mod sync;

use std::path::{Path, PathBuf};

use openlogi_assets::{
    BUTTONS_RENDER_FILES, DeviceEntry, FRONT_RENDER_FILES, Index, METADATA_FILES, Metadata,
};
use openlogi_core::device::DeviceModelInfo;
use tracing::{debug, warn};

use self::images::{buttons_image_for, load_manifest, read_png_dimensions, variant_image_for};
use self::paths::{bundle_assets_root, load_index, user_cache_root};

#[derive(Debug, Clone)]
pub struct ResolvedAsset {
    #[allow(
        dead_code,
        reason = "depot label will be surfaced in the carousel tooltip (P0.4+)"
    )]
    pub depot: String,
    pub display_name: String,
    pub image_path: PathBuf,
    /// The front/hero render (`device_image`, typically `front_*.png`) used for
    /// the device gallery cards — distinct from [`Self::image_path`], which is
    /// the side/buttons view the mouse model aligns hotspots against. `None`
    /// when the depot ships no front render.
    pub hero_image_path: Option<PathBuf>,
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
                    warn!(depot, root = %root.display(), file = meta_name, error = ?e, "failed to parse device metadata");
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
                hero_image_path,
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
}
