//! RGB keyboard lighting controls.
//!
//! A palette of color swatches, an on/off toggle, and a brightness slider,
//! persisted per device via [`AppState::commit_lighting`] and pushed to the
//! keyboard through `openlogi_agent_core::hardware::set_lighting_in_background`
//! (the agent, over IPC — the GUI has no device I/O of its own).

use crate::ui::theme::FocusRingStyle as _;
use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, Render, Role,
    StatefulInteractiveElement as _, Styled, Subscription, Toggled, Window, div, px, rgb,
};
use gpui_kit::base::Button as BaseButton;
use gpui_kit::component::{Selectable as _, h_flex, slider::Slider, v_flex};
use openlogi_core::color::Rgb;
use openlogi_core::config::Lighting;

use crate::state::{AppState, StateEvent};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::components::{Toggle, toggle_row};
use crate::ui::slider_row::SliderRow;
use crate::ui::theme::{self, Palette};

/// The coloured face of a swatch.
const SWATCH: f32 = 24.;
/// The selection ring around a swatch, and the clear gap between the two.
const SWATCH_RING: f32 = 2.;
const SWATCH_RING_GAP: f32 = 2.;

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
        // disturbing an in-progress drag.
        self.brightness.sync(lighting.brightness, window, cx);

        let swatches: Vec<_> = PALETTE
            .iter()
            .map(|&color| swatch(color, &lighting, pal))
            .collect();

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
            // The rings' clear gaps already space the faces; a full gap on top
            // would spread the row.
            .child(h_flex().gap_1().flex_wrap().children(swatches))
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

/// One color swatch. Clicking it turns lighting on and sets that color.
///
/// The selection ring sits a gap outside the colour, the way the keypad's
/// photo keys are ringed: a ring drawn on the swatch's own edge vanishes
/// against a blue or indigo swatch. The ring also shows, muted, under the
/// pointer, so the swatches read as pressable before one is pressed.
fn swatch(color: Rgb, current: &Lighting, pal: Palette) -> impl IntoElement {
    let selected = current.enabled && current.color == color;
    let outer = SWATCH_RING + SWATCH_RING_GAP;
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
        .size(px(SWATCH + 2. * outer))
        .p(px(SWATCH_RING_GAP))
        // The ring follows the swatch's corner, one gap further out.
        .rounded(pal.control_radius + px(outer))
        .border(px(SWATCH_RING))
        .border_color(if selected {
            theme::accent()
        } else {
            gpui::transparent_black()
        })
        .hover(move |style| {
            style.border_color(if selected {
                theme::accent()
            } else {
                pal.text_muted
            })
        })
        .cursor_pointer()
        .focus_visible(move |style| style.focus_ring(pal))
        .child(
            // The hairline keeps a white or pale swatch visible on the card.
            div()
                .size_full()
                .rounded(pal.control_radius)
                .border_1()
                .border_color(pal.border)
                .bg(rgb(color.packed())),
        )
        .on_click(move |_event, _window, cx| {
            AppState::apply(cx, |state| {
                let mut next = state.lighting();
                next.enabled = true;
                next.color = color;
                state.commit_lighting(next)
            });
        })
}
