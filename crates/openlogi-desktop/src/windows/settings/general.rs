//! General settings page.

use super::{
    App, AppState, Entity, FluentBuilder, IconName, InteractiveElement, ParentElement,
    SettingField, SettingGroup, SettingItem, SettingPage, Slider, SliderState, StateEvent, Styled,
    ThumbwheelSensitivity, VerticalScrollSensitivity, div, h_flex, px, theme, v_flex,
};
use crate::ui::theme::Typography as _;
use gpui_base::Button as BaseButton;

use crate::platform::registration::ServiceStatus;

pub(super) fn general_page(
    vertical_scroll_sensitivity_slider: Entity<SliderState>,
    thumbwheel_sensitivity_slider: Entity<SliderState>,
    registration_status: ServiceStatus,
) -> SettingPage {
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
        .item(launch_at_login_item());

    // Switched off under System Settings › Login Items: nothing can start
    // the service until the user flips it back on there — surface it instead
    // of letting the switch above claim a state macOS is overriding.
    let group = if registration_status == ServiceStatus::RequiresApproval {
        group.item(login_item_approval_notice())
    } else {
        group
    };

    // One `show_in_menu_bar` setting drives the macOS status item and the
    // Windows notification-area icon (honored at next agent launch); Linux
    // has no tray, so no switch.
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

/// The launch-at-login switch — a persisted config value the agent reads
/// (the sunk switch); the setter never unregisters.
fn launch_at_login_item() -> SettingItem {
    SettingItem::new(
        tr!("Launch at login"),
        SettingField::switch(
            |cx| AppState::try_read(cx).is_some_and(|s| s.app_settings().launch_at_login),
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
    })
}

/// The `RequiresApproval` notice: with the direct-launch fallback gone, the
/// switched-off login item stops the agent entirely, whatever the preference.
fn login_item_approval_notice() -> SettingItem {
    SettingItem::new(
        tr!("Login item disabled in System Settings"),
        SettingField::render(|_, _, cx| open_login_items_button(cx)),
    )
    .description(tr!(
        "macOS is blocking OpenLogi's background agent: its login item is switched off. The agent cannot run until you turn it back on under Login Items."
    ))
}

/// Deep link to System Settings › Login Items — the only place that can
/// re-enable a service switched off there.
fn open_login_items_button(cx: &App) -> BaseButton {
    let pal = theme::palette(cx);
    BaseButton::new("open-login-items")
        .accessibility_label(tr!("Open Login Items"))
        .px_2()
        .py_1()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(pal.border)
        .text_caption()
        .cursor_pointer()
        .bg(pal.control)
        .hover(move |s| s.bg(pal.control_hover))
        .focus_visible(move |s| s.bg(pal.control_hover))
        .child(tr!("Open Login Items"))
        .on_click(|_, _, _| crate::platform::registration::open_login_items_settings())
}
