//! What each Actions Ring theme looks like at rest: sizes, colours and fills.
//!
//! The overlay draws the ring and the settings app draws a still preview of
//! it beside the slot editor. Both read this module, so the preview cannot
//! drift from the ring the way a copy would. Motion — clocks, easing, the
//! animated arcs and glow — stays in the overlay: a still frame has none.

use gpui::{Background, Hsla, linear_color_stop, linear_gradient};

use crate::color;

const fn neutral(lightness: f32, alpha: f32) -> Hsla {
    Hsla {
        h: 0.0,
        s: 0.0,
        l: lightness,
        a: alpha,
    }
}

/// `a` towards `b` by `t`, channel by channel.
#[must_use]
pub const fn lerp_hsla(a: Hsla, b: Hsla, t: f32) -> Hsla {
    Hsla {
        h: a.h + (b.h - a.h) * t,
        s: a.s + (b.s - a.s) * t,
        l: a.l + (b.l - a.l) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

/// The classic disc of round icon slots.
pub mod classic {
    use super::{Hsla, color, neutral};

    /// The disc's edge, which is also its window's.
    pub const PANEL_SIZE: f32 = 360.0;
    /// How far the dark disc sits inside the panel's edge.
    pub const DISC_INSET: f32 = 18.0;
    pub const SLOT_SIZE: f32 = 54.0;
    /// Distance from the panel's centre to each slot's centre.
    pub const RADIUS: f32 = 122.0;
    pub const SLOT_GLYPH: f32 = 22.0;
    pub const CANCEL_SIZE: f32 = 48.0;
    pub const CANCEL_GLYPH_SIZE: f32 = 20.0;
    /// Where the hovered slot's label sits below the panel's centre, and how
    /// wide it may run.
    pub const LABEL_OFFSET: f32 = 34.0;
    pub const LABEL_WIDTH: f32 = 160.0;

    /// The ring's own neutral scale. It floats over whatever is on the
    /// desktop, so unlike the settings app it cannot take its surfaces from
    /// the OS appearance — it commits to a dark panel and rides its own
    /// contrast. Only the accent is shared with the app.
    pub const PANEL: Hsla = neutral(0.06, 0.82);
    pub const SLOT_RESTING: Hsla = neutral(0.16, 0.98);
    pub const CANCEL_RESTING: Hsla = neutral(0.20, 0.98);
    pub const GLYPH: Hsla = neutral(0.98, 1.0);
    pub const LABEL: Hsla = neutral(0.94, 1.0);
    pub const CANCEL_GLYPH: Hsla = neutral(0.82, 1.0);

    /// The selected slot's fill: the accent deepened for the dark panel, as
    /// the brand lightness sits too close to the white glyph it carries.
    #[must_use]
    pub fn selected_fill() -> Hsla {
        color::accent_at_lightness(0.48)
    }

    /// The ring around the selected slot, lifted against the deepened fill.
    #[must_use]
    pub fn selected_border() -> Hsla {
        color::accent_at_lightness(0.78)
    }
}

/// Captioned pills around a hub, over thin HUD tracks.
pub mod in_motion {
    use super::{Background, Hsla, color, lerp_hsla, linear_color_stop, linear_gradient, neutral};

    /// The panel's designed edge: wide enough for the captions, small enough
    /// for 1080p at 200%.
    pub const DESIGN_EDGE: f32 = 480.0;
    pub const PILL_W: f32 = 132.0;
    pub const PILL_H: f32 = 34.0;
    pub const PILL_GLYPH: f32 = 16.0;
    /// Distance from the panel's centre to each pill's centre.
    pub const RADIUS: f32 = 160.0;
    /// The hub at rest: the cancel button.
    pub const HUB: f32 = 48.0;
    pub const HUB_GLYPH: f32 = 20.0;
    /// The inner and outer HUD tracks, as fractions of [`RADIUS`].
    pub const TRACK_INNER: f32 = 0.62;
    pub const TRACK_OUTER: f32 = 1.16;
    /// The hub's glow, outermost disc first: its radius, its accent alpha,
    /// and how much more alpha it gains at the top of its breath.
    pub const HUB_GLOW: [(f32, f32, f32); 3] =
        [(72.0, 0.12, 0.05), (48.0, 0.16, 0.06), (28.0, 0.22, 0.0)];

    /// A slot's caption and glyph at rest.
    pub const GLYPH: Hsla = neutral(0.98, 1.0);
    /// A hovered slot's caption and glyph, dark on its white pill.
    pub const INK: Hsla = neutral(0.08, 1.0);
    pub const CANCEL_GLYPH: Hsla = neutral(0.84, 1.0);

    /// A pill's ink, `hover` of the way from rest to hovered.
    #[must_use]
    pub const fn slot_ink(hover: f32) -> Hsla {
        lerp_hsla(GLYPH, INK, hover)
    }

    /// A pill's fill, `hover` of the way from its dark rest to the white of a
    /// hovered slot.
    #[must_use]
    pub fn slot_fill(hover: f32) -> Background {
        let (rest_top, rest_bottom) = (neutral(0.18, 0.92), neutral(0.10, 0.92));
        let (hot_top, hot_bottom) = (neutral(1.0, 1.0), neutral(0.86, 1.0));
        linear_gradient(
            160.0,
            linear_color_stop(lerp_hsla(rest_top, hot_top, hover), 0.0),
            linear_color_stop(lerp_hsla(rest_bottom, hot_bottom, hover), 1.0),
        )
    }

    /// The hub's fill at rest.
    #[must_use]
    pub fn hub_fill() -> Background {
        linear_gradient(
            145.0,
            linear_color_stop(color::accent().alpha(0.28), 0.0),
            linear_color_stop(color::accent().alpha(0.06), 1.0),
        )
    }

    /// The hub's fill under the pointer.
    #[must_use]
    pub fn hub_fill_hovered() -> Background {
        linear_gradient(
            145.0,
            linear_color_stop(color::accent_at_lightness(0.52), 0.0),
            linear_color_stop(color::accent_at_lightness(0.28), 1.0),
        )
    }

    /// The HUD tracks and spokes, `breath` (0 to 1) through their pulse.
    #[must_use]
    pub const fn track(breath: f32) -> Hsla {
        neutral(0.58, 0.10 + 0.04 * breath)
    }
}
