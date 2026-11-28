//! The Audio tab of a headset: its sidetone level and its automatic
//! power-off.
//!
//! Sidetone plays the microphone back into the earcups, so the wearer hears
//! their own voice. The slider runs from 0 % (off) to 100 %, as HID++
//! `0x8300` documents. A release goes through [`AppState::commit_sidetone`],
//! which writes the headset; the headset keeps the level across power
//! cycles, so nothing is saved. When the headset refuses the write, the
//! slider returns to the level the headset reads back.
//!
//! The automatic power-off row works the same way over HID++ `0x1F20`: a
//! slider in minutes from 0 (never) to 90, written through
//! [`AppState::commit_power_off_timeout`] and kept by the headset.

use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_kit::component::{h_flex, slider::Slider, v_flex};
use openlogi_core::hid::{PowerOffTimeout, SidetoneLevel};

use crate::state::{AppState, DeviceKey, PowerOffTimeoutLoad, SidetoneLoad, StateEvent};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, Palette, Typography as _};

/// The Audio tab's icon, from the shared action-icon set.
pub const AUDIO_ICON: &str = "action-icons/volume-2.svg";

/// The Audio tab's body, kept across frames for its sliders.
pub struct AudioPanel {
    sidetone: Option<SidetoneSlider>,
    power_off: Option<PowerOffSlider>,
    _state_obs: Subscription,
}

impl AudioPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = AppState::repaint_on(cx, |event| {
            matches!(
                event,
                StateEvent::SidetoneChanged(_) | StateEvent::PowerOffTimeoutChanged(_)
            )
        });
        Self {
            sidetone: None,
            power_off: None,
            _state_obs: state_obs,
        }
    }

    /// The power-off slider, while the row shows one.
    #[cfg(test)]
    pub(crate) fn power_off_slider(
        &self,
    ) -> Option<&gpui::Entity<gpui_kit::component::slider::SliderState>> {
        self.power_off.as_ref().map(|row| row.slider.slider())
    }
}

impl Render for AudioPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let sidetone = SidetoneFacts::current(cx);
        match &sidetone {
            Some(facts) => SidetoneSlider::sync(&mut self.sidetone, facts, window, cx),
            None => self.sidetone = None,
        }
        let power_off = PowerOffFacts::current(cx);
        match &power_off {
            Some(facts) => PowerOffSlider::sync(&mut self.power_off, facts, window, cx),
            None => self.power_off = None,
        }
        let both = sidetone.is_some() && power_off.is_some();
        v_flex()
            .w_full()
            .gap_4()
            .children(sidetone.map(|facts| sidetone_section(facts, self.sidetone.as_ref(), pal)))
            .when(both, |this| {
                this.child(div().h(px(1.)).w_full().bg(pal.border))
            })
            .children(power_off.map(|facts| power_off_section(facts, self.power_off.as_ref(), pal)))
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
            reachable: state.current_sidetone_route().is_some(),
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
        .debug_selector(|| "audio-sidetone-row".into())
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

/// What the automatic power-off row needs about the selected headset.
struct PowerOffFacts {
    key: DeviceKey,
    load: PowerOffTimeoutLoad,
    reachable: bool,
}

impl PowerOffFacts {
    /// The selected headset's facts, or `None` when nothing is selected, the
    /// device has no `0x1F20`, or it refused the timeout read while the
    /// sidetone row still has something to show.
    ///
    /// A headset can list `0x1F20` for its battery alone. Its refusal hides
    /// the row, as the menu-bar menu hides the group, so a working sidetone
    /// slider is not followed by a permanent "unsupported" line. The line
    /// stays when the tab would otherwise be empty.
    fn current(cx: &App) -> Option<Self> {
        let state = AppState::try_read(cx)?;
        if !state.current_power_off_timeout_supported() {
            return None;
        }
        let load = state.current_power_off_timeout_load();
        let sidetone_shown = state.current_sidetone_supported()
            && !matches!(state.current_sidetone_load(), SidetoneLoad::Unsupported(_));
        if matches!(load, PowerOffTimeoutLoad::Unsupported(_)) && sidetone_shown {
            return None;
        }
        let record = state.current_record()?;
        Some(Self {
            key: record.device_key(),
            load,
            reachable: state.current_power_off_timeout_route().is_some(),
        })
    }

    /// The timeout to put the slider on, or `None` while the slider must not
    /// show: the headset is offline, or its reading has not landed.
    fn slider_timeout(&self) -> Option<PowerOffTimeout> {
        match &self.load {
            PowerOffTimeoutLoad::Ready(timeout) if self.reachable => Some(**timeout),
            _ => None,
        }
    }
}

/// The power-off slider, kept across frames, with the headset it was built
/// for.
struct PowerOffSlider {
    key: DeviceKey,
    slider: CommitSlider<PowerOffTimeout>,
}

impl PowerOffSlider {
    /// Keep `current` in step with `facts`, as [`SidetoneSlider::sync`] does.
    fn sync<V: 'static>(
        current: &mut Option<Self>,
        facts: &PowerOffFacts,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let Some(timeout) = facts.slider_timeout() else {
            *current = None;
            return;
        };
        if let Some(existing) = current
            .as_ref()
            .filter(|existing| existing.key == facts.key)
        {
            existing.slider.sync(timeout, window, cx);
            return;
        }
        let slider = CommitSlider::new(
            SliderRange::new(PowerOffTimeout::NEVER, PowerOffTimeout::MAX),
            timeout,
            cx,
            // Resolved on release, like the sidetone slider, so a device
            // switch mid-drag writes the headset now on screen.
            |_, timeout, cx| AppState::apply(cx, |state| state.commit_power_off_timeout(timeout)),
        );
        *current = Some(Self {
            key: facts.key.clone(),
            slider,
        });
    }
}

/// The timeout as the header shows it: "Never" for 0, minutes otherwise.
fn power_off_label(timeout: PowerOffTimeout) -> SharedString {
    match timeout.minutes() {
        0 => tr!("audio.power_off_never"),
        minutes => tr!("audio.power_off_minutes", minutes => minutes),
    }
}

/// The row: a header with the timeout, the slider or a status line, and what
/// the setting does.
fn power_off_section(
    facts: PowerOffFacts,
    slider: Option<&PowerOffSlider>,
    pal: Palette,
) -> AnyElement {
    let shown = facts
        .slider_timeout()
        .map(|timeout| slider.map_or(timeout, |slider| slider.slider.shown(timeout)));
    let PowerOffFacts {
        key,
        load,
        reachable,
    } = facts;
    let body: AnyElement = match (load, slider) {
        _ if !reachable => status_line(tr!("audio.device_offline_power_off_is_unavailable"), pal)
            .into_any_element(),
        (PowerOffTimeoutLoad::Ready(_), Some(slider)) => Slider::new(slider.slider.slider())
            .horizontal()
            .into_any_element(),
        (PowerOffTimeoutLoad::Ready(_), None)
        | (PowerOffTimeoutLoad::Unknown | PowerOffTimeoutLoad::Loading, _) => {
            status_line(tr!("audio.reading_power_off"), pal).into_any_element()
        }
        (PowerOffTimeoutLoad::Failed(_), _) => retry_line(
            "power-off-retry",
            tr!("audio.couldnt_read_power_off_click_to_retry"),
            move |cx| AppState::apply(cx, |state| state.retry_power_off_timeout_read(&key)),
        )
        .into_any_element(),
        (PowerOffTimeoutLoad::Unsupported(_), _) => {
            status_line(tr!("audio.power_off_unsupported"), pal).into_any_element()
        }
    };
    v_flex()
        .debug_selector(|| "audio-power-off-row".into())
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
                        .child(tr!("audio.power_off")),
                )
                .children(shown.map(|timeout| {
                    div()
                        .text_body()
                        .text_color(pal.text_primary)
                        .child(power_off_label(timeout))
                })),
        )
        .child(body)
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!("audio.power_off_description")),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests;
