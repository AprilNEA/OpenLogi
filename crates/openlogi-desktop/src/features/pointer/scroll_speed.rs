//! The scrolling card's scroll-speed slider.
//!
//! Each mouse can override the Settings → General default. The agent publishes
//! the selected mouse's value to the OS hook, which cannot distinguish mice.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, Render, Styled, Subscription, Window,
    div, rgb,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    slider::Slider,
    v_flex,
};
use openlogi_core::config::VerticalScrollSensitivity;

use crate::state::{AppState, DeviceRecord, StateEvent, StateEvents};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::theme::{self, ACCENT_BLUE, Typography as _};

pub struct ScrollSpeedPanel {
    slider: CommitSlider<VerticalScrollSensitivity>,
    _state_obs: Subscription,
}

impl ScrollSpeedPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let slider = CommitSlider::new(
            SliderRange::new(
                VerticalScrollSensitivity::MIN,
                VerticalScrollSensitivity::MAX,
            ),
            saved(cx),
            cx,
            |_, sensitivity, cx| {
                AppState::apply(cx, |state| {
                    if !state.current_device_is_persistent() {
                        return StateEvents::none();
                    }
                    state
                        .current_record()
                        .map(DeviceRecord::device_key)
                        .map_or_else(StateEvents::none, |key| {
                            state.commit_device_vertical_scroll_sensitivity(&key, sensitivity)
                        })
                });
            },
        );
        let state_obs = AppState::repaint_on(cx, |event| {
            matches!(
                event,
                StateEvent::SettingsChanged | StateEvent::DeviceConfigChanged(_)
            )
        });
        Self {
            slider,
            _state_obs: state_obs,
        }
    }
}

/// The selected device's effective sensitivity, or the app-wide default.
fn saved(cx: &gpui::App) -> VerticalScrollSensitivity {
    AppState::try_read(cx).map_or(VerticalScrollSensitivity::DEFAULT, |state| {
        state.current_record().map_or_else(
            || state.app_settings().vertical_scroll_sensitivity,
            |record| state.device_vertical_scroll_sensitivity(&record.config_key),
        )
    })
}

impl Render for ScrollSpeedPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let committed = saved(cx);
        let (default, has_override, persistent) = AppState::try_read(cx).map_or(
            (VerticalScrollSensitivity::DEFAULT, false, false),
            |state| {
                let has_override = state.current_record().is_some_and(|record| {
                    state.has_device_vertical_scroll_sensitivity(&record.config_key)
                });
                (
                    state.app_settings().vertical_scroll_sensitivity,
                    has_override,
                    state.current_device_is_persistent(),
                )
            },
        );
        // Re-seat after a change from Settings or a rolled-back write.
        self.slider.sync(committed, window, cx);
        let shown = self.slider.shown(committed);
        let value = shown.to_string();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_baseline()
                    .gap_4()
                    .child(
                        v_flex()
                            .child(
                                div()
                                    .text_body()
                                    .text_color(pal.text_primary)
                                    .child(tr!("pointer.vertical_scroll_sensitivity")),
                            )
                            .child(
                                div()
                                    .text_caption()
                                    .text_color(pal.text_muted)
                                    .child(tr!("pointer.vertical_scroll_sensitivity_description")),
                            ),
                    )
                    .child(
                        div()
                            .text_body()
                            .text_color(rgb(ACCENT_BLUE))
                            .whitespace_nowrap()
                            .child(value),
                    ),
            )
            .child(
                Slider::new(self.slider.slider())
                    .horizontal()
                    .disabled(!persistent),
            )
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(if has_override {
                        tr!("pointer.vertical_scroll_custom", value => default)
                    } else {
                        tr!("pointer.vertical_scroll_follows_general")
                    }),
            )
            .when(has_override && persistent, |this| {
                this.child(
                    div()
                        .debug_selector(|| "vertical-scroll-use-general".into())
                        .child(
                            Button::new("vertical-scroll-use-general")
                                .ghost()
                                .label(tr!("pointer.vertical_scroll_use_general"))
                                .on_click(|_, _, cx| {
                                    AppState::apply(cx, |state| {
                                        let default =
                                            state.app_settings().vertical_scroll_sensitivity;
                                        state
                                            .current_record()
                                            .map(DeviceRecord::device_key)
                                            .map_or_else(StateEvents::none, |key| {
                                                state.commit_device_vertical_scroll_sensitivity(
                                                    &key, default,
                                                )
                                            })
                                    });
                                }),
                        ),
                )
            })
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("pointer.vertical_scroll_selected_mouse_scope")),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, Modifiers, TestAppContext};
    use gpui_component::slider::SliderEvent;
    use openlogi_core::config::{Config, VerticalScrollSensitivity};

    use super::ScrollSpeedPanel;
    use crate::services::assets::AssetResolver;
    use crate::state::{AppState, Sources, tests::direct_inventory};

    #[gpui::test]
    fn scroll_speed_tracks_the_selected_mouse_and_general_default(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let resolver = AssetResolver::new();
            let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
            let inventories = [
                direct_inventory([1, 2, 3, 4]),
                direct_inventory([5, 6, 7, 8]),
            ];
            let state = cx.new(|_| {
                AppState::new(Sources {
                    inventories: &inventories,
                    ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
                })
            });
            AppState::set_global(state, cx);
        });
        let (panel, cx) = cx.add_window_view(|_, cx| ScrollSpeedPanel::new(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let slider = panel.read_with(cx, |panel, _| panel.slider.slider().clone());
        slider.update(cx, |_, cx| cx.emit(SliderEvent::Release(30.0.into())));
        cx.run_until_parked();
        cx.update(|_, cx| {
            let state = AppState::try_read(cx).unwrap();
            let key = &state.current_record().unwrap().config_key;
            assert!(state.has_device_vertical_scroll_sensitivity(key));
            assert_eq!(
                state.device_vertical_scroll_sensitivity(key),
                VerticalScrollSensitivity::try_new(30).unwrap()
            );
            assert_eq!(
                state.app_settings().vertical_scroll_sensitivity,
                VerticalScrollSensitivity::DEFAULT
            );
        });
        cx.update(|_, cx| AppState::apply(cx, |state| state.select_device(1)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        panel.read_with(cx, |panel, cx| {
            assert_eq!(panel.slider.value(cx), VerticalScrollSensitivity::DEFAULT);
        });
        cx.update(|_, cx| {
            AppState::apply(cx, |state| {
                state.commit_vertical_scroll_sensitivity(
                    VerticalScrollSensitivity::try_new(50).unwrap(),
                )
            });
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        panel.read_with(cx, |panel, cx| {
            assert_eq!(
                panel.slider.value(cx),
                VerticalScrollSensitivity::try_new(50).unwrap()
            );
        });
        cx.update(|_, cx| AppState::apply(cx, |state| state.select_device(0)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        panel.read_with(cx, |panel, cx| {
            assert_eq!(
                panel.slider.value(cx),
                VerticalScrollSensitivity::try_new(30).unwrap()
            );
        });
        let reset = cx
            .debug_bounds("vertical-scroll-use-general")
            .expect("custom value offers a reset button");
        cx.simulate_click(reset.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|_, cx| {
            let state = AppState::try_read(cx).unwrap();
            let key = &state.current_record().unwrap().config_key;
            assert!(!state.has_device_vertical_scroll_sensitivity(key));
            assert_eq!(
                state.device_vertical_scroll_sensitivity(key),
                VerticalScrollSensitivity::try_new(50).unwrap()
            );
            assert_eq!(state.vertical_scroll_override_count(), 0);
        });
    }
}
