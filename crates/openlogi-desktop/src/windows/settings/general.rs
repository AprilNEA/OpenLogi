//! General settings page.

use super::{
    App, AppState, Entity, FluentBuilder, IconName, InteractiveElement, ParentElement,
    SettingField, SettingGroup, SettingItem, SettingPage, Slider, SliderState, StateEvent, Styled,
    ThumbwheelSensitivity, VerticalScrollSensitivity, div, h_flex, px, theme, v_flex,
};
use crate::ui::theme::Typography as _;
use gpui_base::Button as BaseButton;

use crate::platform::registration::ServiceStatus;

/// The page's two sensitivity sliders, named so a call site cannot swap two
/// same-typed `Entity<SliderState>`s without the compiler noticing.
pub(super) struct SensitivitySliders {
    pub(super) vertical_scroll: Entity<SliderState>,
    pub(super) thumbwheel: Entity<SliderState>,
}

pub(super) fn general_page(
    sliders: SensitivitySliders,
    registration_status: ServiceStatus,
) -> SettingPage {
    let SensitivitySliders {
        vertical_scroll,
        thumbwheel,
    } = sliders;
    let group = SettingGroup::new()
        .item(smooth_scrolling_item())
        .item(
            SettingItem::new(
                rust_i18n::t!("pointer.vertical_scroll_sensitivity"),
                SettingField::render(move |_, _, cx| {
                    vertical_scroll_sensitivity_field(&vertical_scroll, cx)
                }),
            )
            .description(gpui::SharedString::from(rust_i18n::t!(
                "pointer.vertical_scroll_sensitivity_description"
            ))),
        )
        .item(
            SettingItem::new(
                rust_i18n::t!("pointer.thumb_wheel_sensitivity"),
                SettingField::render(move |_, _, cx| thumbwheel_sensitivity_field(&thumbwheel, cx)),
            )
            .description(gpui::SharedString::from(rust_i18n::t!(
                "pointer.thumbwheel_sensitivity_description"
            ))),
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
                rust_i18n::t!("app.show_in_menu_bar")
            } else {
                rust_i18n::t!("app.show_in_the_notification_area")
            },
            SettingField::switch(
                |cx| AppState::try_read(cx).is_some_and(|s| s.app_settings().show_in_menu_bar),
                |enabled, cx| {
                    AppState::update(cx, move |state, cx| {
                        state.set_show_in_menu_bar(enabled);
                        cx.emit(StateEvent::SettingsChanged);
                    });
                },
            ),
        )
        .description(if cfg!(target_os = "macos") {
            rust_i18n::t!("app.menu_bar_visibility_description")
        } else {
            rust_i18n::t!("app.notification_area_visibility_description")
        }),
    );

    SettingPage::new(rust_i18n::t!("app.general"))
        .icon(IconName::Settings)
        .resettable(false)
        .group(group)
}

/// The smooth-scrolling switch.
fn smooth_scrolling_item() -> SettingItem {
    SettingItem::new(
        rust_i18n::t!("pointer.smooth_scrolling"),
        SettingField::switch(
            |cx| AppState::try_read(cx).is_some_and(|s| s.app_settings().smooth_scroll),
            |enabled, cx| {
                AppState::update(cx, move |state, cx| {
                    state.set_smooth_scroll(enabled);
                    cx.emit(StateEvent::SettingsChanged);
                });
            },
        ),
    )
    .description(gpui::SharedString::from(rust_i18n::t!(
        "pointer.smooth_scrolling_description"
    )))
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
                    .child(format!("({})", rust_i18n::t!("common.default"))),
            )
        })
}

/// The launch-at-login switch — a persisted config value the agent reads
/// (the sunk switch); the setter never unregisters.
fn launch_at_login_item() -> SettingItem {
    SettingItem::new(
        rust_i18n::t!("app.launch_at_login"),
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
    .description(gpui::SharedString::from(if cfg!(target_os = "macos") {
        rust_i18n::t!("app.launch_at_login_macos_description")
    } else {
        rust_i18n::t!("app.launch_at_login_description")
    }))
}

/// The `RequiresApproval` notice: with the direct-launch fallback gone, the
/// switched-off login item stops the agent entirely, whatever the preference.
fn login_item_approval_notice() -> SettingItem {
    SettingItem::new(
        rust_i18n::t!("app.login_item_disabled_in_system_settings"),
        SettingField::render(|_, _, cx| open_login_items_button(cx)),
    )
    .description(gpui::SharedString::from(rust_i18n::t!(
        "app.login_item_disabled_description"
    )))
}

/// Deep link to System Settings › Login Items — the only place that can
/// re-enable a service switched off there.
fn open_login_items_button(cx: &App) -> BaseButton {
    let pal = theme::palette(cx);
    BaseButton::new("open-login-items")
        .accessibility_label(rust_i18n::t!("app.open_login_items"))
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
        .child(rust_i18n::t!("app.open_login_items"))
        .on_click(|_, _, _| crate::platform::registration::open_login_items_settings())
}
