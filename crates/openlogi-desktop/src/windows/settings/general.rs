//! General settings page.

use super::{
    App, AppState, Entity, FluentBuilder, IconName, InteractiveElement, ParentElement,
    SettingField, SettingGroup, SettingItem, SettingPage, SharedString, Slider, SliderState,
    Styled, ThumbwheelSensitivity, VerticalScrollSensitivity, theme,
};
use crate::ui::commit_slider::SliderUnit;
use crate::ui::slider_row::SliderRow;
use crate::ui::theme::FocusRingStyle as _;
use crate::ui::theme::PressableStyle as _;
use crate::ui::theme::Typography as _;
use gpui::div;
use gpui_kit::base::Button as BaseButton;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::switch::Switch;
use openlogi_core::config::MouseProfileTarget;

use crate::features::pointer::momentum::MomentumPanel;
use crate::platform::registration::ServiceStatus;

/// The page's two sensitivity sliders, named so a call site cannot swap two
/// same-typed `Entity<SliderState>`s without the compiler noticing.
pub(super) struct SensitivitySliders {
    pub(super) vertical_scroll: Entity<SliderState>,
    pub(super) thumbwheel: Entity<SliderState>,
}

pub(super) fn general_page(
    sliders: SensitivitySliders,
    momentum: Entity<MomentumPanel>,
    registration_status: ServiceStatus,
) -> SettingPage {
    let SensitivitySliders {
        vertical_scroll,
        thumbwheel,
    } = sliders;
    let group = SettingGroup::new()
        .item(mouse_profile_target_item())
        .item(smooth_scrolling_item())
        .item(sensitivity_item(
            tr!("pointer.vertical_scroll_sensitivity"),
            tr!("pointer.vertical_scroll_sensitivity_description"),
            vertical_scroll,
            VerticalScrollSensitivity::MAX,
            VerticalScrollSensitivity::DEFAULT,
        ))
        .item(sensitivity_item(
            tr!("pointer.thumb_wheel_sensitivity"),
            tr!("pointer.thumbwheel_sensitivity_description"),
            thumbwheel,
            ThumbwheelSensitivity::MAX,
            ThumbwheelSensitivity::DEFAULT,
        ));
    // Only macOS animates a desktop switch, so only macOS shows the switch.
    // `cfg!` keeps the item and its setter compiled, and used, everywhere.
    let group = if cfg!(target_os = "macos") {
        group.item(animate_desktop_switch_item())
    } else {
        group
    };
    // Only the Windows agent finds UI elements under the pointer, so only
    // Windows shows the switch; `cfg!` keeps it compiled everywhere.
    let group = if cfg!(target_os = "windows") {
        group.item(ui_hover_haptics_item())
    } else {
        group
    };
    let group = group.item(launch_at_login_item());

    // Switched off under System Settings › Login Items: nothing can start
    // the service until the user flips it back on there — surface it instead
    // of letting the switch above claim a state macOS is overriding.
    let group = if registration_status == ServiceStatus::RequiresApproval {
        group.item(login_item_approval_notice())
    } else {
        group
    };

    // One `show_in_menu_bar` setting drives the macOS status item and the
    // Windows notification-area icon (live on macOS, at the next agent launch
    // on Windows); Linux has no tray, so no switch.
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

    let group = group
        .item(low_battery_notifications_item())
        .item(lock_key_notifications_item());

    // Momentum scrolling's app-wide default. A mouse can set its own on its
    // Pointer tab; the card counts those.
    let momentum_group = SettingGroup::new()
        .title(tr!("pointer.momentum_title"))
        .description(tr!("pointer.momentum_general_description"))
        .item(SettingItem::render(move |_, _, _| {
            div()
                .debug_selector(|| "general-momentum".into())
                .child(momentum.clone())
        }));
    SettingPage::new(tr!("app.general"))
        .icon(IconName::Settings)
        .resettable(false)
        .group(group)
        .group(momentum_group)
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
                        .small()
                        .label(tr!("pointer.mouse_profile_pointer"))
                        .debug_selector(|| "mouse-profile-pointer".into()),
                )
                .child(
                    Radio::new("mouse-profile-focused")
                        .small()
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

/// The switch for the Previous/Next Desktop slide.
fn animate_desktop_switch_item() -> SettingItem {
    SettingItem::new(
        tr!("app.animate_desktop_switch"),
        SettingField::switch(
            |cx| AppState::try_read(cx).is_some_and(|s| s.app_settings().animate_desktop_switch),
            |enabled, cx| {
                AppState::apply(cx, |state| state.commit_animate_desktop_switch(enabled));
            },
        ),
    )
    .description(tr!("app.animate_desktop_switch_description"))
}

/// The switch for the haptic tick on hovering an interactive UI element.
fn ui_hover_haptics_item() -> SettingItem {
    SettingItem::new(
        tr!("app.ui_hover_haptics"),
        SettingField::render(|_, _, cx| {
            let enabled = AppState::try_read(cx).is_some_and(|s| s.app_settings().ui_hover_haptics);
            div().debug_selector(|| "ui-hover-haptics".into()).child(
                Switch::new("ui-hover-haptics")
                    .checked(enabled)
                    .accessibility_label(tr!("app.ui_hover_haptics"))
                    .on_click(|enabled, _, cx| {
                        let enabled = *enabled;
                        AppState::apply(cx, |state| state.commit_ui_hover_haptics(enabled));
                    }),
            )
        }),
    )
    .description(tr!("app.ui_hover_haptics_description"))
}

/// The switch for the overlay's low-battery notices.
fn low_battery_notifications_item() -> SettingItem {
    SettingItem::new(
        tr!("app.low_battery_notifications"),
        SettingField::switch(
            |cx| AppState::try_read(cx).is_some_and(|s| s.app_settings().low_battery_notifications),
            |enabled, cx| {
                AppState::apply(cx, |state| state.commit_low_battery_notifications(enabled));
            },
        ),
    )
    .description(tr!("app.low_battery_notifications_description"))
}

/// The switch for the overlay's lock-key indicator.
fn lock_key_notifications_item() -> SettingItem {
    SettingItem::new(
        tr!("app.lock_key_notifications"),
        SettingField::switch(
            |cx| AppState::try_read(cx).is_some_and(|s| s.app_settings().lock_key_notifications),
            |enabled, cx| {
                AppState::apply(cx, |state| state.commit_lock_key_notifications(enabled));
            },
        ),
    )
    // macOS gives Num Lock and Scroll Lock no meaning, so the indicator shows
    // only Caps Lock there (`LockKey::is_indicated_here`).
    .description(if cfg!(target_os = "macos") {
        tr!("app.lock_key_notifications_description_macos")
    } else {
        tr!("app.lock_key_notifications_description")
    })
}

/// A sensitivity slider laid out as the device pages lay theirs out — label
/// and value on one line, the slider across the row — with "Default" beside
/// the value while it rests there, in the kit items' type step so it matches
/// the switches above and below it. A custom item has no title of its own, so
/// its title and description are its search keywords.
fn sensitivity_item<T: SliderUnit + std::fmt::Display>(
    title: SharedString,
    description: SharedString,
    slider: Entity<SliderState>,
    max: T,
    default: T,
) -> SettingItem {
    SettingItem::render({
        let (title, description) = (title.clone(), description.clone());
        move |_, _, cx| {
            let value = T::from_slider(slider.read(cx).value().start());
            SliderRow::new(title.clone(), Slider::new(&slider).horizontal())
                .value(value.to_string(), [max.to_string()])
                .when(value == default, |row| row.marker(tr!("common.default")))
                .description(description.clone())
                .compact()
        }
    })
    .keywords([title, description])
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
        .pressable_fill(false, pal)
        .focus_visible(move |s| s.focus_ring(pal))
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
        AppContext as _, Bounds, Modifiers, TestAppContext, VisualTestContext, point, px, size,
    };
    use openlogi_core::config::{Config, UiScale};

    #[gpui::test]
    fn mouse_profile_target_choices_update_state_and_respect_failed_saves(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
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

    /// Scroll the Settings window down until `selector` lies wholly inside
    /// `viewport`, and return its bounds there. A control further down the
    /// page may not render until it is scrolled near.
    #[cfg(target_os = "windows")]
    fn scroll_into_view(
        visual: &mut VisualTestContext,
        viewport: Bounds<gpui::Pixels>,
        selector: &'static str,
    ) -> Bounds<gpui::Pixels> {
        use gpui::{ScrollDelta, ScrollEvent};

        let inside = |bounds: &Bounds<gpui::Pixels>| {
            viewport.contains(&bounds.origin) && viewport.contains(&bounds.bottom_right())
        };
        let mut bounds = visual.debug_bounds(selector);
        for _ in 0..40 {
            if bounds.as_ref().is_some_and(inside) {
                break;
            }
            visual.simulate_event(ScrollEvent {
                position: viewport.center(),
                delta: ScrollDelta::Pixels(point(px(0.), px(-120.))),
                ..Default::default()
            });
            visual.update(|window, cx| window.draw(cx).clear(cx));
            bounds = visual.debug_bounds(selector);
        }
        let bounds = bounds.unwrap_or_else(|| panic!("{selector} never renders"));
        assert!(
            inside(&bounds),
            "{selector} at {bounds:?} must scroll into {viewport:?}"
        );
        bounds
    }

    /// At 125% in the Settings window's minimum size, the Windows-only hover
    /// switch scrolls into view and toggles the setting, and scrolling on
    /// reaches the General page's last control.
    #[cfg(target_os = "windows")]
    #[gpui::test]
    fn ui_hover_haptics_switch_is_reachable_at_125_percent(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        cx.update(|cx| {
            gpui_kit::init(cx);
            theme::register_builtin_themes(cx);
        });
        let handle = cx.update(|cx| {
            let mut config = Config::ephemeral();
            config.app_settings.ui_scale = UiScale::Large;
            let (commands, _) = tokio::sync::mpsc::unbounded_channel();
            let resolver = AssetResolver::new();
            let sources = Sources::in_memory(config, &resolver, commands);
            AppState::set_global(cx.new(|_| AppState::new(sources)), cx);
            settings::open(cx);
            cx.global::<WindowRegistry>().settings.unwrap()
        });
        let mut visual = VisualTestContext::from_window(handle.into(), cx);
        // The Settings window's authored size, which is also its minimum.
        let window = size(px(920.), px(640.));
        visual.simulate_resize(window);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let viewport = Bounds::new(point(px(0.), px(0.)), window);

        let switch = scroll_into_view(&mut visual, viewport, "ui-hover-haptics");
        visual.simulate_click(switch.center(), Modifiers::default());
        visual.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert!(
                AppState::try_read(cx)
                    .unwrap()
                    .app_settings()
                    .ui_hover_haptics
            );
        });

        scroll_into_view(&mut visual, viewport, "general-momentum");
        visual.update(|window, _| window.remove_window());
    }
}
