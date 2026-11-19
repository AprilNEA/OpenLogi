//! Device image and depot-manifest helpers.

use std::path::Path;

use openlogi_assets::DepotManifest;
use tracing::warn;

/// The leading bytes [`read_image_dimensions`] reads: a WebP `VP8X` or
/// `VP8 ` size ends at byte 30, a PNG one at byte 24.
const DIMENSIONS_HEADER_LEN: usize = 30;

/// Read a render's width and height from its file header.
///
/// The catalog ships renders as PNG and as WebP, many of them under a bare
/// UUID name, so the leading bytes pick the format, not the extension. Only
/// those bytes are read — much cheaper than decoding the whole image.
pub(super) fn read_image_dimensions(path: &Path) -> std::io::Result<(u32, u32)> {
    use std::fs::File;
    use std::io::Read;

    let mut header = Vec::with_capacity(DIMENSIONS_HEADER_LEN);
    File::open(path)?
        .take(DIMENSIONS_HEADER_LEN as u64)
        .read_to_end(&mut header)?;
    png_dimensions(&header)
        .or_else(|| webp_dimensions(&header))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "no PNG or WebP image size in the file header",
            )
        })
}

/// PNG: the 8-byte signature, then the `IHDR` chunk, which is always first.
/// After its 4-byte length and its tag, the data opens with the width and the
/// height as big-endian `u32`s.
fn png_dimensions(header: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

    let ihdr = header
        .strip_prefix(&SIGNATURE)?
        .get(4..)?
        .strip_prefix(b"IHDR")?;
    let (width, rest) = ihdr.split_first_chunk::<4>()?;
    let height = rest.first_chunk::<4>()?;
    Some((u32::from_be_bytes(*width), u32::from_be_bytes(*height)))
}

/// WebP (RFC 9649): a `RIFF` container of form `WEBP`. Its first chunk, after
/// a 4-byte tag and a 4-byte length, carries the size:
/// - `VP8X` (extended format): after 4 bytes of flags, the canvas width and
///   height minus one, each 24-bit little-endian.
/// - `VP8 ` (lossy): after the 3-byte frame tag and the key-frame start code,
///   the width and height as 14-bit little-endian values; the top 2 bits of
///   each `u16` are a scale.
/// - `VP8L` (lossless): after the `0x2f` signature, the width and height minus
///   one, packed as the low two 14-bit fields of a little-endian `u32`.
fn webp_dimensions(header: &[u8]) -> Option<(u32, u32)> {
    const VP8_START_CODE: [u8; 3] = [0x9d, 0x01, 0x2a];
    const VP8L_SIGNATURE: [u8; 1] = [0x2f];

    let form = header
        .strip_prefix(b"RIFF")?
        .get(4..)?
        .strip_prefix(b"WEBP")?;
    let (tag, chunk) = form.split_first_chunk::<4>()?;
    let data = chunk.get(4..)?;
    match tag {
        b"VP8X" => {
            let (width, rest) = data.get(4..)?.split_first_chunk::<3>()?;
            let height = rest.first_chunk::<3>()?;
            Some((u24_le(*width) + 1, u24_le(*height) + 1))
        }
        b"VP8 " => {
            let frame = data.get(3..)?.strip_prefix(&VP8_START_CODE)?;
            let (width, rest) = frame.split_first_chunk::<2>()?;
            let height = rest.first_chunk::<2>()?;
            Some((u14_le(*width), u14_le(*height)))
        }
        b"VP8L" => {
            let bits = data.strip_prefix(&VP8L_SIGNATURE)?.first_chunk::<4>()?;
            let bits = u32::from_le_bytes(*bits);
            Some((
                (bits & FOURTEEN_BITS) + 1,
                ((bits >> 14) & FOURTEEN_BITS) + 1,
            ))
        }
        _ => None,
    }
}

/// The width and height fields of the WebP bitstreams are 14 bits wide.
const FOURTEEN_BITS: u32 = 0x3fff;

fn u24_le([b0, b1, b2]: [u8; 3]) -> u32 {
    u32::from_le_bytes([b0, b1, b2, 0])
}

fn u14_le(bytes: [u8; 2]) -> u32 {
    u32::from(u16::from_le_bytes(bytes)) & FOURTEEN_BITS
}

/// Whether the file at `path` starts like an image format, so the GUI can
/// draw it. The extension says nothing: the catalog names many renders by
/// bare UUID, most of them PNG or WebP. G HUB ships some colour variants
/// encrypted under the same kind of name (the pink and purple G502
/// LIGHTSPEED, #2375), and no decoder reads those.
pub(super) fn is_drawable_image(path: &Path) -> bool {
    use std::fs::File;
    use std::io::Read;

    // `RIFF????WEBP`, the longest signature `guess_format` needs.
    let mut header = Vec::with_capacity(16);
    File::open(path)
        .and_then(|file| file.take(16).read_to_end(&mut header))
        .is_ok_and(|_| image::guess_format(&header).is_ok())
}

/// Look up the colour variant matching `ext` in an already-loaded depot
/// manifest. Returns the `device_image` src filename — falling back to
/// `device_camera_image`, the hero-render key webcam depots use instead — or
/// `None` when the manifest lacks that variant. Pure — the caller loads the
/// manifest once (see [`load_manifest`]) and reuses it across candidate bases.
pub(super) fn variant_image_for(
    manifest: &DepotManifest,
    base_model_id: &str,
    ext: u8,
) -> Option<String> {
    manifest
        .resource_for_variant(base_model_id, ext, "device_image")
        .or_else(|| manifest.resource_for_variant(base_model_id, ext, "device_camera_image"))
        .map(str::to_string)
}

/// Helper that checks variants against candidate model IDs, including the depot name and
/// any stripped `_ext\d+` stem (e.g. for depots like `pro_keyboard_ext1`).
pub(super) fn find_variant_in_manifest<'a, F>(
    manifest: &DepotManifest,
    entry: &'a openlogi_assets::DeviceEntry,
    depot: &'a str,
    ext: u8,
    lookup: F,
) -> Option<String>
where
    F: Fn(&DepotManifest, &str, u8) -> Option<String>,
{
    let candidates = candidate_manifest_bases(entry, depot);
    for base in candidates {
        if let Some(res) = lookup(manifest, &base, ext) {
            return Some(res);
        }
    }
    None
}

/// Enumerate candidate manifest bases: model IDs, the depot name, any stripped `_ext\d+` stem,
/// and the bases of products the catalog files under the same depot (`DEPOT_SIBLINGS`).
pub(super) fn candidate_manifest_bases(
    entry: &openlogi_assets::DeviceEntry,
    depot: &str,
) -> Vec<String> {
    let mut candidates = Vec::new();
    for id in entry.model_id_candidates() {
        if !candidates
            .iter()
            .any(|c: &String| c.eq_ignore_ascii_case(id))
        {
            candidates.push(id.to_string());
        }
    }
    if !candidates
        .iter()
        .any(|c: &String| c.eq_ignore_ascii_case(depot))
    {
        candidates.push(depot.to_string());
    }
    if let Some((stem, suffix)) = depot.rsplit_once("_ext")
        && !suffix.is_empty()
        && suffix.chars().all(|c| c.is_ascii_digit())
        && !candidates
            .iter()
            .any(|c: &String| c.eq_ignore_ascii_case(stem))
    {
        candidates.push(stem.to_string());
    }
    // A sibling's variants keep their own base (`g512_ext6` in the G513
    // depot), tried after the depot's own bases.
    for base in super::depot_sibling_manifest_bases(&entry.display_name) {
        if !candidates
            .iter()
            .any(|c: &String| c.eq_ignore_ascii_case(base))
        {
            candidates.push(base.to_string());
        }
    }
    candidates
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

/// Like [`variant_image_for`] but returns the G-series side render
/// ([`SIDE_VIEW_KEY`](super::SIDE_VIEW_KEY)).
pub(super) fn side_image_for(
    manifest: &DepotManifest,
    base_model_id: &str,
    ext: u8,
) -> Option<String> {
    manifest
        .resource_for_variant(base_model_id, ext, super::SIDE_VIEW_KEY)
        .map(str::to_string)
}

/// Like [`variant_image_for`] but returns the `image_metadata` resource —
/// the hotspot-metadata JSON calibrated against this colour variant's
/// renders. Depots whose variants are *handed* rather than coloured ship no
/// well-known metadata name at all (Lift keys its metadata as
/// `core_metadata_left.json` / `core_metadata_right.json`), so the manifest
/// is the only place their filename appears.
pub(super) fn metadata_for(
    manifest: &DepotManifest,
    base_model_id: &str,
    ext: u8,
) -> Option<String> {
    manifest
        .resource_for_variant(base_model_id, ext, "image_metadata")
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
