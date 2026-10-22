//! Magic Mouse–style momentum scrolling for the Pointer tab.
//!
//! One switch turns it on; only then do its settings show. Each is a slider
//! with seven distinct stops — the default in the middle — its value, and a
//! one-line explanation; a button puts them all back to their defaults. The
//! settings are app-wide ([`MomentumScrolling`]) and take effect on any mouse
//! whose Smooth wheel is on, so the card says so when the selected mouse's
//! is off.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled, Subscription,
    Window, div, px, relative, rgb,
};
use gpui_component::button::Button;
use gpui_component::slider::{Slider, SliderState};
use gpui_component::{Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex};
use openlogi_core::config::MomentumScrolling;

use crate::state::{AppState, StateEvent};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::components::{Toggle, toggle_row};
use crate::ui::section::section_label;
use crate::ui::theme::{self, ACCENT_BLUE, Palette, Typography as _};

/// Stops on every setting's slider.
const STOPS: usize = 7;
/// The last stop's index.
#[expect(clippy::cast_possible_truncation, reason = "seven stops")]
const LAST_STOP: u8 = (STOPS - 1) as u8;

/// Each setting's seven values, low to high, with its default in the middle.
const GLIDE_SPEEDS: [u16; STOPS] = [800, 1100, 1500, 2000, 2700, 3600, 5000];
const GLIDE_LENGTHS: [u16; STOPS] = [100, 160, 220, 300, 400, 550, 750];
const FRICTIONS: [u16; STOPS] = [0, 4, 7, 10, 15, 25, 40];
const RESPONSES: [u16; STOPS] = [14, 16, 18, 20, 22, 24, 26];
const REPEAT_BOOSTS: [u16; STOPS] = [100, 115, 125, 135, 150, 165, 180];
const PIXELS_PER_NOTCH: [u16; STOPS] = [16, 20, 24, 28, 34, 42, 52];
const SPRING_BACKS: [u16; STOPS] = [0, 40, 70, 100, 140, 190, 250];

/// The stop nearest `value`.
fn nearest_stop(stops: &[u16; STOPS], value: u16) -> u8 {
    let index = stops
        .iter()
        .enumerate()
        .min_by_key(|(_, stop)| stop.abs_diff(value))
        .map_or(0, |(index, _)| index);
    u8::try_from(index).unwrap_or(LAST_STOP)
}

/// The value at stop `index`.
fn stop_value(stops: &[u16; STOPS], index: u8) -> u16 {
    stops[usize::from(index.min(LAST_STOP))]
}

/// One setting: its stops, how to read and write it, and its slider.
struct Setting {
    stops: &'static [u16; STOPS],
    get: fn(&MomentumScrolling) -> u16,
    slider: CommitSlider<u8>,
}

impl Setting {
    fn new(
        stops: &'static [u16; STOPS],
        get: fn(&MomentumScrolling) -> u16,
        set: fn(&mut MomentumScrolling, u16),
        cx: &mut Context<MomentumPanel>,
    ) -> Self {
        let slider = CommitSlider::new(
            SliderRange::new(0_u8, LAST_STOP),
            nearest_stop(stops, get(&MomentumScrolling::default())),
            cx,
            move |_, index, cx| commit(cx, |m| set(m, stop_value(stops, index))),
        );
        Self { stops, get, slider }
    }

    /// Re-seat the thumb on the committed value unless mid-drag, and answer
    /// the value to show: the committed one at rest — a hand-edited config
    /// may hold one between stops — or the stop under a dragged thumb.
    fn shown(&self, momentum: &MomentumScrolling, window: &mut Window, cx: &mut App) -> u16 {
        let committed = (self.get)(momentum);
        let seated = nearest_stop(self.stops, committed);
        self.slider.sync(seated, window, cx);
        let index = self.slider.shown(seated);
        self.settle_thumb(index, window, cx);
        if index == seated {
            committed
        } else {
            stop_value(self.stops, index)
        }
    }

    /// Put the thumb exactly on stop `index`. The slider snaps its value to
    /// whole stops but draws the thumb wherever the pointer is, during a drag
    /// and after the release; re-seating it every frame makes the thumb jump
    /// from stop to stop instead.
    fn settle_thumb(&self, index: u8, window: &mut Window, cx: &mut App) {
        let state = self.slider.slider();
        let at = f32::from(index) / f32::from(LAST_STOP);
        if (state.read(cx).percentage().end - at).abs() > f32::EPSILON {
            state.update(cx, |state, cx| {
                state.set_value(f32::from(index), window, cx);
            });
        }
    }
}

pub struct MomentumPanel {
    glide_speed: Setting,
    glide_length: Setting,
    friction: Setting,
    response: Setting,
    repeat_boost: Setting,
    pixels_per_notch: Setting,
    spring_back: Setting,
    _state_obs: Subscription,
}

/// Commit the momentum settings with `change` applied.
fn commit(cx: &mut App, change: impl FnOnce(&mut MomentumScrolling)) {
    AppState::apply(cx, |state| {
        let mut momentum = state.app_settings().momentum_scrolling;
        change(&mut momentum);
        state.commit_momentum_scrolling(momentum)
    });
}

impl MomentumPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = AppState::repaint_on(cx, |event| {
            matches!(
                event,
                StateEvent::SettingsChanged | StateEvent::DeviceConfigChanged(_)
            )
        });
        Self {
            glide_speed: Setting::new(
                &GLIDE_SPEEDS,
                |m| m.glide_speed,
                |m, v| m.glide_speed = v,
                cx,
            ),
            glide_length: Setting::new(&GLIDE_LENGTHS, |m| m.glide_ms, |m, v| m.glide_ms = v, cx),
            friction: Setting::new(
                &FRICTIONS,
                |m| u16::from(m.friction),
                |m, v| m.friction = u8::try_from(v).unwrap_or(MomentumScrolling::FRICTION_MAX),
                cx,
            ),
            response: Setting::new(
                &RESPONSES,
                |m| u16::from(m.response),
                |m, v| m.response = u8::try_from(v).unwrap_or(MomentumScrolling::RESPONSE_MAX),
                cx,
            ),
            repeat_boost: Setting::new(
                &REPEAT_BOOSTS,
                |m| m.repeat_boost,
                |m, v| m.repeat_boost = v,
                cx,
            ),
            pixels_per_notch: Setting::new(
                &PIXELS_PER_NOTCH,
                |m| m.pixels_per_notch,
                |m, v| m.pixels_per_notch = v,
                cx,
            ),
            spring_back: Setting::new(
                &SPRING_BACKS,
                |m| m.spring_back_ms,
                |m, v| m.spring_back_ms = v,
                cx,
            ),
            _state_obs: state_obs,
        }
    }

    /// The settings shown while momentum scrolling is on.
    fn settings(
        &mut self,
        pal: Palette,
        momentum: MomentumScrolling,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let off_or = |value: u16, shown: String| {
            if value == 0 {
                tr!("common.off").to_string()
            } else {
                shown
            }
        };
        let glide_speed = self.glide_speed.shown(&momentum, window, cx);
        let glide_length = self.glide_length.shown(&momentum, window, cx);
        let friction = self.friction.shown(&momentum, window, cx);
        let response = self.response.shown(&momentum, window, cx);
        let repeat_boost = self.repeat_boost.shown(&momentum, window, cx);
        let pixels_per_notch = self.pixels_per_notch.shown(&momentum, window, cx);
        let spring_back = self.spring_back.shown(&momentum, window, cx);
        v_flex()
            .gap_4()
            .child(slider_row(
                pal,
                tr!("pointer.momentum_glide_speed"),
                format!("{glide_speed} px/s"),
                tr!("pointer.momentum_glide_speed_description"),
                self.glide_speed.slider.slider(),
            ))
            .child(slider_row(
                pal,
                tr!("pointer.momentum_glide_length"),
                off_or(glide_length, format!("{glide_length} ms")),
                tr!("pointer.momentum_glide_length_description"),
                self.glide_length.slider.slider(),
            ))
            .child(slider_row(
                pal,
                tr!("pointer.momentum_friction"),
                off_or(friction, format!("{friction}%")),
                tr!("pointer.momentum_friction_description"),
                self.friction.slider.slider(),
            ))
            .child(slider_row(
                pal,
                tr!("pointer.momentum_response"),
                format!("×{:.1}", f64::from(response) / 10.0),
                tr!("pointer.momentum_response_description"),
                self.response.slider.slider(),
            ))
            .child(slider_row(
                pal,
                tr!("pointer.momentum_repeat_boost"),
                if repeat_boost <= 100 {
                    tr!("common.off").to_string()
                } else {
                    format!("×{:.2}", f64::from(repeat_boost) / 100.0)
                },
                tr!("pointer.momentum_repeat_boost_description"),
                self.repeat_boost.slider.slider(),
            ))
            .child(slider_row(
                pal,
                tr!("pointer.momentum_pixels_per_notch"),
                format!("{pixels_per_notch} px"),
                tr!("pointer.momentum_pixels_per_notch_description"),
                self.pixels_per_notch.slider.slider(),
            ))
            .child(slider_row(
                pal,
                tr!("pointer.momentum_spring_back"),
                off_or(spring_back, format!("{spring_back} ms")),
                tr!("pointer.momentum_spring_back_description"),
                self.spring_back.slider.slider(),
            ))
            .child(h_flex().justify_end().child(reset_button(momentum)))
    }
}

impl Render for MomentumPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let (momentum, smooth_wheel) = AppState::try_read(cx).map_or_else(
            || (MomentumScrolling::default(), true),
            |state| {
                (
                    state.app_settings().momentum_scrolling,
                    state.current_smooth_wheel(),
                )
            },
        );
        let settings = momentum
            .enabled
            .then(|| self.settings(pal, momentum, window, cx));
        v_flex()
            .gap_4()
            .w_full()
            .child(toggle_row(
                "momentum-scrolling-row",
                pal,
                tr!("pointer.momentum"),
                tr!("pointer.momentum_description"),
                Toggle::new("momentum-scrolling-toggle")
                    .selected(momentum.enabled)
                    .on_change(move |enabled, _window, cx| {
                        commit(cx, |m| m.enabled = *enabled);
                    }),
            ))
            .when(momentum.enabled && !smooth_wheel, |this| {
                this.child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(tr!("pointer.momentum_needs_smooth_wheel")),
                )
            })
            .children(settings)
    }
}

/// Puts every setting back to its default, leaving the switch as it is.
/// Disabled while they already are.
fn reset_button(momentum: MomentumScrolling) -> impl IntoElement {
    let defaults = MomentumScrolling {
        enabled: momentum.enabled,
        ..MomentumScrolling::default()
    };
    Button::new("momentum-reset")
        .outline()
        .small()
        .label(tr!("pointer.momentum_reset"))
        .disabled(momentum == defaults)
        .on_click(move |_, _, cx| commit(cx, |m| *m = defaults))
}

/// A setting's label and live value, its slider over the seven stops with a
/// mark under each, and what it does.
fn slider_row(
    pal: Palette,
    label: SharedString,
    value: String,
    description: SharedString,
    slider: &Entity<SliderState>,
) -> impl IntoElement {
    v_flex()
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .items_baseline()
                .gap_4()
                .child(section_label(label, pal))
                .child(div().text_body().text_color(rgb(ACCENT_BLUE)).child(value)),
        )
        .child(
            v_flex()
                .gap_1()
                .child(Slider::new(slider).horizontal())
                .child(stop_marks(pal)),
        )
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(description),
        )
}

/// A mark under each of the slider's stops. The slider centres its thumb on
/// its track's fraction, so a mark at the same fraction sits under it.
fn stop_marks(pal: Palette) -> impl IntoElement {
    div()
        .relative()
        .w_full()
        .h(px(6.))
        .children((0..=LAST_STOP).map(|stop| {
            div()
                .absolute()
                .top_0()
                .left(relative(f32::from(stop) / f32::from(LAST_STOP)))
                .ml(px(-1.))
                .w(px(2.))
                .h_full()
                .rounded_full()
                .bg(pal.text_muted.opacity(0.6))
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The middle stop, where every default sits.
    const MIDDLE_STOP: u8 = LAST_STOP / 2;

    #[test]
    fn every_default_is_the_middle_stop() {
        let defaults = MomentumScrolling::default();
        for (stops, value) in [
            (&GLIDE_SPEEDS, defaults.glide_speed),
            (&GLIDE_LENGTHS, defaults.glide_ms),
            (&FRICTIONS, u16::from(defaults.friction)),
            (&RESPONSES, u16::from(defaults.response)),
            (&REPEAT_BOOSTS, defaults.repeat_boost),
            (&PIXELS_PER_NOTCH, defaults.pixels_per_notch),
            (&SPRING_BACKS, defaults.spring_back_ms),
        ] {
            assert_eq!(nearest_stop(stops, value), MIDDLE_STOP, "{stops:?}");
            assert_eq!(stop_value(stops, MIDDLE_STOP), value);
        }
    }

    #[test]
    fn stops_rise_and_stay_in_range() {
        for stops in [
            &GLIDE_SPEEDS,
            &GLIDE_LENGTHS,
            &FRICTIONS,
            &RESPONSES,
            &REPEAT_BOOSTS,
            &PIXELS_PER_NOTCH,
            &SPRING_BACKS,
        ] {
            assert!(stops.windows(2).all(|pair| pair[0] < pair[1]), "{stops:?}");
        }
        assert!(GLIDE_SPEEDS[0] >= MomentumScrolling::GLIDE_SPEED_MIN);
        assert!(GLIDE_SPEEDS[STOPS - 1] <= MomentumScrolling::GLIDE_SPEED_MAX);
        assert!(GLIDE_LENGTHS[STOPS - 1] <= MomentumScrolling::GLIDE_MS_MAX);
        assert!(FRICTIONS[STOPS - 1] <= u16::from(MomentumScrolling::FRICTION_MAX));
        assert!(RESPONSES[0] >= u16::from(MomentumScrolling::RESPONSE_MIN));
        assert!(RESPONSES[STOPS - 1] <= u16::from(MomentumScrolling::RESPONSE_MAX));
        assert!(REPEAT_BOOSTS[STOPS - 1] <= MomentumScrolling::REPEAT_BOOST_MAX);
        assert!(PIXELS_PER_NOTCH[0] >= MomentumScrolling::PIXELS_PER_NOTCH_MIN);
        assert!(PIXELS_PER_NOTCH[STOPS - 1] <= MomentumScrolling::PIXELS_PER_NOTCH_MAX);
        assert!(SPRING_BACKS[STOPS - 1] <= MomentumScrolling::SPRING_BACK_MS_MAX);
    }

    #[test]
    fn a_value_between_stops_seats_on_the_nearest() {
        assert_eq!(nearest_stop(&GLIDE_LENGTHS, 234), 2);
        assert_eq!(nearest_stop(&REPEAT_BOOSTS, 145), 4);
        assert_eq!(nearest_stop(&GLIDE_SPEEDS, 0), 0);
        assert_eq!(nearest_stop(&GLIDE_SPEEDS, u16::MAX), LAST_STOP);
    }
}
