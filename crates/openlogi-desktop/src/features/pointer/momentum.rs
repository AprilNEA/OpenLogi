//! Momentum scrolling, for Settings → General and the Pointer tab.
//!
//! One switch turns it on; only then do its settings show: the distance a
//! wheel notch scrolls, then the glide — its shape, picked from curve tiles
//! ([`glide_shape`](super::glide_shape)), and its max speed, length and
//! repeat speed-up. Each slider has seven distinct stops — the default in the
//! middle — its value, and a one-line explanation; a button puts them all
//! back to their defaults. The settings ([`MomentumScrolling`]) take effect on
//! a mouse whose Smooth wheel is on. Settings → General edits the app-wide
//! default and counts the mice with their own; the Pointer tab edits the
//! selected mouse's own, says whether it has any, and offers to go back to
//! the default. There, turning the glide on offers to turn on the mouse's
//! Smooth wheel too, and the card says so when it is off.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Context, IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window,
    div, px, relative,
};
use gpui_component::WindowExt as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::DialogButtonProps;
use gpui_component::slider::Slider;
use gpui_component::{Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex};
use openlogi_core::config::MomentumScrolling;

use super::glide_shape::glide_shape_picker;
use crate::state::{AppState, StateEvent};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::components::{Toggle, toggle_row};
use crate::ui::slider_row::SliderRow;
use crate::ui::theme::{self, Palette, Typography as _};

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

    /// `value` as the setting's row shows it.
    fn format(self, value: u16) -> String {
        match self {
            Self::PixelsPerNotch => format!("{value} px"),
            Self::MaxGlideSpeed => format!("{value} px/s"),
            Self::GlideLength => format!("{value} ms"),
            Self::RepeatBoost if value <= 100 => tr!("common.off").to_string(),
            Self::RepeatBoost => format!("×{}", f64::from(value) / 100.0),
        }
    }
}

/// Which momentum settings a [`MomentumPanel`] edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MomentumScope {
    /// The app-wide default, in Settings → General.
    General,
    /// The selected mouse's own, on its Pointer tab.
    SelectedDevice,
}

/// One setting and its slider.
struct Setting {
    setting: MomentumSetting,
    slider: CommitSlider<u8>,
}

impl Setting {
    fn new(
        setting: MomentumSetting,
        scope: MomentumScope,
        cx: &mut Context<MomentumPanel>,
    ) -> Self {
        let stops = setting.stops();
        let slider = CommitSlider::new(
            SliderRange::new(0_u8, LAST_STOP),
            nearest_stop(stops, setting.get(&MomentumScrolling::default())),
            cx,
            move |_, index, cx| commit(cx, scope, |m| setting.set(m, stop_value(stops, index))),
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
    scope: MomentumScope,
    pixels_per_notch: Setting,
    max_glide_speed: Setting,
    glide_length: Setting,
    repeat_boost: Setting,
    _state_obs: Subscription,
}

/// Commit `scope`'s momentum settings with `change` applied.
pub(super) fn commit(
    cx: &mut App,
    scope: MomentumScope,
    change: impl FnOnce(&mut MomentumScrolling),
) {
    AppState::apply(cx, |state| match scope {
        MomentumScope::General => {
            let mut momentum = state.app_settings().momentum_scrolling;
            change(&mut momentum);
            state.commit_momentum_scrolling(momentum)
        }
        MomentumScope::SelectedDevice => {
            let mut momentum = state.current_momentum_scrolling();
            change(&mut momentum);
            state.commit_current_momentum_scrolling(momentum)
        }
    });
}

/// What a [`MomentumPanel`] renders from, read once per frame.
#[derive(Clone, Copy, Debug, Default)]
struct Snapshot {
    /// The settings the panel edits.
    momentum: MomentumScrolling,
    /// Settings → General's settings.
    general: MomentumScrolling,
    /// Whether the selected mouse has its own settings.
    has_override: bool,
    /// How many saved mice have their own settings.
    override_count: usize,
    /// Whether the selected mouse's Smooth wheel is on.
    smooth_wheel: bool,
    /// Whether this link can smooth the selected mouse's wheel.
    can_smooth: bool,
}

impl Snapshot {
    fn read(state: &AppState, scope: MomentumScope) -> Self {
        let general = state.app_settings().momentum_scrolling;
        Self {
            momentum: match scope {
                MomentumScope::General => general,
                MomentumScope::SelectedDevice => state.current_momentum_scrolling(),
            },
            general,
            has_override: state.current_has_momentum_override(),
            override_count: state.momentum_override_count(),
            smooth_wheel: state.current_smooth_wheel(),
            can_smooth: state.current_hires_wheel_supported(),
        }
    }
}

impl MomentumPanel {
    pub fn new(scope: MomentumScope, cx: &mut Context<Self>) -> Self {
        let state_obs = AppState::repaint_on(cx, |event| {
            matches!(
                event,
                StateEvent::SettingsChanged | StateEvent::DeviceConfigChanged(_)
            )
        });
        Self {
            scope,
            pixels_per_notch: Setting::new(MomentumSetting::PixelsPerNotch, scope, cx),
            max_glide_speed: Setting::new(MomentumSetting::MaxGlideSpeed, scope, cx),
            glide_length: Setting::new(MomentumSetting::GlideLength, scope, cx),
            repeat_boost: Setting::new(MomentumSetting::RepeatBoost, scope, cx),
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
                tr!("pointer.momentum_pixels_per_notch_description"),
                &self.pixels_per_notch,
                shown.pixels_per_notch,
            ))
            .child(group_heading(tr!("pointer.momentum_section_glide"), pal))
            .child(glide_shape_picker(
                self.scope,
                momentum.glide_shape,
                &shown.params(),
                pal,
            ))
            .child(slider_row(
                pal,
                tr!("pointer.momentum_max_glide_speed"),
                tr!("pointer.momentum_max_glide_speed_description"),
                &self.max_glide_speed,
                shown.max_glide_speed,
            ))
            .child(slider_row(
                pal,
                tr!("pointer.momentum_glide_length"),
                tr!("pointer.momentum_glide_length_description"),
                &self.glide_length,
                shown.glide_ms,
            ))
            .child(slider_row(
                pal,
                tr!("pointer.momentum_repeat_boost"),
                tr!("pointer.momentum_repeat_boost_description"),
                &self.repeat_boost,
                shown.repeat_boost,
            ))
            .child(
                h_flex()
                    .justify_end()
                    .child(reset_button(self.scope, momentum)),
            )
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
        let scope = self.scope;
        let shown = AppState::try_read(cx)
            .map_or_else(Snapshot::default, |state| Snapshot::read(state, scope));
        let momentum = shown.momentum;
        // On a mouse's tab the glide needs its Smooth wheel: turning it on
        // asks to turn that on too, where this link can.
        let on_device = scope == MomentumScope::SelectedDevice;
        let offer_smooth_wheel = on_device && !shown.smooth_wheel && shown.can_smooth;
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
                            commit(cx, scope, |m| m.enabled = *enabled);
                        }
                    }),
            ))
            .child(scope_line(scope, shown, pal))
            .when(
                on_device && momentum.enabled && !shown.smooth_wheel,
                |this| {
                    this.child(
                        div()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(tr!("pointer.momentum_needs_smooth_wheel")),
                    )
                },
            )
            .children(settings)
    }
}

/// Where the panel's settings come from. On a mouse's tab: whether the mouse
/// has its own, with a button back to Settings → General's, or follows
/// them. In Settings → General: how many mice have their own.
fn scope_line(scope: MomentumScope, shown: Snapshot, pal: Palette) -> impl IntoElement {
    let caption = |text: SharedString| div().text_caption().text_color(pal.text_muted).child(text);
    match scope {
        MomentumScope::SelectedDevice if shown.has_override => {
            let custom = if shown.general.enabled {
                tr!("pointer.momentum_custom_general_on")
            } else {
                tr!("pointer.momentum_custom_general_off")
            };
            h_flex()
                .gap_2()
                .items_center()
                .justify_between()
                .child(caption(custom))
                .child(
                    Button::new("momentum-use-general")
                        .ghost()
                        .small()
                        .label(tr!("pointer.momentum_use_general"))
                        .on_click(|_, _, cx| {
                            AppState::apply(cx, |state| {
                                let general = state.app_settings().momentum_scrolling;
                                state.commit_current_momentum_scrolling(general)
                            });
                        }),
                )
        }
        MomentumScope::SelectedDevice => {
            h_flex().child(caption(tr!("pointer.momentum_follows_general")))
        }
        MomentumScope::General => h_flex().child(caption(
            tr!("pointer.momentum_override_count", count => shown.override_count),
        )),
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

/// Puts every setting in `scope` back to its default, leaving the switch as
/// it is. Disabled while they already are.
fn reset_button(scope: MomentumScope, momentum: MomentumScrolling) -> impl IntoElement {
    let defaults = MomentumScrolling {
        enabled: momentum.enabled,
        ..MomentumScrolling::default()
    };
    Button::new("momentum-reset")
        .outline()
        .small()
        .label(tr!("pointer.momentum_reset"))
        .disabled(momentum == defaults)
        .on_click(move |_, _, cx| commit(cx, scope, |m| *m = defaults))
}

/// A setting's label, live value and what it does, then its slider over the
/// seven stops with a mark under each.
fn slider_row(
    pal: Palette,
    label: SharedString,
    description: SharedString,
    setting: &Setting,
    value: u16,
) -> SliderRow {
    let kind = setting.setting;
    SliderRow::new(
        label,
        v_flex()
            .gap_1()
            .child(Slider::new(setting.slider.slider()).horizontal())
            .child(stop_marks(pal)),
    )
    .value(
        kind.format(value),
        kind.stops().iter().map(|&stop| kind.format(stop)),
    )
    .description(description)
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
    use gpui::{AppContext as _, TestAppContext};
    use openlogi_core::config::Config;

    use super::*;
    use crate::services::assets::AssetResolver;
    use crate::state::{Sources, tests::direct_inventory};

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

    /// Each scope commits to its own settings: Settings → General to the
    /// app-wide default, the Pointer tab to the selected mouse's override.
    #[gpui::test]
    fn each_scope_commits_its_own_settings(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let resolver = AssetResolver::new();
            let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
            let inventories = [direct_inventory([1, 2, 3, 4])];
            let state = cx.new(|_| {
                AppState::new(Sources {
                    inventories: &inventories,
                    ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
                })
            });
            AppState::set_global(state, cx);
        });
        let (_panel, cx) =
            cx.add_window_view(|_, cx| MomentumPanel::new(MomentumScope::SelectedDevice, cx));

        cx.update(|_, cx| commit(cx, MomentumScope::SelectedDevice, |m| m.glide_ms = 1200));
        // The card now says the mouse has its own settings, with a way back.
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|_, cx| {
            let state = AppState::try_read(cx).expect("state is installed");
            assert!(state.current_has_momentum_override());
            assert_eq!(state.current_momentum_scrolling().glide_ms, 1200);
            assert_eq!(
                state.app_settings().momentum_scrolling,
                MomentumScrolling::default()
            );
        });

        cx.update(|_, cx| commit(cx, MomentumScope::General, |m| m.glide_ms = 150));
        cx.update(|_, cx| {
            let state = AppState::try_read(cx).expect("state is installed");
            assert_eq!(state.app_settings().momentum_scrolling.glide_ms, 150);
            assert_eq!(
                state.current_momentum_scrolling().glide_ms,
                1200,
                "the mouse keeps its own"
            );
            assert_eq!(state.momentum_override_count(), 1);
        });
    }
}
