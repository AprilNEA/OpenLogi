//! Controls for standalone lights.

use crate::state::{AppState, DeviceRecord, LightCommandStatus, StateEvent};
use crate::ui::components::Toggle;
use crate::ui::theme::{self, ACCENT_BLUE, Palette, Typography as _};
use gpui::{
    App, AppContext as _, BoxShadow, Context, Entity, Hsla, IntoElement, ParentElement, Render,
    Styled, Subscription, Window, div, hsla, point, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::{
    Icon, IconName, Selectable as _, h_flex,
    slider::{Slider, SliderEvent, SliderState},
    v_flex,
};
use openlogi_core::{
    config::LightSettings,
    device::{LightCapabilities, LightValueRange, LightValueUnit},
};

fn update_light(cx: &mut App, update: impl FnOnce(&mut AppState)) {
    AppState::update(cx, |state, cx| {
        let key = state.current_record().map(DeviceRecord::device_key);
        update(state);
        if let Some(key) = key {
            cx.emit(StateEvent::LightingChanged(key));
        }
    });
}

/// Standalone-light panel. The UI is driven by the active device's advertised
/// capabilities; the panel is not Litra-specific even though Litra is the
/// first driver.
pub struct LightPanel {
    brightness: Option<Entity<SliderState>>,
    temperature: Option<Entity<SliderState>>,
    brightness_range: Option<LightValueRange>,
    temperature_range: Option<LightValueRange>,
    device_key: Option<String>,
    last_brightness: u8,
    last_temperature: Option<u16>,
    brightness_sub: Option<Subscription>,
    temperature_sub: Option<Subscription>,
    _state_obs: Subscription,
}

impl LightPanel {
    /// Construct the panel. Capability-shaped sliders are created lazily when
    /// the selected device is known.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let settings = AppState::try_read(cx)
            .map(AppState::light)
            .unwrap_or_default();
        let state_obs = cx.subscribe(&AppState::global(cx), |_, _, event: &StateEvent, cx| {
            let relevant = match event {
                StateEvent::InventoryChanged
                | StateEvent::DeviceSelected(_)
                | StateEvent::CameraChanged => true,
                StateEvent::LightingChanged(key) => AppState::try_read(cx)
                    .and_then(AppState::current_record)
                    .is_some_and(|record| record.device_key() == *key),
                _ => false,
            };
            if relevant {
                cx.notify();
            }
        });
        Self {
            brightness: None,
            temperature: None,
            brightness_range: None,
            temperature_range: None,
            device_key: None,
            last_brightness: settings.brightness_percent,
            last_temperature: settings.temperature_kelvin,
            brightness_sub: None,
            temperature_sub: None,
            _state_obs: state_obs,
        }
    }

    fn ensure_sliders(
        &mut self,
        key: Option<&str>,
        capabilities: Option<LightCapabilities>,
        settings: LightSettings,
        cx: &mut Context<Self>,
    ) {
        let brightness_range = capabilities.and_then(|caps| caps.brightness);
        let temperature_range = capabilities.and_then(|caps| caps.temperature);
        if self.device_key.as_deref() == key
            && self.brightness_range == brightness_range
            && self.temperature_range == temperature_range
        {
            return;
        }

        self.brightness = None;
        self.temperature = None;
        self.brightness_sub = None;
        self.temperature_sub = None;
        self.device_key = key.map(str::to_string);
        self.brightness_range = brightness_range;
        self.temperature_range = temperature_range;

        if let Some(range) = brightness_range {
            let value = range
                .native_for_percent(settings.brightness_percent)
                .unwrap_or_else(|| range.min());
            let slider = cx.new(|_| {
                SliderState::new()
                    .max(f32::from(range.max()))
                    .min(f32::from(range.min()))
                    .step(f32::from(range.step()))
                    .default_value(f32::from(value))
            });
            let subscription =
                cx.subscribe(&slider, move |_panel, _slider, event: &SliderEvent, cx| {
                    if let SliderEvent::Release(value) = event {
                        let native = round_u16(value.start());
                        let Some(percent) = range.percent_for_native(native) else {
                            return;
                        };
                        update_light(cx, |state| {
                            let mut light = state.light();
                            if !state.camera_automation_active() {
                                light.enabled = true;
                            }
                            light.brightness_percent = percent;
                            state.commit_light(light);
                        });
                        cx.notify();
                    }
                });
            self.brightness = Some(slider);
            self.brightness_sub = Some(subscription);
        }

        if let Some(range) = temperature_range {
            let value = settings
                .temperature_kelvin
                .map_or_else(|| midpoint(range), |value| range.quantize(value));
            let slider = cx.new(|_| {
                SliderState::new()
                    .max(f32::from(range.max()))
                    .min(f32::from(range.min()))
                    .step(f32::from(range.step()))
                    .default_value(f32::from(value))
            });
            let subscription =
                cx.subscribe(&slider, move |_panel, _slider, event: &SliderEvent, cx| {
                    if let SliderEvent::Release(value) = event {
                        let kelvin = range.quantize(round_u16(value.start()));
                        update_light(cx, |state| {
                            let mut light = state.light();
                            if !state.camera_automation_active() {
                                light.enabled = true;
                            }
                            light.temperature_kelvin = Some(kelvin);
                            state.commit_light(light);
                        });
                        cx.notify();
                    }
                });
            self.temperature = Some(slider);
            self.temperature_sub = Some(subscription);
        }
    }
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
            record.as_ref().map(|record| record.config_key.as_str()),
            capabilities,
            settings,
            cx,
        );

        if settings.brightness_percent != self.last_brightness {
            self.last_brightness = settings.brightness_percent;
            if let (Some(range), Some(slider)) = (self.brightness_range, &self.brightness) {
                let value = range
                    .native_for_percent(settings.brightness_percent)
                    .unwrap_or_else(|| range.min());
                slider.update(cx, |slider, cx| {
                    slider.set_value(f32::from(value), window, cx);
                });
            }
        }
        if settings.temperature_kelvin != self.last_temperature {
            self.last_temperature = settings.temperature_kelvin;
            if let (Some(range), Some(slider)) = (self.temperature_range, &self.temperature) {
                let value = settings
                    .temperature_kelvin
                    .map_or_else(|| midpoint(range), |kelvin| range.quantize(kelvin));
                slider.update(cx, |slider, cx| {
                    slider.set_value(f32::from(value), window, cx);
                });
            }
        }

        let device_name = record.as_ref().map_or_else(
            || tr!("Lighting").to_string(),
            |record| record.display_name.clone(),
        );
        let online = record.as_ref().is_some_and(|record| record.online);
        let effective_enabled = AppState::try_read(cx).is_some_and(AppState::light_enabled);
        let power = capabilities.is_some_and(|caps| caps.power);

        let mut panel = v_flex().gap_4().w_full();
        if power {
            panel = panel.child(light_hero(&device_name, online, effective_enabled, pal));
            #[cfg(target_os = "macos")]
            {
                panel = panel.child(camera_automation(settings, pal));
            }
            panel = panel.child(div().h(px(1.)).w_full().bg(pal.border.opacity(0.55)));
        }
        if let (Some(range), Some(slider)) = (self.brightness_range, &self.brightness) {
            let value = range
                .native_for_percent(settings.brightness_percent)
                .unwrap_or_else(|| range.min());
            panel = panel.child(control_well(
                tr!("Brightness"),
                format_light_value(value, range.unit()),
                format_range_endpoints(range),
                Slider::new(slider).horizontal(),
                pal,
            ));
        }
        if let (Some(range), Some(slider)) = (self.temperature_range, &self.temperature) {
            let value = settings
                .temperature_kelvin
                .map_or_else(|| midpoint(range), |kelvin| range.quantize(kelvin));
            panel = panel.child(control_well(
                tr!("Colour temperature"),
                format_light_value(value, range.unit()),
                format_range_endpoints(range),
                Slider::new(slider).horizontal(),
                pal,
            ));
        }
        if let Some(status) = AppState::try_read(cx).and_then(AppState::light_command_status) {
            panel = panel.child(light_command_status(status, pal));
        }
        panel
    }
}

fn light_hero(
    device_name: &str,
    online: bool,
    effective_enabled: bool,
    pal: Palette,
) -> impl IntoElement {
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
                .child(light_status(online, effective_enabled, pal)),
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
                    update_light(cx, |state| {
                        state.commit_manual_light_power(*enabled);
                    });
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
                        .child(tr!("Auto-on with camera")),
                )
                .child(div().text_caption().text_color(pal.text_muted).child(tr!(
                    "Turn this light on while any camera is in use and off when cameras stop."
                ))),
        )
        .child(
            Toggle::new("standalone-light-camera-automation")
                .selected(current.auto_camera)
                .min_width(px(72.))
                .on_change(|auto_camera, _window, cx| {
                    update_light(cx, |state| {
                        let mut light = state.light();
                        light.auto_camera = *auto_camera;
                        state.commit_light(light);
                    });
                }),
        )
}

fn light_status(online: bool, enabled: bool, pal: Palette) -> impl IntoElement {
    let (label, color) = if !online {
        (tr!("Offline"), theme::STATUS_OFFLINE)
    } else if enabled {
        (tr!("On"), theme::STATUS_CONNECTED)
    } else {
        (tr!("Off"), theme::STATUS_OFFLINE)
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

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the slider value is clamped to the u16 range before conversion"
)]
fn round_u16(raw: f32) -> u16 {
    raw.clamp(0., f32::from(u16::MAX)).round() as u16
}

fn light_command_status(status: LightCommandStatus, pal: Palette) -> impl IntoElement {
    let (label, color) = match status {
        LightCommandStatus::Pending => (tr!("Applying light setting…").to_string(), pal.text_muted),
        LightCommandStatus::Failed(error) => (
            format!("{}: {error}", tr!("Unavailable")),
            Hsla::from(rgb(theme::STATUS_OFFLINE)),
        ),
        LightCommandStatus::Offline => (
            tr!("Offline").to_string(),
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
