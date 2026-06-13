//! Colors and shared sizes for the OpenLogi UI.
//!
//! Two layers:
//!
//! - **Brand / status** colours are fixed `u32` constants. They're saturated
//!   enough to read on both light and dark backgrounds, so they don't change
//!   with the OS appearance (the OpenLogi accent blue, the connectivity dots).
//! - **Surface / text** colours flip with the appearance and live in
//!   [`Palette`], chosen by [`palette`] from the active gpui-component theme
//!   mode. The bespoke surfaces (window, cards, mouse model)
//!   read these so they track the same light/dark switch as gpui-component's
//!   own widgets — which is what keeps a popover from rendering white under
//!   an otherwise dark UI (see `main.rs`'s appearance wiring).

use gpui::{App, Hsla, Styled, hsla, rgb};
use gpui_component::ActiveTheme as _;

/// Primary action / selection blue. Brand colour, identical in both modes —
/// it reads on the light card surfaces and the dark window alike.
pub const ACCENT_BLUE: u32 = 0x003b_82f6;

/// Status colours for the carousel connectivity dot.
pub const STATUS_CONNECTED: u32 = 0x0022_c55e;
pub const STATUS_CONNECTING: u32 = 0x00ea_b308;
pub const STATUS_OFFLINE: u32 = 0x006b_7280;

/// Sizes that several components need to agree on.
pub const HEADER_H: f32 = 80.;
pub const FOOTER_H: f32 = 50.;

/// Fixed footprint of a device card in the Home gallery. Equal-width cards lay
/// out in a horizontally scrollable row (centred when they fit, scrollable when
/// they don't); `GALLERY_PHOTO_H` is the height of the device photo above the
/// name/battery row.
pub const GALLERY_CARD_W: f32 = 240.;
pub const GALLERY_PHOTO_H: f32 = 230.;

/// Appearance-dependent surface + text colours for the bespoke (non
/// gpui-component) surfaces. Resolved once per render via [`palette`] and
/// passed down to the free helper builders.
///
/// We hand-pick both variants rather than reading gpui-component's tokens:
/// its shadcn-neutral palette collapses the raised-surface and hover fills
/// onto the same neutral step (`accent` falls back to `secondary`), which
/// would flatten the app's layered card/hover look. Keeping our own values
/// preserves the tuned dark appearance and gives a controlled light one.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// Window background.
    pub bg: Hsla,
    /// Raised card / panel fill.
    pub surface: Hsla,
    /// Card hover / armed fill.
    pub surface_hover: Hsla,
    /// Hairline border between cards and surface.
    pub border: Hsla,
    /// Foreground text.
    pub text_primary: Hsla,
    /// De-emphasised labels / metadata.
    pub text_muted: Hsla,
}

impl Palette {
    /// The dark palette — the original OpenLogi look, unchanged.
    #[must_use]
    pub fn dark() -> Self {
        Self {
            bg: rgb(0x001a_1a1d).into(),
            surface: rgb(0x0022_2227).into(),
            surface_hover: rgb(0x002c_2c33).into(),
            border: rgb(0x002f_2f36).into(),
            text_primary: rgb(0x00e8_e8ec).into(),
            text_muted: rgb(0x008a_8a93).into(),
        }
    }

    /// The light palette — white cards on a soft-grey window, tuned to sit
    /// alongside gpui-component's light popover/surface tokens.
    #[must_use]
    pub fn light() -> Self {
        Self {
            bg: rgb(0x00f4_f4f6).into(),
            surface: rgb(0x00ff_ffff).into(),
            surface_hover: rgb(0x00e9_e9ee).into(),
            border: rgb(0x00d9_d9e0).into(),
            text_primary: rgb(0x001a_1a1d).into(),
            text_muted: rgb(0x006b_6b73).into(),
        }
    }
}

/// Resolve the app palette from the active gpui-component theme mode, so the
/// hand-painted surfaces follow the same light/dark switch as the widgets.
#[must_use]
pub fn palette(cx: &App) -> Palette {
    if cx.theme().mode.is_dark() {
        Palette::dark()
    } else {
        Palette::light()
    }
}

/// [`ACCENT_BLUE`] as an [`Hsla`] — the selection accent for borders and fills
/// on selectable controls, so callers stop re-`rgb()`-ing the brand constant.
#[must_use]
pub fn accent() -> Hsla {
    rgb(ACCENT_BLUE).into()
}

/// Faint accent fill marking a *selected* row / chip — tinted, not painted, so
/// it reads on both palettes while the label stays in `text_primary` (a blue
/// label fails AA contrast on the light surface). Hand-matched to [`accent`]
/// (hue 0.6 / sat 0.9 / light 0.6); [`tests::accent_tint_matches_accent`] pins
/// that it stays derived from the brand colour.
#[must_use]
pub fn accent_tint() -> Hsla {
    hsla(0.6, 0.9, 0.6, 0.12)
}

/// [`accent_tint`] deepened for hover on an already-selected row.
#[must_use]
pub fn accent_tint_hover() -> Hsla {
    hsla(0.6, 0.9, 0.6, 0.18)
}

/// Chaining helpers expressing the single "selected" decision — accent border
/// plus a faint accent fill — instead of every pill / chip / row hand-rolling
/// the `if selected { accent } else { border }` ternary (which had drifted into
/// three inconsistent dialects, one of them blue-on-white). Blanket-implemented
/// for every [`Styled`] element, the way gpui-component extends styling.
pub trait SelectableStyle: Styled + Sized {
    /// A 1px accent border when `selected`, the neutral hairline otherwise.
    #[must_use]
    fn selected_border(self, selected: bool, pal: Palette) -> Self {
        self.border_1()
            .border_color(if selected { accent() } else { pal.border })
    }

    /// A faint accent fill when `selected`; leaves the background untouched
    /// otherwise so the caller's resting fill shows through.
    #[must_use]
    fn selected_fill(self, selected: bool) -> Self {
        if selected {
            self.bg(accent_tint())
        } else {
            self
        }
    }
}

impl<E: Styled> SelectableStyle for E {}

#[cfg(test)]
mod tests {
    use super::*;

    /// `accent_tint` is hand-written `hsla` (gpui's `rgb→hsla` isn't `const`),
    /// so pin that it stays derived from `ACCENT_BLUE` rather than drifting into
    /// an arbitrary blue — selected chips must match the accent borders and text
    /// they sit beside.
    #[test]
    fn accent_tint_matches_accent() {
        let a = accent();
        let t = accent_tint();
        assert!((a.h - t.h).abs() < 0.02, "hue {} vs {}", a.h, t.h);
        assert!((a.s - t.s).abs() < 0.05, "sat {} vs {}", a.s, t.s);
        assert!((a.l - t.l).abs() < 0.05, "light {} vs {}", a.l, t.l);
    }
}
