//! Appearance settings page: language, window material, and theme.

use super::language::{LanguageOption, language_select_field};
use crate::ui::theme::FocusRingStyle as _;
use gpui::{ClickEvent, ElementId, Window, img};
use openlogi_core::config::{AppIcon, AppSettings};

use super::{
    ActiveTheme, App, AppState, Appearance, Axis, Button, ButtonGroup, Entity, FluentBuilder, Hsla,
    IconName, InputState, InteractiveElement, IntoElement, Palette, ParentElement, Rc, SelectState,
    Selectable, SettingField, SettingGroup, SettingItem, SettingPage, SettingsView, SharedString,
    StatefulInteractiveElement, Styled, Theme, ThemeColor, ThemeConfig, ThemeFilter, ThemeMode,
    ThemeRegistry, UiScale, div, h_flex, px, rgb, theme, v_flex,
};
use crate::platform::app_icon::AppIconExt as _;
use crate::ui::choice_card::ChoiceCard;
use crate::ui::commit_slider::{CommitSlider, SliderRange, SliderUnit as _};
use crate::ui::components::control_input;
use crate::ui::slider_row::SliderRow;
use crate::ui::theme::{SelectableStyle as _, Typography as _};
use gpui_kit::component::slider::{Slider, SliderState};
use gpui_kit::component::{
    Disableable as _, Sizable as _, button::ButtonVariants as _, switch::Switch,
};

/// The Appearance page: light/dark mode, the theme grid, corner radius, and the
/// interface language. Every theme here re-skins the whole app — the bespoke
/// surfaces read the same `cx.theme()` tokens as the framework widgets.
pub(super) fn appearance_page(
    view: Entity<SettingsView>,
    opacity: Entity<SliderState>,
    filter: ThemeFilter,
    theme_search: Entity<InputState>,
    language_select: Entity<SelectState<Vec<LanguageOption>>>,
) -> SettingPage {
    // Titled groups so the sidebar shows them as sub-items (gpui-component
    // renders a page's groups as nested sidebar entries once there's more than
    // one and each is titled). Item titles stay distinct from their group title.
    let window_group = cfg!(target_os = "macos").then(|| window_group(view.clone(), opacity));
    let mut theme_group = SettingGroup::new().title(tr!("appearance.theme")).item(
        SettingItem::new(
            tr!("appearance.appearance_mode"),
            SettingField::render(move |_, _, cx| mode_segment(cx)),
        )
        .layout(Axis::Vertical)
        .description(tr!("appearance.appearance_mode_description")),
    );

    theme_group = theme_group.item(
        SettingItem::new(
            tr!("appearance.color_theme"),
            SettingField::render(move |_, _, cx| theme_picker(&view, &theme_search, filter, cx)),
        )
        .layout(Axis::Vertical),
    );

    // Only macOS can wear a chosen icon: Windows embeds one into the executable
    // at build time and Linux installs a fixed one from the package.
    if cfg!(target_os = "macos") {
        theme_group = theme_group.item(
            SettingItem::new(
                tr!("appearance.app_icon"),
                SettingField::render(move |_, _, cx| icon_picker(cx)),
            )
            .layout(Axis::Vertical)
            .description(tr!("appearance.app_icon_description")),
        );
    }

    let theme_group = theme_group
        .item(
            // Compact control → inline on the right of the label (HIG), unlike the
            // wide thumbnail/grid controls which stack below.
            SettingItem::new(
                tr!("appearance.corner_radius"),
                SettingField::render(move |_, _, cx| radius_segment(cx)),
            )
            .description(tr!("appearance.roundness_of_buttons_cards_and_controls")),
        )
        .item(
            SettingItem::new(
                tr!("appearance.interface_scale"),
                SettingField::render(move |_, _, cx| scale_segment(cx)),
            )
            .description(tr!("appearance.scale_text_and_interface_spacing")),
        );

    let language_group = SettingGroup::new().title(tr!("appearance.language")).item(
        SettingItem::new(
            tr!("appearance.interface_language"),
            SettingField::render(move |_, _, _| language_select_field(language_select.clone())),
        )
        .description(tr!("appearance.choose_the_interface_language")),
    );

    let page = SettingPage::new(tr!("appearance.appearance"))
        .icon(IconName::Palette)
        .resettable(false)
        .group(language_group);
    let page = if let Some(window_group) = window_group {
        page.group(window_group)
    } else {
        page
    };
    page.group(theme_group)
}

fn window_group(view: Entity<SettingsView>, opacity: Entity<SliderState>) -> SettingGroup {
    SettingGroup::new()
        .title(tr!("appearance.window"))
        .item(
            SettingItem::new(
                tr!("appearance.acrylic_glass"),
                SettingField::render(|options, _, cx| {
                    acrylic_glass_switch(cx)
                        .disabled(options.is_disabled())
                        .with_size(options.size())
                }),
            )
            .description(tr!("appearance.acrylic_glass_description")),
        )
        .item(glass_opacity_item(view, opacity))
}

fn acrylic_glass_switch(cx: &App) -> Switch {
    Switch::new("acrylic-glass")
        .accessibility_label(tr!("appearance.acrylic_glass"))
        .checked(AppState::try_read(cx).is_some_and(|s| s.app_settings().acrylic_glass))
        .on_change(|enabled, _, cx| {
            AppState::apply(cx, |state| state.commit_acrylic_glass(*enabled));
            theme::apply_from_settings(None, cx);
        })
}

/// Keep preview changes in the theme owner and save once when the thumb rests.
pub(super) fn opacity_slider<V: 'static>(cx: &mut gpui::Context<V>) -> CommitSlider<u8> {
    let initial = AppState::try_read(cx).map_or_else(
        || AppSettings::default().acrylic_opacity(),
        |state| state.app_settings().acrylic_opacity(),
    );
    CommitSlider::previewing(
        SliderRange::new(0, AppSettings::clamp_acrylic_opacity(u8::MAX)),
        initial,
        cx,
        |_, value, cx| {
            if theme::uses_glass(cx) {
                theme::preview_acrylic_opacity(value, cx);
            }
        },
        |_, value, cx| {
            if theme::uses_glass(cx) {
                AppState::apply(cx, |state| state.commit_acrylic_opacity(value));
                theme::apply_from_settings(None, cx);
            }
        },
    )
}

fn glass_opacity_item(view: Entity<SettingsView>, slider: Entity<SliderState>) -> SettingItem {
    SettingItem::render(move |options, _, cx| {
        let view = view.clone();
        opacity_row(&slider, options.is_disabled(), cx, move |_, window, cx| {
            view.update(cx, |view, cx| {
                reset_opacity(&view.acrylic_opacity, window, cx);
                cx.notify();
            });
        })
    })
    .keywords([
        tr!("appearance.glass_opacity"),
        tr!("appearance.glass_opacity_description"),
    ])
}

fn opacity_row(
    slider: &Entity<SliderState>,
    disabled: bool,
    cx: &App,
    on_reset: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> SliderRow {
    let disabled = disabled || !theme::uses_glass(cx);
    let default = AppSettings::default().acrylic_opacity();
    let value = u8::from_slider(slider.read(cx).value().start());
    let changed = value != default;
    let reset_description = tr!("appearance.reset_opacity_description", opacity = default);
    SliderRow::new(
        tr!("appearance.glass_opacity"),
        div()
            .id("glass-opacity")
            .debug_selector(|| "glass-opacity-track".into())
            .w_full()
            .flex()
            .role(gpui::Role::Group)
            .aria_label(tr!("appearance.glass_opacity"))
            .child(
                Slider::new(slider)
                    .horizontal()
                    .disabled(disabled)
                    .when(disabled, |slider| slider.bg(cx.theme().muted_foreground)),
            ),
    )
    .value(format!("{value}%"), ["100%"])
    .value_action(
        Button::new("glass-opacity-reset")
            .debug_selector(|| "glass-opacity-reset".into())
            .ghost()
            .xsmall()
            .label(tr!("appearance.reset_opacity"))
            .tooltip(reset_description)
            .disabled(disabled || !changed)
            .when(!changed, Styled::invisible)
            .on_click(on_reset),
    )
    .description(tr!("appearance.glass_opacity_description"))
    .disabled(disabled)
    .compact()
}

fn reset_opacity(opacity: &CommitSlider<u8>, window: &mut Window, cx: &mut App) {
    if !theme::uses_glass(cx) {
        return;
    }
    AppState::apply(cx, |state| {
        state.commit_acrylic_opacity(AppSettings::default().acrylic_opacity())
    });
    // A failed write keeps the accepted setting, including the thumb and material.
    if let Some(state) = AppState::try_read(cx) {
        let accepted = state.app_settings().acrylic_opacity();
        opacity.seat(accepted, window, cx);
    }
    theme::apply_from_settings(None, cx);
}

/// The stored light/dark preference (defaults to following the OS).
fn appearance_of(cx: &App) -> Appearance {
    AppState::try_read(cx).map_or(Appearance::System, |s| s.app_settings().appearance)
}

/// Persist an appearance-mode choice and re-apply the live theme.
fn set_appearance(cx: &mut App, appearance: Appearance) {
    AppState::apply(cx, |state| state.commit_appearance(appearance));
    theme::apply_from_settings(None, cx);
}

/// Persist a corner-radius choice and re-apply the live theme. `None` defers to
/// the active theme's own radius.
fn set_radius(cx: &mut App, radius: Option<u8>) {
    AppState::apply(cx, |state| state.commit_ui_radius(radius));
    theme::apply_from_settings(None, cx);
}

/// Persist an interface-scale choice and repaint every desktop window. Each
/// root applies its own rem size on that repaint, avoiding a re-entrant update
/// of the Settings window currently dispatching this click.
fn set_scale(cx: &mut App, scale: UiScale) {
    AppState::apply(cx, |state| state.commit_ui_scale(scale));
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
            tr!("common.light"),
            ModePreview::Light,
            current == Appearance::Light,
            accent,
            pal,
            Appearance::Light,
        ),
        mode_card(
            "mode-dark",
            tr!("appearance.dark"),
            ModePreview::Dark,
            current == Appearance::Dark,
            accent,
            pal,
            Appearance::Dark,
        ),
        mode_card(
            "mode-system",
            tr!("appearance.follow_system"),
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
        .focus_visible(move |style| style.focus_ring(pal))
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
    let preview = icon.preview();
    ChoiceCard::new(SharedString::from(icon.to_string()), icon_label(icon))
        .selected(selected)
        .gap(px(6.))
        .items_center()
        .cursor_pointer()
        .focus_visible(move |style| style.focus_ring(pal))
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
        .on_click(move |_, _, cx| AppState::apply(cx, |state| state.commit_app_icon(icon)))
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
                .label(tr!("common.sharp"))
                .selected(current == Some(0))
                .selected_segment(current == Some(0), cx),
        )
        .child(
            Button::new("radius-default")
                .label(tr!("common.default"))
                .selected(current.is_none())
                .selected_segment(current.is_none(), cx),
        )
        .child(
            Button::new("radius-round")
                .label(tr!("appearance.round"))
                .selected(current == Some(12))
                .selected_segment(current == Some(12), cx),
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
                .selected_segment(current == scale, cx)
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
                .child(tr!("appearance.no_themes_match_query", query => query))
        })
        .when(!no_matches, |grid| {
            // Columns share the page width, so the grid ends where the filter
            // row above it ends at every interface scale. The Settings window
            // cannot shrink below the width four columns were sized for.
            grid.grid()
                .grid_cols(4)
                .gap_2()
                .children(themes.into_iter().map(|(name, mode, swatch)| {
                    let selected = name == active;
                    theme_card(name, mode, swatch, selected, pal)
                }))
        });

    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    // Translated chip labels vary in width; let the chip row
                    // yield (wrapping onto a second line if it must) so the
                    // fixed-width search input is never pushed out of view.
                    h_flex()
                        .gap_2()
                        .flex_1()
                        .min_w_0()
                        .flex_wrap()
                        .child(filter_chip(
                            view,
                            "filter-all",
                            tr!("common.all"),
                            ThemeFilter::All,
                            filter,
                            pal,
                        ))
                        .child(filter_chip(
                            view,
                            "filter-light",
                            tr!("common.light"),
                            ThemeFilter::Light,
                            filter,
                            pal,
                        ))
                        .child(filter_chip(
                            view,
                            "filter-dark",
                            tr!("appearance.dark"),
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
                ),
        )
        .child(grid)
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
        .focus_visible(move |style| style.focus_ring(pal))
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
                        .min_w_0()
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
                        .child(if dark {
                            tr!("appearance.dark")
                        } else {
                            tr!("common.light")
                        }),
                ),
        )
        .on_click(move |_, _, cx| {
            let chosen = stored.to_string();
            AppState::apply(cx, move |s| {
                let events = s.commit_theme(mode, Some(chosen.clone()));
                // Picking a theme configures the light or dark *slot*. Only pin
                // the mode when the user has already chosen an explicit
                // Light/Dark mode — a "Follow System" preference must survive so
                // configuring (say) the dark slot doesn't force the whole app to
                // dark.
                if s.app_settings().appearance != Appearance::System {
                    return events.and(s.commit_appearance(match mode {
                        ThemeMode::Light => Appearance::Light,
                        ThemeMode::Dark => Appearance::Dark,
                    }));
                }
                events
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
        .selected_border(selected, pal)
        .selected_fill(selected)
        .text_caption()
        .cursor_pointer()
        .map(|this| {
            if selected {
                this.text_color(pal.text_primary)
            } else {
                this.text_color(pal.text_muted)
                    .hover(|h| h.border_color(pal.text_muted))
            }
        })
        .focus_visible(move |h| h.focus_ring(pal))
        .child(label)
        .on_click(move |_, _, cx| {
            view.update(cx, |this, cx| {
                this.theme_filter = value;
                cx.notify();
            });
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::assets::AssetResolver;
    use crate::state::{ConfigPersistence, Sources};
    use gpui::{
        AppContext as _, Context, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, Render,
        TestAppContext, Window,
    };
    use gpui_kit::component::slider::SliderEvent;
    use openlogi_core::config::Config;

    struct GlassSwitch;

    struct GlassOpacity {
        opacity: CommitSlider<u8>,
    }

    impl Render for GlassOpacity {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let saved = AppState::try_read(cx)
                .unwrap()
                .app_settings()
                .acrylic_opacity();
            self.opacity.sync(saved, window, cx);
            let owner = cx.entity();
            div()
                .id("opacity-row")
                .tab_group()
                .w(px(300.))
                .child(opacity_row(
                    self.opacity.slider(),
                    false,
                    cx,
                    move |_, window, cx| {
                        owner.update(cx, |this, cx| {
                            reset_opacity(&this.opacity, window, cx);
                            cx.notify();
                        });
                    },
                ))
        }
    }

    impl Render for GlassSwitch {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().flex().items_start().child(
                div()
                    .debug_selector(|| "glass-switch".into())
                    .child(acrylic_glass_switch(cx)),
            )
        }
    }

    fn install_state(cx: &mut TestAppContext, persistence: ConfigPersistence) {
        install_config(cx, persistence, Config::ephemeral());
    }

    fn install_config(cx: &mut TestAppContext, persistence: ConfigPersistence, config: Config) {
        cx.update(|cx| {
            gpui_kit::component::init(cx);
            theme::register_builtin_themes(cx);
            let (commands, _) = tokio::sync::mpsc::unbounded_channel();
            let state = cx.new(|_| {
                AppState::new(Sources {
                    persistence,
                    ..Sources::in_memory(config, &AssetResolver::new(), commands)
                })
            });
            AppState::set_global(state, cx);
            theme::apply_from_settings(None, cx);
        });
    }

    fn install_custom_opacity(cx: &mut TestAppContext, persistence: ConfigPersistence) {
        let mut config = Config::ephemeral();
        config.app_settings.acrylic_glass = true;
        config.app_settings.acrylic_opacity = 40;
        install_config(cx, persistence, config);
    }

    fn activate_key(cx: &mut gpui::VisualTestContext, key: &'static str) {
        let keystroke = Keystroke::parse(key).unwrap();
        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(KeyUpEvent { keystroke });
    }

    fn assert_opacity(view: &Entity<GlassOpacity>, cx: &mut gpui::VisualTestContext, expected: u8) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        view.read_with(cx, |view, cx| {
            assert_eq!(
                AppState::try_read(cx)
                    .unwrap()
                    .app_settings()
                    .acrylic_opacity(),
                expected
            );
            assert_eq!(view.opacity.value(cx), expected);
            assert_eq!(view.opacity.seated(), expected);
            if theme::uses_glass(cx) {
                assert!(
                    (cx.theme().tokens.background.a - f32::from(expected) / 100.).abs()
                        < f32::EPSILON
                );
            }
        });
    }

    #[gpui::test]
    fn glass_opacity_reset_restores_the_default_without_moving_the_track(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        install_custom_opacity(cx, ConfigPersistence::MemoryOnly);
        let (view, cx) = cx.add_window_view(|_, cx| GlassOpacity {
            opacity: opacity_slider(cx),
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let track = cx.debug_bounds("glass-opacity-track").unwrap();
        let value = cx.debug_bounds("slider-row-value").unwrap();
        let reset = cx.debug_bounds("glass-opacity-reset").unwrap();
        let supported = cx.update(|_, cx| theme::uses_glass(cx));

        // Reset must also cancel a live preview, including CommitSlider's drag state.
        let slider = view.read_with(cx, |view, _| view.opacity.slider().clone());
        if supported {
            cx.update(|window, cx| {
                slider.update(cx, |slider, cx| {
                    slider.set_value(55., window, cx);
                    cx.emit(SliderEvent::Change(55.0.into()));
                });
            });
        }
        cx.run_until_parked();
        cx.simulate_click(reset.center(), Modifiers::default());
        cx.run_until_parked();
        let expected = if supported {
            AppSettings::default().acrylic_opacity()
        } else {
            40
        };
        assert_opacity(&view, cx, expected);
        assert_eq!(cx.debug_bounds("glass-opacity-track").unwrap(), track);
        assert_eq!(cx.debug_bounds("slider-row-value").unwrap(), value);
        cx.update(|window, cx| {
            window.blur(cx);
            window.focus_next(cx);
            assert!(
                window.focused(cx).is_none(),
                "hidden or disabled Reset is not a tab stop"
            );
        });
    }

    #[gpui::test]
    fn glass_opacity_reset_supports_keyboard_and_follows_external_changes(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        install_custom_opacity(cx, ConfigPersistence::MemoryOnly);
        let (view, cx) = cx.add_window_view(|_, cx| GlassOpacity {
            opacity: opacity_slider(cx),
        });
        let supported = cx.update(|_, cx| theme::uses_glass(cx));
        for key in ["enter", "space"] {
            cx.update(|_, cx| {
                AppState::apply(cx, |state| state.commit_acrylic_opacity(40));
            });
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.update(Window::focus_next);
            activate_key(cx, key);
            cx.run_until_parked();
            assert_opacity(
                &view,
                cx,
                if supported {
                    AppSettings::default().acrylic_opacity()
                } else {
                    40
                },
            );
        }
    }

    #[gpui::test]
    fn glass_opacity_reset_preserves_the_accepted_value_after_a_failed_write(
        cx: &mut TestAppContext,
    ) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        install_custom_opacity(cx, ConfigPersistence::ReadOnly("invalid config".into()));
        let (view, cx) = cx.add_window_view(|_, cx| GlassOpacity {
            opacity: opacity_slider(cx),
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let reset = cx.debug_bounds("glass-opacity-reset").unwrap();
        cx.simulate_click(reset.center(), Modifiers::default());
        cx.run_until_parked();
        assert_opacity(&view, cx, 40);
        cx.update(|_, cx| {
            assert_eq!(
                AppState::try_read(cx).unwrap().config_issue(),
                Some("invalid config")
            );
        });
    }

    #[gpui::test]
    fn glass_opacity_reset_is_inert_when_glass_is_off(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        install_custom_opacity(cx, ConfigPersistence::MemoryOnly);
        cx.update(|cx| {
            AppState::apply(cx, |state| state.commit_acrylic_glass(false));
            theme::apply_from_settings(None, cx);
        });
        let (view, cx) = cx.add_window_view(|_, cx| GlassOpacity {
            opacity: opacity_slider(cx),
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let reset = cx.debug_bounds("glass-opacity-reset").unwrap();
        cx.simulate_click(reset.center(), Modifiers::default());
        cx.update(Window::focus_next);
        activate_key(cx, "enter");
        activate_key(cx, "space");
        assert_opacity(&view, cx, 40);
        cx.update(|window, cx| assert!(window.focused(cx).is_none()));
    }

    #[gpui::test]
    fn glass_switch_applies_pointer_and_keyboard_changes(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        install_state(cx, ConfigPersistence::MemoryOnly);
        let (_, cx) = cx.add_window_view(|_, _| GlassSwitch);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let bounds = cx.debug_bounds("glass-switch").expect("glass switch");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.update(|_, cx| {
            assert!(
                AppState::try_read(cx)
                    .expect("state")
                    .app_settings()
                    .acrylic_glass
            );
            assert_eq!(
                cx.theme().tokens.background.a < 1.,
                crate::platform::accessibility::allows_glass()
            );
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let keystroke = Keystroke::parse("space").expect("space key");
        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(KeyUpEvent { keystroke });
        cx.update(|_, cx| {
            assert!(
                !AppState::try_read(cx)
                    .expect("state")
                    .app_settings()
                    .acrylic_glass
            );
            assert_eq!(cx.theme().tokens.background.color, cx.theme().background);
        });
    }

    #[gpui::test]
    fn glass_switch_keeps_the_saved_value_when_config_is_read_only(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        install_state(cx, ConfigPersistence::ReadOnly("invalid config".into()));
        let (_, cx) = cx.add_window_view(|_, _| GlassSwitch);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let bounds = cx.debug_bounds("glass-switch").expect("glass switch");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.update(|_, cx| {
            let state = AppState::try_read(cx).expect("state");
            assert!(!state.app_settings().acrylic_glass);
            assert_eq!(state.config_issue(), Some("invalid config"));
            assert_eq!(cx.theme().tokens.background.color, cx.theme().background);
        });
    }

    #[gpui::test]
    fn glass_opacity_pointer_changes_the_material_only_when_enabled(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        install_state(cx, ConfigPersistence::MemoryOnly);
        let (_, cx) = cx.add_window_view(|_, cx| GlassOpacity {
            opacity: opacity_slider(cx),
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let bounds = cx
            .debug_bounds("glass-opacity-track")
            .expect("opacity track");
        let target = gpui::point(bounds.left() + bounds.size.width * 0.25, bounds.center().y);
        cx.simulate_click(target, Modifiers::default());
        cx.update(|_, cx| {
            assert_eq!(
                AppState::try_read(cx)
                    .unwrap()
                    .app_settings()
                    .acrylic_opacity(),
                AppSettings::default().acrylic_opacity()
            );
            AppState::apply(cx, |state| state.commit_acrylic_glass(true));
            theme::apply_from_settings(None, cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_click(target, Modifiers::default());
        cx.run_until_parked();
        cx.update(|_, cx| {
            let saved = AppState::try_read(cx)
                .unwrap()
                .app_settings()
                .acrylic_opacity();
            if theme::uses_glass(cx) {
                assert!(saved.abs_diff(25) <= 1, "saved opacity: {saved}");
                let background: gpui::Background = cx.theme().tokens.background.into();
                assert_eq!(background.as_solid().unwrap().a, f32::from(saved) / 100.);
            } else {
                assert_eq!(saved, AppSettings::default().acrylic_opacity());
            }
        });
    }

    #[gpui::test]
    fn glass_opacity_previews_live_and_commits_only_on_release(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        install_state(cx, ConfigPersistence::MemoryOnly);
        let (view, cx) = cx.add_window_view(|_, cx| GlassOpacity {
            opacity: opacity_slider(cx),
        });
        let slider = view.read_with(cx, |view, _| view.opacity.slider().clone());
        slider.update(cx, |_, cx| cx.emit(SliderEvent::Change(20.0.into())));
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert_eq!(
                AppState::try_read(cx)
                    .unwrap()
                    .app_settings()
                    .acrylic_opacity(),
                AppSettings::default().acrylic_opacity()
            );
            assert!(!theme::uses_glass(cx));
            AppState::apply(cx, |state| state.commit_acrylic_glass(true));
            theme::apply_from_settings(None, cx);
        });
        let supported = cx.update(|_, cx| theme::uses_glass(cx));
        slider.update(cx, |_, cx| cx.emit(SliderEvent::Change(55.0.into())));
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert_eq!(
                AppState::try_read(cx)
                    .unwrap()
                    .app_settings()
                    .acrylic_opacity(),
                AppSettings::default().acrylic_opacity()
            );
            if supported {
                assert!((cx.theme().tokens.background.a - 0.55).abs() < f32::EPSILON);
            } else {
                assert_eq!(cx.theme().tokens.background.color, cx.theme().background);
            }
        });
        slider.update(cx, |_, cx| cx.emit(SliderEvent::Release(55.0.into())));
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert_eq!(
                AppState::try_read(cx)
                    .unwrap()
                    .app_settings()
                    .acrylic_opacity(),
                if supported {
                    55
                } else {
                    AppSettings::default().acrylic_opacity()
                }
            );
            if supported {
                assert!((cx.theme().tokens.background.a - 0.55).abs() < f32::EPSILON);
            }
            AppState::apply(cx, |state| state.commit_acrylic_glass(false));
            theme::apply_from_settings(None, cx);
        });
        slider.update(cx, |_, cx| cx.emit(SliderEvent::Release(25.0.into())));
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert_eq!(
                AppState::try_read(cx)
                    .unwrap()
                    .app_settings()
                    .acrylic_opacity(),
                if supported {
                    55
                } else {
                    AppSettings::default().acrylic_opacity()
                }
            );
            assert_eq!(cx.theme().tokens.background.color, cx.theme().background);
        });
    }
}
