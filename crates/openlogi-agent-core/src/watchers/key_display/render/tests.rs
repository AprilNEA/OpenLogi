use std::hash::BuildHasher as _;

use image::codecs::png::PngEncoder;
use image::{ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};

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
        let tile = render_tile(&KeyFace::Icon(icon), KEY)
            .unwrap_or_else(|error| panic!("{icon:?}: {error}"));
        assert_eq!(decode(&tile).dimensions(), (118, 118), "{icon:?}");
    }
}

#[test]
fn a_blank_face_is_dark_everywhere() {
    let image = decode(&render_tile(&KeyFace::Blank, KEY).unwrap());
    assert!(image.pixels().all(|pixel| luma(*pixel) < 16));
}

/// White glyph strokes in the middle, untouched black margins at the edges:
/// a `currentColor` left unresolved would draw black on black and fail the
/// first half, a mis-scaled glyph would bleed into the second.
#[test]
fn an_icon_is_drawn_bright_and_centred_inside_dark_margins() {
    let image = decode(&render_tile(&KeyFace::Icon(ActionRingIcon::Copy), KEY).unwrap());
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
        render_tile(&KeyFace::Blank, 0),
        Err(RenderError::TileSize(0))
    ));
}

fn png(image: &RgbaImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        )
        .unwrap();
    bytes
}

fn jpeg(image: &RgbImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, 90)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgb8,
        )
        .unwrap();
    bytes
}

fn opaque(width: u32, height: u32, rgb: [u8; 3]) -> RgbaImage {
    RgbaImage::from_pixel(width, height, Rgba([rgb[0], rgb[1], rgb[2], 255]))
}

/// Whether `pixel` is within a JPEG's error of `rgb`.
fn near(pixel: Rgb<u8>, rgb: [u8; 3]) -> bool {
    pixel
        .0
        .iter()
        .zip(rgb)
        .all(|(&got, want)| got.abs_diff(want) < 40)
}

#[test]
fn an_image_renders_to_a_key_sized_jpeg() {
    for bytes in [
        png(&opaque(300, 300, [220, 30, 30])),
        jpeg(&RgbImage::from_pixel(300, 300, Rgb([220, 30, 30]))),
    ] {
        let face = KeyFace::Image(KeyImage::decode(&bytes).unwrap());
        let tile = decode(&render_tile(&face, KEY).unwrap());
        assert_eq!(tile.dimensions(), (118, 118));
        assert!(near(*tile.get_pixel(59, 59), [220, 30, 30]));
    }
}

/// Red, green and blue thirds side by side: a centre crop keeps only the
/// green, where squeezing the whole image into the square would keep all
/// three.
#[test]
fn a_wide_image_is_cropped_to_its_centre() {
    let colours = [[220, 30, 30], [30, 200, 30], [30, 30, 220]];
    let wide = RgbaImage::from_fn(300, 100, |x, _| {
        let [r, g, b] = colours[(x / 100) as usize];
        Rgba([r, g, b, 255])
    });
    let image = KeyImage::decode(&png(&wide)).unwrap();
    assert_eq!(image.pixels.dimensions(), (100, 100));

    let tile = decode(&render_tile(&KeyFace::Image(image), KEY).unwrap());
    for x in [2, 59, 115] {
        let pixel = *tile.get_pixel(x, 59);
        assert!(near(pixel, [30, 200, 30]), "column {x} is {pixel:?}");
    }
}

#[test]
fn a_tall_image_is_cropped_to_its_centre() {
    let tall = RgbaImage::from_fn(10, 30, |_, y| {
        if (10..20).contains(&y) {
            Rgba([255, 255, 255, 255])
        } else {
            Rgba([0, 0, 0, 255])
        }
    });
    let image = KeyImage::decode(&png(&tall)).unwrap();
    assert!(image.pixels.pixels().all(|pixel| luma(*pixel) == 255));
}

#[test]
fn transparency_is_shown_over_the_panels_black() {
    let clear = RgbaImage::from_pixel(64, 64, Rgba([255, 255, 255, 0]));
    let image = KeyImage::decode(&png(&clear)).unwrap();
    assert!(image.pixels.pixels().all(|pixel| luma(*pixel) == 0));

    let half = RgbaImage::from_pixel(64, 64, Rgba([255, 255, 255, 128]));
    let image = KeyImage::decode(&png(&half)).unwrap();
    assert!(image.pixels.pixels().all(|pixel| luma(*pixel) == 128));
}

/// `jpeg` with the EXIF block a camera writes to say the picture is shown
/// turned: Orientation (tag 0x0112) set to `orientation`.
fn with_exif_orientation(jpeg: &[u8], orientation: u8) -> Vec<u8> {
    let mut exif = b"Exif\0\0MM\x00\x2a\x00\x00\x00\x08\x00\x01".to_vec();
    exif.extend_from_slice(&[0x01, 0x12, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01]);
    exif.extend_from_slice(&[0x00, orientation, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    let len = u16::try_from(exif.len() + 2).unwrap();
    let (soi, rest) = jpeg.split_at(2);
    [soi, &[0xff, 0xe1], &len.to_be_bytes(), &exif, rest].concat()
}

/// A portrait photo is stored on its side with Orientation 6, "turn 90
/// degrees clockwise to show": its stored left edge is the top.
#[test]
fn a_jpeg_is_turned_upright_as_its_exif_orientation_says() {
    let red = [220, 30, 30];
    let blue = [30, 30, 220];
    let left_red = RgbImage::from_fn(64, 64, |x, _| Rgb(if x < 32 { red } else { blue }));
    let stored = jpeg(&left_red);

    let as_stored = KeyImage::decode(&stored).unwrap();
    assert!(near(*as_stored.pixels.get_pixel(4, 32), red));
    assert!(near(*as_stored.pixels.get_pixel(60, 32), blue));

    let turned = KeyImage::decode(&with_exif_orientation(&stored, 6)).unwrap();
    for x in [4, 32, 60] {
        assert!(near(*turned.pixels.get_pixel(x, 4), red), "top at {x}");
        assert!(near(*turned.pixels.get_pixel(x, 60), blue), "bottom at {x}");
    }
}

#[test]
fn a_large_image_is_reduced_and_a_small_one_kept() {
    let large = KeyImage::decode(&png(&opaque(1500, 1000, [9, 9, 9]))).unwrap();
    assert_eq!(
        large.pixels.dimensions(),
        (MAX_SOURCE_SIDE, MAX_SOURCE_SIDE)
    );
    let small = KeyImage::decode(&png(&opaque(40, 40, [9, 9, 9]))).unwrap();
    assert_eq!(small.pixels.dimensions(), (40, 40));
}

#[test]
fn an_image_past_the_decode_limit_is_refused() {
    let wide = png(&opaque(MAX_DECODE_SIDE + 1, 1, [0, 0, 0]));
    assert!(matches!(
        KeyImage::decode(&wide),
        Err(KeyImageDecodeError::TooLarge)
    ));
    let at_limit = png(&opaque(MAX_DECODE_SIDE, 1, [0, 0, 0]));
    assert_eq!(
        KeyImage::decode(&at_limit).unwrap().pixels.dimensions(),
        (1, 1)
    );
}

#[test]
fn bytes_that_are_not_a_png_or_jpeg_are_refused() {
    for bytes in [
        &b""[..],
        b"not an image at all",
        b"GIF89a\x01\x00\x01\x00\x00\x00\x00;",
        b"BM\x1e\x00\x00\x00\x00\x00\x00\x00\x1a\x00\x00\x00",
    ] {
        assert!(
            matches!(
                KeyImage::decode(bytes),
                Err(KeyImageDecodeError::UnsupportedFormat)
            ),
            "{bytes:?}"
        );
    }
}

#[test]
fn a_damaged_png_or_jpeg_is_refused() {
    let png = png(&opaque(64, 64, [1, 2, 3]));
    let jpeg = jpeg(&RgbImage::from_pixel(64, 64, Rgb([1, 2, 3])));
    for truncated in [&png[..png.len() / 2], &jpeg[..jpeg.len() / 2]] {
        assert!(matches!(
            KeyImage::decode(truncated),
            Err(KeyImageDecodeError::Undecodable(_))
        ));
    }
}

/// Equal by content, so a second push of the same picture is no change;
/// equal images hash alike, as the tile cache needs.
#[test]
fn images_are_equal_exactly_when_their_pixels_are() {
    let red = png(&opaque(32, 32, [200, 0, 0]));
    let first = KeyImage::decode(&red).unwrap();
    let again = KeyImage::decode(&red).unwrap();
    assert!(!Arc::ptr_eq(&first.pixels, &again.pixels));
    assert_eq!(first, again);
    let hasher = std::collections::hash_map::RandomState::new();
    assert_eq!(hasher.hash_one(&first), hasher.hash_one(&again));

    let blue = KeyImage::decode(&png(&opaque(32, 32, [0, 0, 200]))).unwrap();
    assert_ne!(first, blue);
    let larger = KeyImage::decode(&png(&opaque(33, 33, [200, 0, 0]))).unwrap();
    assert_ne!(first, larger);
}

#[test]
fn an_image_debug_shows_no_pixels() {
    let image = KeyImage::solid(16, [1, 2, 3]);
    let debug = format!("{image:?}");
    assert!(debug.starts_with("KeyImage { digest: "), "{debug}");
    assert!(debug.ends_with("size: 16x16 }"), "{debug}");
}

#[test]
fn a_zero_sized_image_tile_is_refused() {
    assert!(matches!(
        render_tile(&KeyFace::Image(KeyImage::solid(8, [0, 0, 0])), 0),
        Err(RenderError::TileSize(0))
    ));
}
