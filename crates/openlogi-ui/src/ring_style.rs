//! What each Actions Ring theme looks like at rest: sizes, colours and fills.
//!
//! The overlay draws the ring and the settings app draws a still preview of
//! it beside the slot editor. Both read this module, so the preview cannot
//! drift from the ring the way a copy would. Motion — clocks, easing, the
//! animated arcs and glow — stays in the overlay: a still frame has none.

use gpui::{Background, BoxShadow, Hsla, linear_color_stop, linear_gradient, px, rgba};

use crate::color;

const fn neutral(lightness: f32, alpha: f32) -> Hsla {
    Hsla {
        h: 0.0,
        s: 0.0,
        l: lightness,
        a: alpha,
    }
}

/// The outline of a slot that holds no action. No theme draws one: the ring
/// hides empty slots, and only the editor's preview shows them, as a neutral
/// outline so they can still be picked.
pub const EMPTY_SLOT_OUTLINE: Hsla = neutral(0.58, 0.14);

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

/// `#RRGGBBAA` as a colour, so a theme's tables read like its prototype's
/// stylesheet.
fn hex(rgba_hex: u32) -> Hsla {
    rgba(rgba_hex).into()
}

/// A box shadow in a stylesheet's order: offset, blur, spread, colour.
fn shadow(x: f32, y: f32, blur: f32, spread: f32, color: u32) -> BoxShadow {
    BoxShadow::new(px(x), px(y), hex(color))
        .blur_radius(px(blur))
        .spread_radius(px(spread))
}

fn inset(x: f32, y: f32, blur: f32, color: u32) -> BoxShadow {
    shadow(x, y, blur, 0.0, color).inset()
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

/// Dark glass cards on a dial of orbit lines, around a hub that names the
/// hovered action. Its accent is a mint signal, not the brand blue: the theme
/// is a look of its own, and every colour below is its own.
pub mod obsidian {
    use super::{
        Background, BoxShadow, Hsla, hex, inset, linear_color_stop, linear_gradient, shadow,
    };

    /// The panel's designed edge. The prototype's dial is 420 px; 20 px more
    /// keeps the hovered bottom card's shadow inside the window.
    pub const DESIGN_EDGE: f32 = 440.0;
    /// Distance from the panel's centre to each card's centre.
    pub const RADIUS: f32 = 151.0;
    pub const CARD_W: f32 = 92.0;
    pub const CARD_H: f32 = 64.0;
    pub const CARD_RADIUS: f32 = 16.0;
    pub const CARD_GLYPH: f32 = 19.0;
    /// Between a card's glyph and its label.
    pub const CARD_GAP: f32 = 6.0;
    pub const CARD_LABEL: f32 = 11.0;
    pub const HUB: f32 = 110.0;
    pub const HUB_GLYPH: f32 = 20.0;
    pub const HUB_LABEL: f32 = 12.0;
    /// The two thin orbit lines and the soft disc under everything, as radii.
    pub const ORBIT_OUTER: f32 = 161.7;
    pub const ORBIT_INNER: f32 = 96.6;
    pub const AMBIENT: f32 = 151.2;

    /// The mint signal: selected cards, the hub's glyph, the sector and dot.
    #[must_use]
    pub fn signal() -> Hsla {
        hex(0x8ceb_d7ff)
    }

    /// The violet second accent, in the halo's shadow side.
    #[must_use]
    pub fn iris() -> Hsla {
        hex(0xb5a5_f6ff)
    }

    /// Captions and the hub's label.
    #[must_use]
    pub fn text() -> Hsla {
        hex(0xedf3_faff)
    }

    /// A card's glyph at rest.
    #[must_use]
    pub fn glyph() -> Hsla {
        hex(0xbbc8_d8ff)
    }

    /// A card's fill: smoked glass at rest, letting a little of what is
    /// behind it through, or the mint-tinted glass of the pressed card.
    #[must_use]
    pub fn card_fill(pressed: bool) -> Background {
        if pressed {
            linear_gradient(
                155.0,
                linear_color_stop(hex(0x213b_3ad6), 0.0),
                linear_color_stop(hex(0x1627_2cd6), 0.80),
            )
        } else {
            linear_gradient(
                155.0,
                linear_color_stop(hex(0x1d26_33c7), 0.0),
                linear_color_stop(hex(0x1219_23c7), 0.76),
            )
        }
    }

    #[must_use]
    pub fn card_border(pressed: bool) -> Hsla {
        if pressed {
            hex(0x8ceb_d77a)
        } else {
            hex(0xb9cb_e022)
        }
    }

    /// A card's hovered border, between the rest and pressed ones.
    #[must_use]
    pub fn card_border_hovered() -> Hsla {
        hex(0x9edc_e6a6)
    }

    /// A card's shadows: the glass's lit top edge and shaded bottom edge
    /// inside, and a drop shadow under it; the pressed card adds a thin mint
    /// halo.
    #[must_use]
    pub fn card_shadow(pressed: bool) -> Vec<BoxShadow> {
        if pressed {
            vec![
                inset(0.0, 1.0, 0.0, 0xbeff_ea26),
                inset(0.0, -2.0, 6.0, 0x0000_0030),
                shadow(0.0, 0.0, 0.0, 3.0, 0x8ceb_d70c),
                shadow(0.0, 5.0, 18.0, 0.0, 0x0000_0030),
            ]
        } else {
            vec![
                inset(0.0, 1.0, 0.0, 0xffff_ff14),
                inset(0.0, -2.0, 6.0, 0x0000_0030),
                shadow(0.0, 5.0, 15.0, 0.0, 0x0000_0030),
            ]
        }
    }

    /// A card's shadows under the pointer: the top edge brightens and the
    /// drop shadow deepens as the card lifts.
    #[must_use]
    pub fn card_shadow_hovered() -> Vec<BoxShadow> {
        vec![
            inset(0.0, 1.0, 0.0, 0xffff_ff24),
            inset(0.0, -2.0, 6.0, 0x0000_0030),
            shadow(0.0, 8.0, 24.0, 0.0, 0x0000_0052),
        ]
    }

    /// The hub's fill: lit from the top left, as the prototype's radial is.
    #[must_use]
    pub fn hub_fill() -> Background {
        linear_gradient(
            160.0,
            linear_color_stop(hex(0x2432_3aff), 0.0),
            linear_color_stop(hex(0x0d14_1cff), 1.0),
        )
    }

    #[must_use]
    pub fn hub_border() -> Hsla {
        hex(0xa1d0_d933)
    }

    /// The hub's shadows: a highlight along the top inside, a deep shade
    /// along the bottom inside, and two faint rings around it.
    #[must_use]
    pub fn hub_shadow() -> Vec<BoxShadow> {
        vec![
            inset(0.0, 1.0, 2.0, 0xc9ea_ff12),
            inset(0.0, -7.0, 18.0, 0x0000_0044),
            shadow(0.0, 0.0, 0.0, 6.0, 0xb9f4_e703),
            shadow(0.0, 0.0, 0.0, 7.0, 0xa4cf_de15),
        ]
    }

    #[must_use]
    pub fn orbit_outer() -> Hsla {
        hex(0x9fc3_dd19)
    }

    #[must_use]
    pub fn orbit_inner() -> Hsla {
        hex(0xb6cc_df10)
    }

    /// The soft disc under the dial, a flat stand-in for the prototype's
    /// faint radial wash.
    #[must_use]
    pub fn ambient_fill() -> Hsla {
        hex(0x72cf_d108)
    }

    #[must_use]
    pub fn ambient_shadow() -> Vec<BoxShadow> {
        vec![shadow(0.0, 0.0, 48.0, 0.0, 0x090c_1244)]
    }

    /// The halo under the hovered card: its rim, and its shadows inside and
    /// out.
    #[must_use]
    pub fn halo_border() -> Hsla {
        hex(0xa8eb_ee44)
    }

    #[must_use]
    pub fn halo_shadow() -> Vec<BoxShadow> {
        vec![
            inset(0.0, 1.0, 1.0, 0xebff_f82b),
            inset(0.0, -2.0, 8.0, 0xa59a_e41a),
            shadow(0.0, 0.0, 24.0, 0.0, 0x8ceb_d71c),
        ]
    }
}

/// Ceramic discs with their captions under them, on a light platter around a
/// hub, with a needle that turns to point at the hovered one. A light theme
/// with a terracotta accent; every colour below is its own.
pub mod porcelain {
    use super::{
        Background, BoxShadow, Hsla, hex, inset, linear_color_stop, linear_gradient, shadow,
    };

    /// The panel's designed edge: the prototype's dial, which everything fits
    /// inside, the hovered bottom disc's shadow included.
    pub const DESIGN_EDGE: f32 = 420.0;
    /// Distance from the panel's centre to each node's centre.
    pub const RADIUS: f32 = 151.2;
    /// A node: the hit box holding a disc above its caption, centred on the
    /// slot.
    pub const NODE_W: f32 = 88.0;
    pub const NODE_H: f32 = 84.0;
    /// The column inside a node: this much down to the disc, then the gap to
    /// the caption; the disc's centre lands 11 px above the slot's.
    pub const NODE_PAD_TOP: f32 = 5.0;
    pub const NODE_GAP: f32 = 8.0;
    pub const DISC: f32 = 52.0;
    pub const DISC_GLYPH: f32 = 20.0;
    pub const LABEL: f32 = 11.0;
    pub const HUB: f32 = 128.0;
    pub const HUB_GLYPH: f32 = 23.0;
    pub const HUB_LABEL: f32 = 12.0;
    pub const HUB_GAP: f32 = 7.0;
    /// The platter under everything and the thin ring inside it, as radii.
    pub const PLATTER: f32 = 168.0;
    pub const INSET_RING: f32 = 91.1;
    /// The needle: a capsule this wide between these radii, just inside the
    /// inset ring.
    pub const NEEDLE_INNER: f32 = 75.5;
    pub const NEEDLE_OUTER: f32 = 89.5;
    pub const NEEDLE_W: f32 = 5.0;
    /// The stroke a click draws under the caption.
    pub const MARK_W: f32 = 20.0;
    pub const MARK_H: f32 = 2.0;

    /// The terracotta accent: the needle, the hub's glyph and the click
    /// stroke.
    #[must_use]
    pub fn accent() -> Hsla {
        hex(0xc44c_27ff)
    }

    /// Glyphs and the hub's label.
    #[must_use]
    pub fn ink() -> Hsla {
        hex(0x2529_2cff)
    }

    /// The hub's cancel cross.
    #[must_use]
    pub fn muted() -> Hsla {
        hex(0x6265_5fff)
    }

    /// A caption at rest.
    #[must_use]
    pub fn label() -> Hsla {
        hex(0x4246_40ff)
    }

    /// A caption under the pointer: the accent, darkened to read on paper.
    #[must_use]
    pub fn label_hovered() -> Hsla {
        hex(0x9742_24ff)
    }

    /// The lit tip of the needle.
    #[must_use]
    pub fn needle_highlight() -> Hsla {
        hex(0xf6b4_9aff)
    }

    /// The platter: glazed ceramic lit from the top left.
    #[must_use]
    pub fn platter_fill() -> Background {
        linear_gradient(
            145.0,
            linear_color_stop(hex(0xfaf9_f5ff), 0.0),
            linear_color_stop(hex(0xeeed_e7ff), 0.75),
        )
    }

    #[must_use]
    pub fn platter_border() -> Hsla {
        hex(0xdede_d6ff)
    }

    /// The platter's shadows: a lit top edge and a shaded bottom edge inside,
    /// a shadow falling to the bottom right and light to the top left.
    #[must_use]
    pub fn platter_shadow() -> Vec<BoxShadow> {
        vec![
            inset(0.0, 2.0, 5.0, 0xffff_ffff),
            inset(0.0, -2.0, 4.0, 0xd7d7_ce60),
            shadow(9.0, 12.0, 24.0, 0.0, 0xb8b7_ab19),
            shadow(-8.0, -8.0, 22.0, 0.0, 0xffff_ff90),
        ]
    }

    /// The thin ring inside the platter, with a hairline of light under it.
    #[must_use]
    pub fn inset_ring_border() -> Hsla {
        hex(0xd5d4_cbff)
    }

    #[must_use]
    pub fn inset_ring_shadow() -> Vec<BoxShadow> {
        vec![shadow(0.0, 1.0, 0.0, 0.0, 0xfffe_faff)]
    }

    /// A disc: white ceramic shading to cream at its lower right.
    #[must_use]
    pub fn disc_fill() -> Background {
        linear_gradient(
            150.0,
            linear_color_stop(hex(0xffff_ffff), 0.0),
            linear_color_stop(hex(0xf2f1_e9ff), 0.83),
        )
    }

    #[must_use]
    pub fn disc_border() -> Hsla {
        hex(0xe0df_d5ff)
    }

    /// A disc's border under the pointer: a little warmer and darker.
    #[must_use]
    pub fn disc_border_hovered() -> Hsla {
        hex(0xc6bd_a9ff)
    }

    /// A disc's shadows: a drop shadow under it, a lit top edge and a shaded
    /// bottom edge inside.
    #[must_use]
    pub fn disc_shadow() -> Vec<BoxShadow> {
        vec![
            shadow(0.0, 3.0, 5.0, 0.0, 0xaaa9_9d26),
            inset(0.0, 2.0, 1.0, 0xffff_ffff),
            inset(0.0, -1.0, 1.0, 0xcfce_c64d),
        ]
    }

    /// A disc's shadows under the pointer: the drop shadow deepens as the
    /// disc lifts and the bottom edge's shade goes. The third entry pairs
    /// with the rest list's so the two blend shadow by shadow.
    #[must_use]
    pub fn disc_shadow_hovered() -> Vec<BoxShadow> {
        vec![
            shadow(0.0, 6.0, 10.0, 0.0, 0xaaa9_9d2b),
            inset(0.0, 2.0, 1.0, 0xffff_ffff),
            inset(0.0, -1.0, 1.0, 0xcfce_c600),
        ]
    }

    /// The hub's fill: lit from the top left, as the prototype's radial is.
    #[must_use]
    pub fn hub_fill() -> Background {
        linear_gradient(
            160.0,
            linear_color_stop(hex(0xfffe_faff), 0.0),
            linear_color_stop(hex(0xeeed_e6ff), 1.0),
        )
    }

    #[must_use]
    pub fn hub_border() -> Hsla {
        hex(0xdede_d4ff)
    }

    /// The hub's shadows: a lit top edge inside and a soft shadow under it.
    #[must_use]
    pub fn hub_shadow() -> Vec<BoxShadow> {
        vec![
            inset(0.0, 2.0, 1.0, 0xffff_ffff),
            shadow(0.0, 7.0, 14.0, 0.0, 0xa5a3_9724),
        ]
    }
}
