//! RGB keyboard lighting controls.
//!
//! A palette of color swatches, an on/off toggle, and a brightness slider,
//! with the keyboard's photo for per-key colors when it has one (see
//! [`super::per_key`]), persisted per device via [`AppState::commit_lighting`] and pushed to the
//! keyboard through `openlogi_agent_core::hardware::set_lighting_in_background`
//! (the agent, over IPC — the GUI has no device I/O of its own).

use std::collections::BTreeSet;

use crate::ui::theme::FocusRingStyle as _;
use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, Render, Role,
    StatefulInteractiveElement as _, Styled, Subscription, Toggled, Window, div,
    prelude::FluentBuilder as _, px, rgb,
};
use gpui_kit::base::Button as BaseButton;
use gpui_kit::component::{
    Disableable as _, Selectable as _, Sizable as _,
    button::{Button, ButtonGroup},
    h_flex,
    slider::Slider,
    v_flex,
};
use openlogi_core::color::Rgb;
use openlogi_core::config::{Lighting, LightingEffect, LightingKey, WaveDirection};

use super::per_key::{has_firmware_effects, key_color, per_key_photo, photo_editor};
use crate::state::{AppState, StateEvent};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::components::{Toggle, toggle_row};
use crate::ui::slider_row::SliderRow;
use crate::ui::theme::{self, Palette, SelectableStyle as _, Typography as _};

const SWATCH: f32 = 28.;

/// The effects the selector offers, in its order.
const EFFECTS: [LightingEffect; 5] = [
    LightingEffect::Static,
    LightingEffect::Breathing,
    LightingEffect::Cycle,
    LightingEffect::Ripple,
    LightingEffect::Wave,
];

/// The wave's subtle, medium and strong depths, in percent.
const WAVE_DEPTHS: [u8; 3] = [30, 50, 75];
/// The wave's narrow, medium and wide bands, in percent of the keyboard.
const WAVE_WIDTHS: [u8; 3] = [35, 60, 85];
const WAVE_DIRECTIONS: [WaveDirection; 2] =
    [WaveDirection::LeftToRight, WaveDirection::RightToLeft];

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
    /// Keys picked on the photo; the swatches color these instead of the
    /// whole keyboard while any is picked.
    selected: BTreeSet<LightingKey>,
    /// The device [`Self::selected`] was picked on, so a device switch
    /// starts with nothing picked.
    selection_device: Option<String>,
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
        let state_obs =
            AppState::repaint_on(cx, |event| matches!(event, StateEvent::LightingChanged(_)));
        Self {
            brightness,
            selected: BTreeSet::new(),
            selection_device: None,
            _state_obs: state_obs,
        }
    }

    /// Pick `key` on the photo, or drop it when already picked.
    pub(crate) fn toggle_key(&mut self, key: LightingKey) {
        if !self.selected.remove(&key) {
            self.selected.insert(key);
        }
    }
}

impl Render for LightingPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let state = AppState::try_read(cx);
        let lighting = state.map(AppState::lighting).unwrap_or_default();
        let record = state.and_then(AppState::current_record);
        let effects = record.is_some_and(has_firmware_effects);
        let effect = if effects {
            lighting.effect
        } else {
            LightingEffect::Static
        };
        // Keys are picked for the still colours they keep, alone or held over
        // a drawn wave.
        let photo = record
            .and_then(per_key_photo)
            .filter(|_| matches!(effect, LightingEffect::Static | LightingEffect::Wave));
        let device = record.map(|record| record.config_key.clone());
        if self.selection_device != device || photo.is_none() {
            self.selected.clear();
            self.selection_device = device;
        }

        // Pull the slider thumb to the active device's brightness whenever it
        // changed in `AppState` (device switch / external edit), without
        // disturbing an in-progress drag.
        self.brightness.sync(lighting.brightness, window, cx);

        // The cycle turns through every hue, so it takes no colour.
        let swatches: Vec<_> = if effect == LightingEffect::Cycle {
            Vec::new()
        } else {
            PALETTE
                .iter()
                .map(|&color| swatch(color, &lighting, &self.selected, pal))
                .collect()
        };
        let effect_rows = effects.then(|| effect_rows(effect, &lighting, pal, cx));
        let editor = photo.map(|photo| {
            v_flex()
                .gap_2()
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(tr!("lighting.per_key_hint")),
                )
                .child(photo_editor(&photo, &lighting, &self.selected, cx))
                .child(per_key_actions(&lighting, &self.selected, cx))
        });

        v_flex()
            .gap_3()
            .w_full()
            // The tab and the card already say "Lighting"; the row names what
            // the switch does to the keyboard instead.
            .child(toggle_row(
                "light-row",
                pal,
                tr!("device.light_the_keys"),
                tr!("lighting.keyboard_lighting_description"),
                Toggle::new("light-toggle")
                    .selected(lighting.enabled)
                    .on_change(|enabled, _window, cx| {
                        AppState::apply(cx, |state| {
                            let mut next = state.lighting();
                            next.enabled = *enabled;
                            state.commit_lighting(next)
                        });
                    }),
            ))
            .children(effect_rows)
            .children(editor)
            .child(h_flex().gap_2().flex_wrap().children(swatches))
            .child(
                SliderRow::new(
                    tr!("camera.brightness"),
                    Slider::new(self.brightness.slider()).horizontal(),
                )
                .value(
                    format!("{}%", self.brightness.shown(lighting.brightness)),
                    ["100%"],
                ),
            )
    }
}

/// The effect selector and the running effect's settings.
fn effect_rows(
    effect: LightingEffect,
    lighting: &Lighting,
    pal: Palette,
    cx: &gpui::App,
) -> impl IntoElement + use<> {
    v_flex()
        .gap_3()
        .child(labelled(
            tr!("lighting.effect"),
            effect_control(effect, cx),
            pal,
        ))
        .when(effect != LightingEffect::Static, |rows| {
            rows.child(labelled(
                tr!("lighting.speed"),
                speed_control(lighting, cx),
                pal,
            ))
        })
        .when(effect == LightingEffect::Wave, |rows| {
            let shape = lighting.wave;
            rows.child(labelled(
                tr!("lighting.wave_depth"),
                choice(
                    "wave-depth",
                    [
                        tr!("lighting.depth_subtle"),
                        tr!("lighting.depth_medium"),
                        tr!("lighting.depth_strong"),
                    ],
                    WAVE_DEPTHS,
                    shape.depth,
                    |lighting, depth| lighting.wave.depth = depth,
                    cx,
                ),
                pal,
            ))
            .child(labelled(
                tr!("lighting.wave_width"),
                choice(
                    "wave-width",
                    [
                        tr!("lighting.width_narrow"),
                        tr!("lighting.width_medium"),
                        tr!("lighting.width_wide"),
                    ],
                    WAVE_WIDTHS,
                    shape.width,
                    |lighting, width| lighting.wave.width = width,
                    cx,
                ),
                pal,
            ))
            .child(labelled(
                tr!("lighting.wave_direction"),
                choice(
                    "wave-direction",
                    [
                        tr!("lighting.direction_left_to_right"),
                        tr!("lighting.direction_right_to_left"),
                    ],
                    WAVE_DIRECTIONS,
                    shape.direction,
                    |lighting, direction| lighting.wave.direction = direction,
                    cx,
                ),
                pal,
            ))
        })
}

/// One segment per option; picking one sets it with `set` and commits.
fn choice<T: Copy + PartialEq + 'static, const N: usize>(
    id: &'static str,
    labels: [gpui::SharedString; N],
    values: [T; N],
    current: T,
    set: fn(&mut Lighting, T),
    cx: &gpui::App,
) -> impl IntoElement + use<T, N> {
    values
        .iter()
        .zip(labels)
        .enumerate()
        .fold(
            ButtonGroup::new(id).w_full().outline(),
            |group, (index, (&value, label))| {
                group.child(
                    Button::new((id, index))
                        .flex_1()
                        .label(label)
                        .selected(current == value)
                        .selected_segment(current == value, cx),
                )
            },
        )
        .on_click(move |indices, _window, cx| {
            let Some(&value) = indices.first().and_then(|index| values.get(*index)) else {
                return;
            };
            AppState::apply(cx, |state| {
                let mut next = state.lighting();
                next.enabled = true;
                set(&mut next, value);
                state.commit_lighting(next)
            });
        })
}

/// A caption over `control`.
fn labelled(label: impl IntoElement, control: impl IntoElement, pal: Palette) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(div().text_caption().text_color(pal.text_muted).child(label))
        .child(control)
}

/// One segment per firmware effect; picking one starts it at its own pace.
fn effect_control(effect: LightingEffect, cx: &gpui::App) -> impl IntoElement + use<> {
    let labels = [
        tr!("lighting.effect_static"),
        tr!("lighting.effect_breathing"),
        tr!("lighting.effect_cycle"),
        tr!("lighting.effect_ripple"),
        tr!("lighting.effect_wave"),
    ];
    EFFECTS
        .iter()
        .zip(labels)
        .enumerate()
        .fold(
            ButtonGroup::new("lighting-effect").w_full().outline(),
            |group, (index, (&value, label))| {
                group.child(
                    Button::new(("lighting-effect", index))
                        .flex_1()
                        .label(label)
                        .selected(effect == value)
                        .selected_segment(effect == value, cx),
                )
            },
        )
        .on_click(|indices, _window, cx| {
            let Some(&value) = indices.first().and_then(|index| EFFECTS.get(*index)) else {
                return;
            };
            AppState::apply(cx, |state| {
                let mut next = state.lighting();
                next.enabled = true;
                next.effect = value;
                next.period_ms = None;
                state.commit_lighting(next)
            });
        })
}

/// Slow, medium and fast paces for `effect`, in milliseconds per round. The
/// medium one is the effect's own pace.
fn speed_presets(effect: LightingEffect) -> [u16; 3] {
    let medium = effect.default_period_ms();
    [medium.saturating_mul(2), medium, medium / 2]
}

/// Slow, medium and fast for the running effect.
fn speed_control(lighting: &Lighting, cx: &gpui::App) -> impl IntoElement + use<> {
    let presets = speed_presets(lighting.effect);
    let current = lighting
        .period_ms
        .unwrap_or_else(|| lighting.effect.default_period_ms());
    let labels = [
        tr!("lighting.speed_slow"),
        tr!("lighting.speed_medium"),
        tr!("lighting.speed_fast"),
    ];
    presets
        .iter()
        .zip(labels)
        .enumerate()
        .fold(
            ButtonGroup::new("lighting-speed").w_full().outline(),
            |group, (index, (&period, label))| {
                group.child(
                    Button::new(("lighting-speed", index))
                        .flex_1()
                        .label(label)
                        .selected(current == period)
                        .selected_segment(current == period, cx),
                )
            },
        )
        .on_click(move |indices, _window, cx| {
            let Some(&period) = indices.first().and_then(|index| presets.get(*index)) else {
                return;
            };
            AppState::apply(cx, |state| {
                let mut next = state.lighting();
                next.enabled = true;
                next.period_ms = Some(period);
                state.commit_lighting(next)
            });
        })
}

/// The buttons under the photo: drop the picked keys, give them back the
/// keyboard's color, or give every key back the keyboard's color.
fn per_key_actions(
    lighting: &Lighting,
    selected: &BTreeSet<LightingKey>,
    cx: &mut Context<LightingPanel>,
) -> impl IntoElement {
    let picked: Vec<LightingKey> = selected.iter().copied().collect();
    let picked_have_colors = picked.iter().any(|key| lighting.keys.contains_key(key));
    h_flex()
        .gap_2()
        .flex_wrap()
        .child(
            Button::new("per-key-clear-selection")
                .small()
                .label(tr!("lighting.clear_selection"))
                .disabled(picked.is_empty())
                .on_click(cx.listener(|panel, _event, _window, cx| {
                    panel.selected.clear();
                    cx.notify();
                })),
        )
        .child(
            Button::new("per-key-use-keyboard-color")
                .small()
                .label(tr!("lighting.use_keyboard_color"))
                .disabled(!picked_have_colors)
                .on_click(move |_event, _window, cx| {
                    AppState::apply(cx, |state| {
                        let mut next = state.lighting();
                        for key in &picked {
                            next.keys.remove(key);
                        }
                        state.commit_lighting(next)
                    });
                }),
        )
        .child(
            Button::new("per-key-reset-all")
                .small()
                .label(tr!("lighting.reset_all_keys"))
                .disabled(lighting.keys.is_empty())
                .on_click(|_event, _window, cx| {
                    AppState::apply(cx, |state| {
                        let mut next = state.lighting();
                        next.keys.clear();
                        state.commit_lighting(next)
                    });
                }),
        )
}

/// One color swatch. Clicking it turns lighting on and sets that color: on
/// the keys picked on the photo when any is, else on the whole keyboard.
fn swatch(
    color: Rgb,
    current: &Lighting,
    picked: &BTreeSet<LightingKey>,
    pal: Palette,
) -> impl IntoElement {
    let selected = current.enabled
        && if picked.is_empty() {
            current.color == color
        } else {
            picked.iter().all(|&key| key_color(current, key) == color)
        };
    let picked: Vec<LightingKey> = picked.iter().copied().collect();
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
        .focus_visible(move |style| style.focus_ring(pal))
        .on_click(move |_event, _window, cx| {
            AppState::apply(cx, |state| {
                let mut next = state.lighting();
                next.enabled = true;
                if picked.is_empty() {
                    next.color = color;
                } else {
                    for &key in &picked {
                        next.keys.insert(key, color);
                    }
                }
                state.commit_lighting(next)
            });
        })
}
