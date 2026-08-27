//! Appearance settings page: mode, theme grid, radius, scale, language.

use super::language::{LanguageOption, language_select_field};
use gpui::{ElementId, img};
use gpui_base::Button as BaseButton;
use openlogi_core::config::AppIcon;

use super::{
    ActiveTheme, App, AppState, Appearance, Axis, Button, ButtonGroup, Entity, FluentBuilder, Hsla,
    Icon, IconName, InputState, InteractiveElement, IntoElement, Palette, ParentElement, Rc,
    SelectState, Selectable, SettingField, SettingGroup, SettingItem, SettingPage, SettingsView,
    SharedString, StateEvent, StatefulInteractiveElement, Styled, Theme, ThemeColor, ThemeConfig,
    ThemeFilter, ThemeMode, ThemeRegistry, UiScale, div, h_flex, px, rgb, theme, v_flex,
};
use crate::platform::app_icon;
use crate::ui::choice_card::ChoiceCard;
use crate::ui::components::control_input;
use crate::ui::theme::Typography as _;

/// The Appearance page: light/dark mode, the theme grid, corner radius, and the
/// interface language. Every theme here re-skins the whole app — the bespoke
/// surfaces read the same `cx.theme()` tokens as the framework widgets.
pub(super) fn appearance_page(
    view: Entity<SettingsView>,
    filter: ThemeFilter,
    theme_search: Entity<InputState>,
    language_select: Entity<SelectState<Vec<LanguageOption>>>,
) -> SettingPage {
    // Titled groups so the sidebar shows them as sub-items (gpui-component
    // renders a page's groups as nested sidebar entries once there's more than
    // one and each is titled). Item titles stay distinct from their group title.
    let mut theme_group = SettingGroup::new()
        .title(tr!("Theme"))
        .item(
            SettingItem::new(
                tr!("Appearance mode"),
                SettingField::render(move |_, _, cx| mode_segment(cx)),
            )
            .layout(Axis::Vertical)
            .description(tr!(
                "Light and dark use the matching theme; Follow system tracks the OS setting."
            )),
        )
        .item(
            SettingItem::new(
                tr!("Color theme"),
                SettingField::render(move |_, _, cx| {
                    theme_picker(&view, &theme_search, filter, cx)
                }),
            )
            .layout(Axis::Vertical),
        );

    // Only macOS can wear a chosen icon: Windows embeds one into the executable
    // at build time and Linux installs a fixed one from the package.
    if cfg!(target_os = "macos") {
        theme_group = theme_group.item(
            SettingItem::new(
                tr!("App icon"),
                SettingField::render(move |_, _, cx| icon_picker(cx)),
            )
            .layout(Axis::Vertical)
            .description(tr!(
                "Pick the icon OpenLogi wears in the Dock, Finder and Launchpad."
            )),
        );
    }

    let theme_group = theme_group
        .item(
            // Compact control → inline on the right of the label (HIG), unlike the
            // wide thumbnail/grid controls which stack below.
            SettingItem::new(
                tr!("Corner radius"),
                SettingField::render(move |_, _, cx| radius_segment(cx)),
            )
            .description(tr!("Roundness of buttons, cards, and controls.")),
        )
        .item(
            SettingItem::new(
                tr!("Interface scale"),
                SettingField::render(move |_, _, cx| scale_segment(cx)),
            )
            .description(tr!("Scale text and interface spacing.")),
        );

    let language_group = SettingGroup::new().title(tr!("Language")).item(
        SettingItem::new(
            tr!("Interface language"),
            SettingField::render(move |_, _, _| language_select_field(language_select.clone())),
        )
        .description(tr!("Choose the interface language.")),
    );

    SettingPage::new(tr!("Appearance"))
        .icon(IconName::Palette)
        .resettable(false)
        .group(language_group)
        .group(theme_group)
}

/// The stored light/dark preference (defaults to following the OS).
fn appearance_of(cx: &App) -> Appearance {
    AppState::try_read(cx).map_or(Appearance::System, |s| s.app_settings().appearance)
}

/// Persist an appearance-mode choice and re-apply the live theme.
fn set_appearance(cx: &mut App, appearance: Appearance) {
    AppState::update(cx, |state, cx| {
        state.set_appearance(appearance);
        cx.emit(StateEvent::SettingsChanged);
    });
    theme::apply_from_settings(None, cx);
}

/// Persist a corner-radius choice and re-apply the live theme. `None` defers to
/// the active theme's own radius.
fn set_radius(cx: &mut App, radius: Option<u8>) {
    AppState::update(cx, |state, cx| {
        state.set_ui_radius(radius);
        cx.emit(StateEvent::SettingsChanged);
    });
    theme::apply_from_settings(None, cx);
}

/// Persist an interface-scale choice and repaint every desktop window. Each
/// root applies its own rem size on that repaint, avoiding a re-entrant update
/// of the Settings window currently dispatching this click.
fn set_scale(cx: &mut App, scale: UiScale) {
    AppState::update(cx, |state, cx| {
        state.set_ui_scale(scale);
        cx.emit(StateEvent::SettingsChanged);
    });
    cx.refresh_windows();
}

/// The Light / Dark / Follow-system appearance picker — three macOS-style
/// preview thumbnails, each with a radio + label, mirroring System Settings.
fn mode_segment(cx: &App) -> gpui::Div {
    let pal = theme::palette(cx);
    let current = appearance_of(cx);
    let accent = cx.theme().primary;
    h_flex().gap_4().items_start().children([
        mode_card(
            "mode-light",
            tr!("Light"),
            ModePreview::Light,
            current == Appearance::Light,
            accent,
            pal,
            Appearance::Light,
        ),
        mode_card(
            "mode-dark",
            tr!("Dark"),
            ModePreview::Dark,
            current == Appearance::Dark,
            accent,
            pal,
            Appearance::Dark,
        ),
        mode_card(
            "mode-system",
            tr!("Follow system"),
            ModePreview::Auto,
            current == Appearance::System,
            accent,
            pal,
            Appearance::System,
        ),
    ])
}

/// Which scheme a mode card's thumbnail paints.
#[derive(Clone, Copy)]
enum ModePreview {
    Light,
    Dark,
    Auto,
}

/// One appearance card: a preview thumbnail (with an accent selection ring when
/// active) above a radio dot + label.
fn mode_card(
    id: &'static str,
    label: SharedString,
    preview: ModePreview,
    selected: bool,
    accent: Hsla,
    pal: Palette,
    appearance: Appearance,
) -> impl IntoElement {
    let thumb = div()
        .w(px(104.))
        .h(px(64.))
        .rounded(pal.control_radius)
        .overflow_hidden()
        .border_2()
        .border_color(if selected { accent } else { pal.border })
        .map(|this| match preview {
            ModePreview::Light => this.child(mini_window(false)),
            ModePreview::Dark => this.child(mini_window(true)),
            // A single window split down the middle: light left, dark right.
            ModePreview::Auto => this.child(
                h_flex()
                    .size_full()
                    .child(
                        div()
                            .w(px(50.))
                            .h_full()
                            .overflow_hidden()
                            .child(mini_window(false)),
                    )
                    .child(
                        div()
                            .w(px(50.))
                            .h_full()
                            .flex()
                            .justify_end()
                            .overflow_hidden()
                            .child(mini_window(true)),
                    ),
            ),
        });

    ChoiceCard::new(id, label.clone())
        .selected(selected)
        .gap(px(6.))
        .items_center()
        .cursor_pointer()
        .focus_visible(move |style| style.text_color(pal.text_primary))
        .child(thumb)
        .child(
            h_flex()
                .items_center()
                .gap(px(6.))
                .child(radio_dot(selected, accent, pal))
                .child(div().text_body().child(label)),
        )
        .on_click(move |_, _, cx| set_appearance(cx, appearance))
}

/// The app-icon picker: one card per icon, each showing a render of the
/// compiled icon rather than its artwork, so the choice looks like what macOS
/// will draw.
fn icon_picker(cx: &App) -> gpui::Div {
    let pal = theme::palette(cx);
    let current =
        AppState::try_read(cx).map_or_else(AppIcon::default, |state| state.app_settings().app_icon);
    let accent = cx.theme().primary;
    h_flex()
        .gap_4()
        .items_start()
        .children(AppIcon::ALL.map(|icon| icon_card(icon, current == icon, accent, pal)))
}

/// One icon card: the preview above a radio + label, ringed when it is the icon
/// the app is wearing.
fn icon_card(icon: AppIcon, selected: bool, accent: Hsla, pal: Palette) -> impl IntoElement {
    let preview = app_icon::preview(icon);
    ChoiceCard::new(SharedString::from(icon.to_string()), icon_label(icon))
        .selected(selected)
        .gap(px(6.))
        .items_center()
        .cursor_pointer()
        .focus_visible(move |style| style.text_color(pal.text_primary))
        .child(
            div()
                .size(px(80.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(pal.control_radius)
                .border_2()
                .border_color(if selected { accent } else { pal.border })
                // No preview means a bundle built before this icon existed;
                // the card stays, empty, rather than shifting the row.
                .when_some(preview, |this, path| {
                    this.child(img(path).size(px(64.)))
                }),
        )
        .child(
            h_flex()
                .items_center()
                .gap(px(6.))
                .child(radio_dot(selected, accent, pal))
                .child(div().text_body().child(icon_label(icon))),
        )
        .on_click(move |_, _, cx| {
            AppState::update(cx, |state, cx| {
                state.set_app_icon(icon);
                cx.emit(StateEvent::SettingsChanged);
            });
        })
}

/// What an icon is called in the picker. Proper nouns, so they are not
/// translated — the same reasoning as the theme names in the grid above.
fn icon_label(icon: AppIcon) -> SharedString {
    match icon {
        AppIcon::Openlogi => "OpenLogi".into(),
        AppIcon::Prism => "Prism".into(),
    }
}

/// A miniature window-on-desktop at a fixed 100×60, used inside a mode card.
/// Painted with fixed scheme colours (representing light vs dark) so the
/// thumbnails read the same under any active theme.
fn mini_window(dark: bool) -> impl IntoElement {
    let (wallpaper, window, bar, line) = if dark {
        (
            rgb(0x26_262b),
            rgb(0x1b_1b1e),
            rgb(0x3a_3a42),
            rgb(0x3b_82f6),
        )
    } else {
        (
            rgb(0xdf_e4ec),
            rgb(0xff_ffff),
            rgb(0xcc_d2db),
            rgb(0x3b_82f6),
        )
    };
    let dot = |c: u32| div().size(px(3.)).rounded_full().bg(rgb(c));
    div()
        .w(px(100.))
        .h(px(60.))
        .flex_shrink_0()
        .bg(wallpaper)
        .p(px(7.))
        .child(
            v_flex()
                .size_full()
                .rounded(px(4.))
                .overflow_hidden()
                .bg(window)
                .child(
                    h_flex()
                        .h(px(11.))
                        .w_full()
                        .items_center()
                        .gap(px(2.))
                        .px(px(4.))
                        .bg(bar)
                        .child(dot(0xff_5f57))
                        .child(dot(0xfe_bc2e))
                        .child(dot(0x28_c840)),
                )
                .child(
                    v_flex()
                        .p(px(5.))
                        .gap(px(3.))
                        .child(div().w(px(30.)).h(px(3.)).rounded_full().bg(line))
                        .child(div().w(px(54.)).h(px(3.)).rounded_full().bg(bar))
                        .child(div().w(px(40.)).h(px(3.)).rounded_full().bg(bar)),
                ),
        )
}

/// A small radio indicator: a ring when unselected, a filled accent dot when
/// selected.
fn radio_dot(selected: bool, accent: Hsla, pal: Palette) -> impl IntoElement {
    div()
        .size(px(13.))
        .rounded_full()
        .border_2()
        .flex()
        .items_center()
        .justify_center()
        .border_color(if selected { accent } else { pal.text_muted })
        .when(selected, |this| {
            this.child(div().size(px(6.)).rounded_full().bg(accent))
        })
}

/// The Sharp / Default / Round corner-radius segmented control. "Default" stores
/// `None` — defer to the active theme's own radius — rather than a fixed 6px, so
/// it neither mis-highlights under themes with a different radius nor traps the
/// user away from the theme default.
fn radius_segment(cx: &App) -> ButtonGroup {
    let current = AppState::try_read(cx).and_then(|s| s.app_settings().ui_radius);
    let options: [Option<u8>; 3] = [Some(0), None, Some(12)];
    ButtonGroup::new("corner-radius")
        .outline()
        .child(
            Button::new("radius-sharp")
                .label(tr!("Sharp"))
                .selected(current == Some(0)),
        )
        .child(
            Button::new("radius-default")
                .label(tr!("Default"))
                .selected(current.is_none()),
        )
        .child(
            Button::new("radius-round")
                .label(tr!("Round"))
                .selected(current == Some(12)),
        )
        .on_click(move |clicks, _, cx| {
            if let Some(&ix) = clicks.first() {
                set_radius(cx, options[ix]);
            }
        })
}

/// The supported interface-scale presets. Keeping this finite makes every
/// layout state reviewable and avoids arbitrary values that can render a
/// window unusable.
fn scale_segment(cx: &App) -> ButtonGroup {
    let current =
        AppState::try_read(cx).map_or_else(UiScale::default, |state| state.app_settings().ui_scale);
    ButtonGroup::new("interface-scale")
        .outline()
        .children(UiScale::ALL.map(|scale| {
            Button::new(("interface-scale", u32::from(scale.percent())))
                .label(format!("{}%", scale.percent()))
                .selected(current == scale)
        }))
        .on_click(|clicks, _, cx| {
            let Some(scale) = clicks
                .first()
                .and_then(|index| UiScale::ALL.get(*index))
                .copied()
            else {
                return;
            };
            set_scale(cx, scale);
        })
}

/// Filter chips + the theme grid. Each card previews the theme's own colours
/// and, on click, stores it for the matching mode and switches to that mode.
fn theme_picker(
    view: &Entity<SettingsView>,
    theme_search: &Entity<InputState>,
    filter: ThemeFilter,
    cx: &App,
) -> gpui::Div {
    let pal = theme::palette(cx);
    let active = cx.theme().theme_name().clone();
    let query = theme_search.read(cx).value().trim().to_lowercase();
    // Collect just the preview colours per theme (small + `Copy`), so the 1.8 KB
    // `ThemeColor` isn't held across the element build.
    let themes: Vec<(SharedString, ThemeMode, Swatch)> = {
        let registry = ThemeRegistry::global(cx);
        registry
            .sorted_themes()
            .into_iter()
            .filter(|cfg| match filter {
                ThemeFilter::All => true,
                ThemeFilter::Light => !cfg.mode.is_dark(),
                ThemeFilter::Dark => cfg.mode.is_dark(),
            })
            .filter(|cfg| query.is_empty() || cfg.name.to_lowercase().contains(&query))
            .map(|cfg| {
                let colors = resolved_colors(cfg);
                let swatch = Swatch {
                    bg: colors.background,
                    primary: colors.primary,
                    foreground: colors.foreground,
                };
                (cfg.name.clone(), cfg.mode, swatch)
            })
            .collect()
    };

    let no_matches = themes.is_empty();
    let grid = div()
        .when(no_matches, |grid| {
            grid.text_body()
                .text_color(pal.text_muted)
                .child(tr!("No themes match “%{query}”.", query => query))
        })
        .when(!no_matches, |grid| {
            grid.flex()
                .flex_wrap()
                .gap_2()
                .children(themes.into_iter().map(|(name, mode, swatch)| {
                    let selected = name == active;
                    theme_card(name, mode, swatch, selected, pal)
                }))
        });

    v_flex()
        .w_full()
        .gap_3()
        .child(theme_toolbar(view, theme_search, filter, pal))
        .child(grid)
}

fn theme_toolbar(
    view: &Entity<SettingsView>,
    theme_search: &Entity<InputState>,
    filter: ThemeFilter,
    pal: Palette,
) -> impl IntoElement {
    // Translated chip labels may wrap so the fixed-width search and actions
    // remain available in narrower Settings windows.
    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            h_flex()
                .gap_2()
                .flex_1()
                .min_w_0()
                .flex_wrap()
                .child(filter_chip(
                    view,
                    "filter-all",
                    tr!("All"),
                    ThemeFilter::All,
                    filter,
                    pal,
                ))
                .child(filter_chip(
                    view,
                    "filter-light",
                    tr!("Light"),
                    ThemeFilter::Light,
                    filter,
                    pal,
                ))
                .child(filter_chip(
                    view,
                    "filter-dark",
                    tr!("Dark"),
                    ThemeFilter::Dark,
                    filter,
                    pal,
                )),
        )
        .child(
            div().w(px(200.)).flex_shrink_0().child(
                control_input(theme_search)
                    .cleanable(true)
                    .prefix(IconName::Search),
            ),
        )
        .child(theme_action_button(
            "themes-open",
            tr!("Open"),
            IconName::FolderOpen,
            pal,
            |_| theme::open_user_themes_dir(),
        ))
        .child(theme_action_button(
            "themes-refresh",
            tr!("Refresh"),
            IconName::Undo,
            pal,
            theme::reload_themes,
        ))
}

fn theme_action_button(
    id: &'static str,
    label: SharedString,
    icon: IconName,
    pal: Palette,
    on_click: impl Fn(&mut App) + 'static,
) -> impl IntoElement {
    BaseButton::new(id)
        .accessibility_label(label.clone())
        .flex_shrink_0()
        .px_2()
        .py_1()
        .gap_1()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(pal.border)
        .text_caption()
        .cursor_pointer()
        .bg(pal.control)
        .hover(move |style| style.bg(pal.control_hover))
        .focus_visible(move |style| style.bg(pal.control_hover))
        .child(Icon::new(icon).size_3())
        .child(label)
        .on_click(move |_, _, cx| on_click(cx))
}

/// Resolve a theme config's colours into a concrete [`ThemeColor`] for its
/// preview, without touching the global theme (mirrors gpui-component's own
/// theme picker).
fn resolved_colors(cfg: &Rc<ThemeConfig>) -> ThemeColor {
    let base = if cfg.mode.is_dark() {
        ThemeColor::dark()
    } else {
        ThemeColor::light()
    };
    let mut temp = Theme::from(base.as_ref());
    temp.apply_config(cfg);
    temp.colors
}

/// One theme card: a mini preview, the name, and a light/dark badge. Its low
/// resting shadow strengthens on hover and settles on press.
/// The three resolved colours a theme card previews. Small + `Copy` so it can
/// be collected and passed by value.
#[derive(Clone, Copy)]
struct Swatch {
    bg: Hsla,
    primary: Hsla,
    foreground: Hsla,
}

fn theme_card(
    name: SharedString,
    mode: ThemeMode,
    swatch: Swatch,
    selected: bool,
    pal: Palette,
) -> impl IntoElement {
    let dark = mode.is_dark();
    let stored = name.clone();
    ChoiceCard::new((ElementId::from("theme"), name.clone()), name.clone())
        .selected(selected)
        .w(px(132.))
        .p(px(8.))
        .gap_2()
        .rounded(pal.card_radius)
        .border_1()
        .border_color(if selected { swatch.primary } else { pal.border })
        .bg(pal.panel)
        .shadow_xs()
        .cursor_pointer()
        .hover(move |style| {
            let style = style.shadow_sm();
            if selected {
                style
            } else {
                style.border_color(pal.text_muted)
            }
        })
        .focus_visible(move |style| style.border_color(swatch.primary).shadow_sm())
        .active(gpui::Styled::shadow_2xs)
        .child(
            v_flex()
                .h(px(54.))
                .w_full()
                .rounded(pal.control_radius)
                .overflow_hidden()
                .p(px(7.))
                .gap(px(4.))
                .bg(swatch.bg)
                .child(div().w(px(40.)).h(px(4.)).rounded_full().bg(swatch.primary))
                .child(
                    div()
                        .w(px(66.))
                        .h(px(4.))
                        .rounded_full()
                        .bg(swatch.foreground)
                        .opacity(0.7),
                )
                .child(
                    div()
                        .w(px(34.))
                        .h(px(4.))
                        .rounded_full()
                        .bg(swatch.foreground)
                        .opacity(0.4),
                ),
        )
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .gap_1()
                .child(
                    div()
                        .overflow_hidden()
                        .text_caption()
                        .text_color(pal.text_primary)
                        .child(name),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(9.))
                        .text_color(pal.text_muted)
                        .child(if dark { tr!("Dark") } else { tr!("Light") }),
                ),
        )
        .on_click(move |_, _, cx| {
            let chosen = stored.to_string();
            AppState::update(cx, move |s, cx| {
                s.set_theme(dark, Some(chosen.clone()));
                // Picking a theme configures the light or dark *slot*. Only pin
                // the mode when the user has already chosen an explicit
                // Light/Dark mode — a "Follow System" preference must survive so
                // configuring (say) the dark slot doesn't force the whole app to
                // dark.
                if s.app_settings().appearance != Appearance::System {
                    s.set_appearance(if dark {
                        Appearance::Dark
                    } else {
                        Appearance::Light
                    });
                }
                cx.emit(StateEvent::SettingsChanged);
            });
            theme::apply_from_settings(None, cx);
        })
}

/// A pill that filters the theme grid by mode. View-local; clicking just
/// re-renders with the new filter.
fn filter_chip(
    view: &Entity<SettingsView>,
    id: &'static str,
    label: SharedString,
    value: ThemeFilter,
    current: ThemeFilter,
    pal: Palette,
) -> impl IntoElement {
    let selected = value == current;
    let view = view.clone();
    ChoiceCard::new(id, label.clone())
        .selected(selected)
        .px_3()
        .py_1()
        .rounded_full()
        .border_1()
        .text_caption()
        .cursor_pointer()
        .map(|this| {
            if selected {
                this.border_color(pal.text_primary)
                    .text_color(pal.text_primary)
            } else {
                this.border_color(pal.border)
                    .text_color(pal.text_muted)
                    .hover(|h| h.border_color(pal.text_muted))
                    .focus_visible(|h| h.border_color(pal.text_muted))
            }
        })
        .child(label)
        .on_click(move |_, _, cx| {
            view.update(cx, |this, cx| {
                this.theme_filter = value;
                cx.notify();
            });
        })
}
