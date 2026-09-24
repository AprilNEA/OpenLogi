//! The macOS battery widget opt-in and the agent's publication status.

use gpui::{App, Div, InteractiveElement as _, ParentElement as _, SharedString, Styled as _, div};
use gpui_component::{
    setting::{SettingField, SettingGroup, SettingItem},
    switch::Switch,
    v_flex,
};
use openlogi_core::device::BatteryWidgetStatus;

use crate::{
    state::AppState,
    ui::theme::{self, Typography as _},
};

pub(super) fn settings_group() -> SettingGroup {
    let keywords = [
        tr!("battery_widget.setting"),
        tr!("battery_widget.description"),
    ];
    SettingGroup::new()
        .item(
            SettingItem::new(
                tr!("battery_widget.setting"),
                SettingField::render(|_, _, cx| {
                    div()
                        .debug_selector(|| "macos-battery-widget-toggle".into())
                        .child(
                            Switch::new("macos-battery-widget")
                                .accessibility_label(tr!("battery_widget.setting"))
                                .checked(
                                    AppState::try_read(cx).is_some_and(|state| {
                                        state.app_settings().macos_battery_widget
                                    }),
                                )
                                .on_change(|enabled, _, cx| {
                                    AppState::apply(cx, |state| {
                                        state.commit_macos_battery_widget(*enabled)
                                    });
                                }),
                        )
                }),
            )
            .description(tr!("battery_widget.description")),
        )
        .item(SettingItem::render(|_, _, cx| status_footer(cx)).keywords(keywords))
}

struct StatusText {
    summary: SharedString,
    detail: Option<SharedString>,
    failed: bool,
}

impl StatusText {
    fn plain(summary: SharedString) -> Self {
        Self {
            summary,
            detail: None,
            failed: false,
        }
    }
}

fn status_text(enabled: bool, status: Option<&BatteryWidgetStatus>) -> StatusText {
    // The switch is the user's intent. A status the agent has not caught up
    // with yet must not contradict it.
    if !enabled {
        return StatusText::plain(tr!("common.off"));
    }
    match status {
        Some(BatteryWidgetStatus::Failed {
            published_devices,
            reason,
        }) => StatusText {
            summary: if *published_devices == 1 {
                tr!("battery_widget.failed_singular", count = published_devices)
            } else {
                tr!("battery_widget.failed_plural", count = published_devices)
            },
            detail: Some(reason.clone().into()),
            failed: true,
        },
        Some(BatteryWidgetStatus::Active { published_devices }) => StatusText {
            summary: if *published_devices == 1 {
                tr!("battery_widget.active_singular", count = published_devices)
            } else {
                tr!("battery_widget.active_plural", count = published_devices)
            },
            detail: (*published_devices == 0).then(|| tr!("battery_widget.empty")),
            failed: false,
        },
        Some(BatteryWidgetStatus::Unavailable { reason }) => StatusText {
            summary: tr!("battery_widget.unavailable"),
            detail: Some(reason.clone().into()),
            failed: true,
        },
        Some(BatteryWidgetStatus::Disabled) => StatusText::plain(tr!("battery_widget.starting")),
        None => StatusText::plain(tr!("battery_widget.waiting")),
    }
}

fn status_footer(cx: &App) -> Div {
    let state = AppState::try_read(cx);
    let text = status_text(
        state.is_some_and(|state| state.app_settings().macos_battery_widget),
        state
            .and_then(|state| state.agent_status())
            .map(|status| &status.battery_widget),
    );
    let pal = theme::palette(cx);
    v_flex()
        .w_full()
        .min_w_0()
        .border_t_1()
        .border_color(pal.border)
        .pt_3()
        .gap_1()
        .text_caption()
        .debug_selector(|| "macos-battery-widget-status".into())
        .child(
            div()
                .text_color(if text.failed {
                    pal.text_primary
                } else {
                    pal.text_muted
                })
                .child(text.summary),
        )
        .children(
            text.detail
                .map(|detail| div().text_color(pal.text_muted).child(detail)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_and_pending_states_do_not_claim_the_publisher_is_active() {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        assert_eq!(
            status_text(true, Some(&BatteryWidgetStatus::Disabled)).summary,
            tr!("battery_widget.starting")
        );
        assert_eq!(
            status_text(true, None).summary,
            tr!("battery_widget.waiting")
        );
        let failed = BatteryWidgetStatus::Failed {
            published_devices: 0,
            reason: "IOPSReleasePowerSource failed (0xffffffff)".into(),
        };
        for status in [None, Some(&failed)] {
            let text = status_text(false, status);
            assert_eq!(text.summary, tr!("common.off"));
            assert!(!text.failed && text.detail.is_none());
        }
    }

    #[gpui::test]
    fn the_widget_switch_updates_the_preference_by_pointer_and_keyboard(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::{
            services::assets::AssetResolver,
            state::{Sources, StateEvent},
        };
        use gpui::{
            AppContext as _, Context, IntoElement, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers,
            Render, Subscription, Window, px, size,
        };
        use gpui_component::{
            Root,
            group_box::GroupBoxVariant,
            setting::{SettingPage, Settings},
        };
        use openlogi_core::config::Config;

        struct Card {
            _subscription: Subscription,
        }
        impl Render for Card {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                Settings::new("battery-widget-test")
                    .with_group_variant(GroupBoxVariant::Fill)
                    .page(
                        SettingPage::new("General")
                            .resettable(false)
                            .group(settings_group()),
                    )
            }
        }

        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::theme::register_builtin_themes(cx);
            let (commands, _) = tokio::sync::mpsc::unbounded_channel();
            let state = cx.new(|_| {
                AppState::new(Sources::in_memory(
                    Config::ephemeral(),
                    &AssetResolver::new(),
                    commands,
                ))
            });
            AppState::set_global(state, cx);
        });
        let handle = cx.open_window(size(px(800.), px(500.)), |window, cx| {
            let card = cx.new(|cx| Card {
                _subscription: cx.subscribe(&AppState::global(cx), |_, _, _: &StateEvent, cx| {
                    cx.notify();
                }),
            });
            Root::new(card, window, cx)
        });
        let mut visual = gpui::VisualTestContext::from_window(handle.into(), cx);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let switch = visual
            .debug_bounds("macos-battery-widget-toggle")
            .expect("the native switch renders");
        visual.simulate_click(switch.center(), Modifiers::default());
        visual.update(|window, cx| {
            assert!(
                AppState::try_read(cx)
                    .unwrap()
                    .app_settings()
                    .macos_battery_widget
            );
            assert!(
                window.focused(cx).is_some(),
                "pointer activation focuses the switch"
            );
            window.draw(cx).clear(cx);
        });
        let keystroke = Keystroke::parse("space").unwrap();
        visual.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        visual.simulate_event(KeyUpEvent { keystroke });
        visual.update(|_, cx| {
            assert!(
                !AppState::try_read(cx)
                    .unwrap()
                    .app_settings()
                    .macos_battery_widget
            );
        });
    }
}
