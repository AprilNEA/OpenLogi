//! Headset audio controls panel (Sidetone slider).
//!
//! Managed completely through the agent over IPC, respecting process isolation.

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription,
    Window, div,
};
use gpui_component::{
    h_flex, v_flex,
    slider::{Slider, SliderEvent, SliderState},
};
use openlogi_core::hid::DeviceRoute;

use crate::state::{AppState, StateEvent};
use crate::ui::theme::{self, Typography as _};

pub struct AudioPanel {
    sidetone: Entity<SliderState>,
    last_level: u8,
    synced_level: u8,
    active_route: Option<DeviceRoute>,
    _sidetone_sub: Subscription,
    _state_obs: Subscription,
}

impl AudioPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let initial = 0u8;
        let sidetone = cx.new(|_| {
            SliderState::new()
                .max(100.)
                .min(0.)
                .step(5.)
                .default_value(f32::from(initial))
        });

        let sidetone_sub = cx.subscribe(&sidetone, |panel, _slider, event: &SliderEvent, cx| {
            if let SliderEvent::Release(value) = event {
                #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to 0..=100")]
                let level = value.start().clamp(0., 100.).round() as u8;
                panel.last_level = level;
                if let Some(route) = panel.active_route.clone() {
                    AppState::update(cx, |state, _cx| {
                        state.set_sidetone(route, level);
                    });
                }
                cx.notify();
            }
        });

        let state_obs = cx.subscribe(&AppState::global(cx), |panel, _, event: &StateEvent, cx| {
            if matches!(event, StateEvent::InventoryChanged | StateEvent::DeviceSelected(_)) {
                panel.sync_with_device(cx);
            }
        });

        let mut panel = Self {
            sidetone,
            last_level: initial,
            synced_level: initial,
            active_route: None,
            _sidetone_sub: sidetone_sub,
            _state_obs: state_obs,
        };
        panel.sync_with_device(cx);
        panel
    }

    fn sync_with_device(&mut self, cx: &mut Context<Self>) {
        let current_route = AppState::try_read(cx)
            .and_then(AppState::current_record)
            .and_then(|r| r.route.clone());

        if self.active_route != current_route {
            self.active_route.clone_from(&current_route);
            if let Some(route) = current_route {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if let Some(state) = AppState::try_read(cx) {
                    state.read_sidetone(route, tx);
                }
                cx.spawn(async move |panel, cx| {
                    if let Ok(Ok(level)) = rx.await {
                        let _ = panel.update(&mut *cx, |panel, cx| {
                            panel.synced_level = level;
                            panel.last_level = level;
                            cx.notify();
                        });
                    }
                }).detach();
            }
        }
    }
}

impl Render for AudioPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let level = self.last_level;

        if self.last_level != self.synced_level {
            self.last_level = self.synced_level;
            let val = f32::from(self.synced_level);
            self.sidetone.update(cx, |slider, cx| {
                slider.set_value(val, window, cx);
            });
        }

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
                            .child(format!("{level}%")),
                    ),
            )
            .child(Slider::new(&self.sidetone).horizontal())
    }
}
