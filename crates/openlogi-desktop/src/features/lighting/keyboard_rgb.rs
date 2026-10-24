//! RGB keyboard lighting controls.
//!
//! A palette of color swatches, an on/off toggle, a brightness slider and, on
//! a device that can animate, an effect picker with its cycle length,
//! persisted per device via [`AppState::commit_lighting`] and pushed to the
//! keyboard through `openlogi_agent_core::hardware::set_lighting_in_background`
//! (the agent, over IPC — the GUI has no device I/O of its own).

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, Render, Role,
    StatefulInteractiveElement as _, Styled, Subscription, Toggled, Window, div, px, rgb,
};
use gpui_base::Button as BaseButton;
use gpui_component::button::{Button, ButtonGroup};
use gpui_component::{Selectable as _, h_flex, slider::Slider, v_flex};
use openlogi_core::color::Rgb;
use openlogi_core::config::{Lighting, LightingEffect};

use crate::state::{AppState, StateEvent};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::components::Toggle;
use crate::ui::theme::{self, Palette, Typography as _};

const SWATCH: f32 = 28.;

/// Preset colors. Deliberately small — covering the common keyboard accent
/// colors.
const PALETTE: &[Rgb] = &[
    Rgb::new(0xff, 0x3b, 0x30),
    Rgb::new(0xff, 0x95, 0x00),
    Rgb::new(0xff, 0xcc, 0x00),
    Rgb::new(0x34, 0xc7, 0x59),
    Rgb::new(0x00, 0xc7, 0xbe),
    Rgb::new(0x00, 0x7a, 0xff),
    Rgb::new(0x58, 0x56, 0xd6),
    Rgb::new(0xaf, 0x52, 0xde),
    Rgb::WHITE,
];

pub struct LightingPanel {
    brightness: CommitSlider<u8>,
    period: CommitSlider<u16>,
    _state_obs: Subscription,
}

impl LightingPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let initial = AppState::try_read(cx).map_or(100, |s| s.lighting().brightness);
        // The slider drives the device only on release, to avoid streaming a
        // frame burst to the keyboard for every intermediate drag value.
        let brightness = CommitSlider::new(
            SliderRange::new(0, 100).step(5.),
            initial,
            cx,
            |_, pct, cx| {
                AppState::apply(cx, |state| {
                    let mut lighting = state.lighting();
                    lighting.enabled = true;
                    lighting.brightness = pct;
                    state.commit_lighting(lighting)
                });
            },
        );
        let initial_period = AppState::try_read(cx).map_or_else(
            || Lighting::default().effect_period_ms,
            |s| s.lighting().effect_period_ms,
        );
        let (shortest, longest) = (
            *Lighting::EFFECT_PERIOD_MS.start(),
            *Lighting::EFFECT_PERIOD_MS.end(),
        );
        let period = CommitSlider::new(
            SliderRange::new(shortest, longest).step(500.),
            initial_period,
            cx,
            |_, period_ms, cx| {
                AppState::apply(cx, |state| {
                    let mut lighting = state.lighting();
                    lighting.enabled = true;
                    lighting.effect_period_ms = period_ms;
                    state.commit_lighting(lighting)
                });
            },
        );
        let state_obs =
            AppState::repaint_on(cx, |event| matches!(event, StateEvent::LightingChanged(_)));
        Self {
            brightness,
            period,
            _state_obs: state_obs,
        }
    }
}

impl Render for LightingPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let lighting = AppState::try_read(cx)
            .map(AppState::lighting)
            .unwrap_or_default();
        let animates = AppState::try_read(cx).is_some_and(AppState::lighting_effects_supported);

        // Pull the slider thumb to the active device's brightness whenever it
        // changed in `AppState` (device switch / external edit), without
        // disturbing an in-progress drag.
        self.brightness.sync(lighting.brightness, window, cx);
        self.period.sync(lighting.effect_period_ms, window, cx);

        let swatches: Vec<_> = PALETTE
            .iter()
            .map(|&color| swatch(color, &lighting, pal))
            .collect();

        v_flex()
            .gap_3()
            .w_full()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_body()
                            .text_color(pal.text_muted)
                            .child(tr!("device.lighting")),
                    )
                    .child(
                        Toggle::new("light-toggle")
                            .selected(lighting.enabled)
                            .on_change(|enabled, _window, cx| {
                                AppState::apply(cx, |state| {
                                    let mut next = state.lighting();
                                    next.enabled = *enabled;
                                    state.commit_lighting(next)
                                });
                            }),
                    ),
            )
            .child(h_flex().gap_2().flex_wrap().children(swatches))
            .child(
                h_flex()
                    .justify_between()
                    .items_baseline()
                    .child(
                        div()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(tr!("camera.brightness")),
                    )
                    .child(
                        div()
                            .text_caption()
                            .text_color(pal.text_primary)
                            .child(format!("{}%", self.brightness.shown(lighting.brightness))),
                    ),
            )
            .child(Slider::new(self.brightness.slider()).horizontal())
            .when(animates, |panel| {
                panel
                    .child(
                        div()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(tr!("lighting.effect")),
                    )
                    .child(effect_segment(lighting.effect))
            })
            .when(
                animates && lighting.effect != LightingEffect::Static,
                |panel| {
                    panel
                        .child(
                            h_flex()
                                .justify_between()
                                .items_baseline()
                                .child(
                                    div()
                                        .text_caption()
                                        .text_color(pal.text_muted)
                                        .child(tr!("lighting.effect_period")),
                                )
                                .child(
                                    div().text_caption().text_color(pal.text_primary).child(
                                        seconds(self.period.shown(lighting.effect_period_ms)),
                                    ),
                                ),
                        )
                        .child(Slider::new(self.period.slider()).horizontal())
                },
            )
    }
}

/// Static / Breathing / Color cycle. Clicking one turns lighting on and plays
/// that effect.
///
/// Each button commits its own effect: `ButtonGroup::on_click` runs only on a
/// pointer click on the group, so Enter and Space on a focused button would
/// not reach it.
fn effect_segment(current: LightingEffect) -> ButtonGroup {
    const EFFECTS: [(LightingEffect, &str); 3] = [
        (LightingEffect::Static, "lighting-effect-static"),
        (LightingEffect::Breathing, "lighting-effect-breathing"),
        (LightingEffect::ColorCycle, "lighting-effect-color-cycle"),
    ];
    ButtonGroup::new("lighting-effect")
        .w_full()
        .outline()
        .children(EFFECTS.map(|(effect, id)| {
            let label = match effect {
                LightingEffect::Static => tr!("lighting.effect_static"),
                LightingEffect::Breathing => tr!("lighting.effect_breathing"),
                LightingEffect::ColorCycle => tr!("lighting.effect_color_cycle"),
            };
            Button::new(id)
                .flex_1()
                .label(label)
                .selected(current == effect)
                .debug_selector(move || id.into())
                .on_click(move |_, _, cx| {
                    AppState::apply(cx, |state| {
                        let mut next = state.lighting();
                        next.enabled = true;
                        next.effect = effect;
                        state.commit_lighting(next)
                    });
                })
        }))
}

/// A cycle length in seconds, to the half second the slider steps by.
fn seconds(period_ms: u16) -> String {
    format!("{:.1} s", f32::from(period_ms) / 1000.)
}

/// One color swatch. Clicking it turns lighting on and sets that color.
fn swatch(color: Rgb, current: &Lighting, pal: Palette) -> impl IntoElement {
    let selected = current.enabled && current.color == color;
    BaseButton::new(("light-swatch", color.packed()))
        .role(Role::RadioButton)
        .selected(selected)
        .accessibility_label(format!(
            "{} #{:06X}",
            tr!("device.lighting"),
            color.packed()
        ))
        .aria_toggled(if selected {
            Toggled::True
        } else {
            Toggled::False
        })
        .aria_selected(selected)
        .size(px(SWATCH))
        .rounded(pal.control_radius)
        .border_2()
        .border_color(if selected {
            theme::accent()
        } else {
            pal.border
        })
        .bg(rgb(color.packed()))
        .cursor_pointer()
        .focus_visible(|style| style.border_color(theme::accent()))
        .on_click(move |_event, _window, cx| {
            AppState::apply(cx, |state| {
                let mut next = state.lighting();
                next.enabled = true;
                next.color = color;
                state.commit_lighting(next)
            });
        })
}
