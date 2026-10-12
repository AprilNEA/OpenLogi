//! Headset audio controls panel (Sidetone slider).
//!
//! Managed completely through the agent over IPC, respecting process isolation.

use gpui::{
    AnyElement, App, Context, IntoElement, ParentElement, Render, Styled, Subscription, Window, div,
};
use gpui_component::{h_flex, slider::Slider, v_flex};

use crate::state::{AppState, DeviceKey, SidetoneLoad, StateEvent};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, Palette, Typography as _};

pub struct AudioPanel {
    slider: CommitSlider<u8>,
    committed_level: u8,
    _state_obs: Subscription,
}

impl AudioPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let initial = 0_u8;
        let slider = CommitSlider::previewing(
            SliderRange::new(0, 100).step(5.),
            initial,
            cx,
            |_, _, cx| cx.notify(),
            |panel, level, cx| {
                panel.committed_level = level;
                AppState::apply(cx, |state| state.commit_sidetone(level));
            },
        );

        let state_obs = AppState::repaint_on(cx, |e| matches!(e, StateEvent::SidetoneChanged(_)));

        Self {
            slider,
            committed_level: initial,
            _state_obs: state_obs,
        }
    }

    fn render_slider_content(
        &mut self,
        level: u8,
        pal: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.slider.sync(level, window, cx);
        let shown_level = self.slider.shown(level);

        v_flex()
            .gap_4()
            .w_full()
            .child(
                h_flex()
                    .justify_between()
                    .items_baseline()
                    .child(
                        v_flex()
                            .child(div().text_body().child(tr!("audio.sidetone")))
                            .child(
                                div()
                                    .text_caption()
                                    .text_color(pal.text_muted)
                                    .child(tr!("audio.sidetone_description")),
                            ),
                    )
                    .child(
                        div()
                            .text_caption()
                            .text_color(pal.text_primary)
                            .child(format!("{shown_level}%")),
                    ),
            )
            .child(Slider::new(self.slider.slider()).horizontal())
            .into_any_element()
    }
}

impl Render for AudioPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let state = AppState::try_read(cx);
        let sidetone_load = state
            .as_ref()
            .map(|s| s.current_sidetone_load())
            .unwrap_or_default();
        let current_key = state.as_ref().and_then(|s| {
            s.current_record()
                .map(crate::state::DeviceRecord::device_key)
        });

        match sidetone_load {
            SidetoneLoad::Ready(level) => {
                self.committed_level = *level;
                self.render_slider_content(*level, pal, window, cx)
            }
            SidetoneLoad::Loading | SidetoneLoad::Unknown | SidetoneLoad::Unsupported(_) => {
                status_line(tr!("audio.sidetone"), pal).into_any_element()
            }
            SidetoneLoad::Failed(_) => retry_line(
                "sidetone-read-retry",
                tr!("audio.sidetone_read_failed"),
                pal,
                retry_sidetone_closure(current_key),
            )
            .into_any_element(),
        }
    }
}

fn retry_sidetone_closure(key: Option<DeviceKey>) -> impl Fn(&mut App) + 'static {
    move |cx| {
        if key.is_some() {
            AppState::apply(cx, AppState::retry_current_sidetone);
        }
    }
}
