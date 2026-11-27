//! RGB keyboard lighting controls.
//!
//! A palette of color swatches, an on/off toggle, and a brightness slider,
//! persisted per device via [`AppState::commit_lighting`] and pushed to the
//! keyboard through `openlogi_agent_core::hardware::set_lighting_in_background`
//! (the agent, over IPC — the GUI has no device I/O of its own).

use crate::ui::theme::FocusRingStyle as _;
use gpui::{
    App, AppContext as _, Context, Entity, Focusable as _, Hsla, InteractiveElement, IntoElement,
    ParentElement, Render, Rgba, Role, StatefulInteractiveElement as _, Styled, Subscription,
    Toggled, Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_kit::base::Button as BaseButton;
use gpui_kit::component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_kit::component::input::InputEvent;
use gpui_kit::component::slider::{Slider, SliderEvent};
use gpui_kit::component::{IconName, Selectable as _, Sizable as _, Size, h_flex, v_flex};
use openlogi_core::color::Rgb;
use openlogi_core::config::Lighting;

use crate::state::{AppState, StateEvent};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::components::{Toggle, toggle_row};
use crate::ui::slider_row::SliderRow;
use crate::ui::theme::{self, Palette};

/// The coloured face of a swatch.
const SWATCH: f32 = 24.;
/// The selection ring around a swatch, and the clear gap between the two.
const SWATCH_RING: f32 = 2.;
const SWATCH_RING_GAP: f32 = 2.;

/// Preset colors. Deliberately small — covering the common keyboard accent
/// colors.
const PALETTE: &[Rgb] = &[
    Rgb::new(0xff, 0x3b, 0x30),
    Rgb::new(0xff, 0x95, 0x00),
    Rgb::new(0xff, 0xcc, 0x00),
    Rgb::new(0x34, 0xc7, 0x59),
    Rgb::new(0x00, 0xc7, 0xbe),
    Rgb::new(0x00, 0x7a, 0xff),
    Rgb::new(0x58, 0x56, 0xd6),
    Rgb::new(0xaf, 0x52, 0xde),
    Rgb::WHITE,
];

/// The color the custom control stands for: the current color when the
/// lighting is on and it is none of the presets.
fn custom_color(current: &Lighting) -> Option<Rgb> {
    (current.enabled && !PALETTE.contains(&current.color)).then_some(current.color)
}

/// A picker color as the 8-bit RGB the keyboard takes. The keys have no
/// alpha, so a translucent pick lights them at full color.
fn rgb_of(color: Hsla) -> Rgb {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0.0..=255.0 before the cast"
    )]
    fn channel(value: f32) -> u8 {
        (value.clamp(0., 1.) * 255.).round() as u8
    }
    let color = Rgba::from(color);
    Rgb::new(channel(color.r), channel(color.g), channel(color.b))
}

/// One report from the custom-color picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PickerReport {
    /// It opened, closed or redrew, showing `shown`.
    Notified { shown: Option<Rgb> },
    /// Its color changed to this one.
    Changed(Rgb),
    /// A slider thumb was let go on this color.
    Released(Rgb),
    /// Its hex field was typed in.
    Typed,
    /// Its hex field lost focus, showing `shown`. Leaving the page does that
    /// too, by no longer drawing the field.
    FieldLeft { shown: Option<Rgb> },
}

/// Where an edit in the custom-color picker stands.
///
/// A palette pick or a hex commit closes the picker before it reports the
/// color, so a color a closed picker reports is final. A slider drag reports
/// every step while the picker stays open; its release writes it, the way
/// [`CommitSlider`] does, because leaving the page does not close the picker.
/// A typed hex color is only previewed, so it is written on Enter, when the
/// picker is dismissed, or when the field loses focus — which leaving the
/// page does without closing the picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PickerEdit {
    /// The picker is closed and holds nothing to write.
    Closed,
    /// The picker is open with nothing waiting to be written.
    Open,
    /// The picker is open on a typed color it only previews.
    Typed,
}

impl PickerEdit {
    /// The edit after the picker, now `open`, sent `report`, and the color
    /// to write now, if any.
    fn next(self, open: bool, report: PickerReport) -> (Self, Option<Rgb>) {
        let resting = if open { Self::Open } else { Self::Closed };
        match report {
            PickerReport::Changed(color) if !open => (Self::Closed, Some(color)),
            // A drag step; it replaces a typed color, and its release writes.
            PickerReport::Changed(_) => (Self::Open, None),
            PickerReport::Released(color) => (resting, Some(color)),
            PickerReport::Typed if open => (Self::Typed, None),
            PickerReport::Typed => (Self::Closed, None),
            PickerReport::FieldLeft { shown } => match self {
                Self::Typed => (resting, shown),
                _ => (resting, None),
            },
            PickerReport::Notified { shown } => match (self, open) {
                (Self::Typed, false) => (Self::Closed, shown),
                (Self::Closed, true) => (Self::Open, None),
                (edit, true) => (edit, None),
                (_, false) => (Self::Closed, None),
            },
        }
    }
}

/// Turn the lighting on in `color`. Every explicit pick is written, even one
/// that matches what the panel shows: with nothing saved, that is only the
/// default.
fn light_the_keys(color: Rgb, cx: &mut App) {
    AppState::apply(cx, |state| {
        let mut next = state.lighting();
        next.enabled = true;
        next.color = color;
        state.commit_lighting(next)
    });
}

pub struct LightingPanel {
    brightness: CommitSlider<u8>,
    /// The picker behind the custom-color control.
    picker: Entity<ColorPickerState>,
    /// The edit in that picker. Only [`Self::picker_reported`] moves it.
    edit: PickerEdit,
    _state_obs: Subscription,
    _picker_subscriptions: Vec<Subscription>,
}

impl LightingPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let initial = AppState::try_read(cx).map_or(100, |s| s.lighting().brightness);
        // The slider drives the device only on release, to avoid streaming a
        // frame burst to the keyboard for every intermediate drag value.
        let brightness = CommitSlider::new(
            SliderRange::new(0, 100).step(5.),
            initial,
            cx,
            |_, pct, cx| {
                AppState::apply(cx, |state| {
                    let mut lighting = state.lighting();
                    lighting.enabled = true;
                    lighting.brightness = pct;
                    state.commit_lighting(lighting)
                });
            },
        );
        let state_obs =
            AppState::repaint_on(cx, |event| matches!(event, StateEvent::LightingChanged(_)));
        let picker = cx.new(|cx| ColorPickerState::new(window, cx));
        let (hex_input, sliders) = {
            let picker = picker.read(cx);
            (picker.hex_input().clone(), picker.sliders().clone())
        };
        let hex_focus = hex_input.read(cx).focus_handle(cx);
        let mut subscriptions = vec![
            cx.subscribe(&picker, |this, _, event: &ColorPickerEvent, cx| {
                if let ColorPickerEvent::Change(Some(color)) = event {
                    this.picker_reported(PickerReport::Changed(rgb_of(*color)), cx);
                }
            }),
            // Opening and closing only notify.
            cx.observe(&picker, |this, picker, cx| {
                let shown = picker.read(cx).displayed_color().map(rgb_of);
                this.picker_reported(PickerReport::Notified { shown }, cx);
            }),
            // Only a typed edit changes the field; the picker writes it
            // without an event.
            cx.subscribe(&hex_input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.picker_reported(PickerReport::Typed, cx);
                }
            }),
            cx.on_blur(&hex_focus, window, |this, _, cx| {
                let shown = this.picker.read(cx).displayed_color().map(rgb_of);
                this.picker_reported(PickerReport::FieldLeft { shown }, cx);
            }),
        ];
        for slider in [
            sliders.hue(),
            sliders.saturation(),
            sliders.lightness(),
            sliders.alpha(),
        ] {
            // Each drag step already moved the picker's color, so it holds
            // the released one.
            subscriptions.push(cx.subscribe(slider, |this, _, event: &SliderEvent, cx| {
                if let SliderEvent::Release(_) = event
                    && let Some(color) = this.picker.read(cx).value()
                {
                    this.picker_reported(PickerReport::Released(rgb_of(color)), cx);
                }
            }));
        }
        Self {
            brightness,
            picker,
            edit: PickerEdit::Closed,
            _state_obs: state_obs,
            _picker_subscriptions: subscriptions,
        }
    }

    /// Move the picker edit on a picker report, and write what it settles.
    fn picker_reported(&mut self, report: PickerReport, cx: &mut Context<Self>) {
        let open = self.picker.read(cx).is_open();
        let (edit, write) = self.edit.next(open, report);
        self.edit = edit;
        if let Some(color) = write {
            light_the_keys(color, cx);
        }
    }

    /// The custom-color control: the picker's trigger in a swatch-sized ring.
    /// It shows the current color when that is none of the presets, and a
    /// palette icon otherwise.
    fn custom_control(
        &self,
        current: &Lighting,
        pal: Palette,
        window: &Window,
        cx: &App,
    ) -> impl IntoElement {
        let custom = custom_color(current);
        // The trigger is the picker's own focus target, so the ring it shares
        // with the swatches goes on this frame, under the same keyboard-only
        // rule as `focus_visible`.
        let focus_visible = self.picker.read(cx).focus_handle(cx).is_focused(window)
            && window.last_input_was_keyboard();
        let label = match custom {
            Some(color) => format!("{} #{:06X}", tr!("lighting.custom_color"), color.packed()),
            None => tr!("lighting.custom_color").to_string(),
        };
        let outer = SWATCH_RING + SWATCH_RING_GAP;
        div()
            .debug_selector(|| "lighting-custom-color".into())
            .flex()
            .items_center()
            .justify_center()
            .size(px(SWATCH + 2. * outer))
            .p(px(SWATCH_RING_GAP))
            .rounded(pal.control_radius + px(outer))
            .border(px(SWATCH_RING))
            .border_color(if custom.is_some() {
                theme::accent()
            } else {
                gpui::transparent_black()
            })
            .when(focus_visible, |frame| frame.focus_ring(pal))
            .child(
                ColorPicker::new(&self.picker)
                    .with_size(Size::Size(px(SWATCH)))
                    .featured_colors(
                        PALETTE
                            .iter()
                            .map(|color| Hsla::from(rgb(color.packed())))
                            .collect(),
                    )
                    .accessibility_label(label)
                    .when(custom.is_none(), |picker| picker.icon(IconName::Palette)),
            )
    }
}

impl Render for LightingPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let lighting = AppState::try_read(cx).map_or_else(Default::default, AppState::lighting);

        // Pull the slider thumb to the active device's brightness whenever it
        // changed in `AppState` (device switch / external edit), without
        // disturbing an in-progress drag.
        self.brightness.sync(lighting.brightness, window, cx);

        // Keep the closed picker on the device's color, so it opens there
        // after a device switch or a preset pick.
        let color = Hsla::from(rgb(lighting.color.packed()));
        let picker = self.picker.read(cx);
        if !picker.is_open() && picker.value() != Some(color) {
            self.picker
                .update(cx, |picker, cx| picker.set_value(color, window, cx));
        }

        let swatches: Vec<_> = PALETTE
            .iter()
            .map(|&color| swatch(color, &lighting, pal))
            .collect();
        let custom = self.custom_control(&lighting, pal, window, cx);

        v_flex()
            .gap_3()
            .w_full()
            // The tab and the card already say "Lighting"; the row names what
            // the switch does to the keyboard instead.
            .child(toggle_row(
                "light-row",
                pal,
                tr!("device.light_the_keys"),
                tr!("lighting.keyboard_lighting_description"),
                Toggle::new("light-toggle")
                    .selected(lighting.enabled)
                    .on_change(|enabled, _window, cx| {
                        AppState::apply(cx, |state| {
                            let mut next = state.lighting();
                            next.enabled = *enabled;
                            state.commit_lighting(next)
                        });
                    }),
            ))
            // The rings' clear gaps already space the faces; a full gap on top
            // would spread the row.
            .child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .children(swatches)
                    .child(custom),
            )
            .child(
                SliderRow::new(
                    tr!("camera.brightness"),
                    Slider::new(self.brightness.slider()).horizontal(),
                )
                .value(
                    format!("{}%", self.brightness.shown(lighting.brightness)),
                    ["100%"],
                ),
            )
    }
}

/// One color swatch. Clicking it turns lighting on and sets that color.
///
/// The selection ring sits a gap outside the colour, the way the keypad's
/// photo keys are ringed: a ring drawn on the swatch's own edge vanishes
/// against a blue or indigo swatch. The ring also shows, muted, under the
/// pointer, so the swatches read as pressable before one is pressed.
fn swatch(color: Rgb, current: &Lighting, pal: Palette) -> impl IntoElement {
    let selected = current.enabled && current.color == color;
    let outer = SWATCH_RING + SWATCH_RING_GAP;
    BaseButton::new(("light-swatch", color.packed()))
        .role(Role::RadioButton)
        .selected(selected)
        .accessibility_label(format!(
            "{} #{:06X}",
            tr!("device.lighting"),
            color.packed()
        ))
        .aria_toggled(if selected {
            Toggled::True
        } else {
            Toggled::False
        })
        .aria_selected(selected)
        .size(px(SWATCH + 2. * outer))
        .p(px(SWATCH_RING_GAP))
        // The ring follows the swatch's corner, one gap further out.
        .rounded(pal.control_radius + px(outer))
        .border(px(SWATCH_RING))
        .border_color(if selected {
            theme::accent()
        } else {
            gpui::transparent_black()
        })
        .hover(move |style| {
            style.border_color(if selected {
                theme::accent()
            } else {
                pal.text_muted
            })
        })
        .cursor_pointer()
        .focus_visible(move |style| style.focus_ring(pal))
        .child(
            // The hairline keeps a white or pale swatch visible on the card.
            div()
                .size_full()
                .rounded(pal.control_radius)
                .border_1()
                .border_color(pal.border)
                .bg(rgb(color.packed())),
        )
        .on_click(move |_event, _window, cx| light_the_keys(color, cx))
}

#[cfg(test)]
mod tests;
