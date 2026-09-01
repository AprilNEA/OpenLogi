//! RGB keyboard lighting controls.
//!
//! A palette of color swatches, an on/off toggle, and a brightness slider,
//! persisted per device via [`AppState::commit_lighting`] and pushed to the
//! keyboard through `openlogi_agent_core::hardware::set_lighting_in_background`
//! (the agent, over IPC — the GUI has no device I/O of its own).

use gpui::{
    AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render, Role,
    StatefulInteractiveElement as _, Styled, Subscription, Toggled, Window, div, px, rgb,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    Selectable as _, h_flex,
    slider::{SliderEvent, SliderState},
    v_flex,
};
use openlogi_core::color::Rgb;
use openlogi_core::config::Lighting;

use crate::state::{AppState, DeviceRecord, StateEvent};
use crate::ui::components::{AccessibleSlider, Toggle};
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
    brightness: Entity<SliderState>,
    /// Last brightness pushed into the slider from `AppState`. A change here
    /// (device switch, swatch/toggle that re-reads config) means the slider
    /// must be resynced; an unchanged value during a drag must not, or we'd
    /// fight the user's in-progress drag (which only commits on release).
    last_brightness: u8,
    _brightness_sub: Subscription,
    _state_obs: Subscription,
}

impl LightingPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let initial = AppState::try_read(cx).map_or(100, |s| s.lighting().brightness);
        let brightness = cx.new(|_| {
            SliderState::new()
                .max(100.)
                .min(0.)
                .step(5.)
                .default_value(f32::from(initial))
        });
        // The slider drives the device only on release, to avoid streaming a
        // frame burst to the keyboard for every intermediate drag value.
        let brightness_sub =
            cx.subscribe(&brightness, |_panel, _slider, event: &SliderEvent, cx| {
                if let SliderEvent::Release(value) = event {
                    let pct = clamp_brightness(value.start());
                    AppState::update(cx, |state, cx| {
                        let key = state.current_record().map(DeviceRecord::device_key);
                        let mut lighting = state.lighting();
                        lighting.enabled = true;
                        lighting.brightness = pct;
                        state.commit_lighting(lighting);
                        if let Some(key) = key {
                            cx.emit(StateEvent::LightingChanged(key));
                        }
                    });
                    cx.notify();
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
        Self {
            brightness,
            last_brightness: initial,
            _brightness_sub: brightness_sub,
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

        // Pull the slider thumb to the active device's brightness whenever it
        // changed in `AppState` (device switch / external edit), without
        // disturbing an in-progress drag — see `last_brightness`.
        if lighting.brightness != self.last_brightness {
            self.last_brightness = lighting.brightness;
            let value = f32::from(lighting.brightness);
            self.brightness
                .update(cx, |slider, cx| slider.set_value(value, window, cx));
        }

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
                            .child(tr!("Lighting")),
                    )
                    .child(
                        Toggle::new("light-toggle")
                            .selected(lighting.enabled)
                            .accessibility_label(tr!("Lighting"))
                            .on_change(|enabled, _window, cx| {
                                AppState::update(cx, |state, cx| {
                                    let key = state.current_record().map(DeviceRecord::device_key);
                                    let mut next = state.lighting();
                                    next.enabled = *enabled;
                                    state.commit_lighting(next);
                                    if let Some(key) = key {
                                        cx.emit(StateEvent::LightingChanged(key));
                                    }
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
                            .child(tr!("Brightness")),
                    )
                    .child(
                        div()
                            .text_caption()
                            .text_color(pal.text_primary)
                            .child(format!("{}%", lighting.brightness)),
                    ),
            )
            .child(
                AccessibleSlider::new(&self.brightness)
                    .horizontal()
                    .accessibility_label(tr!("Brightness")),
            )
    }
}

/// One color swatch. Clicking it turns lighting on and sets that color.
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
            AppState::update(cx, |state, cx| {
                let key = state.current_record().map(DeviceRecord::device_key);
                let mut next = state.lighting();
                next.enabled = true;
                next.color = color;
                state.commit_lighting(next);
                if let Some(key) = key {
                    cx.emit(StateEvent::LightingChanged(key));
                }
            });
        })
}

/// Snap a raw slider read to a 0–100 brightness percent.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is rounded and clamped into 0..=100 before the cast"
)]
fn clamp_brightness(raw: f32) -> u8 {
    raw.clamp(0., 100.).round() as u8
}
