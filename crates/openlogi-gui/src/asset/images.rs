//! Image and depot-manifest helpers for asset resolution.

use std::path::Path;

use openlogi_assets::DepotManifest;
use tracing::warn;

/// Read width + height from a PNG's `IHDR` chunk.
///
/// PNG layout: 8-byte signature, then chunks. The first chunk is always
/// `IHDR` per the spec, located at bytes 12–24: 4 bytes length, 4 bytes
/// type tag, then the data. The first 8 data bytes are width + height as
/// big-endian u32s. We only need those 24 leading bytes — much cheaper
/// than decoding the whole image.
pub(super) fn read_png_dimensions(path: &Path) -> std::io::Result<(u32, u32)> {
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

/// Look up the colour variant matching `ext` in an already-loaded depot
/// manifest. Returns the `device_image` src filename or `None` when the
/// manifest lacks that variant. Pure — the caller loads the manifest once
/// (see [`load_manifest`]) and reuses it across candidate bases.
pub(super) fn variant_image_for(
    manifest: &DepotManifest,
    base_model_id: &str,
    ext: u8,
) -> Option<String> {
    manifest
        .resource_for_variant(base_model_id, ext, "device_image")
        .map(str::to_string)
}

/// Like [`variant_image_for`] but returns the `device_buttons_image`
/// resource (typically `side_*.png`) — that's the view Logi calibrates
/// the assignment markers against, so the mouse-model render uses it.
pub(super) fn buttons_image_for(
    manifest: &DepotManifest,
    base_model_id: &str,
    ext: u8,
) -> Option<String> {
    manifest
        .resource_for_variant(base_model_id, ext, "device_buttons_image")
        .map(str::to_string)
}

/// Load and parse a depot's `manifest.json`, or `None` when it's missing /
/// malformed. Read once per [`load_files`](super::AssetResolver::load_files)
/// so the variant lookups above don't re-parse it for each candidate base.
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
