//! Device DPI controls, and the report-rate row below them (see
//! [`super::report_rate`]).
//!
//! The slider range comes from the selected device's HID++ DPI capability
//! (`0x2201` AdjustableDpi or `0x2202` ExtendedAdjustableDpi, whichever it
//! reports). Capability discovery runs in the background and the UI only
//! exposes exact device-supported values once the list is known.

use gpui::{
    AnyElement, AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement,
    Render, Styled, Subscription, Window, div, px,
};
use gpui_component::{
    Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{InputEvent, InputState},
    slider::{Slider, SliderState},
    v_flex,
};
use openlogi_core::hid::{Dpi, DpiCapabilities};
use tracing::debug;

use super::report_rate::{ReportRateFacts, report_rate_section};
use crate::state::{AppState, DeviceKey, DpiLoad, StateEvent};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::components::{PresetChip, control_input};
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, Palette, Typography as _};

pub struct DpiPanel {
    /// Rebuilt whenever the selected device or its reported range changes,
    /// because a slider's range is fixed when it is built.
    slider: Option<DpiSlider>,
    /// The numeric field beside the slider, letting an exact value be typed
    /// instead of dragged. Keyed by device like the slider, but reused across
    /// capability re-reads for the same device since only its displayed text
    /// needs to track those, not its identity.
    numeric: Option<DpiNumericInput>,
    _state_obs: Subscription,
}

struct DpiNumericInput {
    key: DeviceKey,
    state: Entity<InputState>,
    _sub: Subscription,
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
}

impl DpiPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Repaint when the active device changes, DPI discovery completes,
        // or the report-rate read lands. The slider entity is rebuilt in
        // `render` whenever the selected device or reported range changes,
        // because SliderState's range is builder-only.
        let state_obs = AppState::repaint_on(cx, |event| {
            matches!(
                event,
                StateEvent::DpiChanged(_) | StateEvent::ReportRateChanged(_)
            )
        });

        Self {
            slider: None,
            numeric: None,
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

    /// Build (or refresh) the numeric field beside the slider. Rebuilt only
    /// when the active device changes; otherwise its displayed text is
    /// resynced to `dpi` on every render, mirroring how the slider thumb
    /// tracks external changes — except while the field is focused, so a
    /// value landing mid-edit can't overwrite what the user is typing.
    fn ensure_numeric_input(
        &mut self,
        key: &DeviceKey,
        capabilities: &DpiCapabilities,
        dpi: Dpi,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = format!("{}", capabilities.nearest(dpi));

        if let Some(current) = self.numeric.as_ref().filter(|current| current.key == *key) {
            let focused = current.state.focus_handle(cx).is_focused(window);
            if !focused && current.state.read(cx).value().as_ref() != text.as_str() {
                current.state.update(cx, |state, cx| {
                    state.set_value(text, window, cx);
                });
            }
            return;
        }

        let state = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(text)
                .validate(|text, _cx| text.chars().all(|c| c.is_ascii_digit()))
        });
        let sub = cx.subscribe(&state, |_panel, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                commit_numeric_dpi(&input, cx);
            }
        });
        self.numeric = Some(DpiNumericInput {
            key: key.clone(),
            state,
            _sub: sub,
        });
    }
}

/// Parse the numeric field's current text and write it as the active
/// device's DPI through the same [`AppState::normalize_active_dpi`] path the
/// slider commits through — so typing a value the device doesn't support
/// snaps it exactly as dragging to it would.
///
/// Unparsable or empty text is a no-op: the field keeps whatever the user
/// left in it until it next loses focus, at which point
/// [`DpiPanel::ensure_numeric_input`] reflects the last known-good DPI back.
fn commit_numeric_dpi(input: &Entity<InputState>, cx: &mut Context<DpiPanel>) {
    let Some(dpi) = parse_dpi_input(&input.read(cx).value()) else {
        return;
    };
    let dpi = AppState::try_read(cx).map_or(dpi, |state| state.normalize_active_dpi(dpi));
    debug!(%dpi, "numeric field commit → AppState.dpi");
    AppState::apply(cx, |state| state.commit_dpi(dpi));
}

impl Render for DpiPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = dpi_panel_snapshot(cx);
        let pal = theme::palette(cx);

        if let (DpiLoad::Ready(info), Some(key)) = (&snapshot.status, &snapshot.device_key) {
            self.ensure_slider(key, &info.capabilities, snapshot.dpi, window, cx);
            // A device with a single supported DPI has nothing to type either —
            // `slider_element` shows a fixed-value line instead of a control.
            if info.capabilities.min() == info.capabilities.max() {
                self.numeric = None;
            } else {
                self.ensure_numeric_input(key, &info.capabilities, snapshot.dpi, window, cx);
            }
        } else {
            self.slider = None;
            self.numeric = None;
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
        let numeric_input = self
            .numeric
            .as_ref()
            .map(|current| control_input(&current.state).w(px(64.)).into_any_element());

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
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(slider))
                    .children(numeric_input),
            )
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
            .children(report_rate_section(ReportRateFacts::current(cx), pal))
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
            })
        })
        .unwrap_or_else(|| DpiPanelSnapshot {
            device_key: None,
            dpi: crate::state::DEFAULT_DPI,
            presets: Vec::new(),
            status: DpiLoad::Unsupported(tr!("device.no_active_device").to_string()),
            reachable: false,
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

/// Parse the DPI numeric field's raw text into a device value.
///
/// `None` for empty or non-numeric input — [`commit_numeric_dpi`] treats that
/// as "leave the last known-good DPI alone" rather than writing garbage. A
/// digit run too large for the wire format's `u16` saturates to its maximum
/// instead of being rejected, so the commit still snaps to the device's top
/// supported value.
fn parse_dpi_input(text: &str) -> Option<Dpi> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    match text.parse::<u16>() {
        Ok(value) => Some(Dpi::new(value)),
        Err(error) if *error.kind() == std::num::IntErrorKind::PosOverflow => {
            Some(Dpi::new(u16::MAX))
        }
        Err(_) => None,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_plain_digit_string() {
        assert_eq!(parse_dpi_input("1600"), Some(Dpi::new(1600)));
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(parse_dpi_input("  800  "), Some(Dpi::new(800)));
    }

    #[test]
    fn rejects_empty_input() {
        assert_eq!(parse_dpi_input(""), None);
        assert_eq!(parse_dpi_input("   "), None);
    }

    #[test]
    fn rejects_non_numeric_input() {
        assert_eq!(parse_dpi_input("abc"), None);
        assert_eq!(parse_dpi_input("16.5"), None);
        assert_eq!(parse_dpi_input("-100"), None);
    }

    /// The field itself only lets digits through via `InputState::validate`,
    /// so this is reachable only by typing an implausibly long run of them —
    /// but it must still not panic or wrap.
    #[test]
    fn saturates_a_value_above_the_wire_format() {
        assert_eq!(parse_dpi_input("999999"), Some(Dpi::new(u16::MAX)));
    }

    /// A digit string far beyond any machine integer must still saturate
    /// rather than silently fail to commit.
    #[test]
    fn saturates_a_value_that_overflows_any_integer_width() {
        assert_eq!(
            parse_dpi_input("99999999999999999999"),
            Some(Dpi::new(u16::MAX))
        );
    }
}
