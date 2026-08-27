//! Device image and depot-manifest helpers.

use std::path::{Path, PathBuf};

use openlogi_assets::http::safe_component_path;
use openlogi_assets::{
    BUTTONS_RENDER_FILES, DepotManifest, DeviceEntry, FRONT_RENDER_FILES, METADATA_FILES,
};
use openlogi_core::device::DeviceModelInfo;
use tracing::warn;

/// Paths chosen from one on-disk depot for a live HID++ model.
pub(super) struct DepotRenders {
    pub meta_name: String,
    pub image_name: String,
    pub image_path: PathBuf,
    pub hero_image_path: Option<PathBuf>,
    pub side_image_path: Option<PathBuf>,
}

/// Pick metadata + render files for `model` from `dir`.
///
/// G-series payloads key SKU / colour variants on the depot name
/// (`g502_spectrum`, `g513_ext5`) rather than the hex PID the index lists
/// (`c07d`, `c33c`). [`openlogi_assets::variant_lookup_ids`] tries those
/// first; MX-class depots still resolve via `{pid}_ext{N}`.
pub(super) fn resolve_depot_renders(
    dir: &Path,
    depot: &str,
    entry: &DeviceEntry,
    model: &DeviceModelInfo,
    manifest: Option<&DepotManifest>,
) -> Option<DepotRenders> {
    let index_ids: Vec<&str> = entry.model_id_candidates().collect();
    let lookup_ids = openlogi_assets::variant_lookup_ids(
        depot,
        &index_ids,
        &model.model_ids,
        model.extended_model_id,
    );
    let resource = |key: &str| -> Option<String> {
        manifest
            .and_then(|m| m.resource_for_first(&lookup_ids, key))
            .map(str::to_string)
    };

    // Variant `image_metadata` (Spectrum ships `spectrum_metadata.json`)
    // then the depot-wide schemas.
    let meta_name = resource("image_metadata")
        .filter(|name| dir.join(name).exists())
        .or_else(|| {
            METADATA_FILES
                .iter()
                .find(|name| dir.join(name).exists())
                .map(|name| (*name).to_string())
        })?;

    // MX-class depots calibrate markers against `device_buttons_image`
    // (typically `side_*.png`). G-series put most buttons on
    // `device_image` and thumb keys on `device_side`.
    let buttons_name = resource("device_buttons_image");
    let variant_front_name = resource("device_image").or_else(|| resource("device_camera_image"));
    let side_name = resource("device_side");
    let hero_image_path = first_existing(
        dir,
        variant_front_name
            .clone()
            .into_iter()
            .chain(FRONT_RENDER_FILES.map(str::to_string)),
    );
    let image_name = buttons_name
        .clone()
        .or_else(|| variant_front_name.clone())
        .unwrap_or_else(|| "side_core.png".to_string());
    let mut candidates = vec![image_name.clone()];
    candidates.extend(BUTTONS_RENDER_FILES.map(str::to_string));
    candidates.extend(variant_front_name);
    candidates.extend(FRONT_RENDER_FILES.map(str::to_string));
    let image_path = first_existing(dir, candidates)?;
    let side_image_path = first_existing(
        dir,
        side_name.into_iter().chain([
            "side_spectrum.png".to_string(),
            "side_core.png".to_string(),
            "side.png".to_string(),
        ]),
    );
    Some(DepotRenders {
        meta_name,
        image_name,
        image_path,
        hero_image_path,
        side_image_path,
    })
}

fn first_existing(dir: &Path, names: impl IntoIterator<Item = String>) -> Option<PathBuf> {
    names
        .into_iter()
        .filter_map(|n| safe_component_path(dir, &n, "asset file").ok())
        .find(|p| p.exists())
}

/// Read width + height from a PNG's `IHDR` chunk.
///
/// PNG layout: 8-byte signature, then chunks. The first chunk is always
/// `IHDR` per the spec, located at bytes 12–24: 4 bytes length, 4 bytes
/// type tag, then the data. The first 8 data bytes are width + height as
/// big-endian u32s. We only need those 24 leading bytes — much cheaper
/// than decoding the whole image.
pub(crate) fn read_png_dimensions(path: &Path) -> std::io::Result<(u32, u32)> {
    use std::fs::File;
    use std::io::Read;

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
    Ok((width, height))
}

/// Load and parse a depot's `manifest.json`, or `None` when it's missing /
/// malformed. Read once per [`load_files`](super::AssetResolver::load_files).
pub(super) fn load_manifest(dir: &Path) -> Option<DepotManifest> {
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
