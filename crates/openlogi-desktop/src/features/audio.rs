//! The Audio tab of a headset: its sidetone level.
//!
//! Sidetone plays the microphone back into the earcups, so the wearer hears
//! their own voice. The slider runs from 0 % (off) to 100 %, as HID++
//! `0x8300` documents. A release goes through [`AppState::commit_sidetone`],
//! which writes the headset; the headset keeps the level across power
//! cycles, so nothing is saved. When the headset refuses the write, the
//! slider returns to the level the headset reads back.

use gpui::{
    AnyElement, App, Context, IntoElement, ParentElement, Render, Styled, Subscription, Window, div,
};
use gpui_component::{h_flex, slider::Slider, v_flex};
use openlogi_core::hid::SidetoneLevel;

use crate::state::{AppState, DeviceKey, SidetoneLoad, StateEvent};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, Palette, Typography as _};

/// The Audio tab's icon, from the shared action-icon set.
pub const AUDIO_ICON: &str = "action-icons/volume-2.svg";

/// The Audio tab's body, kept across frames for its slider.
pub struct AudioPanel {
    sidetone: Option<SidetoneSlider>,
    _state_obs: Subscription,
}

impl AudioPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs =
            AppState::repaint_on(cx, |event| matches!(event, StateEvent::SidetoneChanged(_)));
        Self {
            sidetone: None,
            _state_obs: state_obs,
        }
    }
}

impl Render for AudioPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let Some(facts) = SidetoneFacts::current(cx) else {
            self.sidetone = None;
            return v_flex().w_full();
        };
        SidetoneSlider::sync(&mut self.sidetone, &facts, window, cx);
        v_flex()
            .w_full()
            .child(sidetone_section(facts, self.sidetone.as_ref(), pal))
    }
}

/// What the sidetone row needs about the selected headset.
struct SidetoneFacts {
    key: DeviceKey,
    load: SidetoneLoad,
    reachable: bool,
}

impl SidetoneFacts {
    /// The selected headset's facts, or `None` when nothing is selected or
    /// the device has no `0x8300`.
    fn current(cx: &App) -> Option<Self> {
        let state = AppState::try_read(cx)?;
        if !state.current_sidetone_supported() {
            return None;
        }
        let record = state.current_record()?;
        Some(Self {
            key: record.device_key(),
            load: state.current_sidetone_load(),
            reachable: record.route.is_some(),
        })
    }

    /// The level to put the slider on, or `None` while the slider must not
    /// show: the headset is offline, or its reading has not landed.
    fn slider_level(&self) -> Option<SidetoneLevel> {
        match &self.load {
            SidetoneLoad::Ready(level) if self.reachable => Some(**level),
            _ => None,
        }
    }
}

/// The slider, kept across frames, with the headset it was built for.
struct SidetoneSlider {
    key: DeviceKey,
    slider: CommitSlider<SidetoneLevel>,
}

impl SidetoneSlider {
    /// Keep `current` in step with `facts`: drop the slider when it must not
    /// show, build one for a newly selected headset, or move the thumb when
    /// the level changed underneath it.
    fn sync<V: 'static>(
        current: &mut Option<Self>,
        facts: &SidetoneFacts,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let Some(level) = facts.slider_level() else {
            *current = None;
            return;
        };
        if let Some(existing) = current
            .as_ref()
            .filter(|existing| existing.key == facts.key)
        {
            existing.slider.sync(level, window, cx);
            return;
        }
        let slider = CommitSlider::new(
            SliderRange::new(SidetoneLevel::OFF, SidetoneLevel::MAX),
            level,
            cx,
            // `commit_sidetone` resolves the headset when the thumb is
            // released, so a device switch mid-drag writes the headset now
            // on screen.
            |_, level, cx| AppState::apply(cx, |state| state.commit_sidetone(level)),
        );
        *current = Some(Self {
            key: facts.key.clone(),
            slider,
        });
    }
}

/// The row: a header with the level as a percent, the slider or a status
/// line, and what the setting does.
fn sidetone_section(
    facts: SidetoneFacts,
    slider: Option<&SidetoneSlider>,
    pal: Palette,
) -> AnyElement {
    let shown = facts
        .slider_level()
        .map(|level| slider.map_or(level, |slider| slider.slider.shown(level)));
    let SidetoneFacts {
        key,
        load,
        reachable,
    } = facts;
    let body: AnyElement = match (load, slider) {
        _ if !reachable => {
            status_line(tr!("audio.device_offline_sidetone_is_unavailable"), pal).into_any_element()
        }
        (SidetoneLoad::Ready(_), Some(slider)) => Slider::new(slider.slider.slider())
            .horizontal()
            .into_any_element(),
        (SidetoneLoad::Ready(_), None) | (SidetoneLoad::Unknown | SidetoneLoad::Loading, _) => {
            status_line(tr!("audio.reading_sidetone"), pal).into_any_element()
        }
        (SidetoneLoad::Failed(_), _) => retry_line(
            "sidetone-retry",
            tr!("audio.couldnt_read_sidetone_click_to_retry"),
            move |cx| AppState::apply(cx, |state| state.retry_sidetone_read(&key)),
        )
        .into_any_element(),
        (SidetoneLoad::Unsupported(_), _) => {
            status_line(tr!("audio.sidetone_unsupported"), pal).into_any_element()
        }
    };
    v_flex()
        .gap_3()
        .w_full()
        .child(
            h_flex()
                .justify_between()
                .items_baseline()
                .child(
                    div()
                        .text_body()
                        .text_color(pal.text_muted)
                        .child(tr!("audio.sidetone")),
                )
                .children(shown.map(|level| {
                    div()
                        .text_body()
                        .text_color(pal.text_primary)
                        .child(tr!("audio.sidetone_percent", percent => level.percent()))
                })),
        )
        .child(body)
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!("audio.sidetone_description")),
        )
        .into_any_element()
}
