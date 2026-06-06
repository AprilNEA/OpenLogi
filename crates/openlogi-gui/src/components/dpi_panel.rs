//! DPI slider for the right-side config column.
//!
//! The slider range comes from the selected device's HID++ AdjustableDpi
//! capability (`0x2201`). Capability discovery runs in the background and the
//! UI only exposes exact device-supported values once the list is known.

use gpui::{
    AnyElement, AppContext as _, BorrowAppContext as _, Context, Entity, InteractiveElement,
    IntoElement, ParentElement, Render, StatefulInteractiveElement as _, Styled, Subscription,
    Window, div, px, rgb,
};
use gpui_component::{
    Icon, IconName, h_flex,
    slider::{Slider, SliderEvent, SliderState},
    v_flex,
};
use openlogi_hid::{DeviceRoute, DpiCapabilities};
use tracing::debug;

use crate::state::{AppState, DpiStatus};
use crate::theme::{self, ACCENT_BLUE, Palette};
use openlogi_agent_core::hardware::{read_dpi_info_blocking, write_dpi_in_background};

/// Slider column width. Matches the right-column layout in `app.rs`.
const PANEL_W: f32 = 300.;

pub struct DpiPanel {
    slider_state: Option<Entity<SliderState>>,
    slider_sub: Option<Subscription>,
    slider_key: Option<String>,
    slider_shape: Option<SliderShape>,
    _state_obs: Subscription,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SliderShape {
    min: u16,
    max: u16,
    step: u16,
}

struct DpiPanelSnapshot {
    device_key: String,
    dpi: u32,
    presets: Vec<u32>,
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
            slider_shape: None,
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

        cx.update_global::<AppState, _>(|state, _| state.mark_dpi_loading(&key));
        // Run the blocking HID++ read on a dedicated OS thread (matching
        // `write_dpi_in_background`) instead of `cx.background_spawn`, so a slow
        // or asleep device can't pin a shared GPUI background-executor thread
        // for the whole `WRITE_BUDGET`. A oneshot hands the result back.
        let key_for_reset = key.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let result = read_dpi_info_blocking(&route);
            let _ = tx.send((key, route, result));
        });
        cx.spawn(async move |_panel, cx| {
            match rx.await {
                Ok((key, route, result)) => {
                    cx.update_global::<AppState, _>(|state, cx| {
                        state.store_dpi_info(key, &route, result);
                        cx.refresh_windows();
                    });
                }
                // The worker vanished without sending (e.g. it panicked). Reset
                // the `Loading` marker so the device isn't stuck on "Reading…".
                Err(_) => {
                    cx.update_global::<AppState, _>(|state, cx| {
                        state.clear_dpi_loading(&key_for_reset);
                        cx.refresh_windows();
                    });
                }
            }
        })
        .detach();
    }

    fn ensure_slider(
        &mut self,
        key: &str,
        capabilities: &DpiCapabilities,
        dpi: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shape = SliderShape {
            min: capabilities.min(),
            max: capabilities.max(),
            step: capabilities.step_hint(),
        };
        if self.slider_key.as_deref() == Some(key) && self.slider_shape == Some(shape) {
            if let Some(slider_state) = &self.slider_state {
                let target = capabilities.nearest(dpi);
                slider_state.update(cx, |state, cx| {
                    // Only re-seat the thumb when `dpi` resolves to a *different
                    // supported value* than the thumb currently rests on.
                    // Comparing in the device's supported space (not raw slider
                    // units) keeps a drag that lands between supported stops —
                    // possible because the slider step is uniform but the
                    // supported set may not be — from yanking the thumb back
                    // every frame.
                    let thumb = capabilities.nearest(slider_raw_to_dpi(state.value().start()));
                    if thumb != target {
                        state.set_value(dpi_to_f32(u32::from(target)), window, cx);
                    }
                });
            }
            return;
        }

        let snapped = capabilities.nearest(dpi);
        // Order matters: `SliderState` defaults to max=100, and `.min(N)`
        // clamps the value against the current max. Setting max first keeps
        // the intermediate state coherent for high-DPI devices.
        let slider_state = cx.new(|_| {
            SliderState::new()
                .max(dpi_to_f32(u32::from(shape.max)))
                .min(dpi_to_f32(u32::from(shape.min)))
                .step(dpi_to_f32(u32::from(shape.step)))
                .default_value(dpi_to_f32(u32::from(snapped)))
        });

        let slider_sub =
            cx.subscribe(
                &slider_state,
                |_panel, _slider, event: &SliderEvent, cx| match event {
                    // Continuous Change drives the in-process state so the numeric
                    // label tracks the drag. The HID write happens once on Release
                    // to keep us from spamming the device with intermediate values.
                    SliderEvent::Change(value) => {
                        let dpi = normalized_slider_dpi(value.start(), cx);
                        debug!(dpi, "slider change → AppState.dpi");
                        cx.update_global::<AppState, _>(|state, _| state.dpi = dpi);
                        cx.notify();
                    }
                    SliderEvent::Release(value) => {
                        let dpi = normalized_slider_dpi(value.start(), cx);
                        // Resolve the target from AppState at fire-time so
                        // carousel-driven device switches route the write to the
                        // now-current device, not whichever was active when this
                        // slider entity was constructed.
                        let target = cx
                            .try_global::<AppState>()
                            .and_then(|s| s.current_record().and_then(|r| r.route.clone()));
                        cx.update_global::<AppState, _>(|state, _| state.dpi = dpi);
                        write_dpi_in_background(None, target, dpi);
                    }
                },
            );

        self.slider_state = Some(slider_state);
        self.slider_sub = Some(slider_sub);
        self.slider_key = Some(key.to_string());
        self.slider_shape = Some(shape);
    }
}

impl Render for DpiPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Self::ensure_dpi_load(cx);

        let snapshot = dpi_panel_snapshot(cx);
        let pal = theme::palette(cx);

        if let DpiStatus::Ready(info) = &snapshot.status {
            self.ensure_slider(
                &snapshot.device_key,
                &info.capabilities,
                snapshot.dpi,
                window,
                cx,
            );
        } else {
            self.slider_state = None;
            self.slider_sub = None;
            self.slider_key = None;
            self.slider_shape = None;
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
            pal,
        );

        v_flex()
            .gap_3()
            .w(px(PANEL_W))
            .child(
                h_flex()
                    .justify_between()
                    .items_baseline()
                    .child(div().text_sm().text_color(pal.text_muted).child(tr!("DPI")))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(ACCENT_BLUE))
                            .child(format!("{}", snapshot.dpi)),
                    ),
            )
            .child(slider)
            .child(
                div()
                    .text_xs()
                    .text_color(pal.text_muted)
                    .child(range_label),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(pal.text_muted)
                            .child(tr!("PRESETS")),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .children(preset_chips)
                            .child(add_preset_chip(pal)),
                    ),
            )
    }
}

fn dpi_panel_snapshot(cx: &mut Context<DpiPanel>) -> DpiPanelSnapshot {
    cx.try_global::<AppState>()
        .and_then(|s| {
            let record = s.current_record()?;
            Some(DpiPanelSnapshot {
                device_key: record.config_key.clone(),
                dpi: s.dpi,
                presets: s.dpi_presets(),
                status: s.current_dpi_status(),
                reachable: record.route.is_some(),
            })
        })
        .unwrap_or_else(|| DpiPanelSnapshot {
            device_key: String::new(),
            dpi: crate::state::DEFAULT_DPI,
            presets: Vec::new(),
            status: DpiStatus::Unsupported("No active device".into()),
            reachable: false,
        })
}

fn dpi_range_label(status: &DpiStatus, reachable: bool) -> String {
    match status {
        DpiStatus::Ready(info) => format!(
            "{}–{} · step {}",
            info.capabilities.min(),
            info.capabilities.max(),
            info.capabilities.step_hint()
        ),
        DpiStatus::Unknown | DpiStatus::Loading if !reachable => {
            "Device offline — reconnect to read DPI range".to_string()
        }
        DpiStatus::Unknown | DpiStatus::Loading => "Loading device DPI range…".to_string(),
        DpiStatus::Failed(message) => format!("DPI read failed: {message}"),
        DpiStatus::Unsupported(message) => format!("DPI range unavailable: {message}"),
    }
}

fn slider_element(
    status: &DpiStatus,
    slider_state: Option<&Entity<SliderState>>,
    reachable: bool,
    pal: Palette,
) -> AnyElement {
    match (status, slider_state) {
        // A device with one supported DPI has nothing to drag — show the value.
        (DpiStatus::Ready(info), _) if info.capabilities.min() == info.capabilities.max() => {
            dpi_status_line(&format!("Fixed DPI: {}", info.capabilities.min()), pal)
        }
        (DpiStatus::Ready(_), Some(slider_state)) => {
            Slider::new(slider_state).horizontal().into_any_element()
        }
        (DpiStatus::Ready(_), None) => dpi_status_line("Preparing DPI slider…", pal),
        (DpiStatus::Unknown | DpiStatus::Loading, _) if !reachable => {
            dpi_status_line("Device offline — DPI unavailable.", pal)
        }
        (DpiStatus::Unknown | DpiStatus::Loading, _) => {
            dpi_status_line("Reading supported DPI values…", pal)
        }
        // Clickable: reselecting is a no-op for a single-device carousel, so the
        // retry must work in place.
        (DpiStatus::Failed(_), _) => dpi_retry_line("Couldn't read DPI — click to retry.", pal),
        (DpiStatus::Unsupported(_), _) => {
            dpi_status_line("This device did not report Adjustable DPI support.", pal)
        }
    }
}

fn dpi_status_line(message: &str, pal: Palette) -> AnyElement {
    div()
        .h(px(CHIP_H))
        .text_sm()
        .text_color(pal.text_muted)
        .child(message.to_string())
        .into_any_element()
}

/// A `Failed`-state line that re-arms DPI discovery for the active device on
/// click. Backs the only recovery path when the carousel holds one device.
fn dpi_retry_line(message: &str, pal: Palette) -> AnyElement {
    div()
        .id("dpi-retry")
        .h(px(CHIP_H))
        .text_sm()
        .text_color(rgb(ACCENT_BLUE))
        .hover(|s| s.text_color(pal.text_primary))
        .child(message.to_string())
        .on_click(|_event, _window, cx| {
            cx.update_global::<AppState, _>(|state, _| state.retry_active_dpi());
            cx.refresh_windows();
        })
        .into_any_element()
}

const CHIP_H: f32 = 28.;

/// One DPI preset rendered as a chip. Clicking the chip writes that DPI to
/// the device and updates `AppState.dpi`; the small × removes the preset.
fn preset_chip(idx: usize, value: u32, active: bool, presets: &[u32], pal: Palette) -> AnyElement {
    let presets_for_remove: Vec<u32> = presets.to_vec();
    h_flex()
        .id(("dpi-preset-chip", idx))
        .h(px(CHIP_H))
        .px_2()
        .gap_2()
        .items_center()
        .rounded_md()
        .border_1()
        .border_color(if active {
            rgb(ACCENT_BLUE).into()
        } else {
            pal.border
        })
        .bg(if active {
            pal.surface_hover
        } else {
            pal.surface
        })
        .hover(|s| s.bg(pal.surface_hover))
        .child(
            div()
                .id(("dpi-preset-apply", idx))
                .text_sm()
                .text_color(if active {
                    rgb(ACCENT_BLUE).into()
                } else {
                    pal.text_primary
                })
                .child(format!("{value}"))
                .on_click(move |_event, _window, cx| {
                    // Only apply once the supported DPI list is known, so the
                    // click writes a snapped, device-valid value — and can't be
                    // clobbered by a discovery result that lands afterwards.
                    let Some((target, dpi)) = cx.try_global::<AppState>().and_then(|s| {
                        let dpi = s.active_dpi_capabilities()?.snap(value);
                        Some((s.current_record().and_then(|r| r.route.clone()), dpi))
                    }) else {
                        return;
                    };
                    cx.update_global::<AppState, _>(|state, _| state.dpi = dpi);
                    write_dpi_in_background(None, target, dpi);
                    cx.refresh_windows();
                }),
        )
        .child(
            div()
                .id(("dpi-preset-remove", idx))
                .text_xs()
                .text_color(pal.text_muted)
                .child(Icon::new(IconName::Close).size_3())
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
fn add_preset_chip(pal: Palette) -> AnyElement {
    h_flex()
        .id("dpi-preset-add")
        .h(px(CHIP_H))
        .px_3()
        .items_center()
        .rounded_md()
        .border_1()
        .border_color(pal.border)
        .bg(pal.surface)
        .hover(|s| s.bg(pal.surface_hover))
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .text_sm()
                .text_color(pal.text_muted)
                .child(Icon::new(IconName::Plus).size_3())
                .child(tr!("Add")),
        )
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

fn dpi_load_target(cx: &mut Context<DpiPanel>) -> Option<(String, DeviceRoute)> {
    cx.try_global::<AppState>().and_then(|state| {
        if !state.current_dpi_unqueried() {
            return None;
        }
        let record = state.current_record()?;
        Some((record.config_key.clone(), record.route.clone()?))
    })
}

/// Round a raw slider position to a non-negative DPI count.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is rounded to a non-negative DPI before use"
)]
fn slider_raw_to_dpi(raw: f32) -> u32 {
    raw.max(0.).round() as u32
}

/// Snap a raw slider read to the selected device's supported DPI list.
fn normalized_slider_dpi(raw: f32, cx: &mut gpui::App) -> u32 {
    let rounded = slider_raw_to_dpi(raw);
    cx.try_global::<AppState>()
        .map_or(rounded, |state| state.normalize_active_dpi(rounded))
}

/// Widen a DPI count into f32 for slider math. DPI uses HID++'s u16 wire field,
/// so it fits comfortably in f32's mantissa with no precision loss.
#[allow(
    clippy::cast_precision_loss,
    reason = "DPI is limited by HID++'s u16 field — well below f32 mantissa precision"
)]
fn dpi_to_f32(dpi: u32) -> f32 {
    dpi as f32
}
