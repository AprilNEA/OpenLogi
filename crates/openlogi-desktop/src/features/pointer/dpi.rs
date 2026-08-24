//! Device DPI controls.
//!
//! The slider range comes from the selected device's HID++ DPI capability
//! (`0x2201` AdjustableDpi or `0x2202` ExtendedAdjustableDpi, whichever it
//! reports). Capability discovery runs in the background and the UI only
//! exposes exact device-supported values once the list is known.

use gpui::{
    AnyElement, AppContext as _, BorrowAppContext as _, Context, Entity, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window, div, px,
};
use gpui_component::{
    IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    slider::{Slider, SliderEvent, SliderState},
    v_flex,
};
use openlogi_core::hid::{DeviceRoute, Dpi, DpiCapabilities};
use tracing::debug;

use crate::state::{AppState, DeviceKey, DpiStatus};
use crate::ui::device_read::issue_device_read;
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, Palette, SelectableStyle, Typography as _};

pub struct DpiPanel {
    slider_state: Option<Entity<SliderState>>,
    slider_sub: Option<Subscription>,
    slider_key: Option<String>,
    slider_capabilities: Option<DpiCapabilities>,
    _state_obs: Subscription,
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
        // Repaint when the carousel switches devices or DPI discovery
        // completes. The slider entity is rebuilt in `render` whenever the
        // selected device or reported range changes, because SliderState's
        // range is builder-only.
        let state_obs = cx.observe_global::<AppState>(|_panel, cx| cx.notify());

        Self {
            slider_state: None,
            slider_sub: None,
            slider_key: None,
            slider_capabilities: None,
            _state_obs: state_obs,
        }
    }

    /// Kick off a one-shot DPI capability read for the active device when it
    /// hasn't been queried yet.
    ///
    /// This is the *only* place discovery is triggered, and it runs from
    /// `render`, so a device's capabilities — and therefore the normalization
    /// applied to the hook's DPI-cycle presets — only populate once this panel
    /// has been rendered for that device. A user who only ever cycles DPI via
    /// the hook (window never opened) keeps the raw, un-normalized presets,
    /// which are still valid DPI values. This lazy coupling is intentional:
    /// `AppState` is a global without its own GPUI context to spawn from.
    fn ensure_dpi_load(cx: &mut Context<Self>) {
        let Some((key, route)) = dpi_load_target(cx) else {
            return;
        };

        cx.update_global::<AppState, _>(|state, _| state.reads.dpi.mark_loading(&key));
        // The agent owns device I/O: request the DPI read over IPC and store the
        // typed reply off the render thread. The typed `WriteError` reaches
        // `store_dpi_info` intact, so a permanent `FeatureUnsupported` /
        // `EmptyDpiList` stops the panel re-probing on every reselect.
        issue_device_read(
            cx,
            key,
            route,
            crate::services::ipc::Command::ReadDpi,
            AppState::store_dpi_info,
            |state, key| state.reads.dpi.clear_loading(key),
        );
    }

    fn ensure_slider(
        &mut self,
        key: &str,
        capabilities: &DpiCapabilities,
        dpi: Dpi,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.slider_key.as_deref() == Some(key)
            && self.slider_capabilities.as_ref() == Some(capabilities)
        {
            if let Some(slider_state) = &self.slider_state {
                let target = capabilities.nearest(dpi);
                slider_state.update(cx, |state, cx| {
                    // The slider's units are indices into the supported-value
                    // list, not raw DPI. This gives every selectable value equal
                    // room even when the device's range becomes much coarser at
                    // high DPI.
                    let thumb = dpi_for_slider_position(capabilities, state.value().start());
                    if thumb != target {
                        state.set_value(dpi_slider_position(capabilities, target), window, cx);
                    }
                });
            }
            return;
        }

        let snapped = capabilities.nearest(dpi);
        let slider_capabilities = capabilities.clone();
        let slider_state = cx.new(|_| {
            SliderState::new()
                .max(dpi_slider_max(capabilities))
                .min(0.)
                .step(1.)
                .default_value(dpi_slider_position(capabilities, snapped))
        });

        let slider_sub = cx.subscribe(
            &slider_state,
            move |_panel, _slider, event: &SliderEvent, cx| match event {
                // Continuous Change drives the in-process state so the numeric
                // label tracks the drag. The HID write happens once on Release
                // to keep us from spamming the device with intermediate values.
                SliderEvent::Change(value) => {
                    let dpi = dpi_for_slider_position(&slider_capabilities, value.start());
                    let dpi = cx
                        .try_global::<AppState>()
                        .map_or(dpi, |state| state.normalize_active_dpi(dpi));
                    debug!(%dpi, "slider change → AppState.dpi");
                    cx.update_global::<AppState, _>(|state, _| state.dpi = dpi);
                    cx.notify();
                }
                SliderEvent::Release(value) => {
                    let dpi = dpi_for_slider_position(&slider_capabilities, value.start());
                    let dpi = cx
                        .try_global::<AppState>()
                        .map_or(dpi, |state| state.normalize_active_dpi(dpi));
                    // `commit_dpi` resolves the target at fire-time, so
                    // carousel-driven device switches route the write to the
                    // now-current device, not whichever was active when this
                    // slider entity was constructed.
                    cx.update_global::<AppState, _>(|state, _| state.commit_dpi(dpi));
                }
            },
        );

        self.slider_state = Some(slider_state);
        self.slider_sub = Some(slider_sub);
        self.slider_key = Some(key.to_string());
        self.slider_capabilities = Some(capabilities.clone());
    }
}

fn dpi_slider_max(capabilities: &DpiCapabilities) -> f32 {
    usize_to_slider_position(capabilities.values().len() - 1)
}

fn dpi_slider_position(capabilities: &DpiCapabilities, dpi: Dpi) -> f32 {
    let target = capabilities.nearest(dpi);
    let index = capabilities
        .values()
        .partition_point(|candidate| *candidate < target);
    usize_to_slider_position(index)
}

fn usize_to_slider_position(index: usize) -> f32 {
    match u16::try_from(index) {
        Ok(index) => f32::from(index),
        // A deduplicated list of u16 DPI values has at most 65,536 entries,
        // so its greatest valid index always fits u16.
        Err(_) => f32::from(u16::MAX),
    }
}

fn dpi_for_slider_position(capabilities: &DpiCapabilities, position: f32) -> Dpi {
    let index = usize::from(Dpi::from_rounded(position).into_inner());
    let index = index.min(capabilities.values().len() - 1);
    capabilities.values()[index]
}

impl Render for DpiPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Self::ensure_dpi_load(cx);

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
            self.slider_state = None;
            self.slider_sub = None;
            self.slider_key = None;
            self.slider_capabilities = None;
        }

        // Highlight at most one chip: when several presets snap to the same
        // supported value as the current DPI, only the first is "active".
        let mut already_highlighted = false;
        let preset_chips: Vec<AnyElement> = snapshot
            .presets
            .iter()
            .enumerate()
            .map(|(idx, value)| {
                let normalized = cx
                    .try_global::<AppState>()
                    .map_or(*value, |state| state.normalize_active_dpi(*value));
                let active = !already_highlighted && normalized == snapshot.dpi;
                already_highlighted |= active;
                preset_chip(idx, *value, active, &snapshot.presets, pal)
            })
            .collect();

        let range_label = dpi_range_label(&snapshot.status, snapshot.reachable);
        let slider = slider_element(
            &snapshot.status,
            self.slider_state.as_ref(),
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
                            .child(tr!("DPI")),
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
                            .child(tr!("Presets")),
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
    cx.try_global::<AppState>()
        .and_then(|s| {
            let record = s.current_record()?;
            let device_key = record.device_key();
            Some(DpiPanelSnapshot {
                status: s.reads.dpi.status(&device_key),
                device_key,
                dpi: s.dpi,
                presets: s.dpi_presets(),
                reachable: record.route.is_some(),
            })
        })
        .unwrap_or_else(|| DpiPanelSnapshot {
            device_key: DeviceKey::default(),
            dpi: crate::state::DEFAULT_DPI,
            presets: Vec::new(),
            status: DpiStatus::Unsupported(tr!("No active device").to_string()),
            reachable: false,
        })
}

fn dpi_range_label(status: &DpiStatus, reachable: bool) -> SharedString {
    match status {
        // The numeric range is digits and symbols only — nothing to translate.
        DpiStatus::Ready(info) => format!(
            "{}–{} · step {}",
            info.capabilities.min(),
            info.capabilities.max(),
            info.capabilities.step_hint()
        )
        .into(),
        DpiStatus::Unknown | DpiStatus::Loading if !reachable => {
            tr!("Device offline — reconnect to read DPI range")
        }
        DpiStatus::Unknown | DpiStatus::Loading => tr!("Loading device DPI range…"),
        DpiStatus::Failed(message) => tr!("DPI read failed: %{message}", message => message),
        DpiStatus::Unsupported(message) => {
            tr!("DPI range unavailable: %{message}", message => message)
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
                tr!("Fixed DPI: %{dpi}", dpi => info.capabilities.min()),
                pal,
            )
        }
        (DpiStatus::Ready(_), Some(slider_state)) => {
            Slider::new(slider_state).horizontal().into_any_element()
        }
        (DpiStatus::Ready(_), None) => status_line(tr!("Preparing DPI slider…"), pal),
        (DpiStatus::Unknown | DpiStatus::Loading, _) if !reachable => {
            status_line(tr!("Device offline — DPI unavailable."), pal)
        }
        (DpiStatus::Unknown | DpiStatus::Loading, _) => {
            status_line(tr!("Reading supported DPI values…"), pal)
        }
        // Clickable: reselecting is a no-op for a single-device carousel, so the
        // retry must work in place.
        (DpiStatus::Failed(_), _) => retry_line(
            "dpi-retry",
            tr!("Couldn't read DPI — click to retry."),
            pal,
            move |cx| {
                cx.update_global::<AppState, _>(|state, _| state.reads.dpi.retry(&key));
                cx.refresh_windows();
            },
        ),
        (DpiStatus::Unsupported(_), _) => status_line(
            tr!("This device did not report Adjustable DPI support."),
            pal,
        ),
    }
}

const CHIP_H: f32 = 28.;

/// One DPI preset rendered as a chip. Clicking the chip writes that DPI to
/// the device and updates `AppState.dpi`; the small × removes the preset.
fn preset_chip(idx: usize, value: Dpi, active: bool, presets: &[Dpi], pal: Palette) -> AnyElement {
    let presets_for_remove: Vec<Dpi> = presets.to_vec();
    h_flex()
        .id(("dpi-preset-chip", idx))
        .h(px(CHIP_H))
        .px_2()
        .gap_2()
        .items_center()
        .rounded(pal.control_radius)
        .selected_border(active, pal)
        .bg(pal.surface)
        .selected_fill(active)
        .hover(|s| s.bg(pal.surface_hover))
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
                    let Some(dpi) = cx
                        .try_global::<AppState>()
                        .and_then(|s| Some(s.active_dpi_capabilities()?.nearest(value)))
                    else {
                        return;
                    };
                    cx.update_global::<AppState, _>(|state, _| state.commit_dpi(dpi));
                    cx.refresh_windows();
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
                    cx.update_global::<AppState, _>(|state, _| state.commit_dpi_presets(next));
                    cx.refresh_windows();
                }),
        )
        .into_any_element()
}

/// "+" chip that snapshots `AppState.dpi` as a new preset.
fn add_preset_chip() -> AnyElement {
    Button::new("dpi-preset-add")
        .compact()
        .outline()
        .h(px(CHIP_H))
        .icon(IconName::Plus)
        .label(tr!("Add"))
        .on_click(|_event, _window, cx| {
            // Append the current DPI to the active device's preset list.
            // Duplicates are allowed — the user might want the same value
            // appearing at multiple cycle positions for muscle-memory reasons.
            cx.update_global::<AppState, _>(|state, _| {
                let mut presets = state.dpi_presets();
                presets.push(state.dpi);
                state.commit_dpi_presets(presets);
            });
            cx.refresh_windows();
        })
        .into_any_element()
}

fn dpi_load_target(cx: &mut Context<DpiPanel>) -> Option<(DeviceKey, DeviceRoute)> {
    cx.try_global::<AppState>().and_then(|state| {
        let record = state.current_record()?;
        let key = record.device_key();
        if !state.reads.dpi.unqueried(&key) {
            return None;
        }
        Some((key, record.route.clone()?))
    })
}

#[cfg(test)]
mod tests {
    use openlogi_core::hid::{Dpi, DpiCapabilities};

    use super::{dpi_for_slider_position, dpi_slider_max, dpi_slider_position};

    fn uneven_capabilities() -> DpiCapabilities {
        DpiCapabilities::new(vec![100, 200, 400, 800, 1_600, 3_200, 8_000, 44_000])
            .expect("test capabilities are non-empty")
    }

    fn assert_position(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < f32::EPSILON);
    }

    #[test]
    fn slider_uses_supported_value_indices_instead_of_raw_dpi() {
        let capabilities = uneven_capabilities();

        assert_position(dpi_slider_max(&capabilities), 7.);
        assert_position(dpi_slider_position(&capabilities, Dpi::new(100)), 0.);
        assert_position(dpi_slider_position(&capabilities, Dpi::new(800)), 3.);
        assert_position(dpi_slider_position(&capabilities, Dpi::new(44_000)), 7.);
    }

    #[test]
    fn every_slider_stop_maps_back_to_its_supported_dpi() {
        let capabilities = uneven_capabilities();

        for &dpi in capabilities.values() {
            let position = dpi_slider_position(&capabilities, dpi);
            assert_eq!(dpi_for_slider_position(&capabilities, position), dpi);
        }
    }

    #[test]
    fn slider_position_rounds_and_clamps_to_supported_values() {
        let capabilities = uneven_capabilities();

        assert_eq!(dpi_for_slider_position(&capabilities, 2.6), Dpi::new(800));
        assert_eq!(
            dpi_for_slider_position(&capabilities, f32::MAX),
            Dpi::new(44_000)
        );
    }
}
