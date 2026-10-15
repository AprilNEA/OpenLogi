//! Controls for standalone lights.

use crate::state::{AppState, DeviceKey, DeviceRecord, LightCommandStatus, StateEvent};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::components::Toggle;

use super::visual::LightView;
use crate::ui::theme::{self, ACCENT_BLUE, Palette, Typography as _};
use gpui::{
    BoxShadow, Context, Hsla, IntoElement, ParentElement, Render, Styled, Subscription, Window,
    div, hsla, point, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::{Icon, IconName, Selectable as _, h_flex, slider::Slider, v_flex};
use openlogi_core::{
    config::LightSettings,
    device::{LightCapabilities, LightValueRange, LightValueUnit},
};

/// Standalone-light panel. The UI is driven by the active device's advertised
/// capabilities; the panel is not Litra-specific even though Litra is the
/// first driver.
pub struct LightPanel {
    /// The device the sliders below were shaped for.
    device_key: Option<DeviceKey>,
    brightness: Option<LightSlider>,
    temperature: Option<LightSlider>,
    _state_obs: Subscription,
}

/// One capability-shaped slider, in the range's native units.
struct LightSlider {
    range: LightValueRange,
    slider: CommitSlider<u16>,
}

impl LightPanel {
    /// Construct the panel. Capability-shaped sliders are created lazily when
    /// the selected device is known.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = AppState::repaint_on(cx, |event| {
            matches!(
                event,
                StateEvent::CameraChanged | StateEvent::LightingChanged(_)
            )
        });
        Self {
            device_key: None,
            brightness: None,
            temperature: None,
            _state_obs: state_obs,
        }
    }

    fn ensure_sliders(
        &mut self,
        key: Option<DeviceKey>,
        capabilities: Option<LightCapabilities>,
        settings: LightSettings,
        cx: &mut Context<Self>,
    ) {
        let brightness_range = capabilities.and_then(|caps| caps.brightness);
        let temperature_range = capabilities.and_then(|caps| caps.temperature);
        if self.device_key == key
            && self.brightness.as_ref().map(|slider| slider.range) == brightness_range
            && self.temperature.as_ref().map(|slider| slider.range) == temperature_range
        {
            return;
        }

        self.device_key = key;
        self.brightness = brightness_range.map(|range| LightSlider {
            range,
            slider: CommitSlider::new(
                slider_range(range),
                brightness_native(range, settings),
                cx,
                move |_, native, cx| {
                    let Some(percent) = range.percent_for_native(native) else {
                        return;
                    };
                    AppState::apply(cx, |state| {
                        let mut light = state.light();
                        if !state.camera_automation_active() {
                            light.enabled = true;
                        }
                        light.brightness_percent = percent;
                        state.commit_light(light)
                    });
                },
            ),
        });
        self.temperature = temperature_range.map(|range| LightSlider {
            range,
            slider: CommitSlider::new(
                slider_range(range),
                temperature_native(range, settings),
                cx,
                move |_, kelvin, cx| {
                    let kelvin = range.quantize(kelvin);
                    AppState::apply(cx, |state| {
                        let mut light = state.light();
                        if !state.camera_automation_active() {
                            light.enabled = true;
                        }
                        light.temperature_kelvin = Some(kelvin);
                        state.commit_light(light)
                    });
                },
            ),
        });
    }
}

fn slider_range(range: LightValueRange) -> SliderRange<u16> {
    SliderRange::new(range.min(), range.max()).step(f32::from(range.step()))
}

/// Where the brightness thumb rests for the saved percentage.
fn brightness_native(range: LightValueRange, settings: LightSettings) -> u16 {
    range
        .native_for_percent(settings.brightness_percent)
        .unwrap_or_else(|| range.min())
}

/// Where the temperature thumb rests: the saved value on the range's grid, or
/// the middle of the range while none is saved.
fn temperature_native(range: LightValueRange, settings: LightSettings) -> u16 {
    settings
        .temperature_kelvin
        .map_or_else(|| midpoint(range), |kelvin| range.quantize(kelvin))
}

impl Render for LightPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let settings = AppState::try_read(cx)
            .map(AppState::light)
            .unwrap_or_default();
        let record = AppState::try_read(cx)
            .and_then(AppState::current_record)
            .cloned();
        let capabilities = record.as_ref().and_then(|record| record.light_capabilities);

        self.ensure_sliders(
            record.as_ref().map(DeviceRecord::device_key),
            capabilities,
            settings,
            cx,
        );

        if let Some(LightSlider { range, slider }) = &self.brightness {
            slider.sync(brightness_native(*range, settings), window, cx);
        }
        if let Some(LightSlider { range, slider }) = &self.temperature {
            slider.sync(temperature_native(*range, settings), window, cx);
        }

        let device_name = record.as_ref().map_or_else(
            || tr!("device.lighting").to_string(),
            |record| record.display_name.clone(),
        );
        let online = record.as_ref().is_some_and(|record| record.online);
        let effective_enabled = AppState::try_read(cx).is_some_and(AppState::light_enabled);
        let power = capabilities.is_some_and(|caps| caps.power);

        let brightness = self.brightness.as_ref();
        let temperature = self.temperature.as_ref();
        let status = AppState::try_read(cx).and_then(AppState::light_command_status);

        v_flex()
            .gap_4()
            .w_full()
            .when(power, |panel| {
                let panel = panel.child(light_hero(
                    &device_name,
                    LightView {
                        online,
                        enabled: effective_enabled,
                    },
                    pal,
                ));
                #[cfg(target_os = "macos")]
                let panel = panel.child(camera_automation(settings, pal));
                panel.child(div().h(px(1.)).w_full().bg(pal.border.opacity(0.55)))
            })
            .when_some(brightness, |panel, LightSlider { range, slider }| {
                panel.child(control_well(
                    tr!("camera.brightness"),
                    format_light_value(
                        slider.shown(brightness_native(*range, settings)),
                        range.unit(),
                    ),
                    format_range_endpoints(*range),
                    Slider::new(slider.slider()).horizontal(),
                    pal,
                ))
            })
            .when_some(temperature, |panel, LightSlider { range, slider }| {
                panel.child(control_well(
                    tr!("lighting.colour_temperature"),
                    format_light_value(
                        slider.shown(temperature_native(*range, settings)),
                        range.unit(),
                    ),
                    format_range_endpoints(*range),
                    Slider::new(slider.slider()).horizontal(),
                    pal,
                ))
            })
            .when_some(status, |panel, status| {
                panel.child(light_command_status(status, pal))
            })
    }
}

fn light_hero(device_name: &str, view: LightView, pal: Palette) -> impl IntoElement {
    let LightView {
        online,
        enabled: effective_enabled,
    } = view;
    h_flex()
        .gap_3()
        .items_center()
        .child(light_emblem(effective_enabled, pal))
        .child(
            v_flex()
                .gap_1()
                .flex_1()
                .min_w_0()
                .child(div().text_heading().child(device_name.to_owned()))
                .child(light_status(
                    LightView {
                        online,
                        enabled: effective_enabled,
                    },
                    pal,
                )),
        )
        .child(
            Toggle::new("standalone-light-toggle")
                .selected(effective_enabled)
                .icon(if effective_enabled {
                    IconName::Sun
                } else {
                    IconName::Moon
                })
                .min_width(px(72.))
                .on_change(|enabled, _window, cx| {
                    AppState::apply(cx, |state| state.commit_manual_light_power(*enabled));
                }),
        )
}

fn light_emblem(enabled: bool, pal: Palette) -> impl IntoElement {
    let halo = if enabled {
        hsla(0.105, 0.9, 0.66, 0.22)
    } else {
        pal.muted
    };
    let icon_color: Hsla = if enabled {
        hsla(0.105, 0.9, 0.66, 1.)
    } else {
        pal.text_muted
    };
    let icon = if enabled {
        IconName::Sun
    } else {
        IconName::Moon
    };

    div()
        .relative()
        .size(px(64.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(pal.card_radius)
        .bg(halo)
        .border_1()
        .border_color(if enabled {
            hsla(0.105, 0.9, 0.66, 0.35)
        } else {
            pal.border
        })
        .when(enabled, |this| {
            this.shadow(vec![BoxShadow {
                color: hsla(0.105, 0.9, 0.66, 0.25),
                offset: point(px(0.), px(0.)),
                blur_radius: px(18.),
                spread_radius: px(1.),
                inset: false,
            }])
        })
        .child(Icon::new(icon).size_7().text_color(icon_color))
}

#[cfg(target_os = "macos")]
fn camera_automation(current: LightSettings, pal: Palette) -> impl IntoElement {
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
                .child(
                    div()
                        .text_body()
                        .text_color(pal.text_primary)
                        .child(tr!("lighting.auto_on_with_camera")),
                )
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(tr!("lighting.camera_light_auto_description")),
                ),
        )
        .child(
            Toggle::new("standalone-light-camera-automation")
                .selected(current.auto_camera)
                .min_width(px(72.))
                .on_change(|auto_camera, _window, cx| {
                    AppState::apply(cx, |state| {
                        let mut light = state.light();
                        light.auto_camera = *auto_camera;
                        state.commit_light(light)
                    });
                }),
        )
}

fn light_status(view: LightView, pal: Palette) -> impl IntoElement {
    let LightView { online, enabled } = view;
    let (label, color) = if !online {
        (tr!("device.offline"), theme::STATUS_OFFLINE)
    } else if enabled {
        (tr!("common.on"), theme::STATUS_CONNECTED)
    } else {
        (tr!("common.off"), theme::STATUS_OFFLINE)
    };
    h_flex()
        .gap_1p5()
        .items_center()
        .text_caption()
        .text_color(pal.text_muted)
        .child(div().size_1p5().rounded_full().bg(rgb(color)))
        .child(label)
}

fn control_well(
    title: gpui::SharedString,
    value: String,
    endpoints: (String, String),
    slider: impl IntoElement,
    pal: Palette,
) -> impl IntoElement {
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
                        .child(value),
                ),
        )
        .child(slider)
        .child(
            h_flex()
                .justify_between()
                .text_caption()
                .text_color(pal.text_muted)
                .child(endpoints.0)
                .child(endpoints.1),
        )
}

fn format_range_endpoints(range: LightValueRange) -> (String, String) {
    (
        format_light_value(range.min(), range.unit()),
        format_light_value(range.max(), range.unit()),
    )
}

fn format_light_value(value: u16, unit: LightValueUnit) -> String {
    match unit {
        LightValueUnit::Lumens => format!("{value} lm"),
        LightValueUnit::Kelvin => format!("{value} K"),
        LightValueUnit::Percent => format!("{value}%"),
    }
}

fn midpoint(range: LightValueRange) -> u16 {
    range.quantize(range.min() + (range.max() - range.min()) / 2)
}

fn light_command_status(status: LightCommandStatus, pal: Palette) -> impl IntoElement {
    let (label, color) = match status {
        LightCommandStatus::Pending => (
            tr!("lighting.applying_light_setting").to_string(),
            pal.text_muted,
        ),
        LightCommandStatus::Failed(error) => (
            format!("{}: {error}", tr!("common.unavailable")),
            Hsla::from(rgb(theme::STATUS_OFFLINE)),
        ),
        LightCommandStatus::Offline => (
            tr!("device.offline").to_string(),
            Hsla::from(rgb(theme::STATUS_OFFLINE)),
        ),
    };
    h_flex()
        .gap_1p5()
        .items_center()
        .text_caption()
        .text_color(pal.text_muted)
        .child(div().size_1p5().rounded_full().bg(color))
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::{format_light_value, midpoint};
    use openlogi_core::device::{LightValueRange, LightValueUnit};

    #[test]
    fn sliders_use_the_advertised_range_and_grid() {
        let range = LightValueRange::new(3000, 5000, 250, LightValueUnit::Kelvin)
            .expect("valid test range");
        assert_eq!(range.quantize(3120), 3000);
        assert_eq!(range.quantize(3370), 3250);
        assert_eq!(midpoint(range), 4000);
    }

    #[test]
    fn range_values_use_capability_units() {
        assert_eq!(format_light_value(20, LightValueUnit::Lumens), "20 lm");
        assert_eq!(format_light_value(2700, LightValueUnit::Kelvin), "2700 K");
        assert_eq!(format_light_value(100, LightValueUnit::Percent), "100%");
    }
}
