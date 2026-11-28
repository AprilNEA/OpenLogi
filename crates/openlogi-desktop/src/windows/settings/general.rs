//! General settings page.

use super::{
    App, AppState, Entity, IconName, InteractiveElement, ParentElement, SettingField, SettingGroup,
    SettingItem, SettingPage, SharedString, Styled, theme,
};
use crate::ui::theme::FocusRingStyle as _;
use crate::ui::theme::PressableStyle as _;
use crate::ui::theme::Typography as _;
use gpui_kit::base::Button as BaseButton;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::radio::{Radio, RadioGroup};
use openlogi_core::config::MouseProfileTarget;

use crate::features::pointer::momentum::MomentumPanel;
use crate::features::profiles::WheelProfileEditor;
use crate::platform::registration::ServiceStatus;

pub(super) fn general_page(
    momentum: Entity<MomentumPanel>,
    wheel_profiles: Entity<WheelProfileEditor>,
    registration_status: ServiceStatus,
) -> SettingPage {
    let profiles_title = tr!("pointer.app_wheel_profiles");
    let profile_group = SettingGroup::new()
        .title(profiles_title.clone())
        .description(tr!("pointer.app_wheel_profiles_description"))
        .item(wheel_profile_item(wheel_profiles, profiles_title));
    let group = SettingGroup::new();
    // Only macOS animates a desktop switch, so only macOS shows the switch.
    // `cfg!` keeps the item and its setter compiled, and used, everywhere.
    let group = if cfg!(target_os = "macos") {
        group.item(animate_desktop_switch_item())
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
        .item(SettingItem::render(move |_, _, _| momentum.clone()));
    SettingPage::new(tr!("app.general"))
        .icon(IconName::Settings)
        .resettable(false)
        .group(profile_group)
        .group(SettingGroup::new().item(mouse_profile_target_item()))
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

/// Keep the selector and all three wheel controls together, including in search results.
fn wheel_profile_item(
    editor: Entity<WheelProfileEditor>,
    group_title: SharedString,
) -> SettingItem {
    SettingItem::render(move |_, _, _| editor.clone()).keywords([
        group_title,
        tr!("pointer.smooth_scrolling_by_app"),
        tr!("pointer.smooth_scrolling"),
        tr!("pointer.vertical_scroll_sensitivity"),
        tr!("pointer.vertical_sensitivity_search_alias"),
        tr!("pointer.horizontal_scroll_sensitivity"),
        tr!("pointer.thumb_wheel_sensitivity"),
        tr!("profiles.profile"),
        tr!("profiles.profile", locale = "en"),
    ])
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
    use crate::app::tests::{assert_main_wheel, assert_slider_value, click_control, set_slider};
    use crate::services::{assets::AssetResolver, i18n::LOCALE_LOCK};
    use crate::state::{
        ConfigPersistence, Sources,
        tests::{KNOWN_MOUSE_KEY, direct_inventory},
    };
    use crate::windows::{WindowRegistry, settings};
    use gpui::{
        AppContext as _, Bounds, Modifiers, TestAppContext, VisualTestContext, point, px, size,
    };
    use openlogi_core::config::{
        Config, ThumbwheelSensitivity, UiScale, VerticalScrollSensitivity,
    };
    use openlogi_core::device::DeviceInventory;

    const SAFARI: &str = "com.apple.Safari";
    const GAME: &str = "com.example.game";
    const HORIZONTAL: &str = "Horizontal scroll sensitivity";

    fn open_scroll_settings(
        cx: &mut TestAppContext,
        config: Config,
        read_only: bool,
        inventories: &[DeviceInventory],
    ) -> VisualTestContext {
        let handle = cx.update(|cx| {
            let (commands, _) = tokio::sync::mpsc::unbounded_channel();
            let resolver = AssetResolver::new();
            let mut sources = Sources {
                inventories,
                ..Sources::in_memory(config, &resolver, commands)
            };
            if read_only {
                sources.persistence = ConfigPersistence::ReadOnly("read-only test".into());
            }
            let mut state = AppState::new(sources);
            let _ = state.set_foreground(openlogi_ipc::ForegroundApps {
                current: None,
                recent: [(SAFARI, "Safari"), (GAME, "Example Game")]
                    .map(|(id, name)| openlogi_core::app::ForegroundApp {
                        id: id.into(),
                        display_name: name.into(),
                    })
                    .into(),
            });
            AppState::set_global(cx.new(|_| state), cx);
            settings::open(cx);
            cx.global::<WindowRegistry>().settings.unwrap()
        });
        let mut visual = VisualTestContext::from_window(handle.into(), cx);
        visual.simulate_resize(size(px(920.), px(640.)));
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual
    }

    fn add_recent_app(visual: &mut VisualTestContext, row: &'static str) {
        click_control(visual, "scroll-profile:add-app-profile");
        visual.update(|window, cx| window.draw(cx).clear(cx));
        click_control(visual, row);
    }

    #[gpui::test]
    fn smooth_scroll_app_profile_can_be_enabled_through_general_settings(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        cx.update(|cx| {
            gpui_kit::init(cx);
            theme::register_builtin_themes(cx);
        });
        for read_only in [false, true] {
            let mut visual = open_scroll_settings(cx, Config::ephemeral(), read_only, &[]);
            visual.simulate_resize(size(px(920.), px(1000.)));
            add_recent_app(&mut visual, "scroll-profile:catalog-app:com.apple.Safari");
            visual.update(|_, cx| {
                let state = AppState::try_read(cx).unwrap();
                assert_eq!(state.editing_scroll_app(), Some(SAFARI));
                assert!(!state.editing_smooth_scroll());
                assert!(state.app_settings().per_app_smooth_scroll.is_empty());
            });
            click_control(&mut visual, "vertical-scroll-profile-inheritance");
            visual.update(|_, cx| {
                let state = AppState::try_read(cx).unwrap();
                assert_eq!(
                    state.editing_vertical_scroll_sensitivity_overridden(),
                    !read_only
                );
                assert_eq!(
                    state.editing_vertical_scroll_sensitivity(),
                    VerticalScrollSensitivity::DEFAULT
                );
            });
            click_control(&mut visual, "thumbwheel-profile-inheritance");
            visual.update(|_, cx| {
                let state = AppState::try_read(cx).unwrap();
                assert_eq!(
                    state.editing_thumbwheel_sensitivity_overridden(),
                    !read_only
                );
                assert_eq!(
                    state.editing_thumbwheel_sensitivity(),
                    ThumbwheelSensitivity::DEFAULT
                );
            });
            click_control(&mut visual, "smooth-scroll-profile-setting-control");
            visual.update(|_, cx| {
                let state = AppState::try_read(cx).unwrap();
                assert_eq!(state.editing_smooth_scroll(), !read_only);
                assert!(!state.app_settings().smooth_scroll);
            });
            click_control(&mut visual, "scroll-profile:default");
            visual.update(|window, cx| {
                let state = AppState::try_read(cx).unwrap();
                assert_eq!(state.editing_scroll_app(), None);
                assert!(!state.editing_smooth_scroll());
                assert_eq!(
                    state
                        .app_settings()
                        .per_app_smooth_scroll
                        .contains_key(SAFARI),
                    !read_only
                );
                window.remove_window();
            });
        }
    }

    #[gpui::test]
    fn main_wheel_profile_game_uses_native_14_while_safari_inherits_18(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        cx.update(|cx| {
            gpui_kit::init(cx);
            theme::register_builtin_themes(cx);
        });
        let mut config = Config::ephemeral();
        config.app_settings.smooth_scroll = true;
        config.app_settings.vertical_scroll_sensitivity =
            VerticalScrollSensitivity::try_new(18).unwrap();
        config.set_selected_device(Some(KNOWN_MOUSE_KEY.into()));
        config.set_device_thumbwheel_sensitivity(
            KNOWN_MOUSE_KEY,
            Some(ThumbwheelSensitivity::try_new(36).unwrap()),
        );
        let mut visual = open_scroll_settings(
            cx,
            config,
            false,
            &[
                direct_inventory([0xa3, 0x93, 0xca, 0xe0]),
                direct_inventory([1, 2, 3, 4]),
            ],
        );
        visual.simulate_resize(size(px(920.), px(1000.)));
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert_main_wheel(&mut visual, None, true, 18);
        assert_slider_value(&mut visual, HORIZONTAL, 14);

        add_recent_app(&mut visual, "scroll-profile:catalog-app:com.example.game");
        assert_main_wheel(&mut visual, Some(GAME), true, 18);
        assert_slider_value(&mut visual, HORIZONTAL, 36);
        click_control(&mut visual, "smooth-scroll-profile-setting-control");
        set_slider(&mut visual, "Vertical scroll sensitivity", 14);
        set_slider(&mut visual, HORIZONTAL, 75);
        assert_slider_value(&mut visual, HORIZONTAL, 75);
        assert_main_wheel(&mut visual, Some(GAME), false, 14);
        visual.update(|_, cx| {
            let state = AppState::try_read(cx).unwrap();
            assert!(state.editing_smooth_scroll_overridden());
            assert!(state.editing_vertical_scroll_sensitivity_overridden());
        });

        add_recent_app(&mut visual, "scroll-profile:catalog-app:com.apple.Safari");
        assert_main_wheel(&mut visual, Some(SAFARI), true, 18);
        assert_slider_value(&mut visual, HORIZONTAL, 36);
        // As the main window selects another mouse, General follows its fallback.
        visual.update(|_, cx| {
            AppState::apply(cx, |state| {
                let other = state
                    .devices()
                    .iter()
                    .position(|record| record.config_key != KNOWN_MOUSE_KEY)
                    .unwrap();
                state.select_device(other)
            });
        });
        assert_slider_value(&mut visual, HORIZONTAL, 14);
        visual.update(|_, cx| {
            let state = AppState::try_read(cx).unwrap();
            assert!(!state.editing_smooth_scroll_overridden());
            assert!(!state.editing_vertical_scroll_sensitivity_overridden());
            assert!(!state.editing_thumbwheel_sensitivity_overridden());
            assert_eq!(
                state.app_settings().thumbwheel_sensitivity,
                ThumbwheelSensitivity::DEFAULT
            );
        });
        click_control(&mut visual, "scroll-profile:default");
        assert_main_wheel(&mut visual, None, true, 18);
        click_control(&mut visual, "scroll-profile:app:com.example.game");
        assert_main_wheel(&mut visual, Some(GAME), false, 14);
        assert_slider_value(&mut visual, HORIZONTAL, 75);
        visual.update(|window, _| window.remove_window());
    }

    #[gpui::test]
    fn main_wheel_profile_pins_and_restores_each_inherited_value(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        cx.update(|cx| {
            gpui_kit::init(cx);
            theme::register_builtin_themes(cx);
        });
        let mut visual = open_scroll_settings(cx, Config::ephemeral(), false, &[]);
        visual.simulate_resize(size(px(920.), px(1000.)));
        add_recent_app(&mut visual, "scroll-profile:catalog-app:com.example.game");
        assert_main_wheel(&mut visual, Some(GAME), false, 14);
        click_control(&mut visual, "vertical-scroll-profile-inheritance");
        visual.update(|_, cx| {
            let state = AppState::try_read(cx).unwrap();
            assert!(state.editing_vertical_scroll_sensitivity_overridden());
            assert!(!state.editing_smooth_scroll_overridden());
        });
        click_control(&mut visual, "smooth-scroll-profile-inheritance");
        click_control(&mut visual, "thumbwheel-profile-inheritance");
        click_control(&mut visual, "scroll-profile:default");
        click_control(&mut visual, "smooth-scroll-profile-setting-control");
        set_slider(&mut visual, "Vertical scroll sensitivity", 18);
        set_slider(&mut visual, HORIZONTAL, 32);
        assert_main_wheel(&mut visual, None, true, 18);

        click_control(&mut visual, "scroll-profile:app:com.example.game");
        assert_main_wheel(&mut visual, Some(GAME), false, 14);
        click_control(&mut visual, "smooth-scroll-profile-inheritance");
        assert_main_wheel(&mut visual, Some(GAME), true, 14);
        click_control(&mut visual, "vertical-scroll-profile-inheritance");
        assert_main_wheel(&mut visual, Some(GAME), true, 18);
        assert_slider_value(&mut visual, HORIZONTAL, 14);
        click_control(&mut visual, "thumbwheel-profile-inheritance");
        assert_slider_value(&mut visual, HORIZONTAL, 32);
        visual.update(|window, cx| {
            let state = AppState::try_read(cx).unwrap();
            assert!(!state.editing_vertical_scroll_sensitivity_overridden());
            assert!(!state.editing_smooth_scroll_overridden());
            window.remove_window();
        });
    }

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
            visual.update(|window, cx| {
                use gpui_kit::test::TestWindowExt as _;
                window.within("settings-sidebar").click("input-state", cx);
                window.input(&tr!("pointer.mouse_button_profiles"), cx);
            });
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

    #[gpui::test]
    fn horizontal_sensitivity_is_reachable_in_a_narrow_settings_window(cx: &mut TestAppContext) {
        use gpui::{ScrollDelta, ScrollEvent};
        use gpui_kit::test::TestQueryExt as _;
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        cx.update(|cx| {
            gpui_kit::init(cx);
            theme::register_builtin_themes(cx);
        });
        let mut visual = open_scroll_settings(cx, Config::ephemeral(), false, &[]);
        visual.simulate_resize(size(px(640.), px(460.)));
        visual.update(gpui_kit::test::TestWindowExt::render_frame);
        add_recent_app(&mut visual, "scroll-profile:catalog-app:com.example.game");
        for _ in 0..8 {
            let bounds = visual.update(|window, _| window.find_by_label(HORIZONTAL).bounds());
            if bounds.top() >= px(40.) && bounds.bottom() <= px(440.) {
                break;
            }
            visual.simulate_event(ScrollEvent {
                position: point(px(470.), px(330.)),
                delta: ScrollDelta::Pixels(point(px(0.), px(-85.))),
                ..Default::default()
            });
            visual.update(gpui_kit::test::TestWindowExt::render_frame);
        }
        set_slider(&mut visual, HORIZONTAL, 38);
        assert_slider_value(&mut visual, HORIZONTAL, 38);
        visual.update(|_, cx| {
            let state = AppState::try_read(cx).unwrap();
            assert!(state.editing_thumbwheel_sensitivity_overridden());
            assert!(!state.editing_smooth_scroll_overridden());
            assert!(!state.editing_vertical_scroll_sensitivity_overridden());
            assert_eq!(state.editing_thumbwheel_sensitivity().into_inner(), 38);
            settings::open_at(settings::SettingsPage::WheelProfiles, cx);
        });
        visual.run_until_parked();
        visual.update(gpui_kit::test::TestWindowExt::render_frame);
        // The same General page was already mounted and vertically scrolled.
        // The shortcut restores the top of the editor and retains the saved app.
        click_control(&mut visual, "scroll-profile:default");
        click_control(&mut visual, "scroll-profile:app:com.example.game");
        visual.update(|window, cx| {
            assert_eq!(
                AppState::try_read(cx)
                    .unwrap()
                    .editing_thumbwheel_sensitivity()
                    .into_inner(),
                38
            );
            window.remove_window();
        });
    }

    #[gpui::test]
    fn searching_for_either_wheel_or_profiles_finds_the_whole_editor(cx: &mut TestAppContext) {
        use gpui_kit::test::{TestQueryExt as _, TestWindowExt as _};

        let _locale = LOCALE_LOCK.lock().unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            theme::register_builtin_themes(cx);
        });
        for (locale, vertical, horizontal, thumb, profile) in [
            (
                "en",
                "Vertical sensitivity",
                "Horizontal scroll",
                "Thumb wheel",
                "Profile",
            ),
            ("zh-CN", "垂直灵敏度", "横向滚动", "拇指滚轮", "配置文件"),
        ] {
            rust_i18n::set_locale(locale);
            let mut visual = open_scroll_settings(cx, Config::ephemeral(), false, &[]);
            visual.update(|window, cx| {
                window.within("settings-sidebar").click("input-state", cx);
            });
            for (query, show_editor) in [
                (vertical, true),
                (horizontal, true),
                (thumb, true),
                (profile, true),
                ("Profile", true),
                ("no-such-wheel-search-term", false),
            ] {
                visual.update(|window, cx| {
                    window.press(
                        if cfg!(target_os = "macos") {
                            "cmd-a"
                        } else {
                            "ctrl-a"
                        },
                        cx,
                    );
                    window.input(query, cx);
                    let shows_slider = |label: &str| {
                        window.try_find_by_label(label).is_some_and(|item| {
                            item.role() == Some(gpui::Role::Slider) && item.visible()
                        })
                    };
                    assert_eq!(
                        shows_slider(&tr!("pointer.vertical_scroll_sensitivity")),
                        show_editor,
                        "{locale}: {query}"
                    );
                    assert_eq!(
                        shows_slider(&tr!("pointer.horizontal_scroll_sensitivity")),
                        show_editor,
                        "{locale}: {query}"
                    );
                });
                assert_eq!(
                    visual
                        .debug_bounds("smooth-scroll-profile-setting-control")
                        .is_some(),
                    show_editor,
                    "{locale}: {query}"
                );
            }
            visual.update(|window, _| window.remove_window());
        }
        rust_i18n::set_locale("en");
    }
}
