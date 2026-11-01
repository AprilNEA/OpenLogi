//! OpenLogi UI theme and shared sizes.
//!
//! Every colour the app paints comes from the active gpui-component theme.
//! [`palette`] resolves the semantic roles the bespoke surfaces need (window,
//! cards, mouse model, selection, status) from `cx.theme()`, so a theme picked
//! in Settings → Appearance re-skins those surfaces exactly as it re-skins the
//! framework's own widgets. The bundled "OpenLogi" theme
//! (`themes/openlogi.json`) supplies the defaults. Raw colour constructors
//! belong in this module only; the `gui-theme-color-owner` ast-grep rule
//! rejects them elsewhere in the crate.

#[cfg(test)]
use gpui::rgb;
use gpui::{
    App, BoxShadow, FontWeight, Hsla, Pixels, Rems, Rgba, Styled, Window, hsla, px, relative, rems,
};
use gpui_component::{ActiveTheme as _, Colorize as _, Theme, ThemeMode, ThemeRegistry};
use openlogi_core::config::{Appearance, UiScale};

use crate::state::AppState;

use super::spacing::DynamicSpacing;

/// Sizes that several components need to agree on. They are rems, so the
/// window chrome grows and shrinks with the interface scale along with the text
/// and spacing it holds. The pixel size in each comment is the size at the
/// standard 16 px rem.
///
/// Home and device-detail header height (64 px).
pub const HEADER_H: Rems = rems(4.);
/// Attention footer height (40 px).
pub const FOOTER_H: Rems = rems(2.5);
/// Device-detail navigation rail width (168 px).
pub const DETAIL_RAIL_W: Rems = rems(10.5);
/// The device-detail tab body's width: the window beside [`DETAIL_RAIL_W`],
/// taken at this window's rem so it follows the interface scale.
pub fn detail_content_width(window: &Window) -> f32 {
    let rail_w = DETAIL_RAIL_W.to_pixels(window.rem_size());
    f32::from(window.viewport_size().width - rail_w)
}

/// [`detail_content_width`] of a `window_w` px window at the standard scale.
#[cfg(test)]
pub(crate) fn standard_detail_content_width(window_w: f32) -> f32 {
    window_w - f32::from(DETAIL_RAIL_W.to_pixels(px(BASE_REM_SIZE)))
}
/// Height of standalone form controls: buttons, text inputs, tabs (30 px).
/// gpui-component's `.small()` maps to a 24 px `h_6`, which reads undersized
/// against this control rhythm — small controls pin the height
/// explicitly (single-line `Input`s via `min_h`; their inherent `h` is
/// multi-line-only and would be ignored).
pub const CONTROL_H: Rems = rems(1.875);

const BASE_REM_SIZE: f32 = 16.;

/// Maximum-width scale for detail-tab content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentWidth {
    /// Compact explanatory copy and empty-state text (440 px at 100%).
    Narrow,
    /// A single compact settings card (560 px at 100%).
    Small,
    /// A wider single panel (680 px at 100%).
    Medium,
    /// A two-column settings layout (920 px at 100%).
    Large,
    /// A device visual beside its controls (980 px at 100%).
    ExtraLarge,
    /// The widest workspace layout (1040 px at 100%).
    DoubleExtraLarge,
}

impl ContentWidth {
    /// Resolve this semantic width in scalable rem units.
    #[must_use]
    pub const fn rems(self) -> Rems {
        match self {
            Self::Narrow => rems(27.5),
            Self::Small => rems(35.),
            Self::Medium => rems(42.5),
            Self::Large => rems(57.5),
            Self::ExtraLarge => rems(61.25),
            Self::DoubleExtraLarge => rems(65.),
        }
    }
}

/// Semantic spacing tokens, so surfaces that must agree share one value
/// instead of each call site hand-picking a `p_*` / `gap_*` step.
///
/// - `SCREEN_PAD` — the inset around a detail-tab body. Uniform across tabs so
///   the content's start doesn't shift when switching tabs (the pointer tab's
///   two-column grid is sized against this exact value; see its card min-width).
/// - `CARD_PAD` / `CARD_GAP` — a card's inner padding and its title-to-content
///   gap, so every [`panel_card`](crate::app) reads the same.
pub const SCREEN_PAD: DynamicSpacing = DynamicSpacing::Base20;
pub const CARD_PAD: DynamicSpacing = DynamicSpacing::Base16;
pub const CARD_GAP: DynamicSpacing = DynamicSpacing::Base12;

/// Apple HIG / WCAG minimum contrast for normal text up to 17pt.
const MIN_TEXT_CONTRAST: f32 = 4.5;
/// WCAG 1.4.11 minimum contrast for icons, status dots, and other graphics.
const MIN_GRAPHIC_CONTRAST: f32 = 3.;

/// Rim of an unselected hotspot mark drawn on a device photo. A photo keeps its
/// own colours whatever the theme, so a mark on it uses a fixed light rim and
/// dark fill that read against both light and dark product shells.
pub const PHOTO_MARK_RIM: Hsla = hsla(0., 0., 0.95, 0.85);
/// Fill of an unselected hotspot mark drawn on a device photo; see
/// [`PHOTO_MARK_RIM`].
pub const PHOTO_MARK_FILL: Hsla = hsla(0., 0., 0.18, 0.85);

/// The glow of a lit key light, shown on its emblem. It depicts the physical
/// light, so it stays warm under every theme.
pub const WARM_LIGHT: Hsla = hsla(0.105, 0.9, 0.66, 1.);

/// The screen of an LCD key, drawn as the device shows it. It depicts the
/// hardware, so it stays black under every theme.
pub const LCD_SCREEN: Hsla = hsla(0., 0., 0., 1.);
/// The white an LCD key draws its glyph and caption in on [`LCD_SCREEN`].
pub const LCD_INK: Hsla = hsla(0., 0., 1., 1.);

/// The colours of the Appearance page's light / dark mode thumbnails. They
/// depict each mode, so they stay fixed whatever theme is active.
#[derive(Clone, Copy)]
pub struct ModePreview {
    pub wallpaper: Hsla,
    pub window: Hsla,
    pub bar: Hsla,
    pub line: Hsla,
}

impl ModePreview {
    pub const LIGHT: Self = Self {
        wallpaper: hsla(0.6026, 0.2549, 0.9, 1.),
        window: hsla(0., 0., 1., 1.),
        bar: hsla(0.6, 0.1724, 0.8294, 1.),
        line: Self::LINE,
    };
    pub const DARK: Self = Self {
        wallpaper: hsla(0.6667, 0.0617, 0.1588, 1.),
        window: hsla(0.6667, 0.0526, 0.1118, 1.),
        bar: hsla(0.6667, 0.0645, 0.2431, 1.),
        line: Self::LINE,
    };
    const LINE: Hsla = hsla(0.6034, 0.9122, 0.598, 1.);
    /// The macOS window controls: close, minimise, zoom.
    pub const TRAFFIC_LIGHTS: [Hsla; 3] = [
        hsla(0.0079, 1., 0.6706, 1.),
        hsla(0.1138, 0.9905, 0.5882, 1.),
        hsla(0.3583, 0.6667, 0.4706, 1.),
    ];
}

/// The colour a light at `kelvin` casts, for illustrations of that light:
/// amber at 2700 K, cooling toward a pale daylight at 6500 K.
#[must_use]
pub fn light_temperature_color(kelvin: u16) -> Hsla {
    let normalized = (f32::from(kelvin.clamp(2700, 6500)) - 2700.) / 3800.;
    let hue = 0.09 + normalized * 0.05;
    let saturation = 0.9 - normalized * 0.48;
    hsla(hue, saturation, 0.68, 1.)
}

/// Responsive bounds for a device card in the Home grid. At standard scale,
/// two cards fit the 720 px minimum window after the screen inset and gap; at
/// the normal wide window, three grow to [`GALLERY_CARD_MAX_W`].
/// `GALLERY_PHOTO_H` is the fixed product-image stage above the scalable
/// identity and status rows.
pub const GALLERY_CARD_MIN_W: Rems = rems(19.375);
pub const GALLERY_CARD_MAX_W: Rems = rems(25.3125);
pub const GALLERY_PHOTO_H: f32 = 196.;

/// Semantic colours and radii for the bespoke (non gpui-component) surfaces.
/// Resolved once per render via [`palette`] and passed down to the free helper
/// builders.
///
/// Every field derives from the active gpui-component theme (see [`palette`]),
/// so the hand-painted surfaces re-skin with whatever theme the user selects in
/// Settings → Appearance — the same `cx.theme()` the framework widgets read.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// Window and page background.
    pub page: Hsla,
    /// Raised card / panel fill.
    pub panel: Hsla,
    /// Resting fill for bespoke interactive controls.
    pub control: Hsla,
    /// Hover fill for bespoke interactive controls.
    pub control_hover: Hsla,
    /// Muted, non-interactive fill for tracks and disabled illustrations.
    pub muted: Hsla,
    /// Hairline border between cards and surface.
    pub border: Hsla,
    /// Foreground text.
    pub text_primary: Hsla,
    /// De-emphasised labels / metadata.
    pub text_muted: Hsla,
    /// Selection and emphasis ink — a selected item's border, check mark, or
    /// value text. The theme's `primary`, made legible as text on the page and
    /// panel surfaces. It is not legible as text on [`Palette::accent_tint`]
    /// or its hover shade, which pull the background toward it.
    pub accent: Hsla,
    /// Faint fill marking a selected row or chip. Text on it stays
    /// `text_primary`; only borders and marks on it take the accent.
    pub accent_tint: Hsla,
    /// [`Palette::accent_tint`] deepened for hover on an already-selected item.
    pub accent_tint_hover: Hsla,
    /// Error text and failure marks, legible as text.
    pub danger: Hsla,
    /// Attention marks — a pending permission, a low battery.
    pub warning: Hsla,
    /// Positive state marks — granted, charging, on.
    pub success: Hsla,
    /// The keyboard focus colour; see [`SelectableStyle::focus_ring`].
    pub ring: Hsla,
    /// Corner radius for the bespoke card / panel surfaces. Derived from the
    /// active gpui-component theme radius (`cx.theme().radius`) so the
    /// hand-painted cards follow the Appearance → radius slider — which the old
    /// hard-coded `rounded_*` helpers (fixed px, blind to the slider) could not.
    ///
    /// Scaled `× 1.5` above the base control radius so a card reads as rounder
    /// than the small controls nested inside it — the concentric-corner
    /// relationship (outer radius > inner radius) that a single flat radius
    /// can't express.
    pub card_radius: Pixels,
    /// Corner radius for the small controls nested inside cards — chips, pills,
    /// segmented items, toggles. The base `cx.theme().radius`, i.e. the same
    /// radius the framework's own controls use, and smaller than
    /// [`Palette::card_radius`] so a control's corner sits concentrically inside
    /// its card's.
    pub control_radius: Pixels,
    /// The radius of a circle or a pill — a round icon button, a status dot.
    /// The theme's `radius_full`, which drops to zero when the user picks
    /// square corners, so nothing stays round in a square UI.
    pub radius_full: Pixels,
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

/// The first shade of `color`, stepping toward `foreground` in Oklab by tenths,
/// that reaches `minimum` contrast on both the page and the card surface.
///
/// Theme status and primary colours are tuned as button fills, and several
/// bundled themes put them below text contrast on their own page (a yellow
/// warning on white, a deep primary on near-black). Moving toward the
/// foreground keeps the hue recognisable while making it readable. A colour
/// that already passes is returned unchanged.
fn legible(color: Hsla, minimum: f32, foreground: Hsla, background: Hsla, surface: Hsla) -> Hsla {
    if minimum_text_contrast(color, background, surface) >= minimum {
        return color;
    }
    (1..=10u8)
        .map(|step| color.mix_oklab(foreground, 1. - f32::from(step) / 10.))
        .find(|shade| minimum_text_contrast(*shade, background, surface) >= minimum)
        .unwrap_or(foreground)
}

/// Derive the app palette from the active gpui-component theme's semantic
/// tokens, so the hand-painted surfaces (window, cards, mouse model) re-skin
/// with the selected theme exactly as the framework widgets do.
///
/// - `page` ← `background` (window and page canvas)
/// - `panel` ← `group_box` (content cards)
/// - `control` / `control_hover` ← `secondary` / `secondary_hover`
/// - `muted` ← `muted` (non-interactive tracks and disabled illustrations)
/// - `border`, `text_primary` ← `foreground`, `text_muted` ← `muted_foreground`
/// - `accent` ← `primary`, and `danger` ← `danger`, both at text contrast
/// - `warning`, `success` ← their roles at graphic contrast; they only mark
///   icons, dots, and fills
#[must_use]
pub fn palette(cx: &App) -> Palette {
    let t = cx.theme();
    let shade = |color, minimum| legible(color, minimum, t.foreground, t.background, t.group_box);
    let accent = shade(t.primary, MIN_TEXT_CONTRAST);
    Palette {
        page: t.background,
        panel: t.group_box,
        control: t.secondary,
        control_hover: t.secondary_hover,
        muted: t.muted,
        border: t.border,
        text_primary: t.foreground,
        text_muted: t.muted_foreground,
        accent,
        accent_tint: accent.opacity(0.12),
        accent_tint_hover: accent.opacity(0.18),
        danger: shade(t.danger, MIN_TEXT_CONTRAST),
        warning: shade(t.warning, MIN_GRAPHIC_CONTRAST),
        success: shade(t.success, MIN_GRAPHIC_CONTRAST),
        ring: t.ring,
        card_radius: t.radius * 1.5,
        control_radius: t.radius,
        radius_full: t.radius_full(),
    }
}

/// Our brand theme (light + dark), encoding the original tuned surfaces. Kept as
/// a readable, committed JSON. The upstream gpui-component themes are *not*
/// vendored into this repo — `build.rs` copies them from the pinned dependency
/// checkout into `OUT_DIR` and generates the `UPSTREAM_THEME_JSON` list included
/// just below (gpui-component doesn't ship them inside its compiled crate, so
/// they must be embedded to be selectable).
const OPENLOGI_THEME_JSON: &str = include_str!("../../themes/openlogi.json");

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

pub(crate) fn rem_size(scale: UiScale) -> Pixels {
    px(BASE_REM_SIZE * f32::from(scale.percent()) / 100.)
}

/// Apply one semantic interface scale to a window.
pub(crate) fn apply_scale(window: &mut Window, scale: UiScale) {
    window.set_rem_size(rem_size(scale));
}

/// Apply the stored interface scale to one desktop window.
///
/// Root views call this before building their elements so text and every
/// rem-based layout token change together. The Actions Ring is a separate
/// process and deliberately keeps its cursor-centred pixel geometry.
pub fn apply_ui_scale(window: &mut Window, cx: &App) {
    let scale =
        AppState::try_read(cx).map_or_else(UiScale::default, |state| state.app_settings().ui_scale);
    apply_scale(window, scale);
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
    let (appearance, light_name, dark_name, radius) = AppState::try_read(cx).map_or_else(
        || (Appearance::default(), None, None, None),
        |state| {
            let s = state.app_settings();
            (
                s.appearance,
                s.theme_light.clone(),
                s.theme_dark.clone(),
                s.ui_radius,
            )
        },
    );

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
    // Theme tokens are app-global and consumed by every open window.
    cx.refresh_windows();
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
            .border_color(if selected { pal.accent } else { pal.border })
    }

    /// A faint accent fill when `selected`; leaves the background untouched
    /// otherwise so the caller's resting fill shows through.
    #[must_use]
    fn selected_fill(self, selected: bool, pal: Palette) -> Self {
        if selected {
            self.bg(pal.accent_tint)
        } else {
            self
        }
    }

    /// The keyboard focus ring for a bespoke control. Apply it inside the
    /// control's `focus_visible` refinement, so it appears only after keyboard
    /// navigation and never on a mouse click.
    ///
    /// It matches the ring gpui-component draws around its own controls — a
    /// solid line in the theme's `ring` colour inside a translucent halo — so
    /// Tab moves through one treatment instead of a different hover-like tint
    /// on every control. The ring is two spread shadows outside the element's
    /// edge: it takes no layout space, and it follows the element's corner
    /// radius. A clipping ancestor cuts it off, so leave it a few pixels of
    /// room. The widths are physical pixels on purpose: like the framework's
    /// ring, they must not grow with the interface scale.
    #[must_use]
    fn focus_ring(self, pal: Palette) -> Self {
        self.shadow(vec![
            BoxShadow::new(px(0.), px(0.), pal.ring.opacity(0.5)).spread_radius(px(3.)),
            BoxShadow::new(px(0.), px(0.), pal.ring).spread_radius(px(1.)),
        ])
    }
}

impl<E: Styled> SelectableStyle for E {}

/// The app's type ramp as semantic roles, so a heading is `.text_heading()`
/// everywhere instead of each call site re-picking a `text_*` size and a
/// `font_weight`. Sizes, weights, and line heights live here once — an
/// Apple-HIG-inspired scale, more generous and higher-contrast than the raw
/// Tailwind steps it replaces — and every screen re-skins by editing this trait.
///
/// Blanket-implemented for every [`Styled`] element, the same way
/// [`SelectableStyle`] extends styling. Colour stays a separate axis (the caller
/// still picks `pal.text_primary` / `text_muted`); this trait only fixes size,
/// weight, and leading.
pub trait Typography: Styled + Sized {
    /// Page / dialog hero title (empty states, connection notices). The
    /// heaviest, largest step — the one place Bold is used.
    #[must_use]
    fn text_title(self) -> Self {
        self.text_size(rems(1.625))
            .font_weight(FontWeight::BOLD)
            .line_height(relative(1.2))
    }

    /// Screen / section heading — the Home title, a device name, a window's
    /// primary heading.
    #[must_use]
    fn text_heading(self) -> Self {
        self.text_size(rems(1.25))
            .font_weight(FontWeight::SEMIBOLD)
            .line_height(relative(1.3))
    }

    /// Card / group title and item names — a heading one rung down, sitting
    /// inside a card rather than titling a screen.
    #[must_use]
    fn text_subheading(self) -> Self {
        self.text_size(rems(0.9375))
            .font_weight(FontWeight::SEMIBOLD)
            .line_height(relative(1.4))
    }

    /// Default body copy — control labels, descriptions, values.
    #[must_use]
    fn text_body(self) -> Self {
        self.text_size(rems(0.9375)).line_height(relative(1.45))
    }

    /// De-emphasised metadata and helper text — the muted line under a label,
    /// battery readouts, hints. Pair with `pal.text_muted`.
    #[must_use]
    fn text_caption(self) -> Self {
        self.text_size(rems(0.75)).line_height(relative(1.4))
    }
}

impl<E: Styled> Typography for E {}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn bundled_theme_picker_keeps_upstream_and_openlogi_themes(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            register_builtin_themes(cx);
            let themes = ThemeRegistry::global(cx).themes();
            for name in [
                OPENLOGI_LIGHT,
                OPENLOGI_DARK,
                "Catppuccin Latte",
                "Catppuccin Mocha",
            ] {
                assert!(themes.contains_key(name), "missing bundled theme: {name}");
            }
        });
    }

    #[test]
    fn content_width_scale_preserves_the_standard_layout() {
        assert_eq!(
            [
                ContentWidth::Narrow,
                ContentWidth::Small,
                ContentWidth::Medium,
                ContentWidth::Large,
                ContentWidth::ExtraLarge,
                ContentWidth::DoubleExtraLarge,
            ]
            .map(|width| width.rems().to_pixels(px(BASE_REM_SIZE))),
            [px(440.), px(560.), px(680.), px(920.), px(980.), px(1040.),]
        );
    }

    #[test]
    fn ui_scale_presets_map_to_expected_rem_sizes() {
        assert_eq!(
            UiScale::ALL.map(rem_size),
            [px(14.4), px(16.), px(17.6), px(20.)]
        );
    }

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

    /// Theme authors tune `primary`, `danger`, `warning`, and `success` as
    /// button fills, and several bundled themes leave them illegible as text or
    /// marks on their own page — a yellow warning on white, a deep primary on
    /// near-black. The palette must still clear its contrast floor on both
    /// surfaces for every theme a user can pick.
    #[gpui::test]
    fn every_bundled_theme_palette_is_legible(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            register_builtin_themes(cx);
            let themes: Vec<_> = ThemeRegistry::global(cx)
                .themes()
                .values()
                .cloned()
                .collect();
            assert!(themes.len() > 2, "upstream themes should be registered");
            for config in themes {
                Theme::global_mut(cx).apply_config(&config);
                normalize_theme_text_contrast(Theme::global_mut(cx));
                let t = cx.theme();
                let (background, surface) = (t.background, t.group_box);
                let pal = palette(cx);
                for (role, color, minimum) in [
                    ("accent", pal.accent, MIN_TEXT_CONTRAST),
                    ("danger", pal.danger, MIN_TEXT_CONTRAST),
                    ("warning", pal.warning, MIN_GRAPHIC_CONTRAST),
                    ("success", pal.success, MIN_GRAPHIC_CONTRAST),
                ] {
                    let contrast = minimum_text_contrast(color, background, surface);
                    assert!(
                        contrast >= minimum,
                        "{}: {role} {color:?} reaches {contrast:.2}:1, below {minimum}:1",
                        config.name
                    );
                }
            }
        });
    }

    #[test]
    fn legible_keeps_a_passing_colour_and_darkens_a_failing_one() {
        let white: Hsla = rgb(0xff_ffff).into();
        let ink: Hsla = rgb(0x1a_1a1d).into();
        let strong: Hsla = rgb(0x1d_4ed8).into();
        assert_eq!(
            legible(strong, MIN_TEXT_CONTRAST, ink, white, white),
            strong
        );

        let yellow: Hsla = rgb(0xea_b308).into();
        let shaded = legible(yellow, MIN_GRAPHIC_CONTRAST, ink, white, white);
        assert!(minimum_text_contrast(shaded, white, white) >= MIN_GRAPHIC_CONTRAST);
        assert_ne!(shaded, ink, "the shade should keep some of its hue");
    }
}
