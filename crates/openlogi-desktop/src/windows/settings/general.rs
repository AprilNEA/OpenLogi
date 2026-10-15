//! General settings page.

use super::{
    App, AppState, Entity, FluentBuilder, IconName, InteractiveElement, ParentElement,
    SettingField, SettingGroup, SettingItem, SettingPage, Slider, SliderState, Styled,
    ThumbwheelSensitivity, VerticalScrollSensitivity, div, h_flex, px, theme, v_flex,
};
use crate::ui::theme::Typography as _;
use gpui_base::Button as BaseButton;
use gpui_component::radio::{Radio, RadioGroup};
use openlogi_core::config::MouseProfileTarget;

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
        .item(mouse_profile_target_item())
        .item(smooth_scrolling_item())
        .item(
            SettingItem::new(
                tr!("pointer.vertical_scroll_sensitivity"),
                SettingField::render(move |_, _, cx| {
                    vertical_scroll_sensitivity_field(&vertical_scroll, cx)
                }),
            )
            .description(tr!("pointer.vertical_scroll_sensitivity_description")),
        )
        .item(
            SettingItem::new(
                tr!("pointer.thumb_wheel_sensitivity"),
                SettingField::render(move |_, _, cx| thumbwheel_sensitivity_field(&thumbwheel, cx)),
            )
            .description(tr!("pointer.thumbwheel_sensitivity_description")),
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
                tr!("app.show_in_menu_bar")
            } else {
                tr!("app.show_in_the_notification_area")
            },
            SettingField::switch(
                |cx| AppState::try_read(cx).is_some_and(|s| s.app_settings().show_in_menu_bar),
                |enabled, cx| {
                    AppState::apply(cx, |state| state.commit_show_in_menu_bar(enabled));
                },
            ),
        )
        .description(if cfg!(target_os = "macos") {
            tr!("app.menu_bar_visibility_description")
        } else {
            tr!("app.notification_area_visibility_description")
        }),
    );

    SettingPage::new(tr!("app.general"))
        .icon(IconName::Settings)
        .resettable(false)
        .group(group)
}

fn mouse_profile_target_item() -> SettingItem {
    SettingItem::new(
        tr!("pointer.mouse_button_profiles"),
        SettingField::render(|_, _, cx| {
            let target = AppState::try_read(cx).map_or(MouseProfileTarget::Pointer, |s| {
                s.app_settings().mouse_profile_target
            });
            RadioGroup::vertical("mouse-profile-target")
                .child(
                    Radio::new("mouse-profile-pointer")
                        .label(tr!("pointer.mouse_profile_pointer"))
                        .debug_selector(|| "mouse-profile-pointer".into()),
                )
                .child(
                    Radio::new("mouse-profile-focused")
                        .label(tr!("pointer.mouse_profile_focused"))
                        .debug_selector(|| "mouse-profile-focused".into()),
                )
                .selected_index(Some(match target {
                    MouseProfileTarget::Pointer => 0,
                    MouseProfileTarget::Focused => 1,
                }))
                .on_change(|index, _, cx| {
                    let target = match index {
                        0 => MouseProfileTarget::Pointer,
                        1 => MouseProfileTarget::Focused,
                        _ => unreachable!("mouse profile target has exactly two choices"),
                    };
                    AppState::apply(cx, |state| state.commit_mouse_profile_target(target));
                })
        }),
    )
    .layout(gpui::Axis::Vertical)
    .description(tr!("pointer.mouse_profile_target_description"))
}

/// The smooth-scrolling switch.
fn smooth_scrolling_item() -> SettingItem {
    SettingItem::new(
        tr!("pointer.smooth_scrolling"),
        SettingField::switch(
            |cx| AppState::try_read(cx).is_some_and(|s| s.app_settings().smooth_scroll),
            |enabled, cx| {
                AppState::apply(cx, |state| state.commit_smooth_scroll(enabled));
            },
        ),
    )
    .description(tr!("pointer.smooth_scrolling_description"))
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
        tr!("app.launch_at_login"),
        SettingField::switch(
            |cx| AppState::try_read(cx).is_some_and(|s| s.app_settings().launch_at_login),
            |enabled, cx| {
                AppState::apply(cx, |state| state.commit_launch_at_login(enabled));
            },
        ),
    )
    .description(if cfg!(target_os = "macos") {
        tr!("app.launch_at_login_macos_description")
    } else {
        tr!("app.launch_at_login_description")
    })
}

/// The `RequiresApproval` notice: with the direct-launch fallback gone, the
/// switched-off login item stops the agent entirely, whatever the preference.
fn login_item_approval_notice() -> SettingItem {
    SettingItem::new(
        tr!("app.login_item_disabled_in_system_settings"),
        SettingField::render(|_, _, cx| open_login_items_button(cx)),
    )
    .description(tr!("app.login_item_disabled_description"))
}

/// Deep link to System Settings › Login Items — the only place that can
/// re-enable a service switched off there.
fn open_login_items_button(cx: &App) -> BaseButton {
    let pal = theme::palette(cx);
    BaseButton::new("open-login-items")
        .accessibility_label(tr!("app.open_login_items"))
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
        .child(tr!("app.open_login_items"))
        .on_click(|_, _, _| crate::platform::registration::open_login_items_settings())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{assets::AssetResolver, i18n::LOCALE_LOCK};
    use crate::state::{ConfigPersistence, Sources};
    use crate::windows::{WindowRegistry, settings};
    use gpui::{
        AppContext as _, Bounds, Modifiers, TestAppContext, VisualTestContext, point, size,
    };
    use openlogi_core::config::{Config, UiScale};

    #[gpui::test]
    fn mouse_profile_target_choices_update_state_and_respect_failed_saves(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            theme::register_builtin_themes(cx);
        });

        for (locale, scale, read_only) in [
            ("en", UiScale::Normal, false),
            ("zh-TW", UiScale::ExtraLarge, false),
            ("en", UiScale::Normal, true),
        ] {
            rust_i18n::set_locale(locale);
            let handle = cx.update(|cx| {
                let mut config = Config::ephemeral();
                config.app_settings.ui_scale = scale;
                let (commands, _) = tokio::sync::mpsc::unbounded_channel();
                let resolver = AssetResolver::new();
                let mut sources = Sources::in_memory(config, &resolver, commands);
                if read_only {
                    sources.persistence = ConfigPersistence::ReadOnly("read-only test".into());
                }
                AppState::set_global(cx.new(|_| AppState::new(sources)), cx);
                settings::open(cx);
                cx.global::<WindowRegistry>().settings.unwrap()
            });
            let mut visual = VisualTestContext::from_window(handle.into(), cx);
            visual.simulate_resize(size(px(920.), px(640.)));
            visual.update(|window, cx| window.draw(cx).clear(cx));
            let viewport = Bounds::new(point(px(0.), px(0.)), size(px(920.), px(640.)));
            let pointer = visual.debug_bounds("mouse-profile-pointer").unwrap();
            let focused = visual.debug_bounds("mouse-profile-focused").unwrap();
            assert!(focused.top() >= pointer.bottom());
            for bounds in [pointer, focused] {
                assert!(
                    viewport.contains(&bounds.origin) && viewport.contains(&bounds.bottom_right())
                );
            }

            visual.simulate_click(focused.center(), Modifiers::default());
            visual.update(|window, cx| {
                window.draw(cx).clear(cx);
                let state = AppState::try_read(cx).unwrap();
                assert_eq!(
                    state.app_settings().mouse_profile_target,
                    if read_only {
                        MouseProfileTarget::Pointer
                    } else {
                        MouseProfileTarget::Focused
                    }
                );
            });
            visual.simulate_click(pointer.center(), Modifiers::default());
            visual.update(|window, cx| {
                window.draw(cx).clear(cx);
                assert_eq!(
                    AppState::try_read(cx)
                        .unwrap()
                        .app_settings()
                        .mouse_profile_target,
                    MouseProfileTarget::Pointer
                );
                window.remove_window();
            });
        }
        rust_i18n::set_locale("en");
    }
}
