//! RGB lighting: G HUB-style effect tiles, parameters, and zone chips.

use std::time::Duration;

use gpui::{
    AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render, Role,
    StatefulInteractiveElement as _, Styled, Subscription, Task, Toggled, Window, div,
    prelude::FluentBuilder as _, px, rgb,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    Selectable as _, h_flex,
    slider::{Slider, SliderEvent, SliderState},
    v_flex,
};
use openlogi_core::color::Rgb;
use openlogi_core::config::Lighting;
use openlogi_core::hid::{LightingEffect, LightingInfo, LightingPrefab};

use crate::state::{AppState, DeviceRecord, Load, StateEvent};
use crate::ui::choice_card::ChoiceCard;
use crate::ui::components::Toggle;
use crate::ui::theme::{self, Palette, Typography as _};

const SWATCH: f32 = 28.;
const TILE_W: f32 = 118.;
const PREVIEW_H: f32 = 28.;

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
    brightness: Entity<SliderState>,
    speed: Entity<SliderState>,
    last_brightness: u8,
    last_speed: u8,
    preview_t: f32,
    _brightness_sub: Subscription,
    _speed_sub: Subscription,
    _state_obs: Subscription,
    _tick: Task<()>,
}

impl LightingPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let initial = AppState::try_read(cx).map_or_else(Lighting::default, AppState::lighting);
        let brightness = cx.new(|_| {
            SliderState::new()
                .max(100.)
                .min(0.)
                .step(5.)
                .default_value(f32::from(initial.brightness))
        });
        let speed = cx.new(|_| {
            SliderState::new()
                .max(100.)
                .min(0.)
                .step(5.)
                .default_value(f32::from(initial.speed))
        });
        let brightness_sub =
            cx.subscribe(&brightness, |_panel, _slider, event: &SliderEvent, cx| {
                if let SliderEvent::Release(value) = event {
                    commit_lighting(cx, |lighting| {
                        lighting.enabled = true;
                        lighting.brightness = clamp_percent(value.start());
                    });
                }
            });
        let speed_sub = cx.subscribe(&speed, |_panel, _slider, event: &SliderEvent, cx| {
            if let SliderEvent::Release(value) = event {
                commit_lighting(cx, |lighting| {
                    lighting.enabled = true;
                    lighting.speed = clamp_percent(value.start());
                });
            }
        });
        let state_obs = cx.subscribe(&AppState::global(cx), |_, _, event: &StateEvent, cx| {
            let relevant = match event {
                StateEvent::InventoryChanged | StateEvent::DeviceSelected(_) => true,
                StateEvent::LightingChanged(key) => AppState::try_read(cx)
                    .and_then(AppState::current_record)
                    .is_some_and(|record| record.device_key() == *key),
                _ => false,
            };
            if relevant {
                cx.notify();
            }
        });
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(80))
                    .await;
                if this
                    .update(cx, |this, cx| {
                        this.preview_t = (this.preview_t + 0.04) % 1.0;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            brightness,
            speed,
            last_brightness: initial.brightness,
            last_speed: initial.speed,
            preview_t: 0.0,
            _brightness_sub: brightness_sub,
            _speed_sub: speed_sub,
            _state_obs: state_obs,
            _tick: tick,
        }
    }

    fn sync_sliders(&mut self, lighting: &Lighting, window: &mut Window, cx: &mut Context<Self>) {
        if lighting.brightness != self.last_brightness {
            self.last_brightness = lighting.brightness;
            let value = f32::from(lighting.brightness);
            self.brightness
                .update(cx, |slider, cx| slider.set_value(value, window, cx));
        }
        if lighting.speed != self.last_speed {
            self.last_speed = lighting.speed;
            let value = f32::from(lighting.speed);
            self.speed
                .update(cx, |slider, cx| slider.set_value(value, window, cx));
        }
    }
}

fn lighting_header(enabled: bool, pal: Palette) -> impl IntoElement {
    h_flex()
        .justify_between()
        .items_center()
        .child(
            div()
                .text_body()
                .text_color(pal.text_muted)
                .child(tr!("Lighting")),
        )
        .child(
            Toggle::new("light-toggle")
                .selected(enabled)
                .on_change(|enabled, _window, cx| {
                    commit_lighting(cx, |lighting| lighting.enabled = *enabled);
                }),
        )
}

impl Render for LightingPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let lighting = AppState::try_read(cx)
            .map(AppState::lighting)
            .unwrap_or_default();
        let info = AppState::try_read(cx).map(AppState::lighting_info);
        let prefabs = match &info {
            Some(Load::Ready(info)) => info.available_prefabs(),
            _ => Vec::new(),
        };
        let selected_prefab = prefabs
            .iter()
            .find(|prefab| prefab.effect == lighting.effect)
            .copied()
            .or_else(|| prefabs.first().copied());
        self.sync_sliders(&lighting, window, cx);
        let preview_t = self.preview_t;
        v_flex()
            .gap_3()
            .w_full()
            .child(lighting_header(lighting.enabled, pal))
            .when(!prefabs.is_empty(), |this| {
                this.child(
                    h_flex().gap_2().flex_wrap().children(
                        prefabs
                            .iter()
                            .copied()
                            .map(|prefab| effect_tile(prefab, &lighting, preview_t, pal)),
                    ),
                )
            })
            .when(
                cfg!(target_os = "macos")
                    && prefabs
                        .iter()
                        .any(|prefab| prefab.effect == LightingEffect::ScreenSampler),
                |this| this.child(screen_recording_hint(pal)),
            )
            .when_some(
                selected_prefab.filter(|prefab| prefab.has_color),
                |this, _| {
                    this.child(
                        h_flex().gap_2().flex_wrap().children(
                            PALETTE
                                .iter()
                                .map(|&color| swatch(color, &lighting, pal))
                                .collect::<Vec<_>>(),
                        ),
                    )
                },
            )
            .when_some(
                selected_prefab.filter(|prefab| prefab.has_speed),
                |this, _| {
                    this.child(percent_slider(
                        tr!("Speed"),
                        lighting.speed,
                        &self.speed,
                        pal,
                    ))
                },
            )
            .when_some(
                selected_prefab.filter(|prefab| prefab.has_brightness),
                |this, _| {
                    this.child(percent_slider(
                        tr!("Brightness"),
                        lighting.brightness,
                        &self.brightness,
                        pal,
                    ))
                },
            )
            .when_some(
                info.and_then(|load| match load {
                    Load::Ready(info) if info.zones.len() > 1 => Some(info),
                    _ => None,
                }),
                |this, info| this.child(zone_chips(&lighting, &info, pal)),
            )
    }
}

fn effect_tile(prefab: LightingPrefab, lighting: &Lighting, t: f32, pal: Palette) -> ChoiceCard {
    let selected = lighting.effect == prefab.effect;
    let label = tr!(prefab.effect.label_key());
    let color = preview_rgb(prefab.effect, t, lighting.color);
    ChoiceCard::new(("light-effect", prefab.effect as u32), label.clone())
        .selected(selected)
        .w(px(TILE_W))
        .p_2()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(if selected {
            theme::accent()
        } else {
            pal.border
        })
        .bg(pal.control)
        .hover(move |style| style.bg(pal.control_hover))
        .focus_visible(move |style| style.border_color(theme::accent()))
        .child(
            v_flex()
                .gap_1()
                .w_full()
                .child(
                    div()
                        .h(px(PREVIEW_H))
                        .w_full()
                        .rounded(pal.control_radius)
                        .bg(rgb(color)),
                )
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_primary)
                        .child(label),
                ),
        )
        .on_click(move |_, _, cx| {
            commit_lighting(cx, |lighting| {
                lighting.enabled = true;
                lighting.effect = prefab.effect;
            });
        })
}

fn screen_recording_hint(pal: Palette) -> impl IntoElement {
    h_flex()
        .gap_2()
        .items_center()
        .child(div().text_caption().text_color(pal.text_muted).child(tr!(
            "Grant Screen Recording in Settings to use this effect."
        )))
        .child(
            BaseButton::new("light-open-screen-settings")
                .accessibility_label(tr!("Open"))
                .px_2()
                .py_1()
                .rounded(pal.control_radius)
                .border_1()
                .border_color(pal.border)
                .text_caption()
                .cursor_pointer()
                .bg(pal.control)
                .hover(move |style| style.bg(pal.control_hover))
                .child(tr!("Open"))
                .on_click(|_, _, _cx| {
                    #[cfg(target_os = "macos")]
                    {
                        openlogi_permissions::open_pane(
                            openlogi_permissions::Permission::ScreenRecording,
                        );
                    }
                }),
        )
}

fn zone_chips(lighting: &Lighting, info: &LightingInfo, pal: Palette) -> impl IntoElement {
    let selected = lighting.zones.clone();
    let info = info.clone();
    h_flex()
        .gap_2()
        .flex_wrap()
        .children(info.zones.clone().into_iter().map(move |zone| {
            let index = zone.index;
            let on = selected.is_empty() || selected.contains(&index);
            let info = info.clone();
            let label = tr!(zone.location.label_key());
            BaseButton::new(("light-zone", u32::from(index)))
                .role(Role::CheckBox)
                .selected(on)
                .accessibility_label(label.clone())
                .aria_toggled(if on { Toggled::True } else { Toggled::False })
                .px_2()
                .h(px(theme::CONTROL_H))
                .rounded(pal.control_radius)
                .border_1()
                .border_color(if on { theme::accent() } else { pal.border })
                .bg(if on {
                    theme::accent_tint()
                } else {
                    pal.control
                })
                .cursor_pointer()
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_primary)
                        .child(label),
                )
                .on_click(move |_, _, cx| {
                    commit_lighting(cx, |lighting| toggle_zone(lighting, &info, index));
                })
        }))
}

fn toggle_zone(lighting: &mut Lighting, info: &LightingInfo, index: u8) {
    let all: Vec<u8> = info.zones.iter().map(|zone| zone.index).collect();
    let mut selected = if lighting.zones.is_empty() {
        all.clone()
    } else {
        lighting.zones.clone()
    };
    if let Some(pos) = selected.iter().position(|zone| *zone == index) {
        if selected.len() > 1 {
            selected.remove(pos);
        }
    } else {
        selected.push(index);
        selected.sort_unstable();
    }
    lighting.zones = if selected.len() == all.len() {
        Vec::new()
    } else {
        selected
    };
}

fn percent_slider(
    caption: impl Into<gpui::SharedString>,
    value: u8,
    slider: &Entity<SliderState>,
    pal: Palette,
) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .justify_between()
                .items_baseline()
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(caption.into()),
                )
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_primary)
                        .child(format!("{value}%")),
                ),
        )
        .child(Slider::new(slider).horizontal())
}

fn commit_lighting(cx: &mut gpui::App, update: impl FnOnce(&mut Lighting)) {
    AppState::update(cx, |state, cx| {
        let key = state.current_record().map(DeviceRecord::device_key);
        let mut lighting = state.lighting();
        update(&mut lighting);
        state.commit_lighting(lighting);
        if let Some(key) = key {
            cx.emit(StateEvent::LightingChanged(key));
        }
    });
}

fn preview_rgb(effect: LightingEffect, t: f32, color: Rgb) -> u32 {
    match effect {
        LightingEffect::Solid | LightingEffect::LightOnPress | LightingEffect::EchoPress => {
            color.packed()
        }
        LightingEffect::ColorCycle
        | LightingEffect::ColorWave
        | LightingEffect::SpectrumPulse
        | LightingEffect::Neon
        | LightingEffect::Ocean => hue(t),
        LightingEffect::Breathing | LightingEffect::Pulsar => dim(color, pulse(t)),
        LightingEffect::Starlight | LightingEffect::SmoothStars | LightingEffect::OuterSpace => {
            if (t * 11.0).fract() > 0.7 {
                0x00ff_ffff
            } else {
                0x0008_1020
            }
        }
        LightingEffect::Ripple | LightingEffect::SmoothWave | LightingEffect::Tide => dim(
            color,
            0.35 + 0.65 * ((t * std::f32::consts::TAU).sin().abs()),
        ),
        LightingEffect::Lightning => {
            if (t * 7.0).fract() > 0.92 {
                0x00ff_ffff
            } else {
                dim(color, 0.15)
            }
        }
        LightingEffect::VerticalFade | LightingEffect::Contrast => hue((t + 0.33) % 1.0),
        LightingEffect::RedWhiteBlue => {
            if t < 1.0 / 3.0 {
                0x00c4_1e3a
            } else if t < 2.0 / 3.0 {
                0x00ff_ffff
            } else {
                0x0000_52a5
            }
        }
        LightingEffect::ScreenSampler => hue((t * 0.2) % 1.0),
        LightingEffect::AudioVisualizer => dim(Rgb::new(0x00, 0xa2, 0xff), pulse(t)),
    }
}

fn pulse(t: f32) -> f32 {
    0.35 + 0.65 * (t * std::f32::consts::TAU).sin().abs()
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "preview dimming maps a 0–1 amount onto 8-bit RGB"
)]
fn dim(color: Rgb, amount: f32) -> u32 {
    let (red, green, blue) = color.components();
    let amount = amount.clamp(0.0, 1.0);
    let scale = |channel: u8| (f32::from(channel) * amount) as u8;
    u32::from(scale(red)) << 16 | u32::from(scale(green)) << 8 | u32::from(scale(blue))
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "preview hue sweep maps a unit phase onto 8-bit RGB"
)]
fn hue(phase: f32) -> u32 {
    let wrapped = phase.rem_euclid(1.0) * 6.0;
    let sector = wrapped.floor();
    let frac = wrapped - sector;
    let inverse = 1.0 - frac;
    let (red, green, blue) = match sector as i32 {
        0 => (1.0, frac, 0.0),
        1 => (inverse, 1.0, 0.0),
        2 => (0.0, 1.0, frac),
        3 => (0.0, inverse, 1.0),
        4 => (frac, 0.0, 1.0),
        _ => (1.0, 0.0, inverse),
    };
    let to = |channel: f32| (channel * 255.0) as u8;
    u32::from(to(red)) << 16 | u32::from(to(green)) << 8 | u32::from(to(blue))
}

fn swatch(color: Rgb, current: &Lighting, pal: Palette) -> impl IntoElement {
    let selected = current.enabled && current.color == color;
    BaseButton::new(("light-swatch", color.packed()))
        .role(Role::RadioButton)
        .selected(selected)
        .accessibility_label(format!("{} #{:06X}", tr!("Lighting"), color.packed()))
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
            commit_lighting(cx, |lighting| {
                lighting.enabled = true;
                lighting.color = color;
            });
        })
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is rounded and clamped into 0..=100 before the cast"
)]
fn clamp_percent(raw: f32) -> u8 {
    raw.clamp(0., 100.).round() as u8
}
