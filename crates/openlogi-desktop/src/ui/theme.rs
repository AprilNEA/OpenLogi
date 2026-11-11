//! OpenLogi UI theme and shared sizes.
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

#[cfg(test)]
use gpui::rgb;
use gpui::{
    App, BoxShadow, FontWeight, Hsla, Pixels, Rems, Rgba, StatefulInteractiveElement, Styled,
    Window, hsla, px, relative, rems,
};
use gpui_kit::component::{
    ActiveTheme as _, Theme, ThemeMode, ThemeRegistry, scroll::ScrollbarMode,
};
use openlogi_core::config::{Appearance, UiScale};

use crate::state::AppState;

use super::spacing::DynamicSpacing;

// The brand accent lives in `openlogi-ui` so the overlay paints the same blue
// this app does — it cannot depend on this crate, and a local copy is how the
// ring ended up with a blue of its own. Re-exported so screens keep reading it
// off `theme::`, beside the status hues and the appearance-derived [`Palette`].
pub use openlogi_ui::color::{ACCENT_BLUE, accent};

/// Status colours for the connectivity readouts. Fixed like the accent, but the
/// overlay draws no status, so they stay with the app that does.
pub const STATUS_CONNECTED: u32 = 0x0022_c55e;
pub const STATUS_CONNECTING: u32 = 0x00ea_b308;
pub const STATUS_OFFLINE: u32 = 0x006b_7280;
/// Ring color for a device the user disabled ("Manage this device" off).
pub const STATUS_DISABLED: u32 = 0x00ef_4444;

/// Sizes that several components need to agree on.
pub const HEADER_H: f32 = 64.;
pub const FOOTER_H: f32 = 40.;
pub const DETAIL_RAIL_W: f32 = 168.;
/// Height of standalone form controls: buttons, text inputs, tabs.
/// gpui-component's `.small()` maps to a 24 px `h_6`, which reads undersized
/// against this 30 px control rhythm — small controls pin the height
/// explicitly (single-line `Input`s via `min_h`; their inherent `h` is
/// multi-line-only and would be ignored).
pub const CONTROL_H: f32 = 30.;

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

/// WCAG minimum contrast for a non-text indicator, such as the fill marking a
/// selected segment against the surface it rests on.
const MIN_NON_TEXT_CONTRAST: f32 = 3.;

/// Chroma below which a theme's `primary` is a neutral (slate, near black, near
/// white) rather than a hue that can mark a selection. Solarized Light's
/// `#586e75` sits at about 0.11; the palest bundled accent, Harper's lavender
/// `#b196c6`, at about 0.19.
const MIN_ACCENT_CHROMA: f32 = 0.15;

/// Responsive bounds for a device card in the Home grid. At standard scale,
/// two cards fit the 720 px minimum window after the screen inset and gap; at
/// the normal wide window, three grow to [`GALLERY_CARD_MAX_W`].
/// `GALLERY_PHOTO_H` is the fixed product-image stage above the scalable
/// identity and status rows.
pub const GALLERY_CARD_MIN_W: Rems = rems(19.375);
pub const GALLERY_CARD_MAX_W: Rems = rems(25.3125);
pub const GALLERY_PHOTO_H: f32 = 196.;

/// Appearance-dependent surface + text colours for the bespoke (non
/// gpui-component) surfaces. Resolved once per render via [`palette`] and
/// passed down to the free helper builders.
///
/// These are now *derived from the active gpui-component theme's semantic
/// tokens* (see [`palette`]), so the hand-painted surfaces re-skin with whatever
/// theme the user selects in Settings → Appearance — the same `cx.theme()` the
/// framework widgets read. The bundled "OpenLogi" theme (`themes/openlogi.json`)
/// provides the default values for those roles.
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
    /// Pressed fill for bespoke interactive controls, deeper than hover, so a
    /// press shows before the button is released.
    pub control_active: Hsla,
    /// Muted, non-interactive fill for tracks and disabled illustrations.
    pub muted: Hsla,
    /// Hairline border between cards and surface.
    pub border: Hsla,
    /// Foreground text.
    pub text_primary: Hsla,
    /// De-emphasised labels / metadata.
    pub text_muted: Hsla,
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
    /// The theme's keyboard-focus colour; see [`FocusRingStyle::focus_ring`].
    pub ring: Hsla,
    /// Error text: the theme's `danger` hue, darkened or lightened until it
    /// reads at 4.5:1 on both the page and a card. `danger` itself is tuned as
    /// a fill and fails as caption text on the light page.
    pub text_danger: Hsla,
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

/// `color` with its hue and saturation kept and its lightness moved away from
/// `background` until it meets [`MIN_TEXT_CONTRAST`] on both surfaces.
fn legible_tint(color: Hsla, background: Hsla, surface: Hsla) -> Hsla {
    let step = if background.l > 0.5 { -0.02 } else { 0.02 };
    let mut tint = color;
    while minimum_text_contrast(tint, background, surface) < MIN_TEXT_CONTRAST
        && (0.0..=1.0).contains(&(tint.l + step))
    {
        tint.l += step;
    }
    tint
}

/// Repair the contrast the active theme leaves too low. Run after every
/// `Theme::change`, which re-resolves every token from the theme file.
fn normalize_theme_contrast(theme: &mut Theme) {
    normalize_theme_text_contrast(theme);
    normalize_theme_accent(theme);
}

fn normalize_theme_text_contrast(theme: &mut Theme) {
    theme.muted_foreground = accessible_muted_text(
        theme.muted_foreground,
        theme.foreground,
        theme.background,
        theme.group_box,
    );
}

/// Replace an `accent` that blends into the theme's surfaces.
///
/// `accent` is the fill a checked `Toggle` and
/// [`SelectableStyle::selected_segment`] paint, so it must clear 3:1 against
/// `secondary` (an unselected segment) and `background` (the page). Most
/// bundled third-party themes ship a neutral accent at about 1:1 to both.
///
/// The replacement keeps the hue of the theme's `primary`, or of the brand
/// blue when `primary` is a neutral, and searches its lightness. A candidate
/// must clear 3:1 against both surfaces yet stand out from the page no more
/// than `foreground` does, so it stays a tint between page and text rather
/// than an inverted black or white block. A Select dropdown paints its
/// highlighted row in `foreground` on `accent`, so the search takes the
/// candidate on which `foreground` reads best, up to 4.5:1, and among equals
/// the one nearest the base lightness. Where a theme's `foreground` sits too
/// close to its surfaces, that row stays below 4.5:1. Only `accent` (with its
/// `tokens.accent` mirror) and `accent_foreground` change.
fn normalize_theme_accent(theme: &mut Theme) {
    let stands_out = |fill: Hsla| {
        contrast_ratio(fill, theme.secondary).min(contrast_ratio(fill, theme.background))
            >= MIN_NON_TEXT_CONTRAST
    };
    if stands_out(theme.accent) {
        return;
    }

    let primary = theme.primary;
    let chroma = (1. - (2. * primary.l - 1.).abs()) * primary.s;
    let base = if chroma >= MIN_ACCENT_CHROMA {
        primary
    } else {
        accent()
    };
    let row_contrast = |fill: Hsla| contrast_ratio(theme.foreground, fill).min(MIN_TEXT_CONTRAST);
    let text_contrast = contrast_ratio(theme.foreground, theme.background);
    let distance = |fill: Hsla| (fill.l - base.l).abs();
    let Some(fill) = (0..=1000u16)
        .map(|step| Hsla {
            l: f32::from(step) / 1000.,
            a: 1.,
            ..base
        })
        .filter(|fill| {
            stands_out(*fill) && contrast_ratio(*fill, theme.background) <= text_contrast
        })
        .max_by(|a, b| {
            row_contrast(*a)
                .total_cmp(&row_contrast(*b))
                .then_with(|| distance(*b).total_cmp(&distance(*a)))
        })
    else {
        return;
    };

    // White or black reads at 4.5:1 on any opaque fill: where both are
    // weakest, each still reaches about 4.58:1.
    theme.accent_foreground = [
        theme.accent_foreground,
        theme.primary_foreground,
        Hsla::white(),
    ]
    .into_iter()
    .find(|label| contrast_ratio(*label, fill) >= MIN_TEXT_CONTRAST)
    .unwrap_or(Hsla::black());
    theme.accent = fill;
    // The resolved mirror `Toggle` and the Select row paint from.
    theme.tokens.accent = fill.into();
}

/// Derive the app palette from the active gpui-component theme's semantic
/// tokens, so the hand-painted surfaces (window, cards, mouse model) re-skin
/// with the selected theme exactly as the framework widgets do.
///
/// - `page` ← `background` (window and page canvas)
/// - `panel` ← `group_box` (content cards)
/// - `control` / `control_hover` / `control_active` ← `secondary` /
///   `secondary_hover` / `secondary_active`
/// - `muted` ← `muted` (non-interactive tracks and disabled illustrations)
/// - `border`, `text_primary` ← `foreground`, `text_muted` ← `muted_foreground`.
#[must_use]
pub fn palette(cx: &App) -> Palette {
    let t = cx.theme();
    Palette {
        page: t.background,
        panel: t.group_box,
        control: t.secondary,
        control_hover: t.secondary_hover,
        control_active: t.secondary_active,
        muted: t.muted,
        border: t.border,
        text_primary: t.foreground,
        text_muted: t.muted_foreground,
        card_radius: t.radius * 1.5,
        control_radius: t.radius,
        ring: t.ring,
        text_danger: legible_tint(t.danger, t.background, t.group_box),
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
/// startup, after `gpui_kit::init` (which seeds the registry global). Our
/// brand theme loads first; the upstream themes follow.
pub fn register_builtin_themes(cx: &mut App) {
    let registry = ThemeRegistry::global_mut(cx);
    for json in std::iter::once(OPENLOGI_THEME_JSON).chain(UPSTREAM_THEME_JSON.iter().copied()) {
        if let Err(error) = registry.load_themes_from_str(json) {
            tracing::warn!(%error, "failed to load a bundled theme");
        }
    }
}

/// Show scrollbars the way the platform setting asks. Call once at startup,
/// after `gpui_kit::init`.
///
/// gpui-kit maps a platform that keeps scrollbars visible to `Hover`, which
/// still hides the bar until the pointer reaches its track. macOS "Show scroll
/// bars: Always", Windows "Always show scrollbars" and a Linux desktop without
/// overlay scrolling all ask for a bar that stays drawn.
pub fn sync_scrollbar_mode(cx: &mut App) {
    Theme::set_scrollbar_mode(scrollbar_mode(cx.should_auto_hide_scrollbars()), cx);
}

fn scrollbar_mode(platform_auto_hides: bool) -> ScrollbarMode {
    if platform_auto_hides {
        ScrollbarMode::Scrolling
    } else {
        ScrollbarMode::Always
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
    normalize_theme_contrast(theme);
    if let Some(radius) = radius {
        theme.radius = px(f32::from(radius));
    }
    // Theme tokens are app-global and consumed by every open window.
    cx.refresh_windows();
}

/// Faint accent fill marking a *selected* row / chip — tinted, not painted, so
/// it reads on both palettes while the label stays in `text_primary` (a blue
/// label fails AA contrast on the light surface). Hand-matched to [`accent`]
/// (hue 0.6 / sat 0.9 / light 0.6); the `accent_tint_matches_accent` test pins
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

/// The resting fill of the one item being configured among selectable
/// ones — a key on the Keys tab while the pointer previews others with
/// [`accent_tint`]: the depth of [`accent_tint_hover`], held without hover.
#[must_use]
pub fn accent_tint_configured() -> Hsla {
    accent_tint_hover()
}

/// [`accent_tint_hover`] deepened again while a selected row is held down.
fn accent_tint_active() -> Hsla {
    hsla(0.6, 0.9, 0.6, 0.24)
}

/// The feedback a bespoke pressable control gives under the pointer: a deeper
/// fill while hovered, deeper still while held, so a press shows before it is
/// released. A GPUI element cannot scale, so the fill is the only press
/// feedback a control has, and the two fills are one decision: a control that
/// hovers without pressing reads as stuck. A control marking a *selection*
/// keeps its accent tint and deepens that instead, so the choice stays
/// readable under the pointer. Blanket-implemented for every stateful element,
/// like [`SelectableStyle`]; the `gui-press-feedback-owner` guard keeps the
/// palette's hover and pressed fills out of hand-rolled `.hover()` closures.
pub trait PressableStyle: StatefulInteractiveElement + Sized {
    /// The hover and pressed fills of a control whose resting fill is
    /// [`Palette::control`], or [`accent_tint`] when `selected`.
    #[must_use]
    fn pressable_fill(self, selected: bool, pal: Palette) -> Self {
        let (hover, active) = if selected {
            (accent_tint_hover(), accent_tint_active())
        } else {
            (pal.control_hover, pal.control_active)
        };
        self.hover(move |style| style.bg(hover))
            .active(move |style| style.bg(active))
    }
}

impl<E: StatefulInteractiveElement> PressableStyle for E {}

/// Chaining helpers expressing the single "selected" decision — accent border
/// plus a faint accent fill — instead of every pill / chip / row hand-rolling
/// the `if selected { accent } else { border }` ternary (which had drifted into
/// three inconsistent dialects, one of them blue-on-white). Segments of one
/// joined control take [`SelectableStyle::selected_segment`] instead, since
/// they have no border of their own to colour. Blanket-implemented for every
/// [`Styled`] element, the way gpui-component extends styling.
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

    /// The theme's solid `accent` fill and its foreground when `selected`: the
    /// look gpui-component's `Toggle` takes when checked, for a `ButtonGroup`
    /// segment, whose stock selected fill barely differs from its resting one.
    /// Segments share their borders, so the fill alone marks the choice. Pass
    /// `false` while the group is disabled: the stock disabled look replays
    /// caller styles, and a disabled group has no choice to mark.
    #[must_use]
    fn selected_segment(self, selected: bool, cx: &App) -> Self {
        if selected {
            let theme = cx.theme();
            self.bg(theme.accent).text_color(theme.accent_foreground)
        } else {
            self
        }
    }
}

impl<E: Styled> SelectableStyle for E {}

/// Width of the halo [`FocusRingStyle::focus_ring`] paints outside the
/// border, matching gpui-component's own `focus_ring_style`.
const FOCUS_RING_WIDTH: Pixels = px(3.);

/// The one keyboard-focus look for bespoke controls, so focus never reads as
/// hover or selection. It matches the ring gpui-component's `Button` draws: the
/// border takes the theme's `ring` colour and a half-opacity halo surrounds it.
/// The halo is a spread shadow rather than the framework's extra child, so it
/// fits inside a `focus_visible` refinement and never shifts layout.
pub trait FocusRingStyle: Styled + Sized {
    /// Style a `focus_visible` refinement with the shared focus ring.
    #[must_use]
    fn focus_ring(self, pal: Palette) -> Self {
        self.border_color(pal.ring).shadow(vec![BoxShadow {
            color: pal.ring.opacity(0.5),
            offset: gpui::point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: FOCUS_RING_WIDTH,
            inset: false,
        }])
    }
}

impl<E: Styled> FocusRingStyle for E {}

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
    use gpui_kit::component::button::Button;

    use super::*;

    #[gpui::test]
    fn bundled_theme_picker_keeps_upstream_and_openlogi_themes(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
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

    /// A platform that keeps scrollbars visible gets a bar that stays drawn,
    /// not one that waits for the pointer; one that hides them shows the bar
    /// while scrolling.
    #[test]
    fn scrollbar_mode_follows_the_platform_setting() {
        assert_eq!(scrollbar_mode(false), ScrollbarMode::Always);
        assert_eq!(scrollbar_mode(true), ScrollbarMode::Scrolling);
    }

    /// The test platform keeps scrollbars visible. The synced mode reaches the
    /// base layer that draws the bar and holds across a theme change.
    #[gpui::test]
    fn the_synced_scrollbar_mode_holds_across_a_theme_change(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            register_builtin_themes(cx);
            sync_scrollbar_mode(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            assert_eq!(cx.theme().scrollbar_mode, ScrollbarMode::Always);
            assert_eq!(
                gpui_kit::base::Theme::global(cx).scrollbar.mode(),
                ScrollbarMode::Always
            );
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
                ("accent.foreground", "accent.background"),
                // A Select dropdown's highlighted row draws `foreground` on `accent`.
                ("foreground", "accent.background"),
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

    /// A segment marked by [`SelectableStyle::selected_segment`] must stand out
    /// from the surfaces an unselected segment rests on by WCAG's 3:1 non-text
    /// minimum, and keep its label readable, under every bundled theme as
    /// [`apply_from_settings`] leaves it. The fill is the `accent` token
    /// gpui-component's `Toggle` paints when checked, so this also holds the
    /// home view switcher and every toggle to it (#2046).
    #[gpui::test]
    fn selected_segment_stands_out_in_every_bundled_theme(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            register_builtin_themes(cx);
            let mut unselected = Button::new("segment").selected_segment(false, cx);
            assert!(
                unselected.style().background.is_none(),
                "an unselected segment keeps the stock fill"
            );

            let configs: Vec<_> = ThemeRegistry::global(cx)
                .themes()
                .values()
                .cloned()
                .collect();
            let mut failures = Vec::new();
            for config in configs {
                let (name, mode) = (config.name.clone(), config.mode);
                let theme = Theme::global_mut(cx);
                if mode.is_dark() {
                    theme.dark_theme = config;
                } else {
                    theme.light_theme = config;
                }
                Theme::change(mode, None, cx);
                normalize_theme_contrast(Theme::global_mut(cx));

                let mut selected = Button::new("segment").selected_segment(true, cx);
                let style = selected.style();
                let Some(fill) = style
                    .background
                    .as_ref()
                    .and_then(gpui::Fill::color)
                    .and_then(|background| background.as_solid())
                else {
                    panic!("{name}: a selected segment should take a solid fill");
                };
                let Some(label) = style.text.color else {
                    panic!("{name}: a selected segment should set its label colour");
                };
                let theme = cx.theme();
                let on_surfaces = contrast_ratio(fill, theme.secondary)
                    .min(contrast_ratio(fill, theme.background));
                let label = contrast_ratio(label, fill);
                if on_surfaces < MIN_NON_TEXT_CONTRAST || label < MIN_TEXT_CONTRAST {
                    failures.push(format!(
                        "{name}: fill {on_surfaces:.2}:1 on its surfaces, label {label:.2}:1"
                    ));
                }
                if gpui::Background::from(theme.tokens.accent) != fill.into() {
                    failures.push(format!("{name}: a checked Toggle paints another fill"));
                }
            }
            failures.sort();
            assert!(
                failures.is_empty(),
                "selected segments that do not stand out:\n{}",
                failures.join("\n")
            );
        });
    }

    /// Error captions sit on the page and on cards in both bundled themes. The
    /// raw `danger` red fails there (3.45:1 on the light page, 4.2:1 on a dark
    /// card), so the derived text colour must pass on both and stay red.
    #[test]
    fn danger_text_reads_on_page_and_card_in_both_openlogi_themes() {
        let Ok(theme_set) = serde_json::from_str::<serde_json::Value>(OPENLOGI_THEME_JSON) else {
            panic!("OpenLogi theme JSON should parse");
        };
        let Some(themes) = theme_set["themes"].as_array() else {
            panic!("OpenLogi theme JSON should contain themes");
        };

        for theme in themes {
            let name = theme["name"].as_str().unwrap_or("unnamed theme");
            let colors = &theme["colors"];
            let danger = theme_color(colors, "danger.background");
            let page = theme_color(colors, "background");
            let card = theme_color(colors, "group_box.background");

            let text = legible_tint(danger, page, card);
            for (surface, color) in [("page", page), ("card", card)] {
                let ratio = contrast_ratio(text, color);
                assert!(
                    ratio >= MIN_TEXT_CONTRAST,
                    "{name}: danger text is {ratio:.2}:1 on the {surface}"
                );
            }
            assert!(
                (text.h - danger.h).abs() < 1e-3 && (text.s - danger.s).abs() < 1e-3,
                "{name}: danger text keeps the danger hue"
            );
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

    /// The pressed tint is the third hand-written `hsla`; pin that it is the
    /// same accent, deeper than hover, so a press on a selected row reads as a
    /// press and not as a colour change.
    #[test]
    fn accent_tint_active_sits_deeper_than_hover() {
        let a = accent();
        let ta = accent_tint_active();
        assert!((a.h - ta.h).abs() < 0.02, "hue {} vs {}", a.h, ta.h);
        assert!((a.s - ta.s).abs() < 0.05, "sat {} vs {}", a.s, ta.s);
        assert!((a.l - ta.l).abs() < 0.05, "light {} vs {}", a.l, ta.l);
        assert!(
            ta.a > accent_tint_hover().a,
            "pressed tint should sit deeper than the hover tint"
        );
    }
}
