//! Device DPI controls.
//!
//! The slider range comes from the selected device's HID++ DPI capability
//! (`0x2201` AdjustableDpi or `0x2202` ExtendedAdjustableDpi, whichever it
//! reports). Capability discovery runs in the background and the UI only
//! exposes exact device-supported values once the list is known.

use gpui::{
    AnyElement, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Window,
    div, px,
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

use crate::state::{AppState, DeviceKey, DpiStatus, StateEvent};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::components::PresetChip;
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, Palette, Typography as _};

pub struct DpiPanel {
    /// Rebuilt whenever the selected device or its reported range changes,
    /// because a slider's range is fixed when it is built.
    slider: Option<DpiSlider>,
    _state_obs: Subscription,
}

/// The slider together with what it was built for.
struct DpiSlider {
    key: String,
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
    device_key: DeviceKey,
    dpi: Dpi,
    presets: Vec<Dpi>,
    status: DpiStatus,
    /// Whether the active device currently has a usable route. An offline
    /// device sits in `Unknown` forever (discovery can't start without a
    /// route), so the UI must say "offline" rather than "reading…".
    reachable: bool,
}

impl DpiPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Repaint when the active device changes or DPI discovery
        // completes. The slider entity is rebuilt in `render` whenever the
        // selected device or reported range changes, because SliderState's
        // range is builder-only.
        let state_obs =
            AppState::repaint_on(cx, |event| matches!(event, StateEvent::DpiChanged(_)));

        Self {
            slider: None,
            _state_obs: state_obs,
        }
    }

    fn ensure_slider(
        &mut self,
        key: &str,
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
            .filter(|current| current.key == key && current.shape == shape)
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
            key: key.to_string(),
            shape,
            slider,
        });
    }
}

impl Render for DpiPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = dpi_panel_snapshot(cx);
        let pal = theme::palette(cx);

        if let DpiStatus::Ready(info) = &snapshot.status {
            self.ensure_slider(
                snapshot.device_key.as_str(),
                &info.capabilities,
                snapshot.dpi,
                window,
                cx,
            );
        } else {
            self.slider = None;
        }

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
                preset_chip(idx, *value, active, &snapshot.presets)
            })
            .collect();

        let range_label = dpi_range_label(&snapshot.status, snapshot.reachable);
        let slider = slider_element(
            &snapshot.status,
            self.slider.as_ref().map(|current| current.slider.slider()),
            snapshot.reachable,
            snapshot.device_key.clone(),
            pal,
        );

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
            .child(
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
                    ),
            )
    }
}

fn dpi_panel_snapshot(cx: &mut Context<DpiPanel>) -> DpiPanelSnapshot {
    AppState::try_read(cx)
        .and_then(|s| {
            let record = s.current_record()?;
            let device_key = record.device_key();
            Some(DpiPanelSnapshot {
                status: s.dpi_status_for(&device_key),
                device_key,
                dpi: s.dpi(),
                presets: s.dpi_presets(),
                reachable: record.route.is_some(),
            })
        })
        .unwrap_or_else(|| DpiPanelSnapshot {
            device_key: DeviceKey::default(),
            dpi: crate::state::DEFAULT_DPI,
            presets: Vec::new(),
            status: DpiStatus::Unsupported(tr!("device.no_active_device").to_string()),
            reachable: false,
        })
}

fn dpi_range_label(status: &DpiStatus, reachable: bool) -> AnyElement {
    match status {
        DpiStatus::Ready(info) => h_flex()
            .gap_1()
            .child(format!(
                "{}–{}",
                info.capabilities.min(),
                info.capabilities.max()
            ))
            .child(Icon::empty().path("action-icons/dot.svg").size_3())
            .child(tr!("pointer.dpi_step", step => info.capabilities.step_hint()))
            .into_any_element(),
        DpiStatus::Unknown | DpiStatus::Loading if !reachable => {
            tr!("pointer.dpi_range_device_offline").into_any_element()
        }
        DpiStatus::Unknown | DpiStatus::Loading => {
            tr!("pointer.loading_device_dpi_range").into_any_element()
        }
        DpiStatus::Failed(message) => {
            tr!("pointer.dpi_read_failed", message => message).into_any_element()
        }
        DpiStatus::Unsupported(message) => {
            tr!("pointer.dpi_range_unavailable", message => message).into_any_element()
        }
    }
}

fn slider_element(
    status: &DpiStatus,
    slider_state: Option<&Entity<SliderState>>,
    reachable: bool,
    key: DeviceKey,
    pal: Palette,
) -> AnyElement {
    match (status, slider_state) {
        // A device with one supported DPI has nothing to drag — show the value.
        (DpiStatus::Ready(info), _) if info.capabilities.min() == info.capabilities.max() => {
            status_line(
                tr!("pointer.fixed_dpi_value", dpi => info.capabilities.min()),
                pal,
            )
            .into_any_element()
        }
        (DpiStatus::Ready(_), Some(slider_state)) => {
            Slider::new(slider_state).horizontal().into_any_element()
        }
        (DpiStatus::Ready(_), None) => {
            status_line(tr!("pointer.preparing_dpi_slider"), pal).into_any_element()
        }
        (DpiStatus::Unknown | DpiStatus::Loading, _) if !reachable => {
            status_line(tr!("pointer.device_offline_dpi_is_unavailable"), pal).into_any_element()
        }
        (DpiStatus::Unknown | DpiStatus::Loading, _) => {
            status_line(tr!("pointer.reading_supported_dpi_values"), pal).into_any_element()
        }
        // Clickable: reselecting is a no-op for a single-device gallery, so the
        // retry must work in place.
        (DpiStatus::Failed(_), _) => retry_line(
            "dpi-retry",
            tr!("pointer.couldnt_read_dpi_click_to_retry"),
            pal,
            move |cx| AppState::apply(cx, |state| state.retry_dpi_read(&key)),
        )
        .into_any_element(),
        (DpiStatus::Unsupported(_), _) => {
            status_line(tr!("pointer.adjustable_dpi_unsupported"), pal).into_any_element()
        }
    }
}

const CHIP_H: f32 = 28.;

/// One DPI preset rendered as a chip. Clicking the chip writes that DPI to
/// the device and updates `AppState.dpi`; the small × removes the preset.
fn preset_chip(idx: usize, value: Dpi, active: bool, presets: &[Dpi]) -> impl IntoElement {
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
