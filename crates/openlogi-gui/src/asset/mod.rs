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

use openlogi_assets::{DeviceEntry, Index, Metadata};
use openlogi_core::device::DeviceModelInfo;
use tracing::{debug, warn};

use self::images::{buttons_image_for, read_png_dimensions, variant_image_for};
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
mod tests {
    use super::*;
    use openlogi_assets::DeviceEntry;
    use openlogi_core::device::DeviceTransports;
    use std::collections::HashMap;

    fn mx_master_3s_index() -> Index {
        let mut devices = HashMap::new();
        devices.insert(
            "mx_master_3s".to_string(),
            DeviceEntry {
                model_id: "2b043".to_string(),
                display_name: "MX Master 3S".to_string(),
                kind: "mouse".to_string(),
                asset_path: "assets/mx_master_3s/".to_string(),
                files: Vec::new(),
            },
        );
        Index {
            schema_version: 1,
            devices,
        }
    }

    /// An MX Master 3S connected over BTLE reports model id `b034` / ext 1 —
    /// absent from the registry, which keys the 3S under `2b043`.
    fn btle_3s_model() -> DeviceModelInfo {
        DeviceModelInfo {
            entity_count: 0,
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
    fn pid_matching_alone_misses_btle_3s() {
        // The bug: no model id the device reports (`b034`) appears in the
        // registry, so strict + suffix PID matching both fail.
        let index = mx_master_3s_index();
        assert!(resolve_in_index(&index, &btle_3s_model(), None).is_none());
    }

    #[test]
    fn codename_bridges_btle_3s_to_its_depot() {
        // The fix: the firmware codename matches the registry displayName.
        let index = mx_master_3s_index();
        let hit = resolve_in_index(&index, &btle_3s_model(), Some("MX Master 3S"));
        assert_eq!(hit.map(|(depot, _)| depot), Some("mx_master_3s"));
    }
}
