//! General settings page.

use super::{
    AnyElement, App, AppState, BorrowAppContext, DEFAULT_THUMBWHEEL_SENSITIVITY, Entity,
    FluentBuilder, IconName, IntoElement, ParentElement, SettingField, SettingGroup, SettingItem,
    SettingPage, Slider, SliderState, Styled, div, h_flex, px, theme, v_flex,
};

pub(super) fn general_page(sensitivity_slider: Entity<SliderState>) -> SettingPage {
    let group = SettingGroup::new()
        .item(
            SettingItem::new(
                tr!("Thumb Wheel Sensitivity"),
                SettingField::render(move |_, _, cx| {
                    sensitivity_field(&sensitivity_slider, cx)
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
                        cx.try_global::<AppState>()
                            .is_some_and(|s| s.app_settings().launch_at_login)
                    },
                    |enabled, cx| {
                        cx.update_global::<AppState, _>(move |s, _| {
                            s.set_launch_at_login(enabled);
                        });
                        cx.refresh_windows();
                    },
                ),
            )
            .description(tr!(
                "Automatically start OpenLogi when you log in."
            )),
        );

    #[cfg(target_os = "macos")]
    let group = group.item(
        SettingItem::new(
            tr!("Show in menu bar"),
            SettingField::switch(
                |cx| {
                    cx.try_global::<AppState>()
                        .is_some_and(|s| s.app_settings().show_in_menu_bar)
                },
                |enabled, cx| {
                    cx.update_global::<AppState, _>(move |s, _| {
                        s.set_show_in_menu_bar(enabled);
                    });
                    cx.refresh_windows();
                },
            ),
        )
        .description(tr!(
            "Keep OpenLogi's icon in the menu bar. When off, it stays in the Dock instead."
        )),
    );

    SettingPage::new(tr!("General"))
        .icon(IconName::Settings)
        .resettable(false)
        .group(group)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "slider value is a stepped 1..=100 figure"
)]
fn sensitivity_field(slider: &Entity<SliderState>, cx: &mut App) -> AnyElement {
    let value = slider.read(cx).value().start().round() as i32;
    let is_default = value == DEFAULT_THUMBWHEEL_SENSITIVITY;
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
                        .text_sm()
                        .text_color(pal.text_muted)
                        .child(value.to_string()),
                ),
        )
        .when(is_default, |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(pal.text_muted)
                    .whitespace_nowrap()
                    .child(format!("({})", rust_i18n::t!("Default"))),
            )
        })
        .into_any_element()
}
