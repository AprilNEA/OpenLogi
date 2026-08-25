//! General settings page.

use super::{
    App, AppState, Button, Entity, FluentBuilder, IconName, ParentElement, SettingField, SettingGroup,
    SettingItem, SettingPage, Slider, SliderState, StateEvent, Styled, ThumbwheelSensitivity,
    VerticalScrollSensitivity, div, h_flex, px, theme, v_flex,
};
use gpui::{
    BorderStyle, Bounds, PathBuilder, canvas, point, quad, rgb, size,
};
use crate::ui::theme::{Palette, Typography as _};

pub(super) fn general_page(
    vertical_scroll_sensitivity_slider: Entity<SliderState>,
    vertical_accel_strength_slider: Entity<SliderState>,
    vertical_accel_max_gain_slider: Entity<SliderState>,
    horizontal_accel_strength_slider: Entity<SliderState>,
    horizontal_accel_max_gain_slider: Entity<SliderState>,
    thumbwheel_sensitivity_slider: Entity<SliderState>,
) -> SettingPage {
    let v_strength_slider_1 = vertical_accel_strength_slider.clone();
    let v_max_gain_slider_1 = vertical_accel_max_gain_slider.clone();
    let v_strength_slider_2 = vertical_accel_strength_slider.clone();
    let v_max_gain_slider_2 = vertical_accel_max_gain_slider.clone();

    let h_strength_slider_1 = horizontal_accel_strength_slider.clone();
    let h_max_gain_slider_1 = horizontal_accel_max_gain_slider.clone();
    let h_strength_slider_2 = horizontal_accel_strength_slider.clone();
    let h_max_gain_slider_2 = horizontal_accel_max_gain_slider.clone();

    let group = SettingGroup::new()
        .item(
            SettingItem::new(
                tr!("Smooth scrolling"),
                SettingField::switch(
                    |cx| {
                        AppState::try_read(cx).is_some_and(|s| s.app_settings().smooth_scroll)
                    },
                    |enabled, cx| {
                        AppState::update(cx, move |state, cx| {
                            state.set_smooth_scroll(enabled);
                            cx.emit(StateEvent::SettingsChanged);
                        });
                    },
                ),
            )
            .description(tr!(
                "Animate traditional mouse-wheel input while leaving trackpad scrolling unchanged."
            )),
        )
        .item(
            SettingItem::new(
                tr!("Vertical Scroll Sensitivity"),
                SettingField::render(move |_, _, cx| {
                    vertical_scroll_sensitivity_field(&vertical_scroll_sensitivity_slider, cx)
                }),
            )
            .description(tr!(
                "Scales all traditional vertical wheel scrolling regardless of speed."
            )),
        )
        .item(
            SettingItem::new(
                tr!("Vertical Scroll Acceleration"),
                SettingField::switch(
                    |cx| {
                        AppState::try_read(cx)
                            .is_some_and(|s| s.app_settings().vertical_acceleration_enabled)
                    },
                    |enabled, cx| {
                        AppState::update(cx, move |state, cx| {
                            state.set_vertical_acceleration_enabled(enabled);
                            cx.emit(StateEvent::SettingsChanged);
                        });
                    },
                ),
            )
            .description(tr!(
                "Progressively increases vertical scroll distance during fast wheel movement."
            )),
        )
        .item(
            SettingItem::new(
                tr!("Vertical Acceleration Controls"),
                SettingField::render(move |_, window, cx| {
                    let enabled = AppState::try_read(cx)
                        .is_some_and(|s| s.app_settings().vertical_acceleration_enabled);
                    let v_strength_slider = v_strength_slider_1.clone();
                    let v_max_gain_slider = v_max_gain_slider_1.clone();

                    acceleration_controls(
                        "vertical",
                        enabled,
                        &v_strength_slider_1,
                        &v_max_gain_slider_1,
                        1.0,
                        2.5,
                        move |window, cx| {
                            AppState::update(cx, |state, cx| {
                                state.reset_vertical_acceleration_preferences();
                                cx.emit(StateEvent::SettingsChanged);
                            });
                            v_strength_slider.update(cx, |s, cx| {
                                s.set_value(100.0, window, cx);
                            });
                            v_max_gain_slider.update(cx, |s, cx| {
                                s.set_value(2.5, window, cx);
                            });
                        },
                        window,
                        cx,
                    )
                }),
            )
            .description(tr!(
                "Acceleration Strength adjusts how rapidly scrolling builds up. Maximum Speed caps the upper bound multiplier."
            )),
        )
        .item(
            SettingItem::new(
                tr!("Horizontal Scroll Acceleration"),
                SettingField::switch(
                    |cx| {
                        AppState::try_read(cx)
                            .is_some_and(|s| s.app_settings().horizontal_acceleration_enabled)
                    },
                    |enabled, cx| {
                        AppState::update(cx, move |state, cx| {
                            state.set_horizontal_acceleration_enabled(enabled);
                            cx.emit(StateEvent::SettingsChanged);
                        });
                    },
                ),
            )
            .description(tr!(
                "Progressively increases horizontal scroll distance during fast wheel movement."
            )),
        )
        .item(
            SettingItem::new(
                tr!("Horizontal Acceleration Controls"),
                SettingField::render(move |_, window, cx| {
                    let enabled = AppState::try_read(cx)
                        .is_some_and(|s| s.app_settings().horizontal_acceleration_enabled);
                    let h_strength_slider = h_strength_slider_1.clone();
                    let h_max_gain_slider = h_max_gain_slider_1.clone();

                    acceleration_controls(
                        "horizontal",
                        enabled,
                        &h_strength_slider_1,
                        &h_max_gain_slider_1,
                        1.0,
                        2.0,
                        move |window, cx| {
                            AppState::update(cx, |state, cx| {
                                state.reset_horizontal_acceleration_preferences();
                                cx.emit(StateEvent::SettingsChanged);
                            });
                            h_strength_slider.update(cx, |s, cx| {
                                s.set_value(100.0, window, cx);
                            });
                            h_max_gain_slider.update(cx, |s, cx| {
                                s.set_value(2.0, window, cx);
                            });
                        },
                        window,
                        cx,
                    )
                }),
            )
            .description(tr!(
                "Acceleration Strength adjusts how rapidly horizontal scrolling builds up. Maximum Speed caps the upper bound multiplier."
            )),
        )
        .item(
            SettingItem::new(
                tr!("Thumb Wheel Sensitivity"),
                SettingField::render(move |_, _, cx| {
                    thumbwheel_sensitivity_field(&thumbwheel_sensitivity_slider, cx)
                }),
            )
            .description(tr!(
                "Scales the thumb wheel's horizontal scroll speed and how readily custom wheel actions trigger."
            )),
        )
        .item(
            SettingItem::new(
                tr!("Launch at login"),
                SettingField::switch(
                    |cx| {
                        AppState::try_read(cx)
                            .is_some_and(|s| s.app_settings().launch_at_login)
                    },
                    |enabled, cx| {
                        AppState::update(cx, move |state, cx| {
                            state.set_launch_at_login(enabled);
                            cx.emit(StateEvent::SettingsChanged);
                        });
                    },
                ),
            )
            .description(if cfg!(target_os = "macos") {
                tr!("Automatically start OpenLogi when you log in to macOS.")
            } else {
                tr!("Automatically start OpenLogi when you log in.")
            }),
        );

    // The same `show_in_menu_bar` setting drives the macOS status item and
    // the Windows notification-area icon (the agent honors it on both; next
    // launch, see tray.rs / tray_windows.rs) — so both platforms get the
    // switch, with platform-fitting wording. Linux has no tray; no switch.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let group = group.item(
        SettingItem::new(
            if cfg!(target_os = "macos") {
                tr!("Show in menu bar")
            } else {
                tr!("Show in the notification area")
            },
            SettingField::switch(
                |cx| {
                    AppState::try_read(cx)
                        .is_some_and(|s| s.app_settings().show_in_menu_bar)
                },
                |enabled, cx| {
                    AppState::update(cx, move |state, cx| {
                        state.set_show_in_menu_bar(enabled);
                        cx.emit(StateEvent::SettingsChanged);
                    });
                },
            ),
        )
        .description(if cfg!(target_os = "macos") {
            tr!("Keep OpenLogi's icon in the menu bar. When off, it stays in the Dock instead.")
        } else {
            tr!(
                "Keep OpenLogi's icon in the taskbar notification area. Takes effect the next time the background agent starts."
            )
        }),
    );

    SettingPage::new(tr!("General"))
        .icon(IconName::Settings)
        .resettable(false)
        .group(group)
}

fn thumbwheel_sensitivity_field(slider: &Entity<SliderState>, cx: &mut App) -> gpui::Div {
    let value = ThumbwheelSensitivity::from_rounded(slider.read(cx).value().start());
    sensitivity_field(
        slider,
        value.to_string(),
        value == ThumbwheelSensitivity::DEFAULT,
        cx,
    )
}

fn vertical_scroll_sensitivity_field(slider: &Entity<SliderState>, cx: &mut App) -> gpui::Div {
    let value = VerticalScrollSensitivity::from_rounded(slider.read(cx).value().start());
    sensitivity_field(
        slider,
        value.to_string(),
        value == VerticalScrollSensitivity::DEFAULT,
        cx,
    )
}

fn sensitivity_field(
    slider: &Entity<SliderState>,
    value: String,
    is_default: bool,
    cx: &mut App,
) -> gpui::Div {
    let pal = theme::palette(cx);
    v_flex()
        .flex_shrink_0()
        .gap_1()
        .child(
            h_flex()
                .items_center()
                .gap_3()
                .child(div().w(px(180.)).child(Slider::new(slider)))
                .child(
                    div()
                        .w(px(72.))
                        .text_body()
                        .text_color(pal.text_muted)
                        .child(value),
                ),
        )
        .when(is_default, |this| {
            this.child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .whitespace_nowrap()
                    .child(format!("({})", rust_i18n::t!("Default"))),
            )
        })
}

fn acceleration_controls(
    axis_name: &'static str,
    enabled: bool,
    factor_slider: &Entity<SliderState>,
    max_gain_slider: &Entity<SliderState>,
    default_factor: f64,
    default_max_gain: f64,
    on_reset: impl Fn(&mut gpui::Window, &mut App) + 'static + Copy,
    _window: &mut gpui::Window,
    cx: &mut App,
) -> gpui::Div {
    let pal = theme::palette(cx);

    let current_factor = (f64::from(factor_slider.read(cx).value().start()) / 100.0).clamp(0.2, 2.0);
    let current_max_gain = f64::from(max_gain_slider.read(cx).value().start()).clamp(1.0, 3.0);

    let is_default = (current_factor - default_factor).abs() < 0.01
        && (current_max_gain - default_max_gain).abs() < 0.01;

    let graph = curve_graph_view(enabled, current_factor, current_max_gain, pal);

    v_flex()
        .gap_3()
        .pt_1()
        .child(
            h_flex()
                .gap_4()
                .items_start()
                .child(graph)
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            v_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .text_caption()
                                        .text_color(if enabled { pal.text_primary } else { pal.text_muted })
                                        .child(rust_i18n::t!("Acceleration Strength")),
                                )
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_3()
                                        .child(div().w(px(140.)).child(Slider::new(factor_slider)))
                                        .child(
                                            div()
                                                .w(px(50.))
                                                .text_caption()
                                                .text_color(pal.text_muted)
                                                .child(format!("{:.0}%", current_factor * 100.0)),
                                        ),
                                ),
                        )
                        .child(
                            v_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .text_caption()
                                        .text_color(if enabled { pal.text_primary } else { pal.text_muted })
                                        .child(rust_i18n::t!("Maximum Speed")),
                                )
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_3()
                                        .child(div().w(px(140.)).child(Slider::new(max_gain_slider)))
                                        .child(
                                            div()
                                                .w(px(50.))
                                                .text_caption()
                                                .text_color(pal.text_muted)
                                                .child(format!("{:.1}x", current_max_gain)),
                                        ),
                                ),
                        )
                        .child(
                            h_flex().pt_1().child(
                                Button::new(format!("reset_{axis_name}"))
                                    .label(rust_i18n::t!("Reset to default"))
                                    .disabled(is_default || !enabled)
                                    .on_click(move |_, window, cx| on_reset(window, cx)),
                            ),
                        ),
                ),
        )
}

fn curve_graph_view(
    enabled: bool,
    acceleration_factor: f64,
    max_gain: f64,
    pal: Palette,
) -> gpui::Div {
    let width = 240.0;
    let height = 110.0;
    let padding_left = 28.0;
    let padding_bottom = 18.0;
    let padding_top = 8.0;
    let padding_right = 8.0;

    let graph_w = width - padding_left - padding_right;
    let graph_h = height - padding_top - padding_bottom;

    let min_y = 1.0;
    let max_y = 3.5;
    let max_x_speed = 30.0;

    let map_x = move |speed: f64| -> f32 {
        padding_left + (speed / max_x_speed * graph_w) as f32
    };

    let map_y = move |gain: f64| -> f32 {
        let normalized = ((gain - min_y) / (max_y - min_y)).clamp(0.0, 1.0);
        (padding_top + graph_h * (1.0 - normalized)) as f32
    };

    canvas(
        move |_, _, _| (),
        move |bounds, _, window, _| {
            let ox = f32::from(bounds.origin.x);
            let oy = f32::from(bounds.origin.y);

            // Background box
            let bg_bounds = Bounds {
                origin: point(px(ox + padding_left), px(oy + padding_top)),
                size: size(px(graph_w), px(graph_h)),
            };
            window.paint_quad(quad(
                bg_bounds,
                px(4.0),
                if enabled { pal.muted.opacity(0.3) } else { pal.muted.opacity(0.1) },
                px(1.0),
                if enabled { pal.text_muted.opacity(0.2) } else { pal.text_muted.opacity(0.1) },
                BorderStyle::default(),
            ));

            // Baseline 1.0x line
            let y_base = oy + map_y(1.0);
            window.paint_quad(quad(
                Bounds {
                    origin: point(px(ox + padding_left), px(y_base)),
                    size: size(px(graph_w), px(1.0)),
                },
                px(0.0),
                if enabled { pal.text_muted.opacity(0.4) } else { pal.text_muted.opacity(0.2) },
                px(0.0),
                pal.transparent,
                BorderStyle::default(),
            ));

            // Configured maximum-speed line
            if enabled && max_gain > 1.0 && max_gain <= max_y {
                let y_max = oy + map_y(max_gain);
                window.paint_quad(quad(
                    Bounds {
                        origin: point(px(ox + padding_left), px(y_max)),
                        size: size(px(graph_w), px(1.0)),
                    },
                    px(0.0),
                    pal.text_muted.opacity(0.3),
                    px(0.0),
                    pal.transparent,
                    BorderStyle::default(),
                ));
            }

            // Curve polygon fill
            let samples = 40;
            let mut polygon_pts = Vec::with_capacity(samples + 3);
            polygon_pts.push(point(px(ox + map_x(0.0)), px(oy + map_y(1.0))));

            for i in 0..=samples {
                let speed = (i as f64) * (max_x_speed / (samples as f64));
                let gain = if enabled {
                    openlogi_core::scroll::compute_acceleration_gain(speed, acceleration_factor, max_gain)
                } else {
                    1.0
                };
                polygon_pts.push(point(px(ox + map_x(speed)), px(oy + map_y(gain))));
            }

            polygon_pts.push(point(px(ox + map_x(max_x_speed)), px(oy + map_y(1.0))));

            let mut path = PathBuilder::fill();
            path.add_polygon(&polygon_pts, true);
            if let Ok(path) = path.build() {
                let fill_color = if enabled {
                    rgb(theme::STATUS_CONNECTED).opacity(0.25)
                } else {
                    pal.text_muted.opacity(0.05)
                };
                window.paint_path(path, fill_color);
            }
        },
    )
    .flex_none()
    .w(px(width))
    .h(px(height))
}
