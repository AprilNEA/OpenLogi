//! Device DPI controls.
//!
//! The slider range comes from the selected device's HID++ DPI capability
//! (`0x2201` AdjustableDpi or `0x2202` ExtendedAdjustableDpi, whichever it
//! reports). Capability discovery runs in the background and the UI only
//! exposes exact device-supported values once the list is known.

use gpui::{
    AnyElement, AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement,
    Render, SharedString, Styled, Subscription, Window, div, px,
};
use gpui_component::{
    IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{InputEvent, InputState},
    slider::{Slider, SliderEvent, SliderState},
    v_flex,
};
use openlogi_core::hid::{Dpi, DpiCapabilities};
use tracing::debug;

use crate::state::{AppState, DeviceKey, DeviceRecord, DpiStatus, StateEvent};
use crate::ui::components::{PresetChip, control_input};
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, Palette, Typography as _};

pub struct DpiPanel {
    slider_state: Option<Entity<SliderState>>,
    slider_sub: Option<Subscription>,
    slider_key: Option<String>,
    slider_shape: Option<SliderShape>,
    /// The numeric field beside the slider, letting an exact value be typed
    /// instead of dragged. Keyed by device like the slider, but reused across
    /// capability re-reads for the same device since only its displayed text
    /// needs to track those, not its identity.
    numeric_state: Option<Entity<InputState>>,
    numeric_sub: Option<Subscription>,
    numeric_key: Option<String>,
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
        // Repaint when the active device changes or DPI discovery
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
            numeric_state: None,
            numeric_sub: None,
            numeric_key: None,
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
                            state.set_dpi_preview(dpi);
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
                        // gallery-driven device switches route the write to the
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

    /// Build/refresh the slider and numeric field for the current snapshot, or
    /// tear both down where neither applies (loading, offline, failed, …).
    fn refresh_controls(
        &mut self,
        snapshot: &DpiPanelSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let DpiStatus::Ready(info) = &snapshot.status else {
            self.slider_state = None;
            self.slider_sub = None;
            self.slider_key = None;
            self.slider_shape = None;
            self.numeric_state = None;
            self.numeric_sub = None;
            self.numeric_key = None;
            return;
        };
        self.ensure_slider(
            snapshot.device_key.as_str(),
            &info.capabilities,
            snapshot.dpi,
            window,
            cx,
        );
        // A device with a single supported DPI has nothing to type either —
        // `slider_element` shows a fixed-value line instead of a control.
        if info.capabilities.min() == info.capabilities.max() {
            self.numeric_state = None;
            self.numeric_sub = None;
            self.numeric_key = None;
        } else {
            self.ensure_numeric_input(
                snapshot.device_key.as_str(),
                &info.capabilities,
                snapshot.dpi,
                window,
                cx,
            );
        }
    }

    /// Build (or refresh) the numeric field beside the slider. Rebuilt only
    /// when the active device changes; otherwise its displayed text is
    /// resynced to `dpi` on every render, mirroring how the slider thumb
    /// tracks external changes — except while the field is focused, so a
    /// value landing mid-edit can't overwrite what the user is typing.
    fn ensure_numeric_input(
        &mut self,
        key: &str,
        capabilities: &DpiCapabilities,
        dpi: Dpi,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapped = capabilities.nearest(dpi);
        let text = format!("{snapped}");

        if self.numeric_key.as_deref() == Some(key) {
            if let Some(numeric_state) = &self.numeric_state {
                let focused = numeric_state.focus_handle(cx).is_focused(window);
                if !focused && numeric_state.read(cx).value().as_ref() != text.as_str() {
                    numeric_state.update(cx, |state, cx| {
                        state.set_value(text, window, cx);
                    });
                }
            }
            return;
        }

        let numeric_state = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(text)
                .validate(|text, _cx| text.chars().all(|c| c.is_ascii_digit()))
        });
        let numeric_sub = cx.subscribe(&numeric_state, |_panel, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                commit_numeric_dpi(&input, cx);
            }
        });
        self.numeric_state = Some(numeric_state);
        self.numeric_sub = Some(numeric_sub);
        self.numeric_key = Some(key.to_string());
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
    AppState::update(cx, |state, cx| {
        let key = state.current_record().map(DeviceRecord::device_key);
        state.commit_dpi(dpi);
        if let Some(key) = key {
            cx.emit(StateEvent::DpiChanged(key));
        }
    });
}

impl Render for DpiPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = dpi_panel_snapshot(cx);
        let pal = theme::palette(cx);

        self.refresh_controls(&snapshot, window, cx);

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
            self.slider_state.as_ref(),
            snapshot.reachable,
            snapshot.device_key.clone(),
            pal,
        );
        let numeric_input = numeric_input_element(&snapshot.status, self.numeric_state.as_ref());

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
            tr!("pointer.dpi_range_device_offline")
        }
        DpiStatus::Unknown | DpiStatus::Loading => tr!("pointer.loading_device_dpi_range"),
        DpiStatus::Failed(message) => tr!("pointer.dpi_read_failed", message => message),
        DpiStatus::Unsupported(message) => {
            tr!("pointer.dpi_range_unavailable", message => message)
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
            move |cx| {
                AppState::retry_dpi_read(cx, key.clone());
            },
        )
        .into_any_element(),
        (DpiStatus::Unsupported(_), _) => {
            status_line(tr!("pointer.adjustable_dpi_unsupported"), pal).into_any_element()
        }
    }
}

/// The numeric field beside the slider, or `None` where no slider is shown
/// either (loading, offline, fixed DPI, …) — see [`slider_element`].
fn numeric_input_element(
    status: &DpiStatus,
    numeric_state: Option<&Entity<InputState>>,
) -> Option<AnyElement> {
    match (status, numeric_state) {
        (DpiStatus::Ready(info), Some(state))
            if info.capabilities.min() != info.capabilities.max() =>
        {
            Some(control_input(state).w(px(64.)).into_any_element())
        }
        _ => None,
    }
}

/// Parse the DPI numeric field's raw text into a device value.
///
/// `None` for empty input — [`DpiPanel::commit_numeric_dpi`] treats that as
/// "leave the last known-good DPI alone" rather than writing garbage. A value
/// too large for the wire format's `u16` saturates to its maximum instead of
/// being rejected outright: the field's own [`InputState::validate`] filter
/// already keeps the text digits-only, so the only way to reach an
/// out-of-range number here is typing more digits than any real DPI needs —
/// and with enough of them the string overflows `u32` before it ever reaches
/// the `u16` conversion, which is why the overflow case is matched directly
/// rather than folded into a single `.ok()`.
fn parse_dpi_input(text: &str) -> Option<Dpi> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    match text.parse::<u32>() {
        Ok(value) => Some(Dpi::new(u16::try_from(value).unwrap_or(u16::MAX))),
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
            AppState::update(cx, |state, cx| {
                let key = state.current_record().map(DeviceRecord::device_key);
                let mut presets = state.dpi_presets();
                presets.push(state.dpi());
                state.commit_dpi_presets(presets);
                if let Some(key) = key {
                    cx.emit(StateEvent::DpiChanged(key));
                }
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

    /// A digit string long enough to overflow the `u32` the text is first
    /// parsed into (not just the final `u16`) must still saturate rather than
    /// silently fail to commit.
    #[test]
    fn saturates_a_value_that_overflows_the_intermediate_parse() {
        assert_eq!(
            parse_dpi_input("99999999999999999999"),
            Some(Dpi::new(u16::MAX))
        );
    }
}
