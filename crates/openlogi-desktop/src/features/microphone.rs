//! Controls for a USB microphone: mic gain and mute, headphone volume and
//! mute, and mic monitoring — the hardware settings of G HUB's Yeti page.
//!
//! Every control is shown only when the agent reported it, and every value is
//! the microphone's own reading: a write goes to the agent, and the slider
//! follows what the microphone read back.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, IntoElement, ParentElement, Render, Styled, Subscription, Window, div, px, rgb,
};
use gpui_component::{Selectable as _, h_flex, slider::Slider, v_flex};
use openlogi_core::microphone::{
    GainStage, Microphone, MicrophoneChange, MicrophoneError, MilliDecibels,
};

use crate::state::{AppState, StateEvent};
use crate::ui::commit_slider::{CommitSlider, SliderRange, SliderUnit};
use crate::ui::components::Toggle;
use crate::ui::theme::{self, ACCENT_BLUE, Palette, Typography as _};

/// How finely the level sliders move, in dB. The microphone settles the
/// value on its own grid (1.5 dB for the Yeti's gain) and the slider follows
/// what it read back.
const SLIDER_STEP_DB: f32 = 0.5;

impl SliderUnit for MilliDecibels {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a gain in dB fits f32 with room to spare"
    )]
    fn to_slider(self) -> f32 {
        self.decibels() as f32
    }

    fn from_slider(raw: f32) -> Self {
        Self::from_decibels(f64::from(raw)).unwrap_or_default()
    }
}

/// The three level controls, each a slider shaped by the range the
/// microphone reported.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Gain,
    Headphones,
    Monitoring,
}

impl Level {
    fn stage(self, microphone: &Microphone) -> Option<GainStage> {
        let controls = &microphone.controls;
        match self {
            Self::Gain => controls.gain,
            Self::Headphones => controls.headphone_volume,
            Self::Monitoring => controls.monitoring_level,
        }
    }

    fn change(self, level: MilliDecibels) -> MicrophoneChange {
        match self {
            Self::Gain => MicrophoneChange::Gain(level),
            Self::Headphones => MicrophoneChange::HeadphoneVolume(level),
            Self::Monitoring => MicrophoneChange::MonitoringLevel(level),
        }
    }
}

/// One level slider and the microphone and range it was shaped for.
struct LevelSlider {
    id: String,
    min: MilliDecibels,
    max: MilliDecibels,
    slider: CommitSlider<MilliDecibels>,
}

/// The microphone panel of the device-detail screen.
pub struct MicrophonePanel {
    gain: Option<LevelSlider>,
    headphones: Option<LevelSlider>,
    monitoring: Option<LevelSlider>,
    _state_obs: Subscription,
}

impl MicrophonePanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs =
            AppState::repaint_on(cx, |event| matches!(event, StateEvent::MicrophoneChanged));
        Self {
            gain: None,
            headphones: None,
            monitoring: None,
            _state_obs: state_obs,
        }
    }

    fn slot(&mut self, level: Level) -> &mut Option<LevelSlider> {
        match level {
            Level::Gain => &mut self.gain,
            Level::Headphones => &mut self.headphones,
            Level::Monitoring => &mut self.monitoring,
        }
    }

    /// Shape `level`'s slider for `microphone`, keeping the one already built
    /// for the same microphone and range, then seat it on the reading.
    fn sync_slider(
        &mut self,
        level: Level,
        microphone: &Microphone,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(stage) = level.stage(microphone) else {
            *self.slot(level) = None;
            return;
        };
        let current = self.slot(level).as_ref().is_some_and(|slider| {
            slider.id == microphone.id && slider.min == stage.min && slider.max == stage.max
        });
        if !current {
            let id = microphone.id.clone();
            let range = SliderRange::new(stage.min, stage.max).step(SLIDER_STEP_DB);
            let slider = CommitSlider::new(range, stage.level, cx, move |_, value, cx| {
                AppState::apply(cx, |state| {
                    state.change_microphone(&id, level.change(value))
                });
            });
            *self.slot(level) = Some(LevelSlider {
                id: microphone.id.clone(),
                min: stage.min,
                max: stage.max,
                slider,
            });
        }
        if let Some(slider) = self.slot(level) {
            slider.slider.sync(stage.level, window, cx);
        }
    }
}

impl Render for MicrophonePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let state = AppState::try_read(cx);
        let microphone = state
            .and_then(|state| {
                state
                    .current_record()
                    .and_then(|record| state.microphone_for(record))
            })
            .cloned();
        let failure = microphone.as_ref().and_then(|microphone| {
            state.and_then(|state| state.microphone_failure(&microphone.id))
        });
        let Some(microphone) = microphone else {
            self.gain = None;
            self.headphones = None;
            self.monitoring = None;
            return v_flex().child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("device.offline")),
            );
        };
        for level in [Level::Gain, Level::Headphones, Level::Monitoring] {
            self.sync_slider(level, &microphone, window, cx);
        }
        self.column(&microphone, failure, pal)
    }
}

impl MicrophonePanel {
    /// The controls `microphone` reported, top to bottom as G HUB orders them.
    fn column(
        &self,
        microphone: &Microphone,
        failure: Option<MicrophoneError>,
        pal: Palette,
    ) -> gpui::Div {
        let controls = microphone.controls;
        let id = microphone.id.clone();
        v_flex()
            .gap_4()
            .w_full()
            .when_some(controls.muted, |panel, muted| {
                panel.child(switch_row(
                    "microphone-mute",
                    tr!("microphone.mute"),
                    Some(tr!("microphone.mute_description")),
                    muted,
                    {
                        let id = id.clone();
                        move |on| (id.clone(), MicrophoneChange::Muted(on))
                    },
                    pal,
                ))
            })
            .when_some(self.gain.as_ref(), |panel, slider| {
                panel.child(level_well(
                    tr!("microphone.gain"),
                    slider,
                    controls.gain,
                    pal,
                ))
            })
            .when(
                controls.headphone_volume.is_some() || controls.headphones_muted.is_some(),
                |panel| {
                    panel.child(
                        div()
                            .pt_2()
                            .text_body()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(tr!("microphone.headphones")),
                    )
                },
            )
            .when_some(self.headphones.as_ref(), |panel, slider| {
                panel.child(level_well(
                    tr!("microphone.headphone_volume"),
                    slider,
                    controls.headphone_volume,
                    pal,
                ))
            })
            .when_some(controls.headphones_muted, |panel, muted| {
                panel.child(switch_row(
                    "microphone-headphones-mute",
                    tr!("microphone.mute_headphones"),
                    None,
                    muted,
                    {
                        let id = id.clone();
                        move |on| (id.clone(), MicrophoneChange::HeadphonesMuted(on))
                    },
                    pal,
                ))
            })
            .when_some(controls.monitoring, |panel, monitoring| {
                panel.child(switch_row(
                    "microphone-monitoring",
                    tr!("microphone.monitoring"),
                    Some(tr!("microphone.monitoring_description")),
                    monitoring,
                    {
                        let id = id.clone();
                        move |on| (id.clone(), MicrophoneChange::Monitoring(on))
                    },
                    pal,
                ))
            })
            .when_some(self.monitoring.as_ref(), |panel, slider| {
                panel.child(level_well(
                    tr!("microphone.monitoring_level"),
                    slider,
                    controls.monitoring_level,
                    pal,
                ))
            })
            .when_some(failure, |panel, error| {
                panel.child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(
                            div()
                                .size_1p5()
                                .rounded_full()
                                .bg(rgb(theme::STATUS_OFFLINE)),
                        )
                        .child(tr!("microphone.write_failed", error => error.to_string())),
                )
            })
    }
}

/// A labelled on/off control. `change` turns the new state into the write.
fn switch_row(
    id: &'static str,
    title: gpui::SharedString,
    description: Option<gpui::SharedString>,
    selected: bool,
    change: impl Fn(bool) -> (String, MicrophoneChange) + 'static,
    pal: Palette,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .justify_between()
        .items_center()
        .gap_3()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(pal.border)
        .bg(pal.muted)
        .p_3()
        .child(
            v_flex()
                .gap_1()
                .flex_1()
                .min_w_0()
                .child(div().text_body().text_color(pal.text_primary).child(title))
                .when_some(description, |column, description| {
                    column.child(
                        div()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(description),
                    )
                }),
        )
        .child(
            Toggle::new(id)
                .selected(selected)
                .min_width(px(72.))
                .on_change(move |on, _window, cx| {
                    let (id, change) = change(*on);
                    AppState::apply(cx, |state| state.change_microphone(&id, change));
                }),
        )
}

/// A level slider with its reading and the range the microphone accepts.
fn level_well(
    title: gpui::SharedString,
    slider: &LevelSlider,
    stage: Option<GainStage>,
    pal: Palette,
) -> impl IntoElement {
    let shown = slider
        .slider
        .shown(stage.map_or(slider.min, |stage| stage.level));
    v_flex()
        .gap_3()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(pal.border)
        .bg(pal.muted)
        .p_3()
        .child(
            h_flex()
                .justify_between()
                .items_baseline()
                .child(div().text_caption().text_color(pal.text_muted).child(title))
                .child(
                    div()
                        .text_body()
                        .text_color(rgb(ACCENT_BLUE))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(format_decibels(shown)),
                ),
        )
        .child(Slider::new(slider.slider.slider()).horizontal())
        .child(
            h_flex()
                .justify_between()
                .text_caption()
                .text_color(pal.text_muted)
                .child(format_decibels(slider.min))
                .child(format_decibels(slider.max)),
        )
}

/// A level as the panel prints it: one decimal and an explicit sign.
fn format_decibels(level: MilliDecibels) -> String {
    format!("{:+.1} dB", level.decibels())
}

#[cfg(test)]
mod tests {
    use openlogi_core::microphone::MilliDecibels;

    use super::format_decibels;
    use crate::ui::commit_slider::SliderUnit as _;

    #[test]
    fn levels_print_with_sign_and_one_decimal() {
        assert_eq!(format_decibels(MilliDecibels::new(12_000)), "+12.0 dB");
        assert_eq!(format_decibels(MilliDecibels::new(-17_625)), "-17.6 dB");
        assert_eq!(format_decibels(MilliDecibels::new(0)), "+0.0 dB");
    }

    #[test]
    fn a_slider_position_round_trips_through_millidecibels() {
        let level = MilliDecibels::new(-6_500);

        assert_eq!(MilliDecibels::from_slider(level.to_slider()), level);
        assert_eq!(
            MilliDecibels::from_slider(f32::NAN),
            MilliDecibels::default()
        );
    }
}
