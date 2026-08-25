//! Debug-only gallery for reviewing shared controls without app runtime state.

use gpui::{
    App, AppContext as _, Bounds, Context, InteractiveElement, IntoElement, ParentElement, Render,
    Role, SharedString, Size, Styled, Window, WindowBounds, WindowOptions, div, px, rems,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Root, Selectable as _, Sizable as _, Theme,
    ThemeMode, ThemeRegistry,
    button::{Button, ButtonVariants as _},
    h_flex,
    scroll::ScrollableElement as _,
    v_flex,
};
use openlogi_core::brand::APP_ID;
use openlogi_core::config::UiScale;
use openlogi_core::device::{BatteryInfo, BatteryLevel, BatteryStatus};

use super::battery::BatteryIndicator;
use super::carousel::Carousel;
use super::choice_card::ChoiceCard;
use super::components::{MenuRow, PanelCard, PresetChip, ProfileTab, Toggle};
use super::theme::{self, ContentWidth, OPENLOGI_DARK, OPENLOGI_LIGHT, Palette, Typography as _};

const TITLE: &str = "OpenLogi Component Gallery";

/// Run the isolated development gallery application.
pub(crate) fn run() {
    let app = gpui_platform::application().with_assets(crate::app_assets::AppAssets);
    app.run(|cx| {
        gpui_component::init(cx);
        theme::register_builtin_themes(cx);
        install_openlogi_themes(cx);

        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, Size::new(px(1100.), px(800.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(Size::new(px(760.), px(600.))),
            app_id: Some(APP_ID.into()),
            titlebar: Some(crate::windows::titlebar_options(TITLE)),
            ..WindowOptions::default()
        };
        let opened = cx.open_window(options, |window, cx| {
            theme::apply_scale(window, UiScale::Normal);
            let view = cx.new(ComponentGallery::new);
            cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
        });

        match opened {
            Ok(handle) => {
                let _ = handle.update(cx, |_, window, _| window.activate_window());
                cx.activate(true);
            }
            Err(error) => {
                tracing::error!(%error, "could not open component gallery");
                cx.quit();
            }
        }
    });
}

fn install_openlogi_themes(cx: &mut App) {
    let (light, dark) = {
        let registry = ThemeRegistry::global(cx);
        (
            registry.themes().get(OPENLOGI_LIGHT).cloned(),
            registry.themes().get(OPENLOGI_DARK).cloned(),
        )
    };
    let active = Theme::global_mut(cx);
    if let Some(light) = light {
        active.light_theme = light;
    }
    if let Some(dark) = dark {
        active.dark_theme = dark;
    }
    Theme::change(ThemeMode::Light, None, cx);
}

struct ComponentGallery {
    mode: ThemeMode,
    scale: UiScale,
    choice_selected: usize,
    toggle_selected: bool,
    menu_selected: usize,
    profile_selected: usize,
    preset_selected: bool,
    carousel_selected: usize,
}

impl ComponentGallery {
    fn new(_: &mut Context<Self>) -> Self {
        Self {
            mode: ThemeMode::Light,
            scale: UiScale::Normal,
            choice_selected: 0,
            toggle_selected: true,
            menu_selected: 0,
            profile_selected: 1,
            preset_selected: true,
            carousel_selected: 1,
        }
    }

    fn toolbar(&self, pal: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.mode;
        let scale = self.scale;
        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .flex_wrap()
            .gap_4()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(pal.border)
            .child(
                v_flex()
                    .gap_0p5()
                    .child(div().text_heading().child(TITLE))
                    .child(
                        div()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child("Debug-only · no config, IPC, or hardware"),
                    ),
            )
            .child(
                h_flex()
                    .items_center()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("gallery-mode-light")
                            .outline()
                            .label("Light")
                            .selected(mode == ThemeMode::Light)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.mode = ThemeMode::Light;
                                Theme::change(ThemeMode::Light, Some(window), cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("gallery-mode-dark")
                            .outline()
                            .label("Dark")
                            .selected(mode == ThemeMode::Dark)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.mode = ThemeMode::Dark;
                                Theme::change(ThemeMode::Dark, Some(window), cx);
                                cx.notify();
                            })),
                    )
                    .children(UiScale::ALL.map(|candidate| {
                        Button::new(("gallery-scale", u32::from(candidate.percent())))
                            .outline()
                            .label(format!("{}%", candidate.percent()))
                            .selected(scale == candidate)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.scale = candidate;
                                theme::apply_scale(window, candidate);
                                cx.notify();
                            }))
                    })),
            )
    }

    fn controls(&self, pal: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .items_start()
            .flex_wrap()
            .gap_4()
            .child(self.choice_panel(pal, cx))
            .child(self.toggle_panel(pal, cx))
            .child(self.menu_panel(pal, cx))
            .child(self.profile_panel(pal, cx))
            .child(self.preset_panel(pal, cx))
            .child(Self::battery_panel(pal))
    }

    fn choice_panel(&self, pal: Palette, cx: &mut Context<Self>) -> gpui::Div {
        gallery_panel(
            "ChoiceCard",
            IconName::LayoutDashboard,
            v_flex()
                .gap_2()
                .child(
                    choice_card(
                        "gallery-choice-active",
                        "Primary choice",
                        self.choice_selected == 0,
                        pal,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.choice_selected = 0;
                        cx.notify();
                    })),
                )
                .child(
                    choice_card(
                        "gallery-choice-secondary",
                        "Secondary choice",
                        self.choice_selected == 1,
                        pal,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.choice_selected = 1;
                        cx.notify();
                    })),
                )
                .child(
                    choice_card("gallery-choice-disabled", "Disabled choice", false, pal)
                        .disabled(true),
                ),
            pal,
        )
    }

    fn toggle_panel(&self, pal: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let label = if self.toggle_selected {
            "Enabled"
        } else {
            "Disabled"
        };
        gallery_panel(
            "Toggle",
            IconName::Settings,
            v_flex()
                .gap_3()
                .child(
                    Toggle::new("gallery-toggle")
                        .selected(self.toggle_selected)
                        .label(Some(SharedString::from(label)))
                        .on_change(cx.listener(|this, selected: &bool, _, cx| {
                            this.toggle_selected = *selected;
                            cx.notify();
                        })),
                )
                .child(
                    Toggle::new("gallery-toggle-disabled")
                        .selected(true)
                        .label(Some(SharedString::from("Unavailable")))
                        .disabled(true),
                ),
            pal,
        )
    }

    fn menu_panel(&self, pal: Palette, cx: &mut Context<Self>) -> gpui::Div {
        gallery_panel(
            "MenuRow",
            IconName::Menu,
            v_flex()
                .gap_1()
                .child(
                    MenuRow::new("gallery-menu-primary")
                        .role(Role::MenuItem)
                        .selected(self.menu_selected == 0)
                        .child("Primary action")
                        .child(Icon::new(IconName::ChevronRight).size_3())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.menu_selected = 0;
                            cx.notify();
                        })),
                )
                .child(
                    MenuRow::new("gallery-menu-secondary")
                        .role(Role::MenuItem)
                        .selected(self.menu_selected == 1)
                        .child("Secondary action")
                        .child(Icon::new(IconName::ChevronRight).size_3())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.menu_selected = 1;
                            cx.notify();
                        })),
                ),
            pal,
        )
    }

    fn profile_panel(&self, pal: Palette, cx: &mut Context<Self>) -> gpui::Div {
        gallery_panel(
            "ProfileTab",
            IconName::Eye,
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(
                    ProfileTab::new("gallery-profile-default", "Default")
                        .selected(self.profile_selected == 0)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.profile_selected = 0;
                            cx.notify();
                        })),
                )
                .child(
                    ProfileTab::new("gallery-profile-custom", "Custom")
                        .selected(self.profile_selected == 1)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.profile_selected = 1;
                            cx.notify();
                        }))
                        .on_delete("gallery-profile-delete", |_, _, _| {}),
                ),
            pal,
        )
    }

    fn preset_panel(&self, pal: Palette, cx: &mut Context<Self>) -> gpui::Div {
        gallery_panel(
            "PresetChip",
            IconName::Cpu,
            PresetChip::new("gallery-preset")
                .selected(self.preset_selected)
                .child(
                    Button::new("gallery-preset-apply")
                        .ghost()
                        .xsmall()
                        .label("800 DPI")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.preset_selected = !this.preset_selected;
                            cx.notify();
                        })),
                )
                .child(
                    BaseButton::new("gallery-preset-remove")
                        .accessibility_label("Remove preset")
                        .px_1()
                        .text_color(pal.text_muted)
                        .child("×"),
                ),
            pal,
        )
    }

    fn battery_panel(pal: Palette) -> gpui::Div {
        let battery = |percentage, level, status| BatteryInfo {
            percentage,
            level,
            status,
        };
        gallery_panel(
            "BatteryIndicator",
            IconName::Battery,
            v_flex()
                .gap_3()
                .child(BatteryIndicator::summary(&battery(
                    78,
                    BatteryLevel::Good,
                    BatteryStatus::Discharging,
                )))
                .child(BatteryIndicator::status(
                    &battery(100, BatteryLevel::Full, BatteryStatus::Full),
                    true,
                ))
                .child(BatteryIndicator::status(
                    &battery(42, BatteryLevel::Good, BatteryStatus::Charging),
                    true,
                ))
                .child(BatteryIndicator::status(
                    &battery(0, BatteryLevel::Unknown, BatteryStatus::Charging),
                    true,
                ))
                .child(BatteryIndicator::status(
                    &battery(14, BatteryLevel::Low, BatteryStatus::Discharging),
                    false,
                ))
                .child(BatteryIndicator::summary(&battery(
                    35,
                    BatteryLevel::Unknown,
                    BatteryStatus::Error,
                ))),
            pal,
        )
    }

    fn carousel(&self, pal: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.carousel_selected;
        let view = cx.entity();
        div().w_full().child(PanelCard::new(
            "Carousel",
            Icon::new(IconName::GalleryVerticalEnd).text_color(pal.text_muted),
            div().w_full().h(rems(13.)).child(
                Carousel::new("gallery-carousel", rems(12.))
                    .len(4)
                    .selected(selected)
                    .render_item(move |index, selected, _, cx| {
                        let pal = theme::palette(cx);
                        let view = view.clone();
                        ChoiceCard::new(
                            ("gallery-carousel-card", index),
                            format!("Card {}", index + 1),
                        )
                        .selected(selected)
                        .size_full()
                        .p_3()
                        .rounded(pal.card_radius)
                        .border_1()
                        .border_color(if selected {
                            theme::accent()
                        } else {
                            pal.border
                        })
                        .bg(pal.control)
                        .items_center()
                        .justify_center()
                        .text_body()
                        .child(format!("Card {}", index + 1))
                        .on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.carousel_selected = index;
                                cx.notify();
                            });
                        })
                        .into_any_element()
                    })
                    .on_select(cx.listener(|this, index: &usize, _, cx| {
                        this.carousel_selected = *index;
                        cx.notify();
                    })),
            ),
        ))
    }
}

impl Render for ComponentGallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        theme::apply_scale(window, self.scale);
        let pal = theme::palette(cx);
        v_flex()
            .size_full()
            .bg(pal.page)
            .text_color(pal.text_primary)
            .child(crate::windows::aux_title_bar(TITLE, cx))
            .child(self.toolbar(pal, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .items_center()
                    .p_5()
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(ContentWidth::DoubleExtraLarge.rems())
                            .gap_4()
                            .child(self.controls(pal, cx))
                            .child(self.carousel(pal, cx)),
                    ),
            )
    }
}

fn gallery_panel(
    title: impl Into<SharedString>,
    icon: IconName,
    content: impl IntoElement,
    pal: Palette,
) -> gpui::Div {
    div()
        .w(rems(20.))
        .min_w(rems(18.))
        .flex_1()
        .child(PanelCard::new(
            title,
            Icon::new(icon).text_color(pal.text_muted),
            content,
        ))
}

fn choice_card(id: &'static str, label: &'static str, selected: bool, pal: Palette) -> ChoiceCard {
    ChoiceCard::new(id, label)
        .selected(selected)
        .w_full()
        .p_2()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(if selected {
            theme::accent()
        } else {
            pal.border
        })
        .bg(pal.control)
        .hover(move |style| style.bg(pal.control_hover))
        .focus_visible(move |style| style.border_color(theme::accent()))
        .child(div().text_body().child(label))
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn gallery_renders_without_application_state(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(theme::register_builtin_themes);
        cx.update(install_openlogi_themes);
        let (view, cx) = cx.add_window_view(|_, cx| ComponentGallery::new(cx));

        cx.update(|window, cx| window.draw(cx).clear(cx));
        view.update(cx, |gallery, cx| {
            gallery.mode = ThemeMode::Dark;
            gallery.scale = UiScale::ExtraLarge;
            cx.notify();
        });
        cx.update(|window, cx| {
            Theme::change(ThemeMode::Dark, Some(window), cx);
            window.draw(cx).clear(cx);
        });
    }
}
