//! Rasterize a key face into the JPEG tile a keypad's LCD key displays.
//!
//! Pure CPU work with no device I/O. A face is drawn back to front:
//!
//! 1. the background colour,
//! 2. a picture the user chose, cropped to fill the key,
//! 3. the bound action's embedded icon (the same glyph the settings app and
//!    the Actions Ring draw),
//! 4. a text label in a system sans-serif font.
//!
//! The result is encoded as the baseline JPEG the vendor image write carries.

use std::fmt::Write as _;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use image::ExtendedColorType;
use image::ImageEncoder as _;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use openlogi_core::binding::ActionRingIcon;
use openlogi_core::color::Rgb;
use openlogi_core::config::{GradientDirection, KeyGradient, KeySymbol};
use openlogi_hid::{KEY_IMAGE_SINGLE_PACKET_MAX, KeyImageJpeg};
use resvg::{tiny_skia, usvg};
use thiserror::Error;
use tracing::warn;

/// What one LCD key shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyFace {
    /// Nothing — the key is unbound, so it does nothing and shows nothing.
    Blank,
    /// The glyph of the action the key is bound to, white on black.
    Icon(ActionRingIcon),
    /// A face the user styled: colours, a picture and/or a label.
    Styled(Arc<StyledFace>),
}

/// The parts of a styled key face. Equal faces render equal tiles, so the
/// watcher caches by value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StyledFace {
    /// The bound action's glyph, unless the user hid it or the key is unbound.
    pub icon: Option<ActionRingIcon>,
    /// Text drawn on the key.
    pub label: Option<String>,
    /// Tile background.
    pub background: Rgb,
    /// Label colour.
    pub foreground: Rgb,
    /// Colour of the action icon or Lucide symbol.
    pub icon_color: Rgb,
    /// A gradient drawn instead of the flat `background`.
    pub gradient: Option<KeyGradient>,
    /// A PNG or JPEG file drawn under the icon and label.
    pub image: Option<PathBuf>,
    /// A Lucide icon or emoji drawn in place of `icon`.
    pub symbol: Option<KeySymbol>,
    /// A bar along the bottom, in percent (a live key's value).
    pub progress: Option<u8>,
    /// Drawn with a frame: the live key the paging keys adjust.
    pub highlight: bool,
}

/// The one glyph a styled face draws above its label: the user's symbol, else
/// the bound action's icon.
enum Glyph<'a> {
    Action(ActionRingIcon),
    Symbol(&'a KeySymbol),
}

/// Why a face could not be turned into a tile. Every arm is a defect in the
/// embedded artwork or the encoder, not a device condition. A user's picture
/// that cannot be read is not an error: the face is drawn without it.
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

/// Share of the tile's side an icon spans on its own; the rest is margin, so
/// the icon clears the key's rounded bezel.
const GLYPH_SCALE: f32 = 0.55;
/// Share of the side an icon spans above a label.
const GLYPH_SCALE_WITH_LABEL: f32 = 0.40;
/// Share of the side a label line may span.
const LABEL_WIDTH: f32 = 0.86;
/// Lines of label text shown at most.
const MAX_LABEL_LINES: usize = 2;
/// Average advance of a sans-serif glyph in ems, used to fit a line.
const AVERAGE_GLYPH_ADVANCE: f32 = 0.56;
/// Font families tried in order; `sans-serif` lets fontdb pick the rest.
const FONT_FAMILIES: &str =
    "'Segoe UI', 'Helvetica Neue', Helvetica, Arial, 'DejaVu Sans', 'Noto Sans', sans-serif";
/// Encoder qualities tried in order: the first whose tile fits one image
/// packet wins. A flat icon tile fits at the first; a detailed picture steps
/// down until it fits.
const JPEG_QUALITIES: [u8; 9] = [90, 80, 70, 60, 50, 40, 30, 20, 10];

/// Render `face` as a `size`-square tile.
pub fn render_tile(face: &KeyFace, size: u16) -> Result<KeyImageJpeg, RenderError> {
    let mut pixmap = tiny_skia::Pixmap::new(u32::from(size), u32::from(size))
        .ok_or(RenderError::TileSize(size))?;
    let tile = f32::from(size);
    match face {
        KeyFace::Blank => pixmap.fill(tiny_skia::Color::BLACK),
        KeyFace::Icon(icon) => {
            pixmap.fill(tiny_skia::Color::BLACK);
            draw_icon(&mut pixmap, *icon, Rgb::WHITE, centred(tile, GLYPH_SCALE))?;
        }
        KeyFace::Styled(styled) => draw_styled(&mut pixmap, styled, tile)?,
    }
    encode(&pixmap, size)
}

/// Where an icon goes: its top-left corner and its side length.
#[derive(Clone, Copy)]
struct IconBox {
    x: f32,
    y: f32,
    side: f32,
}

fn centred(tile: f32, scale: f32) -> IconBox {
    let side = tile * scale;
    IconBox {
        x: (tile - side) / 2.0,
        y: (tile - side) / 2.0,
        side,
    }
}

fn draw_styled(
    pixmap: &mut tiny_skia::Pixmap,
    face: &StyledFace,
    side: f32,
) -> Result<(), RenderError> {
    match face.gradient {
        Some(gradient) => fill_gradient(pixmap, gradient, side),
        None => pixmap.fill(color(face.background, 255)),
    }
    if let Some(path) = &face.image {
        draw_picture(pixmap, path);
    }
    let lines = face.label.as_deref().map(label_lines).unwrap_or_default();
    let glyph = face
        .symbol
        .as_ref()
        .map(Glyph::Symbol)
        .or_else(|| face.icon.map(Glyph::Action));
    match (glyph, lines.is_empty()) {
        (Some(glyph), true) => {
            draw_glyph(pixmap, &glyph, face.icon_color, centred(side, GLYPH_SCALE))?;
        }
        (Some(glyph), false) => {
            let icon_side = side * GLYPH_SCALE_WITH_LABEL;
            let icon_box = IconBox {
                x: (side - icon_side) / 2.0,
                y: side * 0.10,
                side: icon_side,
            };
            draw_glyph(pixmap, &glyph, face.icon_color, icon_box)?;
            let top = icon_box.y + icon_side + side * 0.04;
            draw_label(pixmap, &lines, face, side, top, side - side * 0.06);
        }
        (None, false) => draw_label(pixmap, &lines, face, side, side * 0.08, side * 0.92),
        (None, true) => {}
    }
    if let Some(progress) = face.progress {
        draw_progress(pixmap, progress, face.foreground, side);
    }
    if face.highlight {
        draw_frame(pixmap, face.icon_color, side);
    }
    Ok(())
}

/// A bar along the bottom: a dim track and `progress` percent of it filled.
fn draw_progress(pixmap: &mut tiny_skia::Pixmap, progress: u8, colour: Rgb, side: f32) {
    let height = (side * 0.06).max(2.0);
    let left = side * 0.12;
    let width = side - 2.0 * left;
    let top = side - height - side * 0.07;
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(color(colour, 70));
    if let Some(track) = tiny_skia::Rect::from_xywh(left, top, width, height) {
        pixmap.fill_rect(track, &paint, tiny_skia::Transform::identity(), None);
    }
    let filled = width * f32::from(progress.min(100)) / 100.0;
    paint.set_color(color(colour, 255));
    if let Some(bar) = tiny_skia::Rect::from_xywh(left, top, filled.max(1.0), height) {
        pixmap.fill_rect(bar, &paint, tiny_skia::Transform::identity(), None);
    }
}

/// A frame around the tile's edge.
fn draw_frame(pixmap: &mut tiny_skia::Pixmap, colour: Rgb, side: f32) {
    let width = (side * 0.05).max(2.0);
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(color(colour, 255));
    for rect in [
        tiny_skia::Rect::from_xywh(0.0, 0.0, side, width),
        tiny_skia::Rect::from_xywh(0.0, side - width, side, width),
        tiny_skia::Rect::from_xywh(0.0, 0.0, width, side),
        tiny_skia::Rect::from_xywh(side - width, 0.0, width, side),
    ]
    .into_iter()
    .flatten()
    {
        pixmap.fill_rect(rect, &paint, tiny_skia::Transform::identity(), None);
    }
}

/// Fill the tile with `gradient`, running from its start corner or edge to
/// the opposite one.
fn fill_gradient(pixmap: &mut tiny_skia::Pixmap, gradient: KeyGradient, side: f32) {
    let (start, end) = match gradient.direction {
        GradientDirection::Vertical => ((0.0, 0.0), (0.0, side)),
        GradientDirection::Horizontal => ((0.0, 0.0), (side, 0.0)),
        GradientDirection::Diagonal => ((0.0, 0.0), (side, side)),
    };
    let shader = tiny_skia::LinearGradient::new(
        tiny_skia::Point::from_xy(start.0, start.1),
        tiny_skia::Point::from_xy(end.0, end.1),
        vec![
            tiny_skia::GradientStop::new(0.0, color(gradient.from, 255)),
            tiny_skia::GradientStop::new(1.0, color(gradient.to, 255)),
        ],
        tiny_skia::SpreadMode::Pad,
        tiny_skia::Transform::identity(),
    );
    let (Some(shader), Some(rect)) = (shader, tiny_skia::Rect::from_xywh(0.0, 0.0, side, side))
    else {
        // Two equal stops collapse to a solid colour.
        pixmap.fill(color(gradient.from, 255));
        return;
    };
    let paint = tiny_skia::Paint {
        shader,
        ..tiny_skia::Paint::default()
    };
    pixmap.fill_rect(rect, &paint, tiny_skia::Transform::identity(), None);
}

/// Split a label into at most [`MAX_LABEL_LINES`] lines: explicit `\n`
/// breaks first, else one long multi-word line is broken at the space
/// nearest its middle.
fn label_lines(label: &str) -> Vec<String> {
    // A key shows two short lines; a hand-edited label of any length is cut
    // before it reaches text layout, which runs on every repaint.
    let label: String = label.chars().take(MAX_LABEL_CHARS).collect();
    let mut lines: Vec<String> = label
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    if lines.len() == 1 && lines[0].chars().count() > 9 && lines[0].contains(' ') {
        let line = lines.remove(0);
        let middle = line.len() / 2;
        let split = line
            .match_indices(' ')
            .map(|(at, _)| at)
            .min_by_key(|at| at.abs_diff(middle))
            .unwrap_or(middle);
        let (first, rest) = line.split_at(split);
        lines = vec![first.trim().to_owned(), rest.trim().to_owned()];
    }
    lines.truncate(MAX_LABEL_LINES);
    lines
}

/// Draw `lines` centred between `top` and `bottom`, at the largest size that
/// fits both the width and the height.
fn draw_label(
    pixmap: &mut tiny_skia::Pixmap,
    lines: &[String],
    face: &StyledFace,
    side: f32,
    top: f32,
    bottom: f32,
) {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a label has a handful of lines and characters"
    )]
    let longest = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(1)
        .max(1) as f32;
    #[expect(clippy::cast_precision_loss, reason = "at most two lines")]
    let count = lines.len() as f32;
    let line_height = 1.15;
    let by_width = side * LABEL_WIDTH / (longest * AVERAGE_GLYPH_ADVANCE);
    let by_height = (bottom - top) / (count * line_height);
    let font_size = by_width.min(by_height).clamp(9.0, side * 0.32);
    // A band behind text over a picture keeps it legible.
    if face.image.is_some()
        && let Some(band) =
            tiny_skia::Rect::from_ltrb(0.0, top - side * 0.02, side, bottom + side * 0.02)
    {
        let mut paint = tiny_skia::Paint::default();
        paint.set_color(color(face.background, 170));
        pixmap.fill_rect(band, &paint, tiny_skia::Transform::identity(), None);
    }
    let block = font_size * line_height * count;
    let first_baseline = top + ((bottom - top) - block) / 2.0 + font_size;
    let mut svg =
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{side}" height="{side}">"#);
    for (row, line) in lines.iter().enumerate() {
        #[expect(clippy::cast_precision_loss, reason = "at most two lines")]
        let y = first_baseline + row as f32 * font_size * line_height;
        let _ = write!(
            svg,
            r#"<text x="{x}" y="{y}" font-family="{FONT_FAMILIES}" font-size="{font_size}" font-weight="600" fill="{fill}" text-anchor="middle">{text}</text>"#,
            x = side / 2.0,
            fill = hex(face.foreground),
            text = escape_xml(line),
        );
    }
    svg.push_str("</svg>");
    let options = usvg::Options {
        fontdb: Arc::clone(fonts()),
        ..usvg::Options::default()
    };
    match usvg::Tree::from_str(&svg, &options) {
        Ok(tree) => resvg::render(
            &tree,
            tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        ),
        Err(error) => warn!(%error, "key label could not be laid out — drawn without it"),
    }
}

/// The system fonts, loaded once per process.
fn fonts() -> &'static Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    })
}

/// Draw the picture at `path` cropped to fill the tile. A picture that cannot
/// be read or decoded is logged and left out, so the rest of the face still
/// shows.
fn draw_picture(pixmap: &mut tiny_skia::Pixmap, path: &Path) {
    let picture = match load_picture(path) {
        Ok(picture) => picture,
        Err(error) => {
            warn!(path = %path.display(), %error, "key picture could not be read — drawn without it");
            return;
        }
    };
    let (width, height) = (pixmap.width(), pixmap.height());
    let mut rgba = picture
        .resize_to_fill(width, height, FilterType::Triangle)
        .to_rgba8();
    // tiny-skia stores premultiplied alpha.
    for pixel in rgba.pixels_mut() {
        let [r, g, b, a] = pixel.0;
        let premultiply = |c: u8| u8::try_from(u16::from(c) * u16::from(a) / 255).unwrap_or(c);
        pixel.0 = [premultiply(r), premultiply(g), premultiply(b), a];
    }
    let Some(size) = tiny_skia::IntSize::from_wh(width, height) else {
        return;
    };
    let Some(layer) = tiny_skia::Pixmap::from_vec(rgba.into_raw(), size) else {
        return;
    };
    pixmap.draw_pixmap(
        0,
        0,
        layer.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        tiny_skia::Transform::identity(),
        None,
    );
}

/// The most characters of a label a key face lays out.
const MAX_LABEL_CHARS: usize = 64;

/// The largest picture file a key face reads.
const MAX_PICTURE_BYTES: u64 = 32 * 1024 * 1024;
/// The largest picture side a key face decodes, in pixels.
const MAX_PICTURE_SIDE: u32 = 8192;
/// The most memory decoding one picture may allocate.
const MAX_PICTURE_ALLOC: u64 = 128 * 1024 * 1024;

/// Read the picture a key face names, within bounds: the path comes from a
/// hand-editable config and is read by the agent on every repaint. A network
/// or device path is refused before anything touches it (on Windows, opening
/// `\\host\share\…` would connect to that host), only a regular file of
/// at most [`MAX_PICTURE_BYTES`] is read — a FIFO would block the panel —
/// and the decoder is held to [`MAX_PICTURE_SIDE`] and
/// [`MAX_PICTURE_ALLOC`], so a decompression bomb fails instead of taking
/// the agent's memory.
pub(super) fn load_picture(path: &Path) -> Result<image::DynamicImage, String> {
    use std::path::{Component, Prefix};
    if let Some(Component::Prefix(prefix)) = path.components().next()
        && matches!(
            prefix.kind(),
            Prefix::UNC(..) | Prefix::VerbatimUNC(..) | Prefix::DeviceNS(..) | Prefix::Verbatim(..)
        )
    {
        return Err("network and device paths are not read".to_owned());
    }
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("not a regular file".to_owned());
    }
    if metadata.len() > MAX_PICTURE_BYTES {
        return Err(format!("larger than {MAX_PICTURE_BYTES} bytes"));
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_PICTURE_SIDE);
    limits.max_image_height = Some(MAX_PICTURE_SIDE);
    limits.max_alloc = Some(MAX_PICTURE_ALLOC);
    let mut reader = image::ImageReader::open(path)
        .and_then(image::ImageReader::with_guessed_format)
        .map_err(|error| error.to_string())?;
    reader.limits(limits);
    reader.decode().map_err(|error| error.to_string())
}

/// Font families tried for an emoji, colour fonts first.
const EMOJI_FAMILIES: &str =
    "'Segoe UI Emoji', 'Apple Color Emoji', 'Noto Color Emoji', 'Segoe UI Symbol', sans-serif";
/// An emoji's font size as a share of its box: the glyph's ink sits a little
/// inside its em square.
const EMOJI_SCALE: f32 = 0.86;

fn draw_glyph(
    pixmap: &mut tiny_skia::Pixmap,
    glyph: &Glyph<'_>,
    fill: Rgb,
    at: IconBox,
) -> Result<(), RenderError> {
    match glyph {
        Glyph::Action(icon) => draw_icon(pixmap, *icon, fill, at),
        Glyph::Symbol(KeySymbol::Lucide(name)) => {
            // A user's symbol that does not resolve is not a defect in the
            // artwork: the key is drawn without it.
            let Some(svg) = openlogi_ui::lucide::svg(name) else {
                warn!(%name, "no Lucide icon by that name — key drawn without it");
                return Ok(());
            };
            if let Err(error) = draw_svg(pixmap, &svg, fill, at) {
                warn!(%name, %error, "Lucide icon could not be drawn — key drawn without it");
            }
            Ok(())
        }
        Glyph::Symbol(KeySymbol::Emoji(text)) => {
            draw_emoji(pixmap, text, at);
            Ok(())
        }
    }
}

/// Draw `text` (an emoji) in its own colours, centred in `at`, from the
/// system's colour emoji font.
fn draw_emoji(pixmap: &mut tiny_skia::Pixmap, text: &str, at: IconBox) {
    let font_size = at.side * EMOJI_SCALE;
    // A colour emoji's ink spans about its ascent; this baseline centres it.
    let baseline = at.y + f32::midpoint(at.side, font_size * 0.72);
    let side = pixmap.width();
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{side}" height="{side}"><text x="{x}" y="{baseline}" font-family="{EMOJI_FAMILIES}" font-size="{font_size}" text-anchor="middle">{text}</text></svg>"#,
        x = at.x + at.side / 2.0,
        text = escape_xml(text),
    );
    let options = usvg::Options {
        fontdb: Arc::clone(fonts()),
        ..usvg::Options::default()
    };
    match usvg::Tree::from_str(&svg, &options) {
        Ok(tree) => resvg::render(
            &tree,
            tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        ),
        Err(error) => warn!(%error, "key emoji could not be laid out — drawn without it"),
    }
}

fn draw_icon(
    pixmap: &mut tiny_skia::Pixmap,
    icon: ActionRingIcon,
    fill: Rgb,
    at: IconBox,
) -> Result<(), RenderError> {
    let path = icon.asset_path();
    let svg =
        openlogi_ui::action_icons::embedded_icon(path).ok_or(RenderError::MissingIcon(path))?;
    draw_svg(pixmap, svg, fill, at).map_err(|message| RenderError::Svg { path, message })
}

/// Draw a monochrome SVG glyph tinted `fill`, fitted and centred in `at`.
fn draw_svg(
    pixmap: &mut tiny_skia::Pixmap,
    svg: &[u8],
    fill: Rgb,
    at: IconBox,
) -> Result<(), String> {
    // The glyphs stroke with `currentColor`, which a renderer outside a
    // styled document resolves to black — invisible on a dark key.
    let svg = String::from_utf8_lossy(svg).replace("currentColor", &hex(fill));
    let tree =
        usvg::Tree::from_str(&svg, &usvg::Options::default()).map_err(|error| error.to_string())?;
    let glyph = tree.size();
    let scale = at.side / glyph.width().max(glyph.height());
    let offset_x = at.x + (at.side - glyph.width() * scale) / 2.0;
    let offset_y = at.y + (at.side - glyph.height() * scale) / 2.0;
    let transform =
        tiny_skia::Transform::from_scale(scale, scale).post_translate(offset_x, offset_y);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Ok(())
}

fn color(rgb: Rgb, alpha: u8) -> tiny_skia::Color {
    let (r, g, b) = rgb.components();
    tiny_skia::Color::from_rgba8(r, g, b, alpha)
}

fn hex(rgb: Rgb) -> String {
    let (r, g, b) = rgb.components();
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
    let mut smallest = Vec::new();
    for quality in JPEG_QUALITIES {
        let mut jpeg = Cursor::new(Vec::new());
        JpegEncoder::new_with_quality(&mut jpeg, quality)
            .write_image(
                &rgb,
                u32::from(size),
                u32::from(size),
                ExtendedColorType::Rgb8,
            )
            .map_err(|error| RenderError::Encode(error.to_string()))?;
        smallest = jpeg.into_inner();
        if smallest.len() <= KEY_IMAGE_SINGLE_PACKET_MAX {
            break;
        }
    }
    let len = smallest.len();
    if len > KEY_IMAGE_SINGLE_PACKET_MAX {
        return Err(RenderError::Encode(format!(
            "{len} bytes do not fit one image packet even at the lowest quality"
        )));
    }
    KeyImageJpeg::new(smallest)
        .ok_or_else(|| RenderError::Encode(format!("{len} bytes do not fit the image write")))
}

#[cfg(test)]
mod tests;
