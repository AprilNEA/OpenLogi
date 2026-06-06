//! Basic RGB lighting controls for a keyboard's detail panel.
//!
//! A palette of color swatches, an on/off toggle, and a brightness slider,
//! persisted per device via [`AppState::commit_lighting`] and pushed to the
//! keyboard through [`openlogi_agent_core::hardware::set_lighting_in_background`].

use gpui::{
    AnyElement, AppContext as _, BorrowAppContext as _, Context, Entity, InteractiveElement,
    IntoElement, ParentElement, Render, StatefulInteractiveElement as _, Styled, Subscription,
    Window, div, px, rgb,
};
use gpui_component::{
    h_flex,
    slider::{Slider, SliderEvent, SliderState},
    v_flex,
};
use openlogi_core::config::Lighting;

use crate::state::AppState;
use crate::theme::{self, ACCENT_BLUE, Palette};

const SWATCH: f32 = 28.;

/// Preset colors as 6-hex `"RRGGBB"`. Deliberately small — covering the common
/// keyboard accent colors.
const PALETTE: &[&str] = &[
    "ff3b30", "ff9500", "ffcc00", "34c759", "00c7be", "007aff", "5856d6", "af52de", "ffffff",
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
        let initial = cx
            .try_global::<AppState>()
            .map_or(100, |s| s.lighting().brightness);
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
                    cx.update_global::<AppState, _>(|state, _| {
                        let mut lighting = state.lighting();
                        lighting.enabled = true;
                        lighting.brightness = pct;
                        state.commit_lighting(lighting);
                    });
                    cx.notify();
                }
            });
        let state_obs = cx.observe_global::<AppState>(|_, cx| cx.notify());
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
        let lighting = cx
            .try_global::<AppState>()
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

        let swatches: Vec<AnyElement> = PALETTE
            .iter()
            .enumerate()
            .map(|(idx, hex)| swatch(idx, hex, &lighting, pal))
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
                            .text_sm()
                            .text_color(pal.text_muted)
                            .child(tr!("LIGHTING")),
                    )
                    .child(toggle(&lighting, pal)),
            )
            .child(h_flex().gap_2().flex_wrap().children(swatches))
            .child(
                h_flex()
                    .justify_between()
                    .items_baseline()
                    .child(
                        div()
                            .text_xs()
                            .text_color(pal.text_muted)
                            .child(tr!("BRIGHTNESS")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(ACCENT_BLUE))
                            .child(format!("{}%", lighting.brightness)),
                    ),
            )
            .child(Slider::new(&self.brightness).horizontal())
    }
}

/// One color swatch. Clicking it turns lighting on and sets that color.
fn swatch(idx: usize, hex: &'static str, current: &Lighting, pal: Palette) -> AnyElement {
    let selected = current.enabled && current.color.eq_ignore_ascii_case(hex);
    div()
        .id(("light-swatch", idx))
        .size(px(SWATCH))
        .rounded_md()
        .border_2()
        .border_color(if selected {
            rgb(ACCENT_BLUE).into()
        } else {
            pal.border
        })
        .bg(rgb(parse_hex(hex)))
        .cursor_pointer()
        .on_click(move |_event, _window, cx| {
            cx.update_global::<AppState, _>(|state, _| {
                let mut next = state.lighting();
                next.enabled = true;
                next.color = hex.to_string();
                state.commit_lighting(next);
            });
            cx.refresh_windows();
        })
        .into_any_element()
}

/// On/off pill.
fn toggle(current: &Lighting, pal: Palette) -> AnyElement {
    let on = current.enabled;
    div()
        .id("light-toggle")
        .px_2()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(if on {
            rgb(ACCENT_BLUE).into()
        } else {
            pal.border
        })
        .text_xs()
        .text_color(if on {
            rgb(ACCENT_BLUE).into()
        } else {
            pal.text_muted
        })
        .cursor_pointer()
        .child(if on { tr!("On") } else { tr!("Off") })
        .on_click(|_event, _window, cx| {
            cx.update_global::<AppState, _>(|state, _| {
                let mut next = state.lighting();
                next.enabled = !next.enabled;
                state.commit_lighting(next);
            });
            cx.refresh_windows();
        })
        .into_any_element()
}

/// Snap a raw slider read to a 0–100 brightness percent.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is rounded and clamped into 0..=100 before the cast"
)]
fn clamp_brightness(raw: f32) -> u8 {
    raw.clamp(0., 100.).round() as u8
}

/// Parse `"RRGGBB"` to a `0xRRGGBB` int for `rgb()`. Falls back to white.
fn parse_hex(hex: &str) -> u32 {
    u32::from_str_radix(hex, 16).unwrap_or(0x00ff_ffff)
}
