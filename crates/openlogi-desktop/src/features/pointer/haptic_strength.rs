//! The haptic strength slider on the pointer tab's Haptics card.
//!
//! The strength is device-wide: it sets how strongly the Actions Ring, event
//! haptics, and `openlogi api haptic` all play, whether event haptics are on
//! or not. The slider writes `config.toml` on release
//! ([`AppState::commit_haptic_strength`]); the agent then writes the device,
//! and writes it again whenever the device reconnects. A device the user
//! never chose a strength for keeps its own, and the slider says so.

use gpui::{Context, IntoElement, ParentElement, Render, Styled, Subscription, Window, div, rgb};
use gpui_component::{h_flex, slider::Slider, v_flex};
use openlogi_core::hid::HapticStrength;

use crate::state::{AppState, StateEvent};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::section::section_label;
use crate::ui::theme::{self, ACCENT_BLUE, Typography as _};

/// How far one slider step moves the strength. The MX Master 4 keeps
/// strengths in tens, so finer steps would not be felt.
const STEP: f32 = 10.;

/// The lowest strength the slider offers: one step above silence.
const SLIDER_MIN: HapticStrength = match HapticStrength::try_new(10) {
    Ok(strength) => strength,
    Err(_) => panic!("valid minimum slider strength"),
};

pub struct HapticStrengthPanel {
    slider: CommitSlider<HapticStrength>,
    _state_obs: Subscription,
}

impl HapticStrengthPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let slider = CommitSlider::new(
            SliderRange::new(SLIDER_MIN, HapticStrength::MAX).step(STEP),
            HapticStrength::DEFAULT,
            cx,
            |_, strength, cx| AppState::apply(cx, |state| state.commit_haptic_strength(strength)),
        );
        let state_obs = AppState::repaint_on(cx, |event| {
            matches!(event, StateEvent::DeviceConfigChanged(_))
        });
        Self {
            slider,
            _state_obs: state_obs,
        }
    }
}

impl Render for HapticStrengthPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let chosen = AppState::try_read(cx).and_then(AppState::current_haptic_strength);
        let committed = chosen.unwrap_or(HapticStrength::DEFAULT);
        self.slider.sync(committed, window, cx);
        let shown = self.slider.shown(committed);
        // Until the user moves it, the thumb only stands in for whatever
        // strength the device keeps.
        let value = if chosen.is_none() && shown == committed {
            tr!("pointer.haptic_strength_device")
        } else {
            format!("{} %", shown.into_inner()).into()
        };
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_baseline()
                    .child(section_label(tr!("pointer.haptic_strength"), pal))
                    .child(div().text_body().text_color(rgb(ACCENT_BLUE)).child(value)),
            )
            .child(Slider::new(self.slider.slider()).horizontal())
    }
}
