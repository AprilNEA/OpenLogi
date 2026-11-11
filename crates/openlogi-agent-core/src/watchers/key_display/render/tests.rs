use image::{ImageFormat, Rgb, RgbImage};
use openlogi_core::color::Rgb as Color;

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

fn styled(face: StyledFace) -> KeyFace {
    KeyFace::Styled(Arc::new(face))
}

fn plain(background: Color) -> StyledFace {
    StyledFace {
        icon: None,
        label: None,
        background,
        foreground: Color::WHITE,
        icon_color: Color::WHITE,
        gradient: None,
        image: None,
        symbol: None,
        progress: None,
        highlight: false,
    }
}

#[test]
fn a_live_face_draws_its_bar_and_its_frame() {
    let face = StyledFace {
        label: Some("45%".into()),
        progress: Some(100),
        highlight: true,
        icon_color: Color::new(0, 0xff, 0),
        ..plain(Color::new(0, 0, 0))
    };
    let jpeg = render_tile(&KeyFace::Styled(std::sync::Arc::new(face)), 118).expect("renders");
    assert!(!jpeg.as_bytes().is_empty());
}

fn near(pixel: Rgb<u8>, expected: [u8; 3]) -> bool {
    pixel
        .0
        .iter()
        .zip(expected)
        .all(|(&got, want)| got.abs_diff(want) < 24)
}

#[test]
fn a_styled_background_fills_the_tile() {
    let image = decode(&render_tile(&styled(plain(Color::new(0x20, 0x60, 0xc0))), KEY).unwrap());
    assert!(near(*image.get_pixel(5, 5), [0x20, 0x60, 0xc0]));
    assert!(near(*image.get_pixel(59, 59), [0x20, 0x60, 0xc0]));
}

#[test]
fn a_picture_fills_the_tile_and_an_unreadable_one_is_left_out() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("red.png");
    RgbImage::from_pixel(40, 20, Rgb([0xe0, 0x10, 0x10]))
        .save(&path)
        .unwrap();
    let face = StyledFace {
        image: Some(path),
        ..plain(Color::new(0, 0, 0))
    };
    let image = decode(&render_tile(&styled(face), KEY).unwrap());
    // Cropped to fill: even the corners of a wide picture are covered.
    assert!(near(*image.get_pixel(2, 2), [0xe0, 0x10, 0x10]));
    assert!(near(*image.get_pixel(115, 115), [0xe0, 0x10, 0x10]));

    let missing = StyledFace {
        image: Some(dir.path().join("missing.png")),
        ..plain(Color::new(0, 0x80, 0))
    };
    let image = decode(&render_tile(&styled(missing), KEY).unwrap());
    assert!(near(*image.get_pixel(59, 59), [0, 0x80, 0]));
}

#[test]
fn a_label_draws_text_in_the_foreground_colour() {
    if fonts().is_empty() {
        // A host without any system font cannot lay text out; the face then
        // renders without its label, which the other tests cover.
        return;
    }
    let face = StyledFace {
        label: Some("Export".into()),
        foreground: Color::new(0xff, 0xff, 0x00),
        ..plain(Color::new(0, 0, 0))
    };
    let image = decode(&render_tile(&styled(face), KEY).unwrap());
    let yellow = image
        .pixels()
        .filter(|pixel| pixel.0[0] > 180 && pixel.0[1] > 180 && pixel.0[2] < 90)
        .count();
    assert!(yellow > 60, "only {yellow} label pixels");
}

#[test]
fn an_icon_and_a_label_share_the_key() {
    let face = StyledFace {
        icon: Some(ActionRingIcon::Copy),
        label: Some("Copy it".into()),
        ..plain(Color::new(0, 0, 0))
    };
    let tile = render_tile(&styled(face), KEY).unwrap();
    assert_eq!(decode(&tile).dimensions(), (118, 118));
}

#[test]
fn long_labels_wrap_to_two_lines() {
    assert_eq!(label_lines("Save for web"), vec!["Save", "for web"]);
    assert_eq!(label_lines("a\nb\nc"), vec!["a", "b"]);
    assert_eq!(label_lines("  Short  "), vec!["Short"]);
    assert!(label_lines(" \n ").is_empty());
}

#[test]
fn a_detailed_picture_still_fits_one_image_packet() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("noise.png");
    // Per-pixel noise is the worst case for JPEG size.
    RgbImage::from_fn(118, 118, |x, y| {
        let [v, ..] = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)).to_le_bytes();
        Rgb([v, v.rotate_left(3), v.rotate_left(5)])
    })
    .save(&path)
    .unwrap();
    let face = StyledFace {
        image: Some(path),
        label: Some("Noise".into()),
        ..plain(Color::new(0, 0, 0))
    };
    let tile = render_tile(&styled(face), KEY).unwrap();
    assert!(tile.as_bytes().len() <= KEY_IMAGE_SINGLE_PACKET_MAX);
}

#[test]
fn a_lucide_symbol_replaces_the_action_icon_in_the_text_colour() {
    let face = StyledFace {
        icon: Some(ActionRingIcon::Copy),
        symbol: Some(KeySymbol::Lucide("camera".into())),
        icon_color: Color::new(0x00, 0xff, 0x00),
        ..plain(Color::new(0, 0, 0))
    };
    let image = decode(&render_tile(&styled(face), KEY).unwrap());
    let green = image
        .pixels()
        .filter(|pixel| pixel.0[1] > 180 && pixel.0[0] < 90 && pixel.0[2] < 90)
        .count();
    assert!(green > 60, "only {green} icon pixels");
}

#[test]
fn an_unknown_lucide_name_leaves_the_key_without_a_symbol() {
    let face = StyledFace {
        symbol: Some(KeySymbol::Lucide("no-such-icon".into())),
        ..plain(Color::new(0, 0, 0))
    };
    let image = decode(&render_tile(&styled(face), KEY).unwrap());
    assert!(image.pixels().all(|pixel| luma(*pixel) < 16));
}

#[test]
fn an_emoji_symbol_draws_in_colour() {
    if fonts().is_empty() {
        return;
    }
    let face = StyledFace {
        symbol: Some(KeySymbol::Emoji("🍎".into())),
        ..plain(Color::new(0, 0, 0))
    };
    let image = decode(&render_tile(&styled(face), KEY).unwrap());
    let inked = image.pixels().filter(|pixel| luma(**pixel) > 40).count();
    assert!(inked > 200, "only {inked} emoji pixels");
}

#[test]
fn a_gradient_runs_from_its_start_colour_to_its_end_colour() {
    use openlogi_core::config::{GradientDirection, KeyGradient};
    let face = StyledFace {
        gradient: Some(KeyGradient {
            from: Color::new(0xff, 0x00, 0x00),
            to: Color::new(0x00, 0x00, 0xff),
            direction: GradientDirection::Horizontal,
        }),
        ..plain(Color::new(0, 0, 0))
    };
    let image = decode(&render_tile(&styled(face), KEY).unwrap());
    let left = image.get_pixel(2, 59).0;
    let right = image.get_pixel(115, 59).0;
    assert!(left[0] > 200 && left[2] < 60, "{left:?}");
    assert!(right[2] > 200 && right[0] < 60, "{right:?}");
}
