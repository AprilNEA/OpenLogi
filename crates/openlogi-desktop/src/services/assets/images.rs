//! Device image and depot-manifest helpers.

use std::path::Path;
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    path::PathBuf,
};

use openlogi_assets::DepotManifest;
use tracing::warn;

/// Draw device artwork after filtering it to the display's physical pixel height.
/// GPUI samples textures without mipmaps; feeding it the full-size Logitech
/// renders makes fine surface textures alias when the device is drawn small.
pub(crate) fn device_image(path: std::path::PathBuf, height: gpui::Pixels) -> gpui::Img {
    gpui::img(move |window: &mut gpui::Window, cx: &mut gpui::App| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "layout heights are positive physical pixel counts, rounded before conversion"
        )]
        let physical_height = (f32::from(height) * window.scale_factor()).ceil().max(1.) as u32;
        if cx.try_global::<DeviceImageSizes>().is_none() {
            cx.set_global(DeviceImageSizes::default());
        }
        if let Some(expired) = cx
            .global::<DeviceImageSizes>()
            .record(&path, physical_height)
        {
            evict_device_image(&path, expired, window, cx);
        }
        let loaded = window
            .use_asset::<gpui::AssetLogger<DeviceImage>>(&(path.clone(), physical_height), cx);
        if matches!(loaded, Some(Ok(_)))
            && let Some(expired) = cx
                .global::<DeviceImageSizes>()
                .loaded(&path, physical_height)
        {
            evict_device_image(&path, expired, window, cx);
        }
        loaded.or_else(|| {
            let previous = cx.global::<DeviceImageSizes>().last_loaded(&path)?;
            window.get_asset::<gpui::AssetLogger<DeviceImage>>(&(path.clone(), previous), cx)
        })
    })
}

fn evict_device_image(path: &Path, height: u32, window: &mut gpui::Window, cx: &mut gpui::App) {
    let key = (path.to_owned(), height);
    if let Some(Ok(image)) = window.get_asset::<gpui::AssetLogger<DeviceImage>>(&key, cx) {
        cx.drop_image(image, Some(window));
    }
    cx.remove_asset::<gpui::AssetLogger<DeviceImage>>(&key);
}

// A device can appear at several sizes across the gallery and Buttons view.
// Keep recent sizes plus the last drawable image while a replacement loads.
const RECENT_DEVICE_IMAGE_SIZES: usize = 4;

#[derive(Default)]
struct DeviceImageSizes(RefCell<HashMap<PathBuf, PathImageState>>);

#[derive(Default)]
enum PathImageState {
    #[default]
    Unrequested,
    Pending {
        recent: RecentImageSizes,
    },
    Drawable {
        recent: RecentImageSizes,
        height: u32,
    },
}

enum ImageEvent {
    Requested(u32),
    Loaded(u32),
}

// The newest request is always present, including while no image is drawable.
struct RecentImageSizes {
    newest: u32,
    older: VecDeque<u32>,
}

impl RecentImageSizes {
    fn new(height: u32) -> Self {
        Self {
            newest: height,
            older: VecDeque::new(),
        }
    }

    fn contains(&self, height: u32) -> bool {
        self.newest == height || self.older.contains(&height)
    }

    fn request(&mut self, height: u32) -> Option<u32> {
        if self.newest == height {
            return None;
        }
        self.older.retain(|&size| size != height);
        self.older.push_back(self.newest);
        self.newest = height;
        if self.older.len() >= RECENT_DEVICE_IMAGE_SIZES {
            self.older.pop_front()
        } else {
            None
        }
    }
}

impl PathImageState {
    fn transition(&mut self, event: ImageEvent) -> Option<u32> {
        let (next, expired) = match (std::mem::take(self), event) {
            (Self::Unrequested, ImageEvent::Requested(height)) => (
                Self::Pending {
                    recent: RecentImageSizes::new(height),
                },
                None,
            ),
            (Self::Pending { mut recent }, ImageEvent::Requested(height)) => {
                let expired = recent.request(height);
                (Self::Pending { recent }, expired)
            }
            (Self::Drawable { mut recent, height }, ImageEvent::Requested(requested)) => {
                let expired = recent
                    .request(requested)
                    .filter(|&expired| expired != height);
                (Self::Drawable { recent, height }, expired)
            }
            (Self::Pending { recent }, ImageEvent::Loaded(height)) if recent.contains(height) => {
                (Self::Drawable { recent, height }, None)
            }
            (Self::Drawable { recent, height }, ImageEvent::Loaded(loaded))
                if recent.contains(loaded) =>
            {
                let expired = (!recent.contains(height)).then_some(height);
                (
                    Self::Drawable {
                        recent,
                        height: loaded,
                    },
                    expired,
                )
            }
            (state, ImageEvent::Loaded(_)) => (state, None),
        };
        *self = next;
        expired
    }

    fn drawable_height(&self) -> Option<u32> {
        match self {
            Self::Drawable { height, .. } => Some(*height),
            Self::Unrequested | Self::Pending { .. } => None,
        }
    }
}

impl gpui::Global for DeviceImageSizes {}

impl DeviceImageSizes {
    fn record(&self, path: &Path, height: u32) -> Option<u32> {
        self.0
            .borrow_mut()
            .entry(path.to_owned())
            .or_default()
            .transition(ImageEvent::Requested(height))
    }

    fn loaded(&self, path: &Path, height: u32) -> Option<u32> {
        self.0
            .borrow_mut()
            .get_mut(path)?
            .transition(ImageEvent::Loaded(height))
    }

    fn last_loaded(&self, path: &Path) -> Option<u32> {
        self.0.borrow().get(path)?.drawable_height()
    }
}

struct DeviceImage;

impl gpui::Asset for DeviceImage {
    type Source = (std::path::PathBuf, u32);
    type Output = Result<std::sync::Arc<gpui::RenderImage>, gpui::ImageCacheError>;

    #[expect(
        clippy::manual_async_fn,
        reason = "the Send + 'static future must not capture the non-Send App borrow"
    )]
    fn load(
        (path, height): Self::Source,
        _cx: &mut gpui::App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        async move {
            let decoded = image::ImageReader::open(path)?
                .with_guessed_format()?
                .decode()?;
            let image = prepare_device_image(decoded, height);
            Ok(std::sync::Arc::new(gpui::RenderImage::new(vec![
                image::Frame::new(image),
            ])))
        }
    }
}

fn prepare_device_image(image: image::DynamicImage, height: u32) -> image::RgbaImage {
    let image = if height < image.height() {
        // Filter premultiplied channels so invisible RGB cannot bleed into
        // translucent silhouette edges. GPUI expects straight alpha again.
        let mut pixels = image.into_rgba32f();
        for pixel in pixels.pixels_mut() {
            let alpha = pixel.0[3];
            for channel in &mut pixel.0[..3] {
                *channel *= alpha;
            }
        }
        let mut resized = image::DynamicImage::ImageRgba32F(pixels)
            .resize(u32::MAX, height, image::imageops::FilterType::Lanczos3)
            .into_rgba32f();
        for pixel in resized.pixels_mut() {
            let alpha = pixel.0[3];
            for channel in &mut pixel.0[..3] {
                *channel = if alpha > 0. { *channel / alpha } else { 0. };
            }
        }
        image::DynamicImage::ImageRgba32F(resized)
    } else {
        image
    };
    let mut pixels = image.into_rgba8();
    // RenderImage consumes BGRA, while the decoder returns RGBA.
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    pixels
}

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

#[cfg(test)]
mod rendering_tests {
    use super::{DeviceImage, DeviceImageSizes, RECENT_DEVICE_IMAGE_SIZES, prepare_device_image};
    use gpui::Asset as _;

    #[gpui::test]
    fn extensionless_webp_loads_through_the_device_image_asset(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("logitech-variant-uuid");
        image::RgbaImage::from_pixel(8, 16, image::Rgba([20, 40, 60, 255]))
            .save_with_format(&path, image::ImageFormat::WebP)
            .unwrap();
        let executor = cx.foreground_executor().clone();
        let loaded = cx
            .update(|cx| executor.block_on(DeviceImage::load((path, 4), cx)))
            .unwrap();
        assert_eq!(loaded.size(0).width.0, 2);
        assert_eq!(loaded.size(0).height.0, 4);
        assert_eq!(&loaded.as_bytes(0).unwrap()[..4], &[60, 40, 20, 255]);
    }

    #[test]
    fn transparent_pixels_do_not_tint_filtered_edges() {
        let original = image::RgbaImage::from_fn(16, 32, |x, _| {
            if x < 8 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 255, 0, 0])
            }
        });
        let filtered = prepare_device_image(original.into(), 8);
        let mut translucent = 0;
        for pixel in filtered
            .pixels()
            .filter(|pixel| pixel[3] > 0 && pixel[3] < 255)
        {
            translucent += 1;
            assert_eq!(pixel[0], 0, "blue fringe: {pixel:?}");
            assert_eq!(pixel[1], 0, "hidden green leaked into edge: {pixel:?}");
            assert_eq!(pixel[2], 255, "red silhouette darkened: {pixel:?}");
        }
        assert!(translucent > 0, "fixture must exercise a translucent edge");
    }

    #[test]
    fn resize_history_evicts_old_sizes_per_path_and_refreshes_recent_ones() {
        let history = DeviceImageSizes::default();
        let path = std::path::Path::new("front.png");
        let other = std::path::Path::new("side.png");
        let mut retained = std::collections::HashSet::new();
        assert_eq!(history.record(other, 200), None);
        for height in 100..200 {
            if let Some(expired) = history.record(path, height) {
                assert!(retained.remove(&expired));
            }
            retained.insert(height);
            assert!(retained.len() <= RECENT_DEVICE_IMAGE_SIZES);
        }
        let oldest = *retained.iter().min().unwrap();
        assert_eq!(history.record(path, oldest), None);
        assert_ne!(history.record(path, 300), Some(oldest));
        assert_eq!(history.record(other, 200), None);
    }

    #[test]
    fn completions_without_a_retained_request_do_not_change_the_drawable_size() {
        let history = DeviceImageSizes::default();
        let path = std::path::Path::new("front.png");
        assert_eq!(history.loaded(path, 100), None);
        assert_eq!(history.last_loaded(path), None);
        assert_eq!(history.record(path, 100), None);
        assert_eq!(history.loaded(path, 200), None);
        assert_eq!(history.last_loaded(path), None);
        assert_eq!(history.loaded(path, 100), None);
        for height in 101..200 {
            history.record(path, height);
        }
        assert_eq!(history.loaded(path, 101), None);
        assert_eq!(history.last_loaded(path), Some(100));
        assert_eq!(history.loaded(path, 199), Some(100));
        assert_eq!(history.last_loaded(path), Some(199));
    }

    #[test]
    fn last_drawable_size_survives_pending_resizes_and_is_released_after_replacement() {
        let history = DeviceImageSizes::default();
        let path = std::path::Path::new("front.png");
        assert_eq!(history.record(path, 100), None);
        assert_eq!(history.loaded(path, 100), None);
        for height in 101..200 {
            assert_ne!(history.record(path, height), Some(100));
            assert_eq!(history.last_loaded(path), Some(100));
        }
        assert_eq!(history.loaded(path, 199), Some(100));
        assert_eq!(history.last_loaded(path), Some(199));
    }

    #[test]
    fn minification_averages_fine_texture_instead_of_aliasing() {
        let texture = image::RgbaImage::from_fn(128, 256, |x, y| {
            let value = if (x + y) % 2 == 0 { 0 } else { 255 };
            image::Rgba([value, value, value, 255])
        });
        let filtered = prepare_device_image(texture.into(), 32);
        assert_eq!(filtered.dimensions(), (16, 32));
        for pixel in filtered.pixels() {
            assert!(
                (120..=135).contains(&pixel[0]),
                "texture aliased: {pixel:?}"
            );
            assert_eq!(pixel[3], 255);
        }
    }

    #[test]
    fn small_images_keep_their_resolution_alpha_and_color() {
        let original = image::RgbaImage::from_pixel(2, 3, image::Rgba([20, 40, 60, 128]));
        let prepared = prepare_device_image(original.into(), 300);
        assert_eq!(prepared.dimensions(), (2, 3));
        assert_eq!(prepared.get_pixel(0, 0).0, [60, 40, 20, 128]);
    }
}
