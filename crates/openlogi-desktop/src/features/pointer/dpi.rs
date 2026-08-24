//! Device DPI controls.
//!
//! The slider range comes from the selected device's HID++ DPI capability
//! (`0x2201` AdjustableDpi or `0x2202` ExtendedAdjustableDpi, whichever it
//! reports). Capability discovery runs in the background and the UI only
//! exposes exact device-supported values once the list is known.

use gpui::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, Styled, Subscription, Window, div, px,
};
use gpui_component::{
    IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    slider::{Slider, SliderEvent, SliderState},
    v_flex,
};
use openlogi_core::hid::{Dpi, DpiCapabilities};
use tracing::debug;

use crate::state::{AppState, DeviceKey, DeviceRecord, DpiStatus, StateEvent};
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, Palette, SelectableStyle, Typography as _};

pub struct DpiPanel {
    slider_state: Option<Entity<SliderState>>,
    slider_sub: Option<Subscription>,
    slider_key: Option<String>,
    slider_shape: Option<SliderShape>,
    _state_obs: Subscription,
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
        // Repaint when the carousel switches devices or DPI discovery
        // completes. The slider entity is rebuilt in `render` whenever the
        // selected device or reported range changes, because SliderState's
        // range is builder-only.
        let state_obs = cx.subscribe(
            &AppState::global(cx),
            |_panel, _, event: &StateEvent, cx| {
                let relevant = match event {
                    StateEvent::InventoryChanged | StateEvent::DeviceSelected(_) => true,
                    StateEvent::DpiChanged(key) => AppState::try_read(cx)
                        .and_then(AppState::current_record)
                        .is_some_and(|record| record.device_key() == *key),
                    _ => false,
                };
                if relevant {
                    cx.notify();
                }
            },
        );

        Self {
            slider_state: None,
            slider_sub: None,
            slider_key: None,
            slider_shape: None,
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
                    let thumb = capabilities.nearest(Dpi::from_rounded(state.value().start()));
                    if thumb != target {
                        state.set_value(f32::from(target), window, cx);
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
                .max(shape.max.into())
                .min(shape.min.into())
                .step(shape.step.into())
                .default_value(f32::from(snapped))
        });

        let slider_sub =
            cx.subscribe(
                &slider_state,
                |_panel, _slider, event: &SliderEvent, cx| match event {
                    // Continuous Change drives the in-process state so the numeric
                    // label tracks the drag. The HID write happens once on Release
                    // to keep us from spamming the device with intermediate values.
                    SliderEvent::Change(value) => {
                        let dpi = Dpi::from_rounded(value.start());
                        let dpi = AppState::try_read(cx)
                            .map_or(dpi, |state| state.normalize_active_dpi(dpi));
                        debug!(%dpi, "slider change → AppState.dpi");
                        AppState::update(cx, |state, cx| {
                            let key = state.current_record().map(DeviceRecord::device_key);
                            state.dpi = dpi;
                            if let Some(key) = key {
                                cx.emit(StateEvent::DpiChanged(key));
                            }
                        });
                        cx.notify();
                    }
                    SliderEvent::Release(value) => {
                        let dpi = Dpi::from_rounded(value.start());
                        let dpi = AppState::try_read(cx)
                            .map_or(dpi, |state| state.normalize_active_dpi(dpi));
                        // `commit_dpi` resolves the target at fire-time, so
                        // carousel-driven device switches route the write to the
                        // now-current device, not whichever was active when this
                        // slider entity was constructed.
                        AppState::update(cx, |state, cx| {
                            let key = state.current_record().map(DeviceRecord::device_key);
                            state.commit_dpi(dpi);
                            if let Some(key) = key {
                                cx.emit(StateEvent::DpiChanged(key));
                            }
                        });
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
                let normalized = AppState::try_read(cx)
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
    AppState::try_read(cx)
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
                AppState::retry_dpi_read(cx, key.clone());
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
                    let Some(dpi) = AppState::try_read(cx)
                        .and_then(|s| Some(s.active_dpi_capabilities()?.nearest(value)))
                    else {
                        return;
                    };
                    AppState::update(cx, |state, cx| {
                        let key = state.current_record().map(DeviceRecord::device_key);
                        state.commit_dpi(dpi);
                        if let Some(key) = key {
                            cx.emit(StateEvent::DpiChanged(key));
                        }
                    });
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
                    AppState::update(cx, |state, cx| {
                        let key = state.current_record().map(DeviceRecord::device_key);
                        state.commit_dpi_presets(next);
                        if let Some(key) = key {
                            cx.emit(StateEvent::DpiChanged(key));
                        }
                    });
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
            AppState::update(cx, |state, cx| {
                let key = state.current_record().map(DeviceRecord::device_key);
                let mut presets = state.dpi_presets();
                presets.push(state.dpi);
                state.commit_dpi_presets(presets);
                if let Some(key) = key {
                    cx.emit(StateEvent::DpiChanged(key));
                }
            });
        })
        .into_any_element()
}
