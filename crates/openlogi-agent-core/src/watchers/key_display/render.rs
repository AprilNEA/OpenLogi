//! Rasterize a key face into the JPEG tile a keypad's LCD key displays.
//!
//! Pure CPU work with no device I/O: the action's embedded icon (the same
//! glyph the settings app and the Actions Ring draw) is rendered white on
//! black, centred in a tile the size the panel's keys are, and encoded as
//! the baseline JPEG the vendor image write carries. An image supplied at
//! runtime is decoded once into a [`KeyImage`] and scaled to the tile.

use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Cursor;
use std::sync::Arc;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::{self, FilterType};
use image::{
    DynamicImage, ExtendedColorType, GenericImageView as _, ImageDecoder as _, ImageEncoder as _,
    ImageError, ImageFormat, ImageReader, Limits, Rgb, RgbImage,
};
use openlogi_core::binding::ActionRingIcon;
use openlogi_hid::KeyImageJpeg;
use resvg::{tiny_skia, usvg};
use thiserror::Error;

/// What one LCD key shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyFace {
    /// Nothing — the key is unbound, so it does nothing and shows nothing.
    Blank,
    /// The glyph of the action the key is bound to.
    Icon(ActionRingIcon),
    /// An image supplied at runtime, standing in for the binding's glyph.
    Image(KeyImage),
}

/// A decoded image for an LCD key: opaque, square, and at most
/// [`MAX_SOURCE_SIDE`] pixels on a side, so a tile of any key size can be
/// scaled from it. Cheap to clone.
#[derive(Clone)]
pub struct KeyImage {
    pixels: Arc<RgbImage>,
    digest: u64,
}

/// Why bytes could not be decoded into a [`KeyImage`].
#[derive(Debug, Error)]
pub enum KeyImageDecodeError {
    /// The bytes are neither a PNG nor a JPEG.
    #[error("not a PNG or JPEG image")]
    UnsupportedFormat,
    /// The bytes claim a PNG or JPEG but do not decode as one.
    #[error("the image does not decode: {0}")]
    Undecodable(String),
    /// Decoding would exceed [`MAX_DECODE_SIDE`] or [`MAX_DECODE_ALLOC`].
    #[error("the image is too large to decode")]
    TooLarge,
}

/// The widest and tallest image [`KeyImage::decode`] reads.
pub const MAX_DECODE_SIDE: u32 = 4096;
/// The most memory one decode may allocate for its pixels: a
/// [`MAX_DECODE_SIDE`]-square image at 16 bits per RGBA channel.
pub const MAX_DECODE_ALLOC: u64 = 128 * 1024 * 1024;
/// The side a decoded image is reduced to, past which a key gains nothing:
/// the largest keys are a fraction of it.
pub const MAX_SOURCE_SIDE: u32 = 256;

impl KeyImage {
    /// Decode a PNG or JPEG, composite any transparency over the panel's
    /// black, crop it to a centred square, reduce it to at most
    /// [`MAX_SOURCE_SIDE`] pixels on a side, and turn it upright as its EXIF
    /// orientation says.
    pub fn decode(bytes: &[u8]) -> Result<Self, KeyImageDecodeError> {
        let mut reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|error| KeyImageDecodeError::Undecodable(error.to_string()))?;
        if !matches!(reader.format(), Some(ImageFormat::Png | ImageFormat::Jpeg)) {
            return Err(KeyImageDecodeError::UnsupportedFormat);
        }
        let mut limits = Limits::default();
        limits.max_image_width = Some(MAX_DECODE_SIDE);
        limits.max_image_height = Some(MAX_DECODE_SIDE);
        limits.max_alloc = Some(MAX_DECODE_ALLOC);
        reader.limits(limits.clone());
        let mut decoder = reader.into_decoder().map_err(decode_error)?;
        // `ImageReader::decode` reserves the pixels against the limits too;
        // `into_decoder`, which leaves the orientation readable, does not.
        limits
            .reserve(decoder.total_bytes())
            .map_err(decode_error)?;
        let orientation = decoder.orientation().map_err(decode_error)?;
        let source = DynamicImage::from_decoder(decoder).map_err(decode_error)?;

        let (width, height) = source.dimensions();
        let side = width.min(height);
        if side == 0 {
            return Err(KeyImageDecodeError::Undecodable(
                "the image is empty".into(),
            ));
        }
        // Read the square through a view and convert each pixel once, so no
        // full-size copy is made besides the opaque pixels themselves.
        let square = source.view((width - side) / 2, (height - side) / 2, side, side);
        let opaque = RgbImage::from_fn(side, side, |x, y| {
            let [r, g, b, a] = square.get_pixel(x, y).0;
            Rgb([over_black(r, a), over_black(g, a), over_black(b, a)])
        });
        drop(source);
        let pixels = if side > MAX_SOURCE_SIDE {
            imageops::resize(
                &opaque,
                MAX_SOURCE_SIDE,
                MAX_SOURCE_SIDE,
                FilterType::Lanczos3,
            )
        } else {
            opaque
        };
        // A centred square and its scaling are the same whichever way up the
        // image is, so turning the reduced one gives the same result cheaply.
        let mut upright = DynamicImage::ImageRgb8(pixels);
        upright.apply_orientation(orientation);
        Ok(Self::from_pixels(upright.into_rgb8()))
    }

    fn from_pixels(pixels: RgbImage) -> Self {
        let mut hasher = DefaultHasher::new();
        pixels.dimensions().hash(&mut hasher);
        pixels.as_raw().hash(&mut hasher);
        Self {
            digest: hasher.finish(),
            pixels: Arc::new(pixels),
        }
    }

    /// A `side`-square image of one colour.
    #[cfg(test)]
    pub(crate) fn solid(side: u32, rgb: [u8; 3]) -> Self {
        Self::from_pixels(RgbImage::from_pixel(side, side, Rgb(rgb)))
    }
}

fn decode_error(error: ImageError) -> KeyImageDecodeError {
    match error {
        ImageError::Limits(_) => KeyImageDecodeError::TooLarge,
        error => KeyImageDecodeError::Undecodable(error.to_string()),
    }
}

/// `channel` at `alpha` coverage over black.
fn over_black(channel: u8, alpha: u8) -> u8 {
    let blended = u16::from(channel) * u16::from(alpha) / 255;
    u8::try_from(blended).unwrap_or(u8::MAX)
}

// Equality is by content, not by allocation: the orchestrator builds a fresh
// face on every republish and the watcher repaints a key whose face changed,
// so the same image pushed again must compare equal to stay a no-op.
impl PartialEq for KeyImage {
    fn eq(&self, other: &Self) -> bool {
        self.digest == other.digest
            && (Arc::ptr_eq(&self.pixels, &other.pixels) || self.pixels == other.pixels)
    }
}

impl Eq for KeyImage {}

impl Hash for KeyImage {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.digest.hash(state);
    }
}

impl fmt::Debug for KeyImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (width, height) = self.pixels.dimensions();
        f.debug_struct("KeyImage")
            .field("digest", &format_args!("{:016x}", self.digest))
            .field("size", &format_args!("{width}x{height}"))
            .finish()
    }
}

/// Why a face could not be turned into a tile. Every arm is a defect in the
/// embedded artwork or the encoder, not a device condition.
#[derive(Debug, Error)]
pub enum RenderError {
    /// The icon's asset path names no embedded SVG.
    #[error("no embedded icon at {0}")]
    MissingIcon(&'static str),
    /// The embedded SVG did not parse.
    #[error("icon {path} is not valid SVG: {message}")]
    Svg {
        /// The icon's asset path.
        path: &'static str,
        /// The parser's message.
        message: String,
    },
    /// The tile size cannot be rasterized.
    #[error("cannot rasterize a {0}-pixel tile")]
    TileSize(u16),
    /// The JPEG encoder refused the tile, or produced more than the image
    /// write can declare.
    #[error("cannot encode the tile: {0}")]
    Encode(String),
}

/// Share of the tile's side the glyph spans; the rest is margin, so the icon
/// clears the key's rounded bezel.
const GLYPH_SCALE: f32 = 0.55;
/// Encoder quality for a flat two-tone glyph: high enough that the stroke
/// edges stay clean, while a tile stays near 1–2 KiB, one image packet.
const JPEG_QUALITY: u8 = 90;
/// Encoder quality for a photo: its noise and gradients hide the artifacts
/// a glyph's hard edges would show, and a key-sized tile stays a few KiB.
const PHOTO_JPEG_QUALITY: u8 = 85;

/// Render `face` as a `size`-square tile.
pub fn render_tile(face: &KeyFace, size: u16) -> Result<KeyImageJpeg, RenderError> {
    if let KeyFace::Image(image) = face {
        return render_image(image, size);
    }
    let mut pixmap = tiny_skia::Pixmap::new(u32::from(size), u32::from(size))
        .ok_or(RenderError::TileSize(size))?;
    pixmap.fill(tiny_skia::Color::BLACK);
    if let KeyFace::Icon(icon) = face {
        draw_icon(&mut pixmap, *icon)?;
    }
    encode(&pixmap, size)
}

fn render_image(image: &KeyImage, size: u16) -> Result<KeyImageJpeg, RenderError> {
    if size == 0 {
        return Err(RenderError::TileSize(size));
    }
    let tile = imageops::resize(
        &*image.pixels,
        u32::from(size),
        u32::from(size),
        FilterType::Lanczos3,
    );
    encode_jpeg(tile.as_raw(), size, PHOTO_JPEG_QUALITY)
}

fn draw_icon(pixmap: &mut tiny_skia::Pixmap, icon: ActionRingIcon) -> Result<(), RenderError> {
    let path = icon.asset_path();
    let svg =
        openlogi_ui::action_icons::embedded_icon(path).ok_or(RenderError::MissingIcon(path))?;
    // The glyphs stroke with `currentColor`, which a renderer outside a
    // styled document resolves to black — invisible on the panel.
    let svg = String::from_utf8_lossy(svg).replace("currentColor", "#ffffff");
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).map_err(|error| {
        RenderError::Svg {
            path,
            message: error.to_string(),
        }
    })?;
    #[expect(
        clippy::cast_precision_loss,
        reason = "tile sides are a few hundred pixels, exact in f32"
    )]
    let side = pixmap.width() as f32;
    let glyph = tree.size();
    let scale = side * GLYPH_SCALE / glyph.width().max(glyph.height());
    let offset_x = (side - glyph.width() * scale) / 2.0;
    let offset_y = (side - glyph.height() * scale) / 2.0;
    let transform =
        tiny_skia::Transform::from_scale(scale, scale).post_translate(offset_x, offset_y);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Ok(())
}

fn encode(pixmap: &tiny_skia::Pixmap, size: u16) -> Result<KeyImageJpeg, RenderError> {
    // The tile was filled opaque before drawing, so the premultiplied pixels
    // are already the composited colours: drop alpha.
    let rgb: Vec<u8> = pixmap
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, _]| [r, g, b])
        .collect();
    encode_jpeg(&rgb, size, JPEG_QUALITY)
}

/// Encode `size`-square packed RGB pixels as the baseline JPEG the image
/// write carries.
fn encode_jpeg(rgb: &[u8], size: u16, quality: u8) -> Result<KeyImageJpeg, RenderError> {
    let mut jpeg = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut jpeg, quality)
        .write_image(
            rgb,
            u32::from(size),
            u32::from(size),
            ExtendedColorType::Rgb8,
        )
        .map_err(|error| RenderError::Encode(error.to_string()))?;
    let bytes = jpeg.into_inner();
    let len = bytes.len();
    KeyImageJpeg::new(bytes)
        .ok_or_else(|| RenderError::Encode(format!("{len} bytes do not fit the image write")))
}

#[cfg(test)]
mod tests;
