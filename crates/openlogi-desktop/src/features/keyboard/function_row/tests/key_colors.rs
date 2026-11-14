//! The key-colour rules: the shared palette, custom hex input, the
//! background's solid and gradient modes, and the icon colour's fallback.

use std::collections::HashSet;

use super::super::key_colors::{
    BackgroundMode, ColorField, GRADIENT_PRESETS, PALETTE, apply_gradient_preset, gradient_angle,
    icon_color, parse_hex,
};
use openlogi_core::color::Rgb;
use openlogi_core::config::{GradientDirection, KeyFaceStyle, KeyGradient};

#[test]
fn the_palette_is_large_and_has_no_duplicates() {
    assert!(PALETTE.len() >= 28, "{} colours", PALETTE.len());
    let distinct: HashSet<u32> = PALETTE.iter().map(|color| color.packed()).collect();
    assert_eq!(distinct.len(), PALETTE.len(), "a colour is listed twice");
    assert!(PALETTE.contains(&Rgb::new(0, 0, 0)) && PALETTE.contains(&Rgb::WHITE));
    let presets: HashSet<(u32, u32)> = GRADIENT_PRESETS
        .iter()
        .map(|(from, to)| (from.packed(), to.packed()))
        .collect();
    assert_eq!(presets.len(), GRADIENT_PRESETS.len());
    assert!(GRADIENT_PRESETS.len() >= 12);
}

#[test]
fn a_custom_colour_is_six_hex_digits_with_or_without_a_hash() {
    let orange = Rgb::new(0xff, 0x88, 0x00);
    assert_eq!(parse_hex("ff8800"), Some(orange));
    assert_eq!(parse_hex("#FF8800"), Some(orange));
    assert_eq!(parse_hex("  #ff8800 "), Some(orange));
    for bad in ["", "#", "ff88", "ff88001", "gg8800", "##ff8800"] {
        assert_eq!(parse_hex(bad), None, "{bad:?}");
    }
}

/// Gradient mode starts a face without one on the first preset; solid mode
/// drops the gradient again and leaves the flat colour as it was.
#[test]
fn the_background_mode_switches_between_solid_and_gradient() {
    let mut style = KeyFaceStyle {
        background: Some(Rgb::new(0x20, 0x40, 0x80)),
        ..KeyFaceStyle::default()
    };
    assert_eq!(BackgroundMode::of(&style), BackgroundMode::Solid);

    BackgroundMode::Gradient.apply(&mut style);
    let (from, to) = GRADIENT_PRESETS[0];
    assert_eq!(
        style.gradient,
        Some(KeyGradient {
            from,
            to,
            direction: GradientDirection::Vertical
        })
    );
    assert_eq!(BackgroundMode::of(&style), BackgroundMode::Gradient);

    // A gradient already set stays when gradient mode is picked again.
    style.gradient = Some(KeyGradient {
        from: Rgb::WHITE,
        to: Rgb::new(0, 0, 0),
        direction: GradientDirection::Diagonal,
    });
    let kept = style.gradient;
    BackgroundMode::Gradient.apply(&mut style);
    assert_eq!(style.gradient, kept);

    BackgroundMode::Solid.apply(&mut style);
    assert_eq!(style.gradient, None);
    assert_eq!(style.background, Some(Rgb::new(0x20, 0x40, 0x80)));
}

#[test]
fn a_preset_keeps_the_gradients_direction() {
    let mut style = KeyFaceStyle {
        gradient: Some(KeyGradient {
            from: Rgb::WHITE,
            to: Rgb::WHITE,
            direction: GradientDirection::Horizontal,
        }),
        ..KeyFaceStyle::default()
    };
    apply_gradient_preset(&mut style, GRADIENT_PRESETS[3]);
    let gradient = style.gradient.expect("a gradient");
    assert_eq!((gradient.from, gradient.to), GRADIENT_PRESETS[3]);
    assert_eq!(gradient.direction, GradientDirection::Horizontal);

    let mut plain = KeyFaceStyle::default();
    apply_gradient_preset(&mut plain, GRADIENT_PRESETS[1]);
    assert_eq!(
        plain.gradient.map(|gradient| gradient.direction),
        Some(GradientDirection::Vertical)
    );
}

/// Each chooser edits its own field; a gradient end set on a face without a
/// gradient starts one, and clearing it to default leaves the gradient.
#[test]
fn each_colour_field_sets_its_own_part_of_the_face() {
    let red = Rgb::new(0xff, 0x3b, 0x30);
    let mut style = KeyFaceStyle::default();
    for field in [ColorField::Background, ColorField::Text, ColorField::Icon] {
        field.set(&mut style, Some(red));
        assert_eq!(field.get(&style), Some(red));
        field.set(&mut style, None);
        assert_eq!(field.get(&style), None);
    }

    ColorField::GradientTo.set(&mut style, Some(red));
    let gradient = style.gradient.expect("the end started a gradient");
    assert_eq!((gradient.from, gradient.to), (GRADIENT_PRESETS[0].0, red));
    ColorField::GradientFrom.set(&mut style, None);
    assert_eq!(
        style.gradient,
        Some(gradient),
        "a gradient end is never unset"
    );
}

#[test]
fn the_icon_takes_its_own_colour_else_the_texts_else_white() {
    let yellow = Rgb::new(0xff, 0xcc, 0x00);
    let blue = Rgb::new(0x00, 0x7a, 0xff);
    assert_eq!(icon_color(&KeyFaceStyle::default()), Rgb::WHITE);
    let text_only = KeyFaceStyle {
        color: Some(blue),
        ..KeyFaceStyle::default()
    };
    assert_eq!(icon_color(&text_only), blue);
    let both = KeyFaceStyle {
        icon_color: Some(yellow),
        ..text_only
    };
    assert_eq!(icon_color(&both), yellow);
}

#[test]
fn gradient_directions_map_to_css_angles() {
    assert_eq!(gradient_angle(GradientDirection::Vertical), 180.);
    assert_eq!(gradient_angle(GradientDirection::Horizontal), 90.);
    assert_eq!(gradient_angle(GradientDirection::Diagonal), 135.);
}
