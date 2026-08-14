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

use gpui::{
    App, BoxShadow, FontWeight, Hsla, InteractiveElement, Pixels, Rgba, StatefulInteractiveElement,
    Styled, Window, hsla, point, px, relative, rgb,
};
use gpui_component::{ActiveTheme as _, Theme, ThemeMode, ThemeRegistry};
use openlogi_core::config::Appearance;

use crate::state::AppState;

/// Primary action / selection blue. Brand colour, identical in both modes —
/// it reads on the light card surfaces and the dark window alike.
pub const ACCENT_BLUE: u32 = 0x003b_82f6;

/// Status colours for the carousel connectivity dot.
pub const STATUS_CONNECTED: u32 = 0x0022_c55e;
pub const STATUS_CONNECTING: u32 = 0x00ea_b308;
pub const STATUS_OFFLINE: u32 = 0x006b_7280;
/// Ring color for a device the user disabled ("Manage this device" off).
pub const STATUS_DISABLED: u32 = 0x00ef_4444;

/// Sizes that several components need to agree on.
///
/// Chrome heights are sized for a macOS toolbar and status bar rather than a
/// web banner: at 80px the header read as a page masthead, which is most of
/// why the window looked like a site instead of an app. Shrinking them frees
/// real estate the device model can use, so the two `*_VERTICAL_RESERVE`
/// constants that budget around them track these values.
pub const HEADER_H: f32 = 56.;
pub const FOOTER_H: f32 = 34.;

/// Semantic spacing tokens (px), so surfaces that must agree share one value
/// instead of each call site hand-picking a `p_*` / `gap_*` step.
///
/// - `SCREEN_PAD` — the inset around a detail-tab body. Uniform across tabs so
///   the content's start doesn't shift when switching tabs (the pointer tab's
///   two-column grid is sized against this exact value; see its card min-width).
/// - `CARD_PAD` / `CARD_GAP` — a card's inner padding and its title-to-content
///   gap, so every [`panel_card`](crate::app) reads the same.
pub const SCREEN_PAD: f32 = 16.;
pub const CARD_PAD: f32 = 12.;
pub const CARD_GAP: f32 = 10.;

/// Apple HIG / WCAG minimum contrast for normal text up to 17pt.
const MIN_TEXT_CONTRAST: f32 = 4.5;

/// How much of the macOS window material [`Palette::backdrop`] lets through.
///
/// High on purpose, but not so high the material stops reading: at 0.9 the
/// bleed was invisible against a dark desktop, which is the whole effect
/// wasted. The ceiling is legibility — the muted text ramp is normalised for
/// contrast against the *opaque* colour, so a deeper bleed than this would
/// quietly undercut [`MIN_TEXT_CONTRAST`] over a bright wallpaper.
const BACKDROP_ALPHA: f32 = 0.8;

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
/// These are now *derived from the active gpui-component theme's semantic
/// tokens* (see [`palette`]), so the hand-painted surfaces re-skin with whatever
/// theme the user selects in Settings → Appearance — the same `cx.theme()` the
/// framework widgets read. The bundled "OpenLogi" theme (`themes/openlogi.json`)
/// encodes the original tuned values, so the default look is unchanged.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// Window background.
    pub bg: Hsla,
    /// The main window's backdrop — the fill every screen sits on, below the
    /// cards and panels that paint their own surfaces.
    ///
    /// On macOS this is [`Palette::bg`] at [`BACKDROP_ALPHA`], because that
    /// window is backed by a real `NSVisualEffectView` (see
    /// [`crate::platform::os::configure_window_material`]). The theme still
    /// owns the colour — only the last tenth of it is the live material
    /// underneath, which is what makes the window read as glass rather than as
    /// a flat fill.
    ///
    /// A single translucent layer is the whole trick: everything above it
    /// (cards, panels, popovers) is opaque, so no two translucent GPUI
    /// surfaces ever stack and accumulate alpha into a muddy patch.
    ///
    /// Everywhere else this *is* `bg`: auxiliary windows and the non-macOS
    /// main window are ordinary opaque surfaces.
    pub backdrop: Hsla,
    /// Raised card / panel fill.
    pub surface: Hsla,
    /// Hairline border between cards and surface.
    pub border: Hsla,
    /// The hairline, raised — a hovered or otherwise emphasised edge. An alpha
    /// tint rather than a second opaque value, so "emphasised" is the same
    /// *step* on every surface; call sites used to reach for `text_muted` here,
    /// which is a text weight and reads as an outline rather than an edge.
    pub border_strong: Hsla,
    /// Foreground text.
    pub text_primary: Hsla,
    /// De-emphasised labels / metadata. The contrast-normalised step (see
    /// [`accessible_muted_text`]), so it is the *floor* for anything the user
    /// has to read — the step below it is deliberately under AA.
    pub text_muted: Hsla,
    /// Decorative marks only — leader lines, placeholder outlines, disabled
    /// glyphs. Deliberately under AA: never body copy, and never the only
    /// carrier of a meaning.
    pub text_ghost: Hsla,
    /// The one neutral interaction wash: hover, and any transient highlight.
    ///
    /// An alpha tint of the foreground rather than an opaque fill, so it
    /// composites correctly over *any* surface it lands on (window, card,
    /// nested card) instead of matching exactly one of them — and so hover
    /// reads the same on all three without a per-surface variant. Reach for
    /// this before inventing a fill: hover states had drifted into four
    /// dialects before it existed.
    pub wash: Hsla,
    /// The same wash, deeper. Two jobs, both "neutral but more committed than
    /// hover": an armed / highlighted resting state, and a recessed well such
    /// as a meter track.
    pub wash_strong: Hsla,
    /// Keyboard focus ring. The theme's own `ring` token, so a hand-painted
    /// control's focus treatment matches the framework widgets' beside it.
    pub ring: Hsla,
    /// Corner radius for the bespoke card / panel surfaces. Derived from the
    /// active gpui-component theme radius (`cx.theme().radius`) so the
    /// hand-painted cards follow the Appearance → radius slider — which the old
    /// hard-coded `rounded_*` helpers (fixed px, blind to the slider) could not.
    ///
    /// Scaled `× 2` above the base control radius so a card reads as rounder
    /// than the small controls nested inside it — the concentric-corner
    /// relationship (outer radius > inner radius) that a single flat radius
    /// can't express.
    ///
    /// At the theme's default 6px control radius that puts cards at 12, the
    /// step native apps use. The previous `× 1.5` landed on 9, close enough to
    /// the controls inside that the nesting stopped reading.
    pub card_radius: Pixels,
    /// Corner radius for the small controls nested inside cards — chips, pills,
    /// segmented items, toggles. The base `cx.theme().radius`, i.e. the same
    /// radius the framework's own controls use, and smaller than
    /// [`Palette::card_radius`] so a control's corner sits concentrically inside
    /// its card's.
    pub control_radius: Pixels,
}

fn contrast_ratio(foreground: Hsla, background: Hsla) -> f32 {
    fn luminance(color: Rgba) -> f32 {
        let linear = |channel: f32| {
            if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
    }

    let background = background.to_rgb();
    let foreground = background.blend(foreground.to_rgb());
    let (lighter, darker) = {
        let foreground = luminance(foreground);
        let background = luminance(background);
        if foreground > background {
            (foreground, background)
        } else {
            (background, foreground)
        }
    };
    (lighter + 0.05) / (darker + 0.05)
}

fn minimum_text_contrast(color: Hsla, background: Hsla, surface: Hsla) -> f32 {
    contrast_ratio(color, background).min(contrast_ratio(color, surface))
}

fn accessible_muted_text(muted: Hsla, foreground: Hsla, background: Hsla, surface: Hsla) -> Hsla {
    if minimum_text_contrast(muted, background, surface) >= MIN_TEXT_CONTRAST {
        return muted;
    }

    let softened = foreground.opacity(0.6);
    if minimum_text_contrast(softened, background, surface) >= MIN_TEXT_CONTRAST {
        return softened;
    }

    [foreground, Hsla::black(), Hsla::white()]
        .into_iter()
        .max_by(|a, b| {
            minimum_text_contrast(*a, background, surface)
                .total_cmp(&minimum_text_contrast(*b, background, surface))
        })
        .unwrap_or(foreground)
}

fn normalize_theme_text_contrast(theme: &mut Theme) {
    theme.muted_foreground = accessible_muted_text(
        theme.muted_foreground,
        theme.foreground,
        theme.background,
        theme.group_box,
    );
}

/// Derive the app palette from the active gpui-component theme's semantic
/// tokens, so the hand-painted surfaces (window, cards, mouse model) re-skin
/// with the selected theme exactly as the framework widgets do.
///
/// - `bg` ← `background` (window), `surface` ← `group_box` (content cards).
/// - `border`, `text_primary` ← `foreground`, `text_muted` ← `muted_foreground`.
///
/// `text_ghost` fades `muted_foreground` toward whatever it is painted on
/// rather than picking its own colour. That keeps the ramp ordered by
/// construction — an alpha below 1 can only move a colour toward its
/// background — so no user-selected theme can invert it past `muted`, and it
/// inherits the AA normalisation already applied to `muted`.
///
/// The washes and `border_strong` tint `foreground` instead, which is what lets
/// one value serve every surface: 6% of the text colour over a card and over
/// the window are different pixels but the same *step*.
#[must_use]
pub fn palette(cx: &App) -> Palette {
    let t = cx.theme();
    Palette {
        bg: t.background,
        backdrop: if cfg!(target_os = "macos") {
            t.background.opacity(BACKDROP_ALPHA)
        } else {
            t.background
        },
        surface: t.group_box,
        border: t.border,
        border_strong: t.foreground.opacity(0.2),
        text_primary: t.foreground,
        text_muted: t.muted_foreground,
        text_ghost: t.muted_foreground.opacity(0.5),
        wash: t.foreground.opacity(0.06),
        wash_strong: t.foreground.opacity(0.1),
        ring: t.ring,
        card_radius: t.radius * 2.,
        control_radius: t.radius,
    }
}

/// Our brand theme (light + dark), encoding the original tuned surfaces. Kept as
/// a readable, committed JSON. The upstream gpui-component themes are *not*
/// vendored into this repo — `build.rs` copies them from the pinned dependency
/// checkout into `OUT_DIR` and generates the `UPSTREAM_THEME_JSON` list included
/// just below (gpui-component doesn't ship them inside its compiled crate, so
/// they must be embedded to be selectable).
const OPENLOGI_THEME_JSON: &str = include_str!("../themes/openlogi.json");

// Defines `static UPSTREAM_THEME_JSON: &[&str]` from build-time-embedded copies.
include!(concat!(env!("OUT_DIR"), "/builtin_themes.rs"));

/// The default brand theme names — slots [`apply_from_settings`] falls back to.
pub const OPENLOGI_LIGHT: &str = "OpenLogi Light";
pub const OPENLOGI_DARK: &str = "OpenLogi Dark";

/// Register every bundled theme into the [`ThemeRegistry`]. Call once at
/// startup, after `gpui_component::init` (which seeds the registry global). Our
/// brand theme loads first; the upstream themes follow.
pub fn register_builtin_themes(cx: &mut App) {
    let registry = ThemeRegistry::global_mut(cx);
    for json in std::iter::once(OPENLOGI_THEME_JSON).chain(UPSTREAM_THEME_JSON.iter().copied()) {
        if let Err(error) = registry.load_themes_from_str(json) {
            tracing::warn!(%error, "failed to load a bundled theme");
        }
    }
}

/// Resolve the user's stored appearance preference and apply it to the global
/// [`Theme`]. Reads [`AppState`] live, so it is the single entry point for first
/// paint, OS-appearance changes, and live edits on the Appearance page:
///
/// - the chosen named themes fill the light / dark slots (falling back to the
///   OpenLogi brand theme);
/// - `System` follows the OS appearance, `Light` / `Dark` force it;
/// - a chosen corner radius is applied last (after `Theme::change`, which would
///   otherwise reset it to the theme's own radius).
///
/// Pass the window being built (first paint / appearance observer) so its OS
/// appearance is read directly and it repaints; pass `None` from a settings
/// edit (no window in hand) — every open window is refreshed instead.
pub fn apply_from_settings(window: Option<&mut Window>, cx: &mut App) {
    let (appearance, light_name, dark_name, radius) =
        cx.try_global::<AppState>()
            .map_or((Appearance::default(), None, None, None), |state| {
                let s = state.app_settings();
                (
                    s.appearance,
                    s.theme_light.clone(),
                    s.theme_dark.clone(),
                    s.ui_radius,
                )
            });

    // Sync the native window chrome (titlebar) to the pref first, so the
    // `System` branch below reads the *real* OS appearance rather than a stale
    // forced override.
    crate::platform::os::set_app_appearance(appearance);
    // Read the OS appearance from the window in hand (a borrow-free field read)
    // rather than `cx.window_appearance()`. On Linux the latter routes through
    // the platform client's `RefCell` (`with_common`), and this is called from
    // the window-appearance observer, which gpui fires from inside its
    // xdg-desktop-portal handler while that same `RefCell` is already borrowed —
    // querying it there panics with "RefCell already borrowed". With no window
    // (a settings edit), the platform query is safe and gives every window's
    // shared appearance.
    let os_appearance = window
        .as_ref()
        .map_or_else(|| cx.window_appearance(), |w| w.appearance());

    // Pull the chosen configs out of the registry before borrowing the Theme
    // mutably (both live as globals).
    let (light, dark) = {
        let registry = ThemeRegistry::global(cx);
        let pick = |name: Option<&str>, fallback: &str| {
            name.and_then(|n| registry.themes().get(n).cloned())
                .or_else(|| registry.themes().get(fallback).cloned())
        };
        (
            pick(light_name.as_deref(), OPENLOGI_LIGHT),
            pick(dark_name.as_deref(), OPENLOGI_DARK),
        )
    };
    {
        let theme = Theme::global_mut(cx);
        if let Some(light) = light {
            theme.light_theme = light;
        }
        if let Some(dark) = dark {
            theme.dark_theme = dark;
        }
    }

    let mode = match appearance {
        Appearance::System => ThemeMode::from(os_appearance),
        Appearance::Light => ThemeMode::Light,
        Appearance::Dark => ThemeMode::Dark,
    };
    Theme::change(mode, window, cx);

    let theme = Theme::global_mut(cx);
    normalize_theme_text_contrast(theme);
    if let Some(radius) = radius {
        theme.radius = px(f32::from(radius));
    }
    cx.refresh_windows();
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

/// The neutral hover decision, in one place — the counterpart to
/// [`SelectableStyle`] for the *transient* half of interaction.
///
/// The split is deliberate and is the whole point of the two-axis colour
/// system: an accent tint means "this is the chosen one" (a fact about state),
/// a neutral wash means "the pointer is here" (a fact about the pointer). A row
/// that is both keeps its accent fill and takes [`accent_tint_hover`] instead,
/// so the two axes never fight over the same pixel.
pub trait WashStyle: InteractiveElement + Sized {
    /// The neutral wash under the pointer.
    #[must_use]
    fn hover_wash(self, pal: Palette) -> Self {
        self.hover(move |style| style.bg(pal.wash))
    }
}

impl<E: InteractiveElement> WashStyle for E {}

/// What separates a hand-painted `div` from a real control: a native cursor,
/// a tab stop, and a visible keyboard focus ring.
///
/// Framework widgets ([`gpui_component::button::Button`] and friends) already
/// carry this. These methods are for the surfaces we paint ourselves — gallery
/// cards, mouse-model labels, key targets, theme swatches — which had none of
/// it and were reachable by mouse only.
///
/// **Activation comes free.** gpui maps enter / space to an element's click
/// listeners while it is focused, so making the element focusable is the whole
/// job; `on_click` stays exactly as the caller wrote it. gpui also keeps the
/// focus handle for us, in element state under the element's id, so a control
/// needs no `FocusHandle` field of its own — it only needs an `.id(..)`.
pub trait ControlStyle: StatefulInteractiveElement + Styled + Sized {
    /// Cursor, tab stop, and focus ring — the part every control wants, and
    /// nothing that touches the element's fill.
    ///
    /// Fills stay separate because they are not universal: a neutral row wants
    /// [`WashStyle::hover_wash`] and [`Self::press_wash`], a selectable one
    /// wants [`accent_tint_hover`], and an element that paints its own colour
    /// (a page dot, a peeking card) wants neither — a wash would overwrite the
    /// very thing that identifies it.
    #[must_use]
    fn control(self, pal: Palette) -> Self {
        self.cursor_default()
            .tab_index(0)
            .focus_visible(move |style| style.shadow(vec![focus_ring(pal)]))
    }

    /// The pressed state, one step past [`WashStyle::hover_wash`].
    #[must_use]
    fn press_wash(self, pal: Palette) -> Self {
        self.active(move |style| style.bg(pal.wash_strong))
    }
}

impl<E: StatefulInteractiveElement + Styled> ControlStyle for E {}

/// The focus ring: an outer glow, not a border.
///
/// A border would resize the element the moment it takes focus, and these
/// controls sit in tight rows and grids where one pixel of growth reflows the
/// whole line. A shadow with no blur and a small spread draws the same ring
/// outside the bounds, follows the corner radius, and costs no layout.
fn focus_ring(pal: Palette) -> BoxShadow {
    BoxShadow {
        color: pal.ring.opacity(0.6),
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(2.),
        inset: false,
    }
}

/// The app's type ramp as semantic roles, so a heading is `.text_heading()`
/// everywhere instead of each call site re-picking a `text_*` size and a
/// `font_weight`. Sizes, weights, and line heights live here once, and every
/// screen re-skins by editing this trait.
///
/// The sizes are AppKit's own text styles — title1 22, title2 17, headline /
/// body 13, subheadline 11 — not a scale "inspired by" them. An earlier pass
/// deliberately ran a rung larger and heavier than HIG; the result read as a
/// web page rather than a Mac app, which is most of what "rough" meant. Native
/// chrome is small text with roomy leading, so the *leading ratios are
/// unchanged* — density comes from the size, not from crowding the lines.
///
/// Weight tops out at SEMIBOLD, again as HIG does: hierarchy is carried by the
/// colour ramp ([`Palette::text_primary`] → `text_muted` → `text_ghost`), which
/// is a quieter signal than size and weight both shouting.
///
/// Blanket-implemented for every [`Styled`] element, the same way
/// [`SelectableStyle`] extends styling. Colour stays a separate axis (the caller
/// still picks `pal.text_primary` / `text_muted`); this trait only fixes size,
/// weight, and leading.
pub trait Typography: Styled + Sized {
    /// Page / dialog hero title (empty states, connection notices). AppKit
    /// title1.
    #[must_use]
    fn text_title(self) -> Self {
        self.text_size(px(22.))
            .font_weight(FontWeight::SEMIBOLD)
            .line_height(relative(1.2))
    }

    /// Screen / section heading — the Home title, a device name, a window's
    /// primary heading. AppKit title2.
    #[must_use]
    fn text_heading(self) -> Self {
        self.text_size(px(17.))
            .font_weight(FontWeight::SEMIBOLD)
            .line_height(relative(1.3))
    }

    /// Card / group title and item names — a heading one rung down, sitting
    /// inside a card rather than titling a screen. AppKit headline: body size
    /// at semibold, so a card title aligns with the values under it instead of
    /// stepping out of the grid.
    #[must_use]
    fn text_subheading(self) -> Self {
        self.text_size(px(13.))
            .font_weight(FontWeight::SEMIBOLD)
            .line_height(relative(1.4))
    }

    /// Default body copy — control labels, descriptions, values. AppKit body,
    /// which is also the system control size.
    #[must_use]
    fn text_body(self) -> Self {
        self.text_size(px(13.)).line_height(relative(1.45))
    }

    /// De-emphasised metadata and helper text — the muted line under a label,
    /// battery readouts, hints. AppKit subheadline. Pair with
    /// `pal.text_muted`.
    #[must_use]
    fn text_caption(self) -> Self {
        self.text_size(px(11.)).line_height(relative(1.4))
    }
}

impl<E: Styled> Typography for E {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openlogi_theme_text_pairs_meet_normal_text_contrast() {
        let Ok(theme_set) = serde_json::from_str::<serde_json::Value>(OPENLOGI_THEME_JSON) else {
            panic!("OpenLogi theme JSON should parse");
        };
        let Some(themes) = theme_set["themes"].as_array() else {
            panic!("OpenLogi theme JSON should contain themes");
        };

        for theme in themes {
            let name = theme["name"].as_str().unwrap_or("unnamed theme");
            let colors = &theme["colors"];
            for (foreground, background) in [
                ("primary.foreground", "primary.background"),
                ("danger.foreground", "danger.background"),
                ("info.foreground", "info.background"),
                ("success.foreground", "success.background"),
                ("warning.foreground", "warning.background"),
            ] {
                let foreground = theme_color(colors, foreground);
                let background = theme_color(colors, background);
                assert!(
                    contrast_ratio(foreground, background) >= MIN_TEXT_CONTRAST,
                    "{name}: {foreground:?} on {background:?} is below {MIN_TEXT_CONTRAST}:1"
                );
            }
        }
    }

    #[test]
    fn muted_text_is_adjusted_for_page_and_content_surfaces() {
        let background: Hsla = rgb(0xff_ffff).into();
        let surface: Hsla = rgb(0xf5_f5f5).into();
        let muted: Hsla = rgb(0x73_7373).into();
        let foreground: Hsla = rgb(0x0a_0a0a).into();

        let adjusted = accessible_muted_text(muted, foreground, background, surface);

        assert!(contrast_ratio(adjusted, background) >= MIN_TEXT_CONTRAST);
        assert!(contrast_ratio(adjusted, surface) >= MIN_TEXT_CONTRAST);
        assert_ne!(
            adjusted, foreground,
            "muted hierarchy should remain visible"
        );
    }

    #[test]
    fn compliant_muted_text_is_preserved() {
        let background: Hsla = rgb(0xf4_f4f6).into();
        let surface: Hsla = rgb(0xff_ffff).into();
        let muted: Hsla = rgb(0x6b_6b73).into();
        let foreground: Hsla = rgb(0x1a_1a1d).into();

        assert_eq!(
            accessible_muted_text(muted, foreground, background, surface),
            muted
        );
    }

    fn theme_color(colors: &serde_json::Value, key: &str) -> Hsla {
        let Some(value) = colors[key].as_str() else {
            panic!("{key} should be a color string");
        };
        let Ok(color) = Rgba::try_from(value) else {
            panic!("{key} should be a six-digit hex color");
        };
        color.into()
    }

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

    /// `accent_tint_hover` is also a hand-written `hsla`; pin that it stays
    /// derived from `ACCENT_BLUE` and sits deeper than the resting `accent_tint`.
    #[test]
    fn accent_tint_hover_matches_accent() {
        let a = accent();
        let th = accent_tint_hover();
        assert!((a.h - th.h).abs() < 0.02, "hue {} vs {}", a.h, th.h);
        assert!((a.s - th.s).abs() < 0.05, "sat {} vs {}", a.s, th.s);
        assert!((a.l - th.l).abs() < 0.05, "light {} vs {}", a.l, th.l);
        assert!(
            th.a > accent_tint().a,
            "hover tint should sit deeper than the resting tint"
        );
    }
}
