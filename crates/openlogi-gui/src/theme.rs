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

use gpui::{App, Hsla, Pixels, Styled, Window, hsla, px, rgb};
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

/// Sizes that several components need to agree on.
pub const HEADER_H: f32 = 80.;
pub const FOOTER_H: f32 = 50.;

/// Semantic spacing tokens (px), so surfaces that must agree share one value
/// instead of each call site hand-picking a `p_*` / `gap_*` step.
///
/// - `SCREEN_PAD` — the inset around a detail-tab body. Uniform across tabs so
///   the content's start doesn't shift when switching tabs (the pointer tab's
///   two-column grid is sized against this exact value; see its card min-width).
/// - `CARD_PAD` / `CARD_GAP` — a card's inner padding and its title-to-content
///   gap, so every [`panel_card`](crate::app) reads the same.
pub const SCREEN_PAD: f32 = 20.;
pub const CARD_PAD: f32 = 16.;
pub const CARD_GAP: f32 = 12.;

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
}

/// Derive the app palette from the active gpui-component theme's semantic
/// tokens, so the hand-painted surfaces (window, cards, mouse model) re-skin
/// with the selected theme exactly as the framework widgets do.
///
/// - `bg` ← `background` (window)
/// - `surface` / `surface_hover` ← `secondary` / `secondary_hover` (cards). The
///   bundled OpenLogi theme sets `group_box` to the same colour, so the Fill
///   group-box cards and the bespoke `pal.surface` cards match.
/// - `border`, `text_primary` ← `foreground`, `text_muted` ← `muted_foreground`.
#[must_use]
pub fn palette(cx: &App) -> Palette {
    let t = cx.theme();
    Palette {
        bg: t.background,
        surface: t.secondary,
        surface_hover: t.secondary_hover,
        border: t.border,
        text_primary: t.foreground,
        text_muted: t.muted_foreground,
        card_radius: t.radius * 1.5,
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

    if let Some(radius) = radius {
        Theme::global_mut(cx).radius = px(f32::from(radius));
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
