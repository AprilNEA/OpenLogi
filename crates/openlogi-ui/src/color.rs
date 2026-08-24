//! The brand accent — the one colour both GPUI processes paint with.
//!
//! It does *not* flip with the OS appearance: it is saturated enough to read on
//! the settings app's light card surfaces and on the overlay's near-black ring
//! panel alike, so a single constant serves both. That is why it lives here
//! rather than in either binary — the overlay cannot depend on
//! `openlogi-desktop`, and a local copy is how the ring ended up drawing a blue
//! of its own, five degrees of hue off the brand.
//!
//! Everything else stays with its owner: the settings app's status hues and its
//! appearance-derived surface and text colours are resolved from the active
//! `gpui-component` theme, which this crate deliberately does not depend on, and
//! the overlay never draws them.

use gpui::{Hsla, rgb};

/// Primary action / selection blue. The brand colour, identical in both
/// appearances.
pub const ACCENT_BLUE: u32 = 0x003b_82f6;

/// [`ACCENT_BLUE`] as an [`Hsla`], so callers stop re-`rgb()`-ing the constant.
#[must_use]
pub fn accent() -> Hsla {
    rgb(ACCENT_BLUE).into()
}

/// The accent at a different lightness, keeping its hue and saturation.
///
/// The overlay draws its ring on a near-black panel, where the brand lightness
/// sits too close to the white glyph the slot carries; the settings app never
/// needs to move it. Shifting only `l` is what keeps that a *shade of the
/// accent* rather than a second blue — the drift this module exists to prevent.
#[must_use]
pub fn accent_at_lightness(lightness: f32) -> Hsla {
    Hsla {
        l: lightness,
        ..accent()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The overlay derives both its selected-slot fill and that slot's border
    /// from [`accent_at_lightness`]. Pin that the derivation carries the brand
    /// hue and saturation through, so an edit there cannot quietly reintroduce
    /// the second accent this module replaced.
    #[test]
    fn accent_shades_keep_the_brand_hue_and_saturation() {
        let brand = accent();
        for lightness in [0.48, 0.78] {
            let shade = accent_at_lightness(lightness);
            assert!(
                (shade.h - brand.h).abs() < f32::EPSILON,
                "hue drifted at l={lightness}: {} vs {}",
                shade.h,
                brand.h
            );
            assert!(
                (shade.s - brand.s).abs() < f32::EPSILON,
                "saturation drifted at l={lightness}: {} vs {}",
                shade.s,
                brand.s
            );
            assert!(
                (shade.l - lightness).abs() < f32::EPSILON,
                "lightness not applied: {}",
                shade.l
            );
        }
    }
}
