//! Rasterize a key face into the JPEG tile a keypad's LCD key displays.
//!
//! Pure CPU work with no device I/O: the action's embedded icon (the same
//! glyph the settings app and the Actions Ring draw) is rendered white on
//! black, centred in a tile the size the panel's keys are, and encoded as
//! the baseline JPEG the vendor image write carries.

use std::io::Cursor;

use image::ExtendedColorType;
use image::ImageEncoder as _;
use image::codecs::jpeg::JpegEncoder;
use openlogi_core::binding::ActionRingIcon;
use openlogi_hid::KeyImageJpeg;
use resvg::{tiny_skia, usvg};
use thiserror::Error;

/// What one LCD key shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyFace {
    /// Nothing — the key is unbound, so it does nothing and shows nothing.
    Blank,
    /// The glyph of the action the key is bound to.
    Icon(ActionRingIcon),
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

/// Render `face` as a `size`-square tile.
pub fn render_tile(face: KeyFace, size: u16) -> Result<KeyImageJpeg, RenderError> {
    let mut pixmap = tiny_skia::Pixmap::new(u32::from(size), u32::from(size))
        .ok_or(RenderError::TileSize(size))?;
    pixmap.fill(tiny_skia::Color::BLACK);
    if let KeyFace::Icon(icon) = face {
        draw_icon(&mut pixmap, icon)?;
    }
    encode(&pixmap, size)
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
    let mut jpeg = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY)
        .write_image(
            &rgb,
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
