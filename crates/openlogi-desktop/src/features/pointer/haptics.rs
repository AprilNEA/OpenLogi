//! Haptic feedback controls for the Pointer tab.
//!
//! Three controls over the HID++ `0x19b0` configuration: the firmware engine,
//! the intensity, and battery-saving haptics. Each change goes through
//! [`AppState::commit_haptics`], which persists the settings and writes the
//! device; the agent re-applies them when the device reconnects. Releasing
//! the intensity slider also asks the device for one short pulse at the new
//! strength. While the engine is off the intensity has nothing to act on, so
//! its slider greys out, as SmartShift's do outside ratchet mode.

use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement, Render, Styled,
    Subscription, Window, div, prelude::FluentBuilder as _, rgb,
};
use gpui_component::{Disableable as _, Selectable as _, h_flex, slider::Slider, v_flex};
use openlogi_core::hid::{HapticIntensity, HapticLevels, HapticSettings};

use crate::state::{AppState, DeviceKey, HapticsLoad, StateEvent, StateEvents};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::components::{Toggle, disabled_track, toggle_row};
use crate::ui::section::section_label;
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, ACCENT_BLUE, Typography as _};

/// What the panel needs about the selected device.
struct HapticsFacts {
    key: DeviceKey,
    load: HapticsLoad,
    /// The settings the controls show: the reading, else the saved ones.
    shown: Option<HapticSettings>,
    reachable: bool,
}

impl HapticsFacts {
    /// The selected device's facts, or `None` when nothing is selected or
    /// the device reports no haptic feedback.
    fn current(cx: &App) -> Option<Self> {
        let state = AppState::try_read(cx)?;
        if !state.current_haptics_supported() {
            return None;
        }
        let record = state.current_record()?;
        Some(Self {
            key: record.device_key(),
            load: state.current_haptics_load(),
            shown: state.current_haptics_shown(),
            reachable: record.route.is_some(),
        })
    }
}

/// The intensity slider, kept across frames, with the device and the level
/// set it was built for: its range is builder-only.
struct IntensitySlider {
    key: DeviceKey,
    levels: HapticLevels,
    slider: CommitSlider<HapticIntensity>,
}

/// The slider range a device's level set allows, or `None` for a set
/// OpenLogi cannot list, which takes no intensity at all.
fn intensity_range(levels: HapticLevels) -> Option<SliderRange<HapticIntensity>> {
    match levels {
        HapticLevels::Any => Some(SliderRange::new(HapticIntensity::OFF, HapticIntensity::MAX)),
        HapticLevels::Quarters => Some(
            SliderRange::new(HapticIntensity::from_rounded(25.), HapticIntensity::MAX).step(25.),
        ),
        HapticLevels::Other(_) => None,
    }
}

/// The quarter step nearest `intensity`, never below 25: a quarters device
/// refuses 0.
fn snap_to_quarter(intensity: HapticIntensity) -> HapticIntensity {
    let quarters = (f32::from(intensity) / 25.).round().max(1.);
    HapticIntensity::from_rounded(quarters * 25.)
}

pub struct HapticsPanel {
    intensity: Option<IntensitySlider>,
    _state_obs: Subscription,
}

impl HapticsPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = AppState::repaint_on(cx, |event| {
            matches!(
                event,
                StateEvent::HapticsChanged(_) | StateEvent::DeviceConfigChanged(_)
            )
        });
        Self {
            intensity: None,
            _state_obs: state_obs,
        }
    }

    /// Keep the slider in step with the device on screen: build one for a
    /// newly selected device or level set, or re-seat the thumb when the
    /// intensity changed underneath it.
    fn sync_intensity(
        &mut self,
        key: &DeviceKey,
        levels: HapticLevels,
        intensity: HapticIntensity,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(range) = intensity_range(levels) else {
            self.intensity = None;
            return;
        };
        let intensity = range.clamp(intensity);
        if let Some(existing) = self
            .intensity
            .as_ref()
            .filter(|existing| existing.key == *key && existing.levels == levels)
        {
            match levels {
                HapticLevels::Quarters => {
                    existing
                        .slider
                        .sync_snapped(intensity, snap_to_quarter, window, cx);
                }
                HapticLevels::Any | HapticLevels::Other(_) => {
                    existing.slider.sync(intensity, window, cx);
                }
            }
            return;
        }
        let slider = CommitSlider::new(range, intensity, cx, |_, intensity, cx| {
            // Resolved on release, so a device switch mid-drag writes the
            // device now on screen.
            AppState::apply(cx, |state| {
                state
                    .current_haptics_shown()
                    .map_or_else(StateEvents::none, |shown| {
                        state.commit_haptics(HapticSettings { intensity, ..shown }, true)
                    })
            });
        });
        self.intensity = Some(IntensitySlider {
            key: key.clone(),
            levels,
            slider,
        });
    }

    fn ready_body(
        &mut self,
        key: &DeviceKey,
        levels: HapticLevels,
        shown: HapticSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let pal = theme::palette(cx);
        self.sync_intensity(key, levels, shown.intensity, window, cx);
        let slider = self.intensity.as_ref().map(|intensity| &intensity.slider);
        let display = slider.map_or(shown.intensity, |slider| slider.shown(shown.intensity));
        let adjustable = shown.enabled && slider.is_some();
        let value_color = if adjustable {
            rgb(ACCENT_BLUE).into()
        } else {
            pal.text_muted
        };
        let intensity_row = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_baseline()
                    .child(section_label(tr!("pointer.haptic_intensity"), pal))
                    .child(
                        div()
                            .text_body()
                            .text_color(value_color)
                            .child(format!("{display}%")),
                    ),
            )
            .map(|row| match slider.filter(|_| adjustable) {
                Some(slider) => row.child(Slider::new(slider.slider()).horizontal()),
                None => row.child(disabled_track(pal)),
            })
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("pointer.haptic_intensity_description")),
            );
        v_flex()
            .gap_4()
            .w_full()
            .child(toggle_row(
                "haptics-engine-row",
                pal,
                tr!("pointer.haptic_engine"),
                tr!("pointer.haptic_engine_description"),
                Toggle::new("haptics-engine-toggle")
                    .selected(shown.enabled)
                    .on_change(move |enabled, _window, cx| {
                        commit(
                            cx,
                            HapticSettings {
                                enabled: *enabled,
                                ..shown
                            },
                        );
                    }),
            ))
            .child(intensity_row)
            .child(toggle_row(
                "haptics-power-save-row",
                pal,
                tr!("pointer.haptic_power_save"),
                tr!("pointer.haptic_power_save_description"),
                Toggle::new("haptics-power-save-toggle")
                    .selected(shown.power_save)
                    .disabled(!shown.enabled)
                    .on_change(move |power_save, _window, cx| {
                        commit(
                            cx,
                            HapticSettings {
                                power_save: *power_save,
                                ..shown
                            },
                        );
                    }),
            ))
    }
}

impl Render for HapticsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let Some(facts) = HapticsFacts::current(cx) else {
            self.intensity = None;
            return v_flex().w_full();
        };
        let HapticsFacts {
            key,
            load,
            shown,
            reachable,
        } = facts;
        let content: AnyElement = match (load, shown) {
            _ if !reachable => {
                status_line(tr!("pointer.device_offline_haptics_is_unavailable"), pal)
                    .into_any_element()
            }
            (HapticsLoad::Ready(reading), Some(shown)) => self
                .ready_body(&key, reading.levels, shown, window, cx)
                .into_any_element(),
            (HapticsLoad::Ready(_), None) | (HapticsLoad::Unknown | HapticsLoad::Loading, _) => {
                status_line(tr!("pointer.reading_haptic_settings"), pal).into_any_element()
            }
            (HapticsLoad::Failed(_), _) => retry_line(
                "haptics-retry",
                tr!("pointer.couldnt_read_haptics_click_to_retry"),
                pal,
                move |cx| AppState::apply(cx, |state| state.retry_haptics_read(&key)),
            )
            .into_any_element(),
            (HapticsLoad::Unsupported(_), _) => {
                status_line(tr!("pointer.this_device_does_not_support_haptics"), pal)
                    .into_any_element()
            }
        };
        v_flex()
            .debug_selector(|| "haptics-panel".into())
            .gap_3()
            .w_full()
            .child(content)
    }
}

fn commit(cx: &mut App, settings: HapticSettings) {
    AppState::apply(cx, |state| state.commit_haptics(settings, false));
}

#[cfg(test)]
mod tests {
    use openlogi_core::hid::{HapticIntensity, HapticLevels};

    use super::{intensity_range, snap_to_quarter};

    #[test]
    fn the_slider_offers_only_the_levels_the_device_accepts() {
        let any = intensity_range(HapticLevels::Any).expect("any level is adjustable");
        assert_eq!(
            any.clamp(HapticIntensity::OFF),
            HapticIntensity::OFF,
            "a device that takes any level can be set to zero"
        );
        let quarters = intensity_range(HapticLevels::Quarters).expect("quarters are adjustable");
        assert_eq!(
            quarters.clamp(HapticIntensity::OFF),
            HapticIntensity::from_rounded(25.),
            "a quarters device refuses zero, so the slider starts at 25"
        );
        assert!(intensity_range(HapticLevels::Other(7)).is_none());
    }

    #[test]
    fn a_quarters_thumb_snaps_to_the_nearest_quarter() {
        let at = |raw: f32| snap_to_quarter(HapticIntensity::from_rounded(raw));
        assert_eq!(at(60.), HapticIntensity::from_rounded(50.));
        assert_eq!(at(63.), HapticIntensity::from_rounded(75.));
        assert_eq!(at(0.), HapticIntensity::from_rounded(25.));
        assert_eq!(at(100.), HapticIntensity::MAX);
    }
}
