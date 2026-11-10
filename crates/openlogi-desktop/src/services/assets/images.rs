//! Device image and depot-manifest helpers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use gpui::{
    App, Bounds, Corners, IntoElement, Pixels, RenderImage, RenderOnce, Styled as _, Window,
    canvas, img, point, px, size,
};
use image::imageops::FilterType;
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

const MAX_SCALED_CACHE_ENTRIES: usize = 128;

type ScaledImageEntry = (Arc<RenderImage>, u32, u32);
type ScaledImageMap = HashMap<(PathBuf, u32, u32), ScaledImageEntry>;

static SCALED_IMAGE_CACHE: OnceLock<Mutex<ScaledImageMap>> = OnceLock::new();

/// Evict all cached Lanczos-3 [`RenderImage`] textures when [`super::AssetResolver::new`]
/// or [`super::clear_cache`] runs so freshly downloaded or cleared assets take effect
/// immediately.
pub(crate) fn clear_scaled_image_cache() {
    if let Some(cache) = SCALED_IMAGE_CACHE.get()
        && let Ok(mut guard) = cache.lock()
    {
        guard.clear();
    }
}

/// Contain-fit a box of `aspect` (`width / height`) centered inside `bounds`.
///
/// Shared by [`ScaledImage::fill_stage`] and Home's keyboard `glow_canvas` so
/// a wide RGB keyboard's product render and its inter-key glow always fit the
/// exact same stage rectangle at any card width up to `GALLERY_CARD_MAX_W`.
#[must_use]
pub(crate) fn contain_fit_bounds(bounds: Bounds<Pixels>, aspect: f32) -> Bounds<Pixels> {
    let bw = f32::from(bounds.size.width);
    let bh = f32::from(bounds.size.height);
    if bw <= 0.0 || bh <= 0.0 || aspect <= 0.0 || !aspect.is_finite() {
        return Bounds::default();
    }
    let (rw, rh) = if bw / bh > aspect {
        (bh * aspect, bh)
    } else {
        (bw, bw / aspect)
    };
    let ox = f32::from(bounds.origin.x) + (bw - rw) * 0.5;
    let oy = f32::from(bounds.origin.y) + (bh - rh) * 0.5;
    Bounds {
        origin: point(px(ox), px(oy)),
        size: size(px(rw), px(rh)),
    }
}

#[derive(Clone, Copy)]
enum ScaledFit {
    /// Fit inside the parent stage's runtime layout bounds (`size_full`),
    /// sharing [`contain_fit_bounds`] with `glow_canvas`.
    StageContain,
    /// Fit inside explicit `(max_w, max_h)` logical bounds while preserving aspect ratio.
    BoxContain { max_w: f32, max_h: f32 },
    /// Render into an already aspect-fitted logical box `(width, height)`.
    Exact { width: f32, height: f32 },
}

/// HiDPI-aware GPUI image element that downscales high-resolution asset renders
/// once via premultiplied-alpha Lanczos-3 to match the exact physical pixel box
/// (`logical_size * window.scale_factor()`), eliminating single-pass bilinear
/// aliasing when GPUI lacks texture mipmaps.
#[derive(IntoElement)]
pub(crate) struct ScaledImage {
    path: PathBuf,
    fit: ScaledFit,
}

impl ScaledImage {
    /// Fit `path` dynamically to the full runtime bounds of its parent stage
    /// (`size_full`), sharing [`contain_fit_bounds`] with `glow_canvas`.
    #[must_use]
    pub fn fill_stage(path: PathBuf) -> Self {
        Self {
            path,
            fit: ScaledFit::StageContain,
        }
    }

    /// Fit `path` inside `(max_w, max_h)` logical pixels while preserving aspect ratio.
    #[must_use]
    pub fn contain(path: PathBuf, max_w: f32, max_h: f32) -> Self {
        Self {
            path,
            fit: ScaledFit::BoxContain {
                max_w: max_w.max(1.),
                max_h: max_h.max(1.),
            },
        }
    }

    /// Render `path` into an already aspect-fitted logical box `(width, height)`.
    #[must_use]
    pub fn exact(path: PathBuf, width: f32, height: f32) -> Self {
        Self {
            path,
            fit: ScaledFit::Exact {
                width: width.max(1.),
                height: height.max(1.),
            },
        }
    }
}

impl RenderOnce for ScaledImage {
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "bounded logical and physical pixel dimensions fit within f32/u32"
    )]
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        match self.fit {
            ScaledFit::StageContain => {
                let path = self.path;
                canvas(
                    move |_, _, _| path,
                    move |bounds, path, window, _| {
                        let bw = f32::from(bounds.size.width);
                        let bh = f32::from(bounds.size.height);
                        if bw <= 0.0 || bh <= 0.0 {
                            return;
                        }
                        let scale_factor = window.scale_factor().clamp(1.0, 4.0);
                        let phys_max_w = (bw * scale_factor).round().max(1.0) as u32;
                        let phys_max_h = (bh * scale_factor).round().max(1.0) as u32;
                        if let Some((render_img, dst_w, dst_h)) =
                            scaled_render_image(&path, phys_max_w, phys_max_h)
                        {
                            let aspect = (dst_w as f32) / (dst_h.max(1) as f32);
                            let image_bounds = contain_fit_bounds(bounds, aspect);
                            let _ = window.paint_image(
                                bounds,
                                image_bounds,
                                Corners::default(),
                                render_img,
                                0,
                                false,
                            );
                        }
                    },
                )
                .size_full()
                .into_any_element()
            }
            ScaledFit::BoxContain { max_w, max_h } => {
                let scale_factor = window.scale_factor().clamp(1.0, 4.0);
                let phys_max_w = (max_w * scale_factor).round().max(1.0) as u32;
                let phys_max_h = (max_h * scale_factor).round().max(1.0) as u32;
                if let Some((render_img, phys_dst_w, phys_dst_h)) =
                    scaled_render_image(&self.path, phys_max_w, phys_max_h)
                {
                    let logical_w = (phys_dst_w as f32) / scale_factor;
                    let logical_h = (phys_dst_h as f32) / scale_factor;
                    return img(render_img)
                        .w(px(logical_w))
                        .h(px(logical_h))
                        .into_any_element();
                }
                img(self.path).max_w_full().max_h_full().into_any_element()
            }
            ScaledFit::Exact { width, height } => {
                let scale_factor = window.scale_factor().clamp(1.0, 4.0);
                let phys_max_w = (width * scale_factor).round().max(1.0) as u32;
                let phys_max_h = (height * scale_factor).round().max(1.0) as u32;
                if let Some((render_img, _, _)) =
                    scaled_render_image(&self.path, phys_max_w, phys_max_h)
                {
                    return img(render_img)
                        .w(px(width))
                        .h(px(height))
                        .into_any_element();
                }
                img(self.path).w(px(width)).h(px(height)).into_any_element()
            }
        }
    }
}

/// Decode `path` once and downscale with premultiplied-alpha Lanczos-3 to fit
/// within `(max_w, max_h)` physical pixels, returning a cached `(RenderImage, dst_w, dst_h)`
/// so GPUI maps 1 texel to 1 physical screen pixel without single-pass bilinear aliasing.
#[must_use]
pub(crate) fn scaled_render_image(
    path: &Path,
    max_w: u32,
    max_h: u32,
) -> Option<(Arc<RenderImage>, u32, u32)> {
    let key = (path.to_path_buf(), max_w.max(1), max_h.max(1));
    let cache = SCALED_IMAGE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock()
        && let Some(cached) = guard.get(&key)
    {
        return Some(cached.clone());
    }

    let built = build_scaled_render_image(path, key.1, key.2)?;
    if let Ok(mut guard) = cache.lock() {
        if guard.len() >= MAX_SCALED_CACHE_ENTRIES {
            guard.clear();
        }
        guard.insert(key, built.clone());
    }
    Some(built)
}

#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "bounded image dimensions and RGBA channel math fit within f32/u32/u8"
)]
fn build_scaled_render_image(
    path: &Path,
    max_w: u32,
    max_h: u32,
) -> Option<(Arc<RenderImage>, u32, u32)> {
    let mut rgba = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?
        .into_rgba8();
    let (src_w, src_h) = rgba.dimensions();
    if src_w == 0 || src_h == 0 {
        return None;
    }

    let scale = (max_w as f32 / src_w as f32)
        .min(max_h as f32 / src_h as f32)
        .min(1.0);
    let dst_w = ((src_w as f32 * scale).round() as u32).max(1);
    let dst_h = ((src_h as f32 * scale).round() as u32).max(1);

    // 1. Premultiply RGB by alpha so transparent (0,0,0,0) exterior pixels never
    // bleed dark fringes into light silhouette edges during Lanczos-3 resampling.
    for p in rgba.pixels_mut() {
        let a = u16::from(p[3]);
        p[0] = ((u16::from(p[0]) * a + 127) / 255) as u8;
        p[1] = ((u16::from(p[1]) * a + 127) / 255) as u8;
        p[2] = ((u16::from(p[2]) * a + 127) / 255) as u8;
    }

    let mut resized = if dst_w == src_w && dst_h == src_h {
        rgba
    } else {
        image::imageops::resize(&rgba, dst_w, dst_h, FilterType::Lanczos3)
    };

    // 2. Un-premultiply back to straight alpha.
    for p in resized.pixels_mut() {
        let a = u16::from(p[3]);
        if a > 0 && a < 255 {
            p[0] = ((u16::from(p[0]) * 255 + a / 2) / a).min(255) as u8;
            p[1] = ((u16::from(p[1]) * 255 + a / 2) / a).min(255) as u8;
            p[2] = ((u16::from(p[2]) * 255 + a / 2) / a).min(255) as u8;
        }
    }

    // 3. Apply a mild interior-only unsharp mask on larger renders (e.g. Keys view)
    // so fine key sub-legends (F1..F12) stay crisp without ringing silhouette borders.
    if dst_w >= 200 && dst_h >= 120 {
        sharpen_opaque_interior(&mut resized);
    }

    // 4. GPUI's RenderImage expects BGRA byte order.
    for p in resized.pixels_mut() {
        p.0.swap(0, 2);
    }

    let render_img = Arc::new(RenderImage::new(vec![image::Frame::new(resized)]));
    Some((render_img, dst_w, dst_h))
}

fn sharpen_opaque_interior(img: &mut image::RgbaImage) {
    let (w, h) = img.dimensions();
    if w < 3 || h < 3 {
        return;
    }
    let src = img.clone();
    for y in 1..(h - 1) {
        for x in 1..(w - 1) {
            let center = src.get_pixel(x, y);
            if center[3] < 245 {
                continue;
            }
            let top = src.get_pixel(x, y - 1);
            let bottom = src.get_pixel(x, y + 1);
            let left = src.get_pixel(x - 1, y);
            let right = src.get_pixel(x + 1, y);
            if top[3] < 245 || bottom[3] < 245 || left[3] < 245 || right[3] < 245 {
                continue;
            }
            let out = img.get_pixel_mut(x, y);
            for ch in 0..3 {
                let c = i32::from(center[ch]);
                let avg = (i32::from(top[ch])
                    + i32::from(bottom[ch])
                    + i32::from(left[ch])
                    + i32::from(right[ch]))
                    / 4;
                let diff = c - avg;
                // Mild micro-contrast boost (0.35x) only for subtle legend transitions.
                if diff.abs() <= 48 {
                    out[ch] =
                        u8::try_from((c + (diff * 35) / 100).clamp(0, 255)).unwrap_or(center[ch]);
                }
            }
        }
    }
}
