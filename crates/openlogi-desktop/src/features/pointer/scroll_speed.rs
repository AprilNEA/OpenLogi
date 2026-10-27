//! The scrolling card's scroll-speed slider.
//!
//! The same app-wide [`VerticalScrollSensitivity`] as Settings → General,
//! shown beside the other wheel controls the way Logi Options+ shows its
//! scroll speed. Both sliders commit through
//! [`AppState::commit_vertical_scroll_sensitivity`] and re-seat from the saved
//! value, so a change made in one shows in the other.

use gpui::{Context, IntoElement, ParentElement, Render, Styled, Subscription, Window, div, rgb};
use gpui_component::{h_flex, slider::Slider, v_flex};
use openlogi_core::config::VerticalScrollSensitivity;

use crate::state::{AppState, StateEvent};
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
                    state.commit_vertical_scroll_sensitivity(sensitivity)
                });
            },
        );
        let state_obs =
            AppState::repaint_on(cx, |event| matches!(event, StateEvent::SettingsChanged));
        Self {
            slider,
            _state_obs: state_obs,
        }
    }
}

/// The saved app-wide sensitivity, or the default before state is ready.
fn saved(cx: &gpui::App) -> VerticalScrollSensitivity {
    AppState::try_read(cx).map_or(VerticalScrollSensitivity::DEFAULT, |state| {
        state.app_settings().vertical_scroll_sensitivity
    })
}

impl Render for ScrollSpeedPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let committed = saved(cx);
        // Re-seat after a change from Settings or a rolled-back write.
        self.slider.sync(committed, window, cx);
        let shown = self.slider.shown(committed);
        let value = if shown == VerticalScrollSensitivity::DEFAULT {
            format!("{shown} ({})", rust_i18n::t!("common.default"))
        } else {
            shown.to_string()
        };
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
            .child(Slider::new(self.slider.slider()).horizontal())
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};
    use gpui_component::slider::SliderEvent;
    use openlogi_core::config::{Config, VerticalScrollSensitivity};

    use super::ScrollSpeedPanel;
    use crate::services::assets::AssetResolver;
    use crate::state::{AppState, Sources};

    fn saved_speed(cx: &mut gpui::VisualTestContext) -> VerticalScrollSensitivity {
        cx.update(|_, cx| {
            AppState::try_read(cx)
                .expect("state is installed")
                .app_settings()
                .vertical_scroll_sensitivity
        })
    }

    /// The card's slider and the Settings slider edit one app-wide value: a
    /// release here saves it, and a change made there moves this thumb.
    #[gpui::test]
    fn the_card_slider_and_settings_share_one_scroll_speed(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let resolver = AssetResolver::new();
            let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
            let state = cx.new(|_| {
                AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands))
            });
            AppState::set_global(state, cx);
        });
        let (panel, cx) = cx.add_window_view(|_, cx| ScrollSpeedPanel::new(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(saved_speed(cx), VerticalScrollSensitivity::DEFAULT);

        let slider = panel.read_with(cx, |panel, _| panel.slider.slider().clone());
        slider.update(cx, |_, cx| cx.emit(SliderEvent::Release(30.0.into())));
        cx.run_until_parked();
        assert_eq!(
            saved_speed(cx),
            VerticalScrollSensitivity::from_rounded(30.0)
        );

        cx.update(|_, cx| {
            AppState::apply(cx, |state| {
                state.commit_vertical_scroll_sensitivity(VerticalScrollSensitivity::from_rounded(
                    50.0,
                ))
            });
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        panel.read_with(cx, |panel, cx| {
            assert_eq!(
                panel.slider.value(cx),
                VerticalScrollSensitivity::from_rounded(50.0),
                "a change made in Settings re-seats the card's thumb"
            );
        });
    }
}
