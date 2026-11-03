use image::{ImageFormat, Rgb, RgbImage};

use super::*;

const KEY: u16 = 118;

fn decode(tile: &KeyImageJpeg) -> RgbImage {
    image::load_from_memory_with_format(tile.as_bytes(), ImageFormat::Jpeg)
        .expect("a rendered tile is a valid JPEG")
        .to_rgb8()
}

fn luma(pixel: Rgb<u8>) -> u16 {
    pixel.0.iter().copied().map(u16::from).sum::<u16>() / 3
}

#[test]
fn every_ring_icon_renders_to_a_key_sized_jpeg() {
    for icon in ActionRingIcon::ALL {
        let tile = render_tile(KeyFace::Icon(icon), KEY)
            .unwrap_or_else(|error| panic!("{icon:?}: {error}"));
        assert_eq!(decode(&tile).dimensions(), (118, 118), "{icon:?}");
    }
}

#[test]
fn a_blank_face_is_dark_everywhere() {
    let image = decode(&render_tile(KeyFace::Blank, KEY).unwrap());
    assert!(image.pixels().all(|pixel| luma(*pixel) < 16));
}

/// White glyph strokes in the middle, untouched black margins at the edges:
/// a `currentColor` left unresolved would draw black on black and fail the
/// first half, a mis-scaled glyph would bleed into the second.
#[test]
fn an_icon_is_drawn_bright_and_centred_inside_dark_margins() {
    let image = decode(&render_tile(KeyFace::Icon(ActionRingIcon::Copy), KEY).unwrap());
    let margin = u32::from(KEY) / 8;
    let (width, height) = image.dimensions();

    let bright_centre = image
        .enumerate_pixels()
        .filter(|(x, y, _)| {
            (margin..width - margin).contains(x) && (margin..height - margin).contains(y)
        })
        .filter(|(_, _, pixel)| luma(**pixel) > 200)
        .count();
    assert!(bright_centre > 100, "only {bright_centre} bright pixels");

    let edges_dark = image
        .enumerate_pixels()
        .filter(|(x, y, _)| {
            *x < margin || *y < margin || *x >= width - margin || *y >= height - margin
        })
        .all(|(_, _, pixel)| luma(*pixel) < 24);
    assert!(edges_dark, "glyph reaches into the margin");
}

#[test]
fn a_zero_sized_tile_is_refused() {
    assert!(matches!(
        render_tile(KeyFace::Blank, 0),
        Err(RenderError::TileSize(0))
    ));
}
