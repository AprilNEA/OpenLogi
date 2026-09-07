//! Headset audio controls panel (Sidetone slider).
//!
//! Managed completely through the agent over IPC, respecting process isolation.

use gpui::{Context, IntoElement, ParentElement, Render, Styled, Subscription, Window, div};
use gpui_component::{h_flex, slider::Slider, v_flex};
use openlogi_core::hid::DeviceRoute;

use crate::state::AppState;
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::theme::{self, Typography as _};

pub struct AudioPanel {
    slider: CommitSlider<u8>,
    committed_level: u8,
    active_route: Option<DeviceRoute>,
    read_flight: u64,
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
                if let Some(route) = panel.active_route.clone() {
                    AppState::update(cx, |state, _cx| {
                        state.set_sidetone(route, level);
                    });
                }
            },
        );

        let state_obs = AppState::repaint_on(cx, |_| false);

        Self {
            slider,
            committed_level: initial,
            active_route: None,
            read_flight: 0,
            _state_obs: state_obs,
        }
    }

    fn sync_with_device(&mut self, cx: &mut Context<Self>) {
        let current_route = AppState::try_read(cx)
            .and_then(AppState::current_record)
            .and_then(|r| r.route.clone());

        if self.active_route != current_route {
            self.active_route.clone_from(&current_route);
            self.read_flight = self.read_flight.wrapping_add(1);
            let flight = self.read_flight;
            if let Some(route) = current_route {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if let Some(state) = AppState::try_read(cx) {
                    state.read_sidetone(route.clone(), tx);
                }
                cx.spawn(async move |panel, cx| {
                    if let Ok(Ok(level)) = rx.await {
                        let _ = panel.update(&mut *cx, |panel, cx| {
                            if panel.read_flight == flight
                                && panel.active_route.as_ref() == Some(&route)
                            {
                                panel.committed_level = level;
                                cx.notify();
                            }
                        });
                    }
                })
                .detach();
            }
        }
    }
}

impl Render for AudioPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_with_device(cx);
        let pal = theme::palette(cx);
        self.slider.sync(self.committed_level, window, cx);
        let shown_level = self.slider.shown(self.committed_level);

        v_flex()
            .gap_4()
            .w_full()
            .child(
                h_flex()
                    .justify_between()
                    .items_baseline()
                    .child(
                        v_flex()
                            .child(div().text_body().child("Sidetone (Voice Loopback)"))
                            .child(
                                div()
                                    .text_caption()
                                    .text_color(pal.text_muted)
                                    .child("Microphone feedback in earcups"),
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
    }
}
