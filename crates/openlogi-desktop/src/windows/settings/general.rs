//! General settings page.

use super::{
    AnyElement, App, AppState, Entity, FluentBuilder, IconName, IntoElement, ParentElement,
    SettingField, SettingGroup, SettingItem, SettingPage, Slider, SliderState, StateEvent, Styled,
    ThumbwheelSensitivity, VerticalScrollSensitivity, div, h_flex, px, theme, v_flex,
};
use crate::ui::theme::Typography as _;

pub(super) fn general_page(
    vertical_scroll_sensitivity_slider: Entity<SliderState>,
    thumbwheel_sensitivity_slider: Entity<SliderState>,
) -> SettingPage {
    let group = SettingGroup::new()
        .item(
            SettingItem::new(
                tr!("Vertical Scroll Sensitivity"),
                SettingField::render(move |_, _, cx| {
                    vertical_scroll_sensitivity_field(&vertical_scroll_sensitivity_slider, cx)
                }),
            )
            .description(tr!(
                "Scales traditional mouse-wheel vertical distance without changing trackpad scrolling."
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

fn thumbwheel_sensitivity_field(slider: &Entity<SliderState>, cx: &mut App) -> AnyElement {
    let value = ThumbwheelSensitivity::from_rounded(slider.read(cx).value().start());
    sensitivity_field(
        slider,
        value.to_string(),
        value == ThumbwheelSensitivity::DEFAULT,
        cx,
    )
}

fn vertical_scroll_sensitivity_field(slider: &Entity<SliderState>, cx: &mut App) -> AnyElement {
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
) -> AnyElement {
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
        .into_any_element()
}
