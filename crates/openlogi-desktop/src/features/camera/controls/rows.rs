//! The rows the camera controls panel renders: the profile chips, one slider or chip row per control, and the reset button.

use crate::state::AppState;
use crate::ui::components::{ProfileTab, control_button, segment, segment_track};
use crate::ui::section::{section_label, settings_group};
use crate::ui::theme::{self, ACCENT_BLUE, Palette, Typography as _};
use gpui::{
    ClickEvent, Context, ElementId, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, Rems, Role, SharedString, StatefulInteractiveElement as _, Styled, Toggled,
    Window, div, prelude::FluentBuilder as _, rems, rgb,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    IconName, Selectable as _, Sizable as _, h_flex, slider::Slider, switch::Switch, v_flex,
};
use openlogi_camera::CameraControl;

use super::{BUILTIN_PROFILES, CameraControlsPanel, ControlSlider};

// In rems, so the lanes scale with the text they hold.
/// Fits an English label ("White balance") and the Auto segment beside it.
const LABEL_W: Rems = rems(9.5);
/// Fits a four-digit reading such as a white-balance temperature.
const VALUE_W: Rems = rems(3.);
/// One row of a group, whatever control it holds.
const ROW_H: Rems = rems(2.5);

/// The groups the controls read in. Lens sits under the preview because it
/// changes what the preview frames; the other two shape the light and colour.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ControlGroup {
    Lens,
    Exposure,
    Image,
}

impl ControlGroup {
    pub(super) fn of(control: CameraControl) -> Self {
        match control {
            CameraControl::Zoom
            | CameraControl::Pan
            | CameraControl::Tilt
            | CameraControl::Focus => Self::Lens,
            CameraControl::Exposure
            | CameraControl::PowerLineFrequency
            | CameraControl::LowLightCompensation
            | CameraControl::Gain
            | CameraControl::BacklightCompensation => Self::Exposure,
            CameraControl::Brightness
            | CameraControl::Contrast
            | CameraControl::Saturation
            | CameraControl::Sharpness
            | CameraControl::WhiteBalance
            | CameraControl::Tint => Self::Image,
        }
    }

    fn title(self) -> SharedString {
        match self {
            Self::Lens => tr!("camera.lens"),
            Self::Exposure => tr!("camera.exposure"),
            Self::Image => tr!("camera.image"),
        }
    }
}

/// The preset bar: the same chrome as the device profile bar, because a preset
/// is the same idea as a profile. Built-ins, saved customs, then New, with the
/// reset that undoes them on the far edge. Empty until the panel has rows.
pub(super) fn presets_bar(
    panel: &mut CameraControlsPanel,
    cx: &mut Context<CameraControlsPanel>,
) -> gpui::Div {
    let pal = theme::palette(cx);
    let Some(key) = panel.key.clone().filter(|_| !panel.sliders.is_empty()) else {
        return div();
    };
    let state = AppState::try_read(cx);
    let active = state.and_then(|s| s.camera_active_profile(&key));
    let customs: Vec<String> = state
        .map(|s| s.camera_profiles(&key).keys().cloned().collect())
        .unwrap_or_default();

    let mut tabs = h_flex()
        .flex_1()
        .min_w_0()
        .flex_wrap()
        .items_center()
        .gap_1();
    for (ix, builtin) in BUILTIN_PROFILES.iter().enumerate() {
        let id = builtin.id;
        tabs = tabs.child(
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
        tabs = tabs.child(
            ProfileTab::new(("camera-profile-custom", ix), name)
                .selected(is_active)
                .on_click(on_apply)
                .on_delete(("camera-profile-del", ix), on_delete),
        );
    }
    tabs = tabs.child(
        ProfileTab::new("camera-profile-save", tr!("common.new"))
            .icon(IconName::Plus)
            .on_click(cx.listener(|panel, _: &ClickEvent, _window, cx| {
                panel.save_profile(cx);
            })),
    );
    h_flex()
        .flex_shrink_0()
        .w_full()
        .items_center()
        .gap_2()
        .border_b_1()
        .border_color(pal.border)
        .bg(pal.panel)
        .px_4()
        .py_2()
        .child(
            div()
                .flex_none()
                .text_body()
                .text_color(pal.text_muted)
                .child(tr!("common.presets")),
        )
        .child(tabs)
        .child(reset_button(cx))
}

/// The lens group on its own, for the column under the preview. Empty until
/// the panel has a camera's rows, or when the camera has no lens controls.
pub(super) fn lens_section(
    panel: &mut CameraControlsPanel,
    cx: &mut Context<CameraControlsPanel>,
) -> gpui::Div {
    control_section(panel, ControlGroup::Lens, cx).unwrap_or_else(div)
}

/// One titled group of control rows, the title on the rows' inset; `None` when
/// the camera reports no control in the group.
pub(super) fn control_section(
    panel: &CameraControlsPanel,
    group: ControlGroup,
    cx: &Context<CameraControlsPanel>,
) -> Option<gpui::Div> {
    let pal = theme::palette(cx);
    let mut indices: Vec<usize> = panel
        .sliders
        .iter()
        .enumerate()
        .filter(|(_, s)| ControlGroup::of(s.control) == group)
        .map(|(ix, _)| ix)
        .collect();
    // Sliders, then switches and choices: rows of one shape stay together, so
    // the eye runs down a column instead of zig-zagging. The sort is stable.
    indices.sort_by_key(|ix| is_discrete(&panel.sliders[*ix]));
    if indices.is_empty() {
        return None;
    }
    let mut rows = settings_group(pal);
    for (position, ix) in indices.into_iter().enumerate() {
        rows = rows.child(control_row(panel, ix, position == 0, cx));
    }
    Some(
        v_flex()
            .w_full()
            .gap_2()
            .child(section_label(group.title(), pal).px_4())
            .child(rows),
    )
}

/// Whether the control is a choice rather than a position along a range.
fn is_discrete(slider: &ControlSlider) -> bool {
    is_frequency_choice(slider) || is_binary(slider)
}

fn is_frequency_choice(slider: &ControlSlider) -> bool {
    slider.control == CameraControl::PowerLineFrequency
        && [0, 1, 2, 3]
            .into_iter()
            .any(|value| slider.range.supports(value))
}

fn is_binary(slider: &ControlSlider) -> bool {
    slider.range.min == 0 && slider.range.max == 1
}

/// One compact control line: label · slider · live value (· Auto chip when the
/// device pairs one). Double-click anywhere on the line resets that control.
fn control_row(
    panel: &CameraControlsPanel,
    ix: usize,
    first: bool,
    cx: &Context<CameraControlsPanel>,
) -> gpui::Stateful<gpui::Div> {
    let pal = theme::palette(cx);
    let slider = &panel.sliders[ix];
    if is_frequency_choice(slider) {
        return frequency_row(panel, ix, first, cx, pal);
    }
    if is_binary(slider) {
        return binary_control_row(panel, ix, first, cx, pal);
    }
    let value = slider.value(cx);
    let auto_on = panel.auto_state_for(slider.control);
    let dimmed = auto_on == Some(true);

    // Auto qualifies the control's name, so it rides the label rather than
    // taking a lane that would strand every value from its slider.
    let mut label_cell = h_flex()
        .w(LABEL_W)
        .flex_shrink_0()
        .items_center()
        .gap_2()
        .child(row_label(slider, pal).flex_1().min_w_0().truncate());
    if let Some(on) = auto_on
        && let Some(toggle) = slider.control.auto_toggle()
        && let Some(auto_ix) = panel.autos.iter().position(|a| a.toggle == toggle)
    {
        label_cell = label_cell.child(
            segment_track(pal).child(
                segment(
                    (ElementId::from("camera-control-auto"), toggle.name()),
                    on,
                    pal,
                )
                .role(Role::CheckBox)
                .accessibility_label(tr!("common.auto"))
                .aria_toggled(if on { Toggled::True } else { Toggled::False })
                .child(tr!("common.auto"))
                .on_click(cx.listener(
                    move |panel, _: &ClickEvent, _window, cx| {
                        panel.toggle_auto(auto_ix, cx);
                    },
                )),
            ),
        );
    }
    let row = row_shell(ix, first, pal)
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
        .child(label_cell)
        .child(
            div()
                .flex_1()
                // The thumb overhangs the track ends by its radius; the inset
                // keeps it clear of the lanes on either side.
                .px_2()
                // Dimmed while auto owns the value, but still draggable —
                // grabbing the slider takes the control over to manual.
                .when(dimmed, |s| s.opacity(0.55))
                .child(Slider::new(slider.slider.slider()).horizontal()),
        );

    let value_button = BaseButton::new(("camera-control-value", ix))
        .w(VALUE_W)
        .flex_shrink_0()
        .justify_end()
        .rounded_md()
        .text_body()
        .when(dimmed, |s| s.opacity(0.55))
        .text_color(pal.text_muted)
        .hover(move |s| s.bg(pal.control_hover))
        .focus_visible(move |s| s.bg(pal.control_hover))
        .accessibility_label(tr!("camera.set_value_label", label => slider.label.clone()))
        .child(format!("{value}"))
        .on_click(cx.listener(move |panel, _: &ClickEvent, window, cx| {
            panel.open_value_dialog(ix, window, cx);
        }));

    row.child(value_button)
}

/// One row's outer box: every kind of row shares its height and inset, so a
/// group keeps one rhythm whichever controls a camera reports.
fn row_shell(ix: usize, first: bool, pal: Palette) -> gpui::Stateful<gpui::Div> {
    h_flex()
        .id(("camera-control-row", ix))
        .w_full()
        .min_h(ROW_H)
        .px_4()
        .gap_3()
        .items_center()
        .when(!first, |row| row.border_t_1().border_color(pal.border))
}

fn row_label(slider: &ControlSlider, pal: Palette) -> gpui::Div {
    div()
        .text_body()
        .text_color(pal.text_primary)
        .child(slider.label.clone())
}

/// Anti-flicker as its discrete modes. UVC numbers them 0 = off, 1 = 50 Hz,
/// 2 = 60 Hz, 3 = auto; a camera offers a subset.
fn frequency_row(
    panel: &CameraControlsPanel,
    ix: usize,
    first: bool,
    cx: &Context<CameraControlsPanel>,
    pal: Palette,
) -> gpui::Stateful<gpui::Div> {
    let slider = &panel.sliders[ix];
    let current = slider.value(cx);
    let mut choices = segment_track(pal);
    for (value, label) in [
        (0, tr!("common.off")),
        (1, SharedString::from("50 Hz")),
        (2, SharedString::from("60 Hz")),
        (3, tr!("common.auto")),
    ]
    .into_iter()
    .filter(|(value, _)| slider.range.supports(*value))
    {
        let active = value == current;
        choices = choices.child(
            segment(("camera-frequency", value.cast_unsigned()), active, pal)
                .role(Role::RadioButton)
                .accessibility_label(label.clone())
                .aria_toggled(if active {
                    Toggled::True
                } else {
                    Toggled::False
                })
                .aria_selected(active)
                .child(label)
                .on_click(cx.listener(move |panel, _: &ClickEvent, window, cx| {
                    panel.commit_choice(ix, value, window, cx);
                })),
        );
    }

    row_shell(ix, first, pal)
        .child(row_label(slider, pal).flex_1().min_w_0())
        .child(choices)
}

/// A control the camera reports as 0 or 1 is a switch, not a two-stop slider.
fn binary_control_row(
    panel: &CameraControlsPanel,
    ix: usize,
    first: bool,
    cx: &Context<CameraControlsPanel>,
    pal: Palette,
) -> gpui::Stateful<gpui::Div> {
    let slider = &panel.sliders[ix];
    let this = cx.entity();
    row_shell(ix, first, pal)
        .child(row_label(slider, pal).flex_1().min_w_0())
        .child(
            Switch::new(("camera-control-switch", ix))
                .checked(slider.value(cx) != 0)
                .small()
                .color(rgb(ACCENT_BLUE))
                .accessibility_label(slider.label.clone())
                .on_click(move |on, window, cx| {
                    this.update(cx, |panel, cx| {
                        panel.commit_choice(ix, i32::from(*on), window, cx);
                    });
                }),
        )
}

impl CameraControlsPanel {
    /// Commit a value picked from a discrete row, which has no slider release
    /// to do it.
    fn commit_choice(
        &mut self,
        ix: usize,
        value: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(key), Some(uid)) = (self.key.clone(), self.uid.clone()) else {
            return;
        };
        let Some(slider) = self.sliders.get(ix) else {
            return;
        };
        let control = slider.control;
        slider.seat(value, window, cx);
        self.commit_release(control, &uid, &key, value, cx);
    }
}

fn reset_button(cx: &mut Context<CameraControlsPanel>) -> impl IntoElement {
    control_button("camera-controls-reset")
        .outline()
        .label(tr!("camera.reset_to_defaults"))
        .on_click(cx.listener(|panel, _: &ClickEvent, window, cx| {
            panel.reset(window, cx);
        }))
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
