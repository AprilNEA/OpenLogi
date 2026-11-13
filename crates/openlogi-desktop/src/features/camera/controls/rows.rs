//! The rows the camera controls panel renders: the profile chips, one slider or chip row per control, and the reset button.

use crate::state::AppState;
use crate::ui::components::ProfileTab;
use crate::ui::slider_row::SliderRow;
use crate::ui::theme::FocusRingStyle as _;
use crate::ui::theme::{self, ACCENT_BLUE, Palette, PressableStyle as _, Typography as _};
use gpui::{
    ClickEvent, Context, ElementId, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, Role, SharedString, StatefulInteractiveElement as _, Styled, Toggled, div,
    prelude::FluentBuilder as _, rgb,
};
use gpui_kit::base::Button as BaseButton;
use gpui_kit::component::{IconName, Selectable as _, h_flex, slider::Slider};
use openlogi_camera::CameraControl;

use super::{BUILTIN_PROFILES, CameraControlsPanel, ControlSlider};

/// Indices of the lens (camera-terminal) or image (processing-unit) sliders,
/// preserving [`CameraControl::ALL`] order.
pub(super) fn section_indices(sliders: &[ControlSlider], lens: bool) -> Vec<usize> {
    sliders
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            matches!(
                s.control,
                CameraControl::Zoom
                    | CameraControl::Pan
                    | CameraControl::Tilt
                    | CameraControl::Focus
                    | CameraControl::Exposure
            ) == lens
        })
        .map(|(ix, _)| ix)
        .collect()
}

/// The one-click profile chips: built-ins, saved customs, then Save.
pub(super) fn profiles_row(key: &str, cx: &mut Context<CameraControlsPanel>) -> gpui::Div {
    let state = AppState::try_read(cx);
    let active = state.and_then(|s| s.camera_active_profile(key));
    let customs: Vec<String> = state.map_or_else(Default::default, |s| {
        s.camera_profiles(key).keys().cloned().collect()
    });

    let mut row = h_flex().flex_wrap().gap_1p5().items_center();
    for (ix, builtin) in BUILTIN_PROFILES.iter().enumerate() {
        let id = builtin.id;
        row = row.child(
            ProfileTab::new(("camera-profile-builtin", ix), builtin_label(id))
                .selected(active.as_deref() == Some(id))
                .on_click(cx.listener(move |panel, _: &ClickEvent, window, cx| {
                    panel.apply_profile(id, window, cx);
                })),
        );
    }
    for (ix, name) in customs.into_iter().enumerate() {
        let is_active = active.as_deref() == Some(name.as_str());
        let apply_name = name.clone();
        let delete_name = name.clone();
        let on_apply = cx.listener(move |panel, _: &ClickEvent, window, cx| {
            panel.apply_profile(&apply_name, window, cx);
        });
        let on_delete = cx.listener(move |panel, _: &ClickEvent, _window, cx| {
            panel.delete_profile(&delete_name, cx);
        });
        row = row.child(
            ProfileTab::new(("camera-profile-custom", ix), name)
                .selected(is_active)
                .on_click(on_apply)
                .on_delete(("camera-profile-del", ix), on_delete),
        );
    }
    row = row.child(
        ProfileTab::new("camera-profile-save", tr!("common.new"))
            .icon(IconName::Plus)
            .on_click(cx.listener(|panel, _: &ClickEvent, _window, cx| {
                panel.save_profile(cx);
            })),
    );
    row
}

/// One section's controls, two to a line: a camera reports a dozen or more,
/// and stacked one to a line they would push the panel far below the preview
/// it sits beside. A chip row takes a line of its own.
pub(super) fn control_grid(
    panel: &CameraControlsPanel,
    indices: Vec<usize>,
    cx: &Context<CameraControlsPanel>,
) -> gpui::Div {
    div()
        .grid()
        .grid_cols(2)
        .gap_x_4()
        .gap_y_3()
        .w_full()
        .children(indices.into_iter().map(|ix| control_row(panel, ix, cx)))
}

/// One control as a [`SliderRow`]: its label and value on one line, with the
/// Auto chip before the value when the device pairs one, and the slider
/// across the row. The value opens an exact entry; double-click anywhere on
/// the row resets that control.
fn control_row(
    panel: &CameraControlsPanel,
    ix: usize,
    cx: &Context<CameraControlsPanel>,
) -> gpui::Stateful<gpui::Div> {
    let pal = theme::palette(cx);
    let slider = &panel.sliders[ix];
    if slider.control == CameraControl::PowerLineFrequency
        && [1, 2, 3]
            .into_iter()
            .any(|value| slider.range.supports(value))
    {
        return frequency_row(panel, ix, cx, pal);
    }
    if slider.control == CameraControl::LowLightCompensation
        && slider.range.min == 0
        && slider.range.max == 1
    {
        return binary_control_row(panel, ix, cx, pal);
    }
    let value = slider.value(cx);
    let auto_on = panel.auto_state_for(slider.control);
    let dimmed = auto_on == Some(true);

    let auto_chip = auto_on
        .zip(slider.control.auto_toggle())
        .and_then(|(on, toggle)| {
            let auto_ix = panel.autos.iter().position(|a| a.toggle == toggle)?;
            let accent = rgb(ACCENT_BLUE);
            Some(
                BaseButton::new((ElementId::from("camera-control-auto"), toggle.name()))
                    .role(Role::CheckBox)
                    .selected(on)
                    .accessibility_label(tr!("common.auto"))
                    .aria_toggled(if on { Toggled::True } else { Toggled::False })
                    .px_1p5()
                    .py_0p5()
                    .rounded_full()
                    .border_1()
                    .border_color(if on { accent.into() } else { pal.border })
                    .text_caption()
                    .text_color(if on { pal.text_primary } else { pal.text_muted })
                    .bg(if on {
                        theme::accent_tint()
                    } else {
                        pal.control
                    })
                    .pressable_fill(on, pal)
                    .focus_visible(move |s| s.focus_ring(pal))
                    .child(tr!("common.auto"))
                    .on_click(cx.listener(move |panel, _: &ClickEvent, _window, cx| {
                        panel.toggle_auto(auto_ix, cx);
                    })),
            )
        });

    div()
        .id(("camera-control-row", ix))
        .w_full()
        // Capture phase, so the double-click wins over the slider's own
        // handlers: the thumb's mouse-down stops propagation (a bubbled click
        // never fires), and a track click would jump the value and then
        // re-commit it from its deferred Release event after the reset ran.
        .capture_any_mouse_down(cx.listener(
            move |panel, event: &MouseDownEvent, window, cx| {
                if event.button == MouseButton::Left && event.click_count == 2 {
                    cx.stop_propagation();
                    panel.reset_control(ix, window, cx);
                }
            },
        ))
        .child(
            SliderRow::new(
                slider.label.clone(),
                div()
                    .w_full()
                    // Dimmed while auto owns the value, but still draggable —
                    // grabbing the slider takes the control over to manual.
                    .when(dimmed, |s| s.opacity(0.55))
                    .child(Slider::new(slider.slider.slider()).horizontal()),
            )
            .value(
                value.to_string(),
                [slider.range.min.to_string(), slider.range.max.to_string()],
            )
            .when_some(auto_chip, SliderRow::marker)
            .value_entry(
                ("camera-control-value", ix),
                tr!("camera.set_value_label", label => slider.label.clone()),
                cx.listener(move |panel, _: &ClickEvent, window, cx| {
                    panel.open_value_dialog(ix, window, cx);
                }),
            )
            .disabled(dimmed),
        )
}

/// A chip row: the label, then the control's chips at the end of a line the
/// width of the panel.
fn choice_row(
    ix: usize,
    label: SharedString,
    choices: impl IntoElement,
    pal: Palette,
) -> gpui::Stateful<gpui::Div> {
    h_flex()
        .id(("camera-control-row", ix))
        .col_span_full()
        .w_full()
        .gap_4()
        .items_center()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_body()
                .text_color(pal.text_primary)
                .child(label),
        )
        .child(div().flex_shrink_0().child(choices))
}

fn frequency_row(
    panel: &CameraControlsPanel,
    ix: usize,
    cx: &Context<CameraControlsPanel>,
    pal: Palette,
) -> gpui::Stateful<gpui::Div> {
    let slider = &panel.sliders[ix];
    let current = slider.value(cx);
    let mut choices = h_flex().gap_1();
    for (value, id, label) in [
        (1, 1_u32, SharedString::from("50 Hz")),
        (2, 2_u32, SharedString::from("60 Hz")),
        (3, 3_u32, tr!("common.auto")),
    ]
    .into_iter()
    .filter(|(value, _, _)| slider.range.supports(*value))
    {
        let active = value == current;
        let accent = rgb(ACCENT_BLUE);
        let accessibility_label = label.clone();
        choices = choices.child(
            BaseButton::new(("camera-frequency", id))
                .role(Role::RadioButton)
                .selected(active)
                .accessibility_label(accessibility_label)
                .aria_toggled(if active {
                    Toggled::True
                } else {
                    Toggled::False
                })
                .aria_selected(active)
                .px_1p5()
                .py_0p5()
                .rounded_full()
                .border_1()
                .border_color(if active { accent.into() } else { pal.border })
                .text_caption()
                .text_color(if active {
                    pal.text_primary
                } else {
                    pal.text_muted
                })
                .bg(if active {
                    theme::accent_tint()
                } else {
                    pal.control
                })
                .pressable_fill(active, pal)
                .focus_visible(move |s| s.focus_ring(pal))
                .child(label)
                .on_click(cx.listener(move |panel, _: &ClickEvent, window, cx| {
                    let (Some(key), Some(uid)) = (panel.key.clone(), panel.uid.clone()) else {
                        return;
                    };
                    panel.sliders[ix].seat(value, window, cx);
                    panel.commit_release(CameraControl::PowerLineFrequency, &uid, &key, value, cx);
                })),
        );
    }

    choice_row(ix, slider.label.clone(), choices, pal)
}

fn binary_control_row(
    panel: &CameraControlsPanel,
    ix: usize,
    cx: &Context<CameraControlsPanel>,
    pal: Palette,
) -> gpui::Stateful<gpui::Div> {
    let slider = &panel.sliders[ix];
    let on = slider.value(cx) != 0;
    let accent = rgb(ACCENT_BLUE);
    choice_row(
        ix,
        slider.label.clone(),
        BaseButton::new("camera-low-light")
            .role(Role::CheckBox)
            .selected(on)
            .accessibility_label(tr!("camera.low_light_compensation"))
            .aria_toggled(if on { Toggled::True } else { Toggled::False })
            .px_1p5()
            .py_0p5()
            .rounded_full()
            .border_1()
            .border_color(if on { accent.into() } else { pal.border })
            .text_caption()
            .text_color(if on { pal.text_primary } else { pal.text_muted })
            .bg(if on {
                theme::accent_tint()
            } else {
                pal.control
            })
            .pressable_fill(on, pal)
            .focus_visible(move |s| s.focus_ring(pal))
            .child(if on {
                tr!("common.on")
            } else {
                tr!("common.off")
            })
            .on_click(cx.listener(move |panel, _: &ClickEvent, window, cx| {
                let (Some(key), Some(uid)) = (panel.key.clone(), panel.uid.clone()) else {
                    return;
                };
                let value = i32::from(!on);
                panel.sliders[ix].seat(value, window, cx);
                panel.commit_release(CameraControl::LowLightCompensation, &uid, &key, value, cx);
            })),
        pal,
    )
}

pub(super) fn reset_button(cx: &mut Context<CameraControlsPanel>) -> gpui::Div {
    let pal = theme::palette(cx);
    h_flex().w_full().justify_end().child(
        BaseButton::new("camera-controls-reset")
            .accessibility_label(tr!("camera.reset_to_defaults"))
            .px_2p5()
            .py_0p5()
            .rounded_md()
            .border_1()
            .border_color(pal.border)
            .bg(pal.control)
            .pressable_fill(false, pal)
            .focus_visible(move |s| s.focus_ring(pal))
            .text_caption()
            .text_color(pal.text_muted)
            .child(tr!("camera.reset_to_defaults"))
            .on_click(cx.listener(|panel, _: &ClickEvent, window, cx| {
                panel.reset(window, cx);
            })),
    )
}

fn builtin_label(id: &str) -> SharedString {
    match id {
        "streaming" => tr!("camera.streaming"),
        "video_call" => tr!("camera.video_call"),
        _ => tr!("common.default"),
    }
}

pub(super) fn control_label(control: CameraControl) -> SharedString {
    match control {
        CameraControl::Zoom => tr!("common.zoom"),
        CameraControl::Pan => tr!("camera.pan"),
        CameraControl::Tilt => tr!("camera.tilt"),
        CameraControl::Focus => tr!("camera.focus"),
        CameraControl::Exposure => tr!("camera.exposure"),
        CameraControl::PowerLineFrequency => tr!("camera.anti_flicker"),
        CameraControl::LowLightCompensation => tr!("camera.low_light_compensation"),
        CameraControl::Brightness => tr!("camera.brightness"),
        CameraControl::Contrast => tr!("camera.contrast"),
        CameraControl::Saturation => tr!("camera.saturation"),
        CameraControl::Sharpness => tr!("camera.sharpness"),
        CameraControl::Gain => tr!("camera.gain"),
        CameraControl::BacklightCompensation => tr!("camera.backlight_compensation"),
        CameraControl::WhiteBalance => tr!("camera.white_balance"),
        CameraControl::Tint => tr!("camera.tint"),
    }
}

/// A UVC control value as the GPUI slider wants it.
#[expect(
    clippy::cast_precision_loss,
    reason = "a UVC control range is far below f32's exact integer range"
)]
pub(super) fn to_slider(value: i32) -> f32 {
    value as f32
}

/// Inverse of [`to_slider`].
#[expect(
    clippy::cast_possible_truncation,
    reason = "the slider steps by 1 over the control's own i32 range"
)]
pub(super) fn from_slider(value: f32) -> i32 {
    value.round() as i32
}
