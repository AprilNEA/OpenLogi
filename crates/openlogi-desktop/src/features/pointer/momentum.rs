//! Magic Mouse–style momentum scrolling for the Pointer tab.
//!
//! One switch turns it on; only then do its settings show: the distance a
//! wheel notch scrolls, then the glide — its shape, picked from curve tiles
//! ([`glide_shape`](super::glide_shape)), and its max speed, length and
//! repeat speed-up. Each slider has seven distinct stops — the default in the
//! middle — its value, and a one-line explanation; a button puts them all
//! back to their defaults. The settings are app-wide ([`MomentumScrolling`])
//! and take effect on any mouse whose Smooth wheel is on: turning the glide
//! on offers to turn on the selected mouse's Smooth wheel too, and the card
//! says so when it is off.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled, Subscription,
    Window, div, px, relative, rgb,
};
use gpui_component::WindowExt as _;
use gpui_component::button::Button;
use gpui_component::dialog::DialogButtonProps;
use gpui_component::slider::{Slider, SliderState};
use gpui_component::{Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex};
use openlogi_core::config::MomentumScrolling;

use super::glide_shape::glide_shape_picker;
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
const PIXELS_PER_NOTCH: [u16; STOPS] = [16, 20, 24, 28, 34, 42, 52];
const MAX_GLIDE_SPEEDS: [u16; STOPS] = [2500, 4000, 6000, 8000, 10000, 12500, 15000];
const GLIDE_LENGTHS: [u16; STOPS] = [150, 200, 300, 400, 550, 800, 1200];
const REPEAT_BOOSTS: [u16; STOPS] = [100, 115, 125, 135, 150, 175, 200];

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

/// One of momentum scrolling's slider settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MomentumSetting {
    PixelsPerNotch,
    MaxGlideSpeed,
    GlideLength,
    RepeatBoost,
}

impl MomentumSetting {
    #[cfg(test)]
    const ALL: [Self; 4] = [
        Self::PixelsPerNotch,
        Self::MaxGlideSpeed,
        Self::GlideLength,
        Self::RepeatBoost,
    ];

    /// The setting's seven values, low to high.
    const fn stops(self) -> &'static [u16; STOPS] {
        match self {
            Self::PixelsPerNotch => &PIXELS_PER_NOTCH,
            Self::MaxGlideSpeed => &MAX_GLIDE_SPEEDS,
            Self::GlideLength => &GLIDE_LENGTHS,
            Self::RepeatBoost => &REPEAT_BOOSTS,
        }
    }

    /// The setting's stored value in `momentum`.
    const fn get(self, momentum: &MomentumScrolling) -> u16 {
        match self {
            Self::PixelsPerNotch => momentum.pixels_per_notch,
            Self::MaxGlideSpeed => momentum.max_glide_speed,
            Self::GlideLength => momentum.glide_ms,
            Self::RepeatBoost => momentum.repeat_boost,
        }
    }

    /// Store `value` as the setting in `momentum`.
    const fn set(self, momentum: &mut MomentumScrolling, value: u16) {
        match self {
            Self::PixelsPerNotch => momentum.pixels_per_notch = value,
            Self::MaxGlideSpeed => momentum.max_glide_speed = value,
            Self::GlideLength => momentum.glide_ms = value,
            Self::RepeatBoost => momentum.repeat_boost = value,
        }
    }
}

/// One setting and its slider.
struct Setting {
    setting: MomentumSetting,
    slider: CommitSlider<u8>,
}

impl Setting {
    fn new(setting: MomentumSetting, cx: &mut Context<MomentumPanel>) -> Self {
        let stops = setting.stops();
        let slider = CommitSlider::new(
            SliderRange::new(0_u8, LAST_STOP),
            nearest_stop(stops, setting.get(&MomentumScrolling::default())),
            cx,
            move |_, index, cx| commit(cx, |m| setting.set(m, stop_value(stops, index))),
        );
        Self { setting, slider }
    }

    /// Re-seat the thumb on the committed value unless mid-drag, and answer
    /// the value to show: the committed one at rest — a hand-edited config
    /// may hold one between stops — or the stop under a dragged thumb.
    fn shown(&self, momentum: &MomentumScrolling, window: &mut Window, cx: &mut App) -> u16 {
        let stops = self.setting.stops();
        let committed = self.setting.get(momentum);
        let seated = nearest_stop(stops, committed);
        self.slider.sync(seated, window, cx);
        let index = self.slider.shown(seated);
        self.settle_thumb(index, window, cx);
        if index == seated {
            committed
        } else {
            stop_value(stops, index)
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
    pixels_per_notch: Setting,
    max_glide_speed: Setting,
    glide_length: Setting,
    repeat_boost: Setting,
    _state_obs: Subscription,
}

/// Commit the momentum settings with `change` applied.
pub(super) fn commit(cx: &mut App, change: impl FnOnce(&mut MomentumScrolling)) {
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
            pixels_per_notch: Setting::new(MomentumSetting::PixelsPerNotch, cx),
            max_glide_speed: Setting::new(MomentumSetting::MaxGlideSpeed, cx),
            glide_length: Setting::new(MomentumSetting::GlideLength, cx),
            repeat_boost: Setting::new(MomentumSetting::RepeatBoost, cx),
            _state_obs: state_obs,
        }
    }

    /// The settings shown while momentum scrolling is on: the wheel's notch
    /// distance, then the glide's.
    fn settings(
        &mut self,
        pal: Palette,
        momentum: MomentumScrolling,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // What the sliders show, a dragged thumb's stop included.
        let shown = MomentumScrolling {
            pixels_per_notch: self.pixels_per_notch.shown(&momentum, window, cx),
            max_glide_speed: self.max_glide_speed.shown(&momentum, window, cx),
            glide_ms: self.glide_length.shown(&momentum, window, cx),
            repeat_boost: self.repeat_boost.shown(&momentum, window, cx),
            ..momentum
        };
        v_flex()
            .gap_4()
            .child(group_heading(tr!("pointer.momentum_section_wheel"), pal))
            .child(slider_row(
                pal,
                tr!("pointer.momentum_pixels_per_notch"),
                format!("{} px", shown.pixels_per_notch),
                tr!("pointer.momentum_pixels_per_notch_description"),
                self.pixels_per_notch.slider.slider(),
            ))
            .child(group_heading(tr!("pointer.momentum_section_glide"), pal))
            .child(glide_shape_picker(
                momentum.glide_shape,
                &shown.params(),
                pal,
            ))
            .child(slider_row(
                pal,
                tr!("pointer.momentum_max_glide_speed"),
                format!("{} px/s", shown.max_glide_speed),
                tr!("pointer.momentum_max_glide_speed_description"),
                self.max_glide_speed.slider.slider(),
            ))
            .child(slider_row(
                pal,
                tr!("pointer.momentum_glide_length"),
                format!("{} ms", shown.glide_ms),
                tr!("pointer.momentum_glide_length_description"),
                self.glide_length.slider.slider(),
            ))
            .child(slider_row(
                pal,
                tr!("pointer.momentum_repeat_boost"),
                if shown.repeat_boost <= 100 {
                    tr!("common.off").to_string()
                } else {
                    format!("×{}", f64::from(shown.repeat_boost) / 100.0)
                },
                tr!("pointer.momentum_repeat_boost_description"),
                self.repeat_boost.slider.slider(),
            ))
            .child(h_flex().justify_end().child(reset_button(momentum)))
    }
}

/// A group's heading inside the card, set off from the group above it by a
/// hairline.
fn group_heading(label: SharedString, pal: Palette) -> impl IntoElement {
    v_flex()
        .gap_3()
        .pt_1()
        .child(div().h(px(1.)).w_full().bg(pal.border))
        .child(div().text_body().text_color(pal.text_primary).child(label))
}

impl Render for MomentumPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let (momentum, smooth_wheel, can_smooth) = AppState::try_read(cx).map_or_else(
            || (MomentumScrolling::default(), true, false),
            |state| {
                (
                    state.app_settings().momentum_scrolling,
                    state.current_smooth_wheel(),
                    state.current_hires_wheel_supported(),
                )
            },
        );
        // The glide needs this mouse's Smooth wheel: turning it on asks to
        // turn that on too, where this link can.
        let offer_smooth_wheel = !smooth_wheel && can_smooth;
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
                    .on_change(move |enabled, window, cx| {
                        if *enabled && offer_smooth_wheel {
                            confirm_turning_on_with_smooth_wheel(window, cx);
                        } else {
                            commit(cx, |m| m.enabled = *enabled);
                        }
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

/// Turning the glide on while this mouse's Smooth wheel is off: ask to turn
/// both on, since the glide does nothing without it. Cancel changes nothing.
fn confirm_turning_on_with_smooth_wheel(window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, |alert, _, _| {
        alert
            .title(tr!("pointer.momentum_turn_on_both_title"))
            .description(tr!("pointer.momentum_turn_on_both_description"))
            .button_props(
                DialogButtonProps::default()
                    .ok_text(tr!("pointer.momentum_turn_on_both"))
                    .cancel_text(tr!("common.cancel"))
                    .show_cancel(true),
            )
            .on_ok(|_, _, cx| {
                AppState::apply(cx, |state| state.commit_glide_with_smooth_wheel(true));
                true
            })
    });
}

/// Turning off this mouse's Smooth wheel while the glide is on: ask to turn
/// both off, since the glide does nothing without it. Cancel changes nothing.
pub(crate) fn confirm_turning_off_with_smooth_wheel(window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, |alert, _, _| {
        alert
            .title(tr!("pointer.momentum_turn_off_both_title"))
            .description(tr!("pointer.momentum_turn_off_both_description"))
            .button_props(
                DialogButtonProps::default()
                    .ok_text(tr!("pointer.momentum_turn_off_both"))
                    .cancel_text(tr!("common.cancel"))
                    .show_cancel(true),
            )
            .on_ok(|_, _, cx| {
                AppState::apply(cx, |state| state.commit_glide_with_smooth_wheel(false));
                true
            })
    });
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
        for setting in MomentumSetting::ALL {
            let (stops, value) = (setting.stops(), setting.get(&defaults));
            assert_eq!(nearest_stop(stops, value), MIDDLE_STOP, "{setting:?}");
            assert_eq!(stop_value(stops, MIDDLE_STOP), value);
        }
    }

    #[test]
    fn every_setting_writes_back_what_it_reads() {
        for setting in MomentumSetting::ALL {
            let mut momentum = MomentumScrolling::default();
            for &stop in setting.stops() {
                setting.set(&mut momentum, stop);
                assert_eq!(setting.get(&momentum), stop, "{setting:?}");
            }
        }
    }

    #[test]
    fn stops_rise_and_stay_in_range() {
        for stops in MomentumSetting::ALL.map(MomentumSetting::stops) {
            assert!(stops.windows(2).all(|pair| pair[0] < pair[1]), "{stops:?}");
        }
        assert!(PIXELS_PER_NOTCH[0] >= MomentumScrolling::PIXELS_PER_NOTCH_MIN);
        assert!(PIXELS_PER_NOTCH[STOPS - 1] <= MomentumScrolling::PIXELS_PER_NOTCH_MAX);
        assert!(MAX_GLIDE_SPEEDS[0] >= MomentumScrolling::MAX_GLIDE_SPEED_MIN);
        assert!(MAX_GLIDE_SPEEDS[STOPS - 1] <= MomentumScrolling::MAX_GLIDE_SPEED_MAX);
        assert!(GLIDE_LENGTHS[0] >= MomentumScrolling::GLIDE_MS_MIN);
        assert!(GLIDE_LENGTHS[STOPS - 1] <= MomentumScrolling::GLIDE_MS_MAX);
        assert!(REPEAT_BOOSTS[STOPS - 1] <= MomentumScrolling::REPEAT_BOOST_MAX);
    }

    #[test]
    fn a_value_between_stops_seats_on_the_nearest() {
        assert_eq!(nearest_stop(&GLIDE_LENGTHS, 260), 2);
        assert_eq!(nearest_stop(&REPEAT_BOOSTS, 145), 4);
        assert_eq!(nearest_stop(&MAX_GLIDE_SPEEDS, 0), 0);
        assert_eq!(nearest_stop(&MAX_GLIDE_SPEEDS, u16::MAX), LAST_STOP);
    }
}
