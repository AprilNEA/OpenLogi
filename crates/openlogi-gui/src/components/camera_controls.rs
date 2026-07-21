//! Camera controls for the Camera tab: lens (zoom/focus/exposure) and image
//! (brightness/contrast/…) sliders, auto toggles, and profiles.
//!
//! Each slider drives a UVC control straight on the device, so a change is
//! seen by every app that opens the camera — Google Meet, Zoom, OBS — not just
//! our preview. Values are persisted per-camera and re-applied over USB when
//! the camera is next viewed, since the hardware only holds them until it
//! loses power. Focus/exposure/white-balance carry an Auto chip mirroring the
//! device's auto modes; their sliders disable while auto owns the value.
//!
//! Profiles are one-click control snapshots: three built-ins (Default /
//! Streaming / Video call) plus user-saved customs, applied to the hardware in
//! a single batched device-open.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "UVC control values are small integers; slider math goes through f32"
)]

use gpui::{
    AnyElement, AppContext as _, BorrowAppContext as _, ClickEvent, Context, Entity,
    InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Render,
    SharedString, StatefulInteractiveElement as _, Styled, Subscription, Window, div,
    prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::{
    h_flex,
    slider::{Slider, SliderEvent, SliderState},
    v_flex,
};
use openlogi_camera::{AutoToggle, CameraControl, ControlRange};
use openlogi_core::config::CameraControls;
use tracing::debug;

use crate::state::AppState;
use crate::theme::{self, ACCENT_BLUE, Palette};

/// Built-in profiles: `values` are fractions of each control's own range, so
/// they scale to whatever the camera reports. Auto modes all engage — the
/// point of a preset is a good picture without babysitting.
const BUILTIN_PROFILES: [BuiltinProfile; 3] = [
    BuiltinProfile {
        id: "default",
        values: &[],
    },
    BuiltinProfile {
        id: "streaming",
        values: &[
            (CameraControl::Brightness, 0.50),
            (CameraControl::Contrast, 0.58),
            (CameraControl::Saturation, 0.62),
            (CameraControl::Sharpness, 0.60),
        ],
    },
    BuiltinProfile {
        id: "video_call",
        values: &[
            (CameraControl::Brightness, 0.55),
            (CameraControl::Contrast, 0.52),
            (CameraControl::Saturation, 0.55),
            (CameraControl::Sharpness, 0.48),
        ],
    },
];

/// One built-in profile: an id for persistence plus range-relative targets
/// (an empty list means "device defaults for everything").
struct BuiltinProfile {
    id: &'static str,
    values: &'static [(CameraControl, f32)],
}

pub struct CameraControlsPanel {
    /// The `camera-<uid>` config key the panel is currently built for.
    key: Option<String>,
    sliders: Vec<ControlSlider>,
    autos: Vec<AutoRow>,
    #[allow(dead_code, reason = "held to keep the AppState observer alive")]
    state_obs: Subscription,
}

struct ControlSlider {
    control: CameraControl,
    label: SharedString,
    range: ControlRange,
    state: Entity<SliderState>,
    #[allow(dead_code, reason = "held to keep the slider subscription alive")]
    sub: Subscription,
}

/// Live UI state for one device-supported auto mode.
struct AutoRow {
    toggle: AutoToggle,
    on: bool,
    default: bool,
}

impl CameraControlsPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = cx.observe_global::<AppState>(|_panel, cx| cx.notify());
        Self {
            key: None,
            sliders: Vec::new(),
            autos: Vec::new(),
            state_obs,
        }
    }

    /// The active camera's config key, if a webcam is the selected device.
    fn active_key(cx: &Context<Self>) -> Option<String> {
        let record = cx.try_global::<AppState>()?.current_record()?;
        matches!(record.kind, openlogi_core::device::DeviceKind::Camera)
            .then(|| record.config_key.clone())
    }

    /// Build the sliders and auto rows for `key` from the device's reported
    /// state, re-applying any saved values in one batched device write. Cheap
    /// no-op when already built for this camera.
    fn ensure_built(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.key.as_deref() == Some(key) {
            return;
        }
        self.key = Some(key.to_string());
        self.sliders.clear();
        self.autos.clear();
        let uid = uid_of(key).to_string();

        // One device-open reads every control and auto state — keeps the USB
        // seize count low while the camera is also streaming the preview.
        let snap = openlogi_camera::read_camera_state(&uid).unwrap_or_default();

        // Saved auto states win over the device's, then saved values win for
        // controls whose auto is off; the differences push back in one open.
        let mut desired_autos = Vec::new();
        let mut apply_autos = Vec::new();
        for (toggle, st) in &snap.autos {
            let saved = cx
                .try_global::<AppState>()
                .and_then(|s| s.camera_auto(key, *toggle));
            let on = saved.unwrap_or(st.current);
            if on != st.current {
                apply_autos.push((*toggle, on));
            }
            desired_autos.push((*toggle, on, *st));
        }
        let auto_desired = |control: CameraControl| {
            let toggle = control.auto_toggle()?;
            desired_autos
                .iter()
                .find(|(t, ..)| *t == toggle)
                .map(|(_, on, _)| *on)
        };
        let mut desired_values = Vec::new();
        let mut apply_values = Vec::new();
        for (control, range) in &snap.controls {
            let saved = cx
                .try_global::<AppState>()
                .and_then(|s| s.camera_control(key, *control));
            let initial = saved.unwrap_or(range.current).clamp(range.min, range.max);
            if saved.is_some()
                && saved != Some(range.current)
                && !auto_desired(*control).is_some_and(|on| on)
            {
                apply_values.push((*control, initial));
            }
            desired_values.push((*control, *range, initial));
        }

        // Saved state only sticks when the hardware takes it: after a rejected
        // batch the panel builds from the device's live values instead, so the
        // UI never claims settings the camera doesn't actually have.
        let synced = if apply_autos.is_empty() && apply_values.is_empty() {
            true
        } else {
            openlogi_camera::apply_settings(&uid, &apply_autos, &apply_values)
                .map_err(|e| debug!(error = %e, "saved camera state reapply failed"))
                .is_ok()
        };

        for (toggle, on, st) in desired_autos {
            self.autos.push(AutoRow {
                toggle,
                on: if synced { on } else { st.current },
                default: st.default,
            });
        }
        for (control, range, initial) in desired_values {
            let shown = if synced { initial } else { range.current };
            let state = cx.new(|_| {
                SliderState::new()
                    .max(range.max as f32)
                    .min(range.min as f32)
                    .step(1.0)
                    .default_value(shown as f32)
            });

            let uid_for_event = uid.clone();
            let key_for_event = key.to_string();
            let sub = cx.subscribe(&state, move |panel, _slider, event: &SliderEvent, cx| {
                match event {
                    // Drag updates the label; the USB write lands once on release
                    // so we don't flood the camera with intermediate values.
                    SliderEvent::Change(_) => cx.notify(),
                    SliderEvent::Release(value) => {
                        let v = value.start().round() as i32;
                        panel.commit_release(control, &uid_for_event, &key_for_event, v, cx);
                    }
                }
            });

            self.sliders.push(ControlSlider {
                control,
                label: control_label(control),
                range,
                state,
                sub,
            });
        }
    }

    /// One slider release: write the value — taking the control over to manual
    /// first when its auto mode owns it (the camera rejects gated values, and
    /// grabbing the slider *is* the take-over gesture, as in G HUB) — then
    /// persist exactly what the device took.
    fn commit_release(
        &mut self,
        control: CameraControl,
        uid: &str,
        key: &str,
        v: i32,
        cx: &mut Context<Self>,
    ) {
        let takeover = control.auto_toggle().and_then(|toggle| {
            let ix = self.autos.iter().position(|a| a.toggle == toggle && a.on)?;
            Some((toggle, ix))
        });
        let written = match takeover {
            Some((toggle, _)) => {
                openlogi_camera::apply_settings(uid, &[(toggle, false)], &[(control, v)])
            }
            None => openlogi_camera::set_control(uid, control, v),
        };
        if let Err(e) = written {
            // The device didn't take it — persisting would lie.
            debug!(?control, value = v, error = %e, "camera control write failed");
            return;
        }
        if let Some((toggle, ix)) = takeover {
            self.autos[ix].on = false;
            cx.update_global::<AppState, _>(|state, _| {
                state.commit_camera_auto(key, toggle, false);
            });
        }
        cx.update_global::<AppState, _>(|state, _| {
            state.commit_camera_control(key, control, v);
        });
        self.sync_active_custom(cx);
        cx.notify();
    }

    /// The current auto state gating `control`, if the device has that toggle.
    fn auto_state_for(&self, control: CameraControl) -> Option<bool> {
        let toggle = control.auto_toggle()?;
        self.autos.iter().find(|a| a.toggle == toggle).map(|a| a.on)
    }

    /// Flip one auto mode. Turning auto off re-asserts the slider's value so
    /// the hardware ends where the UI shows, in the same device-open.
    fn toggle_auto(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        let Some(row) = self.autos.get(ix) else {
            return;
        };
        let toggle = row.toggle;
        let on = !row.on;
        let uid = uid_of(&key).to_string();
        let mut values = Vec::new();
        if !on
            && let Some(slider) = self
                .sliders
                .iter()
                .find(|s| s.control.auto_toggle() == Some(toggle))
        {
            values.push((
                slider.control,
                slider.state.read(cx).value().start().round() as i32,
            ));
        }
        if let Err(e) = openlogi_camera::apply_settings(&uid, &[(toggle, on)], &values) {
            // The device kept its mode — flipping the chip would lie.
            debug!(?toggle, on, error = %e, "camera auto write failed");
            return;
        }
        self.autos[ix].on = on;
        cx.update_global::<AppState, _>(|state, _| {
            state.commit_camera_auto(&key, toggle, on);
        });
        self.sync_active_custom(cx);
        cx.notify();
    }

    /// Reset every control and auto mode to the device defaults.
    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for ix in 0..self.sliders.len() {
            self.reset_control(ix, window, cx);
        }
    }

    /// Reset one control to its device default — auto mode back to the
    /// device's default state, the value re-seated and persisted.
    fn reset_control(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        let Some((control, default, state)) = self
            .sliders
            .get(ix)
            .map(|s| (s.control, s.range.default, s.state.clone()))
        else {
            return;
        };
        let uid = uid_of(&key).to_string();
        let mut autos = Vec::new();
        let auto_pos = control.auto_toggle().and_then(|toggle| {
            let pos = self.autos.iter().position(|a| a.toggle == toggle)?;
            autos.push((toggle, self.autos[pos].default));
            Some(pos)
        });
        if let Err(e) = openlogi_camera::apply_settings(&uid, &autos, &[(control, default)]) {
            // The device kept its state — leave the UI and config matching it.
            debug!(?control, value = default, error = %e, "camera control reset failed");
            return;
        }
        if let Some(pos) = auto_pos {
            let (toggle, auto_default) = autos[0];
            self.autos[pos].on = auto_default;
            cx.update_global::<AppState, _>(|state, _| {
                state.commit_camera_auto(&key, toggle, auto_default);
            });
        }
        state.update(cx, |slider, cx| {
            slider.set_value(default as f32, window, cx);
        });
        cx.update_global::<AppState, _>(|state, _| {
            state.commit_camera_control(&key, control, default);
        });
        self.sync_active_custom(cx);
        cx.notify();
    }

    /// Apply a built-in or saved profile: compute each control's target, push
    /// everything to the hardware in one batched open, re-seat the sliders,
    /// persist the values, and remember the selection.
    fn apply_profile(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        let uid = uid_of(&key).to_string();
        let custom = cx
            .try_global::<AppState>()
            .map(|s| s.camera_profiles(&key))
            .unwrap_or_default();

        // Auto targets: built-ins engage every auto mode except Default, which
        // restores the device's own default states; customs use their snapshot
        // (falling back to the current state for toggles they don't record).
        let mut autos: Vec<(AutoToggle, bool)> = Vec::new();
        let mut values: Vec<(CameraControl, i32)> = Vec::new();
        if let Some(builtin) = BUILTIN_PROFILES.iter().find(|p| p.id == id) {
            for row in &self.autos {
                autos.push((
                    row.toggle,
                    if builtin.id == "default" {
                        row.default
                    } else {
                        true
                    },
                ));
            }
            for slider in &self.sliders {
                let target = builtin
                    .values
                    .iter()
                    .find(|(c, _)| *c == slider.control)
                    .map_or(slider.range.default, |(_, pct)| {
                        let span = (slider.range.max - slider.range.min) as f32;
                        slider.range.min + (span * pct).round() as i32
                    });
                values.push((
                    slider.control,
                    target.clamp(slider.range.min, slider.range.max),
                ));
            }
        } else if let Some(snap) = custom.get(id) {
            for row in &self.autos {
                let on = snap.0.get(row.toggle.name()).map_or(row.on, |v| *v != 0);
                autos.push((row.toggle, on));
            }
            for slider in &self.sliders {
                if let Some(v) = snap.0.get(slider.control.name()) {
                    values.push((
                        slider.control,
                        (*v).clamp(slider.range.min, slider.range.max),
                    ));
                }
            }
        } else {
            return;
        }

        if let Err(e) = openlogi_camera::apply_settings(&uid, &autos, &values) {
            // Some writes may have landed; rebuild from the device's live state
            // rather than persisting a profile the hardware didn't fully take.
            debug!(profile = id, error = %e, "camera profile apply failed");
            self.key = None;
            cx.notify();
            return;
        }
        for (toggle, on) in &autos {
            if let Some(row) = self.autos.iter_mut().find(|a| a.toggle == *toggle) {
                row.on = *on;
            }
        }
        for (control, value) in &values {
            if let Some(slider) = self.sliders.iter().find(|s| s.control == *control) {
                slider.state.clone().update(cx, |s, cx| {
                    s.set_value(*value as f32, window, cx);
                });
            }
        }
        cx.update_global::<AppState, _>(|state, _| {
            for (toggle, on) in &autos {
                state.commit_camera_auto(&key, *toggle, *on);
            }
            for (control, value) in &values {
                state.commit_camera_control(&key, *control, *value);
            }
            state.set_camera_active_profile(&key, Some(id.to_string()));
        });
        cx.notify();
    }

    /// The current control values + auto states as a profile snapshot.
    fn snapshot(&self, cx: &Context<Self>) -> CameraControls {
        let mut snap = CameraControls::default();
        for slider in &self.sliders {
            snap.0.insert(
                slider.control.name().to_string(),
                slider.state.read(cx).value().start().round() as i32,
            );
        }
        for row in &self.autos {
            snap.0
                .insert(row.toggle.name().to_string(), i32::from(row.on));
        }
        snap
    }

    /// Keep the active *custom* profile tracking live edits: any slider or
    /// auto change writes back into its snapshot, so a profile is always what
    /// you last saw while it was selected. Built-ins are never edited.
    fn sync_active_custom(&self, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        let snap = self.snapshot(cx);
        cx.update_global::<AppState, _>(|state, _| {
            let Some(active) = state.camera_active_profile(&key) else {
                return;
            };
            if state.camera_profiles(&key).contains_key(&active) {
                state.save_camera_profile(&key, &active, snap);
            }
        });
    }

    /// Save the current control values + auto states as a new custom profile
    /// (auto-named `Custom N`) and mark it active.
    fn save_profile(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        let snap = self.snapshot(cx);
        cx.update_global::<AppState, _>(|state, _| {
            let existing = state.camera_profiles(&key);
            let mut n = existing.len() + 1;
            let mut name = format!("Custom {n}");
            while existing.contains_key(&name) {
                n += 1;
                name = format!("Custom {n}");
            }
            state.save_camera_profile(&key, &name, snap);
            state.set_camera_active_profile(&key, Some(name));
        });
        cx.notify();
    }

    /// Delete a saved custom profile. The hardware keeps whatever it's set to —
    /// only the snapshot (and, if it named this profile, the selection) goes.
    fn delete_profile(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        cx.update_global::<AppState, _>(|state, _| {
            state.delete_camera_profile(&key, name);
        });
        cx.notify();
    }
}

impl Render for CameraControlsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let Some(key) = Self::active_key(cx) else {
            self.key = None;
            self.sliders.clear();
            self.autos.clear();
            return div().into_any_element();
        };
        self.ensure_built(&key, cx);

        if self.sliders.is_empty() {
            return div()
                .text_sm()
                .text_color(pal.text_muted)
                .child(tr!("This camera exposes no adjustable image controls."))
                .into_any_element();
        }

        let lens: Vec<usize> = section_indices(&self.sliders, true);
        let image: Vec<usize> = section_indices(&self.sliders, false);

        let mut panel = v_flex().gap_2().w_full().child(profiles_row(&key, pal, cx));
        if !lens.is_empty() && !image.is_empty() {
            panel = panel.child(section_label(tr!("Lens"), pal));
        }
        for ix in lens {
            panel = panel.child(control_row(self, ix, cx, pal));
        }
        if !image.is_empty() && self.sliders.len() != image.len() {
            panel = panel.child(section_label(tr!("Image"), pal));
        }
        for ix in image {
            panel = panel.child(control_row(self, ix, cx, pal));
        }
        panel.child(reset_button(pal, cx)).into_any_element()
    }
}

/// Indices of the lens (camera-terminal) or image (processing-unit) sliders,
/// preserving [`CameraControl::ALL`] order.
fn section_indices(sliders: &[ControlSlider], lens: bool) -> Vec<usize> {
    sliders
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            matches!(
                s.control,
                CameraControl::Zoom | CameraControl::Focus | CameraControl::Exposure
            ) == lens
        })
        .map(|(ix, _)| ix)
        .collect()
}

/// The one-click profile chips: built-ins, saved customs, then Save.
fn profiles_row(key: &str, pal: Palette, cx: &mut Context<CameraControlsPanel>) -> AnyElement {
    let state = cx.try_global::<AppState>();
    let active = state.and_then(|s| s.camera_active_profile(key));
    let customs: Vec<String> = state
        .map(|s| s.camera_profiles(key).keys().cloned().collect())
        .unwrap_or_default();

    let mut row = h_flex().flex_wrap().gap_1p5().items_center();
    for (ix, builtin) in BUILTIN_PROFILES.iter().enumerate() {
        let id = builtin.id;
        row = row.child(profile_chip(
            ("camera-profile-builtin", ix),
            builtin_label(id),
            active.as_deref() == Some(id),
            pal,
            cx.listener(move |panel, _: &ClickEvent, window, cx| {
                panel.apply_profile(id, window, cx);
            }),
        ));
    }
    for (ix, name) in customs.into_iter().enumerate() {
        let is_active = active.as_deref() == Some(name.as_str());
        row = row.child(custom_profile_chip(ix, name, is_active, pal, cx));
    }
    row = row.child(
        div()
            .id("camera-profile-save")
            .px_2()
            .py_0p5()
            .rounded_full()
            .border_1()
            .border_color(pal.border)
            .text_xs()
            .text_color(pal.text_muted)
            .hover(|s| s.bg(pal.surface_hover))
            .child(format!("+ {}", tr!("New")))
            .on_click(cx.listener(|panel, _: &ClickEvent, _window, cx| {
                panel.save_profile(cx);
            })),
    );
    row.into_any_element()
}

fn profile_chip(
    id: (&'static str, usize),
    label: SharedString,
    active: bool,
    pal: Palette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let accent = rgb(ACCENT_BLUE);
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded_full()
        .border_1()
        .border_color(if active { accent.into() } else { pal.border })
        .text_xs()
        .text_color(if active {
            accent.into()
        } else {
            pal.text_muted
        })
        .when(active, |s| s.bg(pal.surface))
        .hover(move |s| s.bg(pal.surface_hover))
        .child(label)
        .on_click(on_click)
        .into_any_element()
}

/// A saved custom profile's chip: click applies it, the trailing `×` deletes
/// it (stopping propagation so a delete never also applies the profile).
fn custom_profile_chip(
    ix: usize,
    name: String,
    active: bool,
    pal: Palette,
    cx: &mut Context<CameraControlsPanel>,
) -> AnyElement {
    let accent = rgb(ACCENT_BLUE);
    let apply_name = name.clone();
    let delete_name = name.clone();
    h_flex()
        .id(("camera-profile-custom", ix))
        .pl_2()
        .pr_1()
        .py_0p5()
        .gap_1()
        .items_center()
        .rounded_full()
        .border_1()
        .border_color(if active { accent.into() } else { pal.border })
        .text_xs()
        .text_color(if active {
            accent.into()
        } else {
            pal.text_muted
        })
        .when(active, |s| s.bg(pal.surface))
        .hover(move |s| s.bg(pal.surface_hover))
        .child(SharedString::from(name))
        .on_click(cx.listener(move |panel, _: &ClickEvent, window, cx| {
            panel.apply_profile(&apply_name, window, cx);
        }))
        .child(
            div()
                .id(("camera-profile-del", ix))
                .px_0p5()
                .rounded_full()
                .text_color(pal.text_muted)
                .hover(|s| s.text_color(gpui::white()))
                .child("×")
                .on_click(cx.listener(move |panel, _: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    panel.delete_profile(&delete_name, cx);
                })),
        )
        .into_any_element()
}

fn section_label(text: SharedString, pal: Palette) -> AnyElement {
    div()
        .mt_1()
        .text_xs()
        .text_color(pal.text_muted)
        .child(text)
        .into_any_element()
}

/// One compact control line: label · slider · live value (· Auto chip when the
/// device pairs one). Double-click anywhere on the line resets that control.
fn control_row(
    panel: &CameraControlsPanel,
    ix: usize,
    cx: &Context<CameraControlsPanel>,
    pal: Palette,
) -> AnyElement {
    let slider = &panel.sliders[ix];
    let value = slider.state.read(cx).value().start().round() as i32;
    let auto_on = panel.auto_state_for(slider.control);
    let dimmed = auto_on == Some(true);

    let mut row = h_flex()
        .id(("camera-control-row", ix))
        .w_full()
        .gap_3()
        .items_center()
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
            div()
                .w(px(96.))
                .flex_shrink_0()
                .truncate()
                .text_sm()
                .text_color(pal.text_muted)
                .child(slider.label.clone()),
        )
        .child(
            div()
                .flex_1()
                // Dimmed while auto owns the value, but still draggable —
                // grabbing the slider takes the control over to manual.
                .when(dimmed, |s| s.opacity(0.55))
                .child(Slider::new(&slider.state).horizontal()),
        )
        .child(
            div()
                .w(px(36.))
                .flex_shrink_0()
                .text_right()
                .text_sm()
                .text_color(if dimmed {
                    pal.text_muted
                } else {
                    rgb(ACCENT_BLUE).into()
                })
                .child(format!("{value}")),
        );

    // Every row carries the trailing Auto column — empty for controls without
    // an auto mode — so the sliders and values align across the whole panel.
    let mut auto_cell = div().w(px(46.)).flex_shrink_0().flex().justify_end();
    if let Some(on) = auto_on
        && let Some(toggle) = slider.control.auto_toggle()
        && let Some(auto_ix) = panel.autos.iter().position(|a| a.toggle == toggle)
    {
        let accent = rgb(ACCENT_BLUE);
        auto_cell = auto_cell.child(
            div()
                .id(("camera-control-auto", ix))
                .px_1p5()
                .py_0p5()
                .rounded_full()
                .border_1()
                .border_color(if on { accent.into() } else { pal.border })
                .text_xs()
                .text_color(if on { accent.into() } else { pal.text_muted })
                .hover(|s| s.bg(pal.surface_hover))
                .child(tr!("Auto"))
                .on_click(cx.listener(move |panel, _: &ClickEvent, _window, cx| {
                    panel.toggle_auto(auto_ix, cx);
                })),
        );
    }
    row = row.child(auto_cell);

    row.into_any_element()
}

fn reset_button(pal: Palette, cx: &mut Context<CameraControlsPanel>) -> AnyElement {
    h_flex()
        .w_full()
        .justify_end()
        .child(
            div()
                .id("camera-controls-reset")
                .px_2p5()
                .py_0p5()
                .rounded_md()
                .border_1()
                .border_color(pal.border)
                .bg(pal.surface)
                .hover(|s| s.bg(pal.surface_hover))
                .text_xs()
                .text_color(pal.text_muted)
                .child(tr!("Reset to defaults"))
                .on_click(cx.listener(|panel, _: &ClickEvent, window, cx| {
                    panel.reset(window, cx);
                })),
        )
        .into_any_element()
}

fn builtin_label(id: &str) -> SharedString {
    match id {
        "streaming" => tr!("Streaming"),
        "video_call" => tr!("Video call"),
        _ => tr!("Default"),
    }
}

fn control_label(control: CameraControl) -> SharedString {
    match control {
        CameraControl::Zoom => tr!("Zoom"),
        CameraControl::Focus => tr!("Focus"),
        CameraControl::Exposure => tr!("Exposure"),
        CameraControl::Brightness => tr!("Brightness"),
        CameraControl::Contrast => tr!("Contrast"),
        CameraControl::Saturation => tr!("Saturation"),
        CameraControl::Sharpness => tr!("Sharpness"),
        CameraControl::WhiteBalance => tr!("White balance"),
        CameraControl::Tint => tr!("Tint"),
    }
}

/// Strip the `camera-` config-key prefix back to the AVFoundation unique id the
/// UVC layer matches on.
fn uid_of(key: &str) -> &str {
    key.strip_prefix("camera-").unwrap_or(key)
}
