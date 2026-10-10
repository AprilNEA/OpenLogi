//! Device DPI controls.
//!
//! The slider range comes from the selected device's HID++ DPI capability
//! (`0x2201` AdjustableDpi or `0x2202` ExtendedAdjustableDpi, whichever it
//! reports). Capability discovery runs in the background and the UI only
//! exposes exact device-supported values once the list is known.

use gpui::{
    AnyElement, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription,
    WeakEntity, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    slider::{Slider, SliderState},
    v_flex,
};
use openlogi_core::hid::{Dpi, DpiCapabilities};
use tracing::debug;

use crate::state::{AppState, DeviceKey, DpiLoad, StateEvent};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::components::PresetChip;
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, Palette, Typography as _};

pub struct DpiPanel {
    /// Rebuilt whenever the selected device or its reported range changes,
    /// because a slider's range is fixed when it is built.
    slider: Option<DpiSlider>,
    /// A preset was clicked while the onboard profile owns the DPI: show why
    /// nothing happened until onboard profiles are turned off.
    preset_blocked: bool,
    _state_obs: Subscription,
}

/// The slider together with what it was built for.
struct DpiSlider {
    key: DeviceKey,
    shape: SliderShape,
    slider: CommitSlider<Dpi>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SliderShape {
    min: Dpi,
    max: Dpi,
    step: Dpi,
}

struct DpiPanelSnapshot {
    /// The active device, or `None` when nothing is selected.
    device_key: Option<DeviceKey>,
    dpi: Dpi,
    presets: Vec<Dpi>,
    status: DpiLoad,
    /// Whether the active device currently has a usable route. An offline
    /// device sits in `Unknown` forever (discovery can't start without a
    /// route), so the UI must say "offline" rather than "reading…".
    reachable: bool,
    /// The device's onboard profile controls DPI (`0x8100` in onboard mode),
    /// so a host write would be rejected.
    onboard_profile: bool,
}

impl DpiPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Repaint when the active device changes, DPI discovery completes, or
        // onboard profiles switch (they decide between slider and hint). The
        // slider entity is rebuilt in `render` whenever the selected device
        // or reported range changes, because SliderState's range is
        // builder-only.
        let state_obs = AppState::repaint_on(cx, |event| {
            matches!(
                event,
                StateEvent::DpiChanged(_) | StateEvent::OnboardProfilesChanged(_)
            )
        });

        Self {
            slider: None,
            preset_blocked: false,
            _state_obs: state_obs,
        }
    }

    fn ensure_slider(
        &mut self,
        key: &DeviceKey,
        capabilities: &DpiCapabilities,
        dpi: Dpi,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shape = SliderShape {
            min: capabilities.min(),
            max: capabilities.max(),
            step: capabilities.step_hint(),
        };
        if let Some(current) = self
            .slider
            .as_ref()
            .filter(|current| current.key == *key && current.shape == shape)
        {
            // Only re-seat the thumb when `dpi` resolves to a *different
            // supported value* than the thumb currently rests on. Comparing in
            // the device's supported space (not raw slider units) keeps a drag
            // that lands between supported stops — possible because the slider
            // step is uniform but the supported set may not be — from yanking
            // the thumb back every frame.
            current.slider.sync_snapped(
                capabilities.nearest(dpi),
                |thumb| capabilities.nearest(thumb),
                window,
                cx,
            );
            return;
        }

        let slider = CommitSlider::previewing(
            SliderRange::new(shape.min, shape.max).step(shape.step.into()),
            capabilities.nearest(dpi),
            cx,
            // Dragging drives the in-process state so the numeric label tracks
            // the thumb. The HID write happens once on release to keep us from
            // spamming the device with intermediate values.
            |_, dpi, cx| {
                let dpi =
                    AppState::try_read(cx).map_or(dpi, |state| state.normalize_active_dpi(dpi));
                debug!(%dpi, "slider change → AppState.dpi");
                AppState::apply(cx, |state| state.set_dpi_preview(dpi));
            },
            |_, dpi, cx| {
                let dpi =
                    AppState::try_read(cx).map_or(dpi, |state| state.normalize_active_dpi(dpi));
                // `commit_dpi` resolves the target at fire-time, so
                // gallery-driven device switches route the write to the
                // now-current device, not whichever was active when this
                // slider was built.
                AppState::apply(cx, |state| state.commit_dpi(dpi));
            },
        );
        self.slider = Some(DpiSlider {
            key: key.clone(),
            shape,
            slider,
        });
    }
}

impl DpiPanel {
    /// The slider, or the onboard-profile hint in its place while the
    /// device's own profile owns the DPI.
    fn slider_row(&self, snapshot: &DpiPanelSnapshot, pal: Palette) -> AnyElement {
        if snapshot.onboard_profile {
            return status_line(tr!("pointer.dpi_set_by_onboard_profile"), pal).into_any_element();
        }
        slider_element(
            &snapshot.status,
            self.slider.as_ref().map(|current| current.slider.slider()),
            snapshot.reachable,
            snapshot.device_key.clone(),
            pal,
        )
    }

    /// The preset chips. While the onboard profile owns the DPI a click shows
    /// why it cannot apply instead of changing the value.
    fn presets_section(
        &mut self,
        snapshot: &DpiPanelSnapshot,
        pal: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if !snapshot.onboard_profile {
            self.preset_blocked = false;
        }
        let panel = cx.entity().downgrade();
        // Highlight at most one chip: when several presets snap to the same
        // supported value as the current DPI, only the first is "active".
        let mut already_highlighted = false;
        let preset_chips: Vec<_> = snapshot
            .presets
            .iter()
            .enumerate()
            .map(|(idx, value)| {
                let normalized = AppState::try_read(cx)
                    .map_or(*value, |state| state.normalize_active_dpi(*value));
                let active = !already_highlighted && normalized == snapshot.dpi;
                already_highlighted |= active;
                preset_chip(
                    idx,
                    *value,
                    active,
                    &snapshot.presets,
                    snapshot.onboard_profile.then(|| panel.clone()),
                )
            })
            .collect();

        v_flex()
            .gap_2()
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("common.presets")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .children(preset_chips)
                    .child(add_preset_chip()),
            )
            .when(self.preset_blocked, |column| {
                column.child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(tr!("pointer.presets_blocked_by_onboard_profile")),
                )
            })
    }
}

impl Render for DpiPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = dpi_panel_snapshot(cx);
        let pal = theme::palette(cx);

        if let (DpiLoad::Ready(info), Some(key)) = (&snapshot.status, &snapshot.device_key) {
            self.ensure_slider(key, &info.capabilities, snapshot.dpi, window, cx);
        } else {
            self.slider = None;
        }

        let range_label = dpi_range_label(&snapshot.status, snapshot.reachable);
        let slider = self.slider_row(&snapshot, pal);
        let presets = self.presets_section(&snapshot, pal, cx);

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
                            .child(tr!("pointer.dpi")),
                    )
                    .child(
                        div()
                            .text_body()
                            .text_color(pal.text_primary)
                            .child(format!("{}", snapshot.dpi)),
                    ),
            )
            .child(slider)
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(range_label),
            )
            .child(presets)
    }
}

fn dpi_panel_snapshot(cx: &mut Context<DpiPanel>) -> DpiPanelSnapshot {
    AppState::try_read(cx)
        .and_then(|s| {
            let record = s.current_record()?;
            let device_key = record.device_key();
            Some(DpiPanelSnapshot {
                status: s.dpi_load_for(&device_key),
                device_key: Some(device_key),
                dpi: s.dpi(),
                presets: s.dpi_presets(),
                reachable: record.route.is_some(),
                onboard_profile: s.current_onboard_profiles_supported()
                    && s.current_onboard_profiles_shown(),
            })
        })
        .unwrap_or_else(|| DpiPanelSnapshot {
            device_key: None,
            dpi: crate::state::DEFAULT_DPI,
            presets: Vec::new(),
            status: DpiLoad::Unsupported(tr!("device.no_active_device").to_string()),
            reachable: false,
            onboard_profile: false,
        })
}

fn dpi_range_label(status: &DpiLoad, reachable: bool) -> AnyElement {
    match status {
        DpiLoad::Ready(info) => h_flex()
            .gap_1()
            .child(format!(
                "{}–{}",
                info.capabilities.min(),
                info.capabilities.max()
            ))
            .child(Icon::empty().path("action-icons/dot.svg").size_3())
            .child(tr!("pointer.dpi_step", step => info.capabilities.step_hint()))
            .into_any_element(),
        DpiLoad::Unknown | DpiLoad::Loading if !reachable => {
            tr!("pointer.dpi_range_device_offline").into_any_element()
        }
        DpiLoad::Unknown | DpiLoad::Loading => {
            tr!("pointer.loading_device_dpi_range").into_any_element()
        }
        DpiLoad::Failed(message) => {
            tr!("pointer.dpi_read_failed", message => message).into_any_element()
        }
        DpiLoad::Unsupported(message) => {
            tr!("pointer.dpi_range_unavailable", message => message).into_any_element()
        }
    }
}

fn slider_element(
    status: &DpiLoad,
    slider_state: Option<&Entity<SliderState>>,
    reachable: bool,
    key: Option<DeviceKey>,
    pal: Palette,
) -> AnyElement {
    match (status, slider_state) {
        // A device with one supported DPI has nothing to drag — show the value.
        (DpiLoad::Ready(info), _) if info.capabilities.min() == info.capabilities.max() => {
            status_line(
                tr!("pointer.fixed_dpi_value", dpi => info.capabilities.min()),
                pal,
            )
            .into_any_element()
        }
        (DpiLoad::Ready(_), Some(slider_state)) => {
            Slider::new(slider_state).horizontal().into_any_element()
        }
        (DpiLoad::Ready(_), None) => {
            status_line(tr!("pointer.preparing_dpi_slider"), pal).into_any_element()
        }
        (DpiLoad::Unknown | DpiLoad::Loading, _) if !reachable => {
            status_line(tr!("pointer.device_offline_dpi_is_unavailable"), pal).into_any_element()
        }
        (DpiLoad::Unknown | DpiLoad::Loading, _) => {
            status_line(tr!("pointer.reading_supported_dpi_values"), pal).into_any_element()
        }
        // Clickable: reselecting is a no-op for a single-device gallery, so the
        // retry must work in place.
        (DpiLoad::Failed(_), _) => retry_line(
            "dpi-retry",
            tr!("pointer.couldnt_read_dpi_click_to_retry"),
            pal,
            move |cx| {
                if let Some(key) = &key {
                    AppState::apply(cx, |state| state.retry_dpi_read(key));
                }
            },
        )
        .into_any_element(),
        (DpiLoad::Unsupported(_), _) => {
            status_line(tr!("pointer.adjustable_dpi_unsupported"), pal).into_any_element()
        }
    }
}

const CHIP_H: f32 = 28.;

/// One DPI preset rendered as a chip. Clicking the chip writes that DPI to
/// the device and updates `AppState.dpi`; the small × removes the preset.
/// While the onboard profile owns the DPI, `blocked_by` is the panel told
/// instead, so it can say why nothing changed.
fn preset_chip(
    idx: usize,
    value: Dpi,
    active: bool,
    presets: &[Dpi],
    blocked_by: Option<WeakEntity<DpiPanel>>,
) -> impl IntoElement {
    let presets_for_remove: Vec<Dpi> = presets.to_vec();
    PresetChip::new(("dpi-preset-chip", idx))
        .selected(active)
        .child(
            Button::new(("dpi-preset-apply", idx))
                .compact()
                .ghost()
                .h_full()
                .flex()
                .items_center()
                .label(format!("{value}"))
                .selected(active)
                .on_click(move |_event, _window, cx| {
                    if let Some(panel) = &blocked_by {
                        let _ = panel.update(cx, |panel, cx| {
                            panel.preset_blocked = true;
                            cx.notify();
                        });
                        return;
                    }
                    // Only apply once the supported DPI list is known, so the
                    // click writes a snapped, device-valid value — and can't be
                    // clobbered by a discovery result that lands afterwards.
                    let Some(dpi) = AppState::try_read(cx)
                        .and_then(|s| Some(s.active_dpi_capabilities()?.nearest(value)))
                    else {
                        return;
                    };
                    AppState::apply(cx, |state| state.commit_dpi(dpi));
                }),
        )
        .child(
            Button::new(("dpi-preset-remove", idx))
                .xsmall()
                .ghost()
                .icon(IconName::Close)
                .on_click(move |_event, _window, cx| {
                    let mut next = presets_for_remove.clone();
                    if idx < next.len() {
                        next.remove(idx);
                    }
                    AppState::apply(cx, |state| state.commit_dpi_presets(next));
                }),
        )
}

/// "+" chip that snapshots `AppState.dpi` as a new preset.
fn add_preset_chip() -> impl IntoElement {
    Button::new("dpi-preset-add")
        .compact()
        .outline()
        .h(px(CHIP_H))
        .icon(IconName::Plus)
        .label(tr!("common.add"))
        .on_click(|_event, _window, cx| {
            // Append the current DPI to the active device's preset list.
            // Duplicates are allowed — the user might want the same value
            // appearing at multiple cycle positions for muscle-memory reasons.
            AppState::apply(cx, |state| {
                let mut presets = state.dpi_presets();
                presets.push(state.dpi());
                state.commit_dpi_presets(presets)
            });
        })
}
