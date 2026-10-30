//! Device DPI controls, and the report-rate row below them (see
//! [`super::report_rate`]). A mouse that scales its pointer with HID++
//! `0x2205` instead of adjustable DPI gets the pointer-speed row in place of
//! the DPI controls (see [`super::pointer_speed`]).
//!
//! The slider range comes from the selected device's HID++ DPI capability
//! (`0x2201` AdjustableDpi or `0x2202` ExtendedAdjustableDpi, whichever it
//! reports). Capability discovery runs in the background and the UI only
//! exposes exact device-supported values once the list is known.

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement,
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

use super::pointer_speed::{PointerSpeedFacts, PointerSpeedSlider, pointer_speed_section};
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
    /// The pointer-speed slider of a mouse without adjustable DPI.
    pointer_speed: Option<PointerSpeedSlider>,
    _state_obs: Subscription,
}

struct DpiNumericInput {
    key: DeviceKey,
    state: Entity<InputState>,
    text: FieldText,
    _sub: Subscription,
}

/// What the numeric field's text holds.
///
/// Focus alone cannot protect an edit: GPUI moves focus before it draws and
/// reports the blur after the draw, so the render in between already sees
/// the field unfocused. This state keeps that render from replacing the
/// typed text before the blur commits it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum FieldText {
    /// The device's DPI. Unfocused renders keep it current.
    #[default]
    Device,
    /// The user's edit, not committed yet by Enter or blur.
    Draft,
}

impl DpiNumericInput {
    /// End the edit, so the field shows the device's DPI again. Returns the
    /// typed DPI, or `None` when nothing was typed since the last commit or
    /// the text is not a number.
    fn take_draft(&mut self, cx: &App) -> Option<Dpi> {
        match std::mem::take(&mut self.text) {
            FieldText::Draft => parse_dpi_input(&self.state.read(cx).value()),
            FieldText::Device => None,
        }
    }
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
                StateEvent::DpiChanged(_)
                    | StateEvent::ReportRateChanged(_)
                    | StateEvent::PointerSpeedChanged(_)
            )
        });

        Self {
            slider: None,
            numeric: None,
            pointer_speed: None,
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

        // A uniform slider step can land between two stops; the value snaps
        // to the nearest stop before the device's own list resolves it, so a
        // drag never picks a value the slider does not offer.
        let (preview_stops, commit_stops) = (capabilities.clone(), capabilities.clone());
        let slider = CommitSlider::previewing(
            SliderRange::new(shape.min, shape.max).step(shape.step.into()),
            capabilities.nearest(dpi),
            cx,
            // Dragging drives the in-process state so the numeric label tracks
            // the thumb. The HID write happens once on release to keep us from
            // spamming the device with intermediate values.
            move |_, dpi, cx| {
                let dpi = preview_stops.nearest(dpi);
                let dpi =
                    AppState::try_read(cx).map_or(dpi, |state| state.normalize_active_dpi(dpi));
                debug!(%dpi, "slider change → AppState.dpi");
                AppState::apply(cx, |state| state.set_dpi_preview(dpi));
            },
            move |_, dpi, cx| {
                let dpi = commit_stops.nearest(dpi);
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
    /// when the active device changes. Otherwise a render puts `dpi` in the
    /// field, as the slider thumb follows external changes, but only while
    /// the field shows the device's value and is not focused. A value that
    /// lands mid-edit, or the render between focus loss and the blur, must
    /// not replace what the user typed.
    fn ensure_numeric_input(
        &mut self,
        key: &DeviceKey,
        capabilities: &DpiCapabilities,
        dpi: Dpi,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = capabilities.nearest(dpi).to_string();

        if let Some(current) = self.numeric.as_ref().filter(|current| current.key == *key) {
            let focused = current.state.focus_handle(cx).is_focused(window);
            if current.text == FieldText::Device
                && !focused
                && current.state.read(cx).value().as_ref() != text.as_str()
            {
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
        let sub = cx.subscribe_in(
            &state,
            window,
            |panel, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => panel.edit_numeric(input),
                InputEvent::PressEnter { .. } | InputEvent::Blur => {
                    panel.commit_numeric(input, window, cx);
                }
                InputEvent::Focus => {}
            },
        );
        self.numeric = Some(DpiNumericInput {
            key: key.clone(),
            state,
            text: FieldText::Device,
            _sub: sub,
        });
    }

    /// The numeric field `input` belongs to, unless it was replaced since.
    fn numeric_for(&mut self, input: &Entity<InputState>) -> Option<&mut DpiNumericInput> {
        self.numeric
            .as_mut()
            .filter(|field| field.state.entity_id() == input.entity_id())
    }

    /// The user changed the field's text: keep it until Enter or blur.
    fn edit_numeric(&mut self, input: &Entity<InputState>) {
        if let Some(field) = self.numeric_for(input) {
            field.text = FieldText::Draft;
        }
    }

    /// Write the field's edit on Enter or blur.
    ///
    /// Only an edit is written, so a blur after Enter, or focusing and then
    /// leaving the field, sends nothing. The edit goes only to the device the
    /// field was built for, and only while that device is selected: a blur
    /// that lands after a device switch must not move the other mouse. The
    /// value snaps to a supported step through
    /// [`AppState::normalize_active_dpi`], the path a slider release takes,
    /// and the field then shows the snapped value. Text that is not a number
    /// writes nothing; the next unfocused render shows the device's DPI.
    fn commit_numeric(
        &mut self,
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(field) = self.numeric_for(input) else {
            return;
        };
        let key = field.key.clone();
        let Some(typed) = field.take_draft(cx) else {
            return;
        };
        let Some(dpi) = AppState::try_read(cx)
            .filter(|state| state.is_current_device(&key))
            .map(|state| state.normalize_active_dpi(typed))
        else {
            debug!(key = %key, "numeric field left after a device switch — edit dropped");
            return;
        };
        debug!(%dpi, "numeric field commit → AppState.dpi");
        AppState::apply(cx, |state| state.commit_dpi(dpi));
        input.update(cx, |state, cx| state.set_value(dpi.to_string(), window, cx));
    }
}

impl Render for DpiPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let main = if let Some(facts) = PointerSpeedFacts::current(cx) {
            self.slider = None;
            self.numeric = None;
            PointerSpeedSlider::sync(&mut self.pointer_speed, &facts, window, cx);
            pointer_speed_section(facts, self.pointer_speed.as_ref(), pal)
        } else {
            self.pointer_speed = None;
            self.dpi_section(window, cx)
        };
        v_flex()
            .gap_3()
            .w_full()
            .child(main)
            .children(report_rate_section(ReportRateFacts::current(cx), pal))
    }
}

impl DpiPanel {
    /// The DPI value, slider, range and presets.
    fn dpi_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let snapshot = dpi_panel_snapshot(cx);
        let pal = theme::palette(cx);

        if let (DpiLoad::Ready(info), Some(key)) = (&snapshot.status, &snapshot.device_key) {
            // The slider spans the stops a hand can drag across; the field
            // beside it takes any value the device supports.
            let stops = info.capabilities.slider_stops();
            self.ensure_slider(key, &stops, snapshot.dpi, window, cx);
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
        let preset_chips: Vec<_> = presets_by_size(&snapshot.presets)
            .into_iter()
            .map(|(idx, value)| {
                let normalized =
                    AppState::try_read(cx).map_or(value, |state| state.normalize_active_dpi(value));
                let active = !already_highlighted && normalized == snapshot.dpi;
                already_highlighted |= active;
                preset_chip(idx, value, active, &snapshot.presets)
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
            .into_any_element()
    }
}

/// The presets in ascending order for display, each with its position in the
/// stored list. The stored order is the DPI-button cycle order and stays as
/// the user built it; removing a chip addresses it by that position.
fn presets_by_size(presets: &[Dpi]) -> Vec<(usize, Dpi)> {
    let mut sorted: Vec<_> = presets.iter().copied().enumerate().collect();
    sorted.sort_by_key(|&(_, dpi)| dpi);
    sorted
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
        // The range the slider spans, not the whole list: beside a slider that
        // ends at 8000, a gaming sensor's "100–48000 · step 1" reads as if the
        // slider still covered it.
        DpiLoad::Ready(info) => {
            let stops = info.capabilities.slider_stops();
            h_flex()
                .gap_1()
                .child(format!("{}–{}", stops.min(), stops.max()))
                .child(Icon::empty().path("action-icons/dot.svg").size_3())
                .child(tr!("pointer.dpi_step", step => stops.step_hint()))
                .into_any_element()
        }
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
/// `None` for empty or non-numeric input — [`DpiPanel::commit_numeric`]
/// treats that as "leave the last known-good DPI alone" rather than writing
/// garbage. A digit run too large for the wire format's `u16` saturates to
/// its maximum instead of being rejected, so the commit still snaps to the
/// device's top supported value.
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
    use std::sync::Arc;

    use gpui::{FocusHandle, InteractiveElement as _, TestAppContext, VisualTestContext};
    use openlogi_core::config::Config;
    use openlogi_core::device::DeviceInventory;
    use openlogi_core::hid::DpiInfo;
    use tokio::sync::mpsc::UnboundedReceiver;

    use super::*;
    use crate::services::assets::AssetResolver;
    use crate::services::ipc::{Command, SetDpi};
    use crate::state::Sources;
    use crate::state::tests::{KNOWN_MOUSE_KEY, direct_inventory};

    #[test]
    fn parses_a_plain_digit_string() {
        assert_eq!(parse_dpi_input("1600"), Some(Dpi::new(1600)));
    }

    #[test]
    fn presets_show_smallest_first_and_keep_their_stored_position() {
        let stored = [1600, 400, 3200, 800, 1600].map(Dpi::new);

        assert_eq!(
            presets_by_size(&stored),
            [(1, 400), (3, 800), (0, 1600), (4, 1600), (2, 3200)]
                .map(|(idx, dpi)| (idx, Dpi::new(dpi)))
        );
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

    /// The DPI the known mouse reports before each field test.
    const SAVED: Dpi = Dpi::new(800);

    /// Every DPI from 400 to 3200 in steps of 50.
    fn supported() -> DpiCapabilities {
        DpiCapabilities::new((400..=3200).step_by(50).collect()).expect("valid DPI list")
    }

    /// A second mouse, so a test can change the selected device.
    fn second_mouse() -> DeviceInventory {
        let mut inventory = direct_inventory([0x11, 0x22, 0x33, 0x44]);
        inventory.receiver.name = "MX Anywhere 3S".to_string();
        inventory.receiver.product_id = 0xb037;
        inventory
    }

    /// The position of the first device whose config key `matches` accepts.
    fn device_at(state: &AppState, matches: impl Fn(&str) -> bool) -> usize {
        state
            .devices()
            .iter()
            .position(|record| matches(&record.config_key))
            .expect("the fixture pairs that mouse")
    }

    /// The known mouse selected beside a second one, with live reads as the
    /// app installs them. The known mouse answers its DPI read with [`SAVED`]
    /// out of [`supported`]. Returns the commands sent from then on, and the
    /// other reads, kept unanswered so that no retry fires during a test.
    fn install_mice(cx: &mut TestAppContext) -> (UnboundedReceiver<Command>, Vec<Command>) {
        cx.update(gpui_component::init);
        cx.update(theme::register_builtin_themes);
        let mut receiver = cx.update(|cx| {
            let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
            let runtime: Arc<dyn swr_core::Runtime> = Arc::new(swr_gpui::GpuiRuntime::new(cx));
            let client = swr_core::SwrClient::builder().build(runtime.clone());
            let resolver = AssetResolver::new();
            let state = cx.new(|_| {
                let mut state = AppState::new(Sources {
                    inventories: &[direct_inventory([0xa3, 0x93, 0xca, 0xe0]), second_mouse()],
                    ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
                });
                let known = device_at(&state, |key| key == KNOWN_MOUSE_KEY);
                let _ = state.select_device(known);
                state.connect_device_reads(client, runtime);
                state
            });
            AppState::set_global(state, cx);
            AppState::load_current_device_reads(cx);
            receiver
        });
        cx.run_until_parked();
        let mut answered = false;
        let mut other_reads = Vec::new();
        while let Ok(command) = receiver.try_recv() {
            if let Command::ReadDpi(read) = command {
                read.reply
                    .send(Ok(DpiInfo {
                        current: SAVED,
                        capabilities: supported(),
                    }))
                    .expect("the DPI read waits for its answer");
                answered = true;
            } else {
                other_reads.push(command);
            }
        }
        assert!(answered, "the selected mouse reads its DPI");
        cx.run_until_parked();
        (receiver, other_reads)
    }

    /// The DPI panel beside a focus target outside it.
    struct PanelHost {
        panel: Entity<DpiPanel>,
        elsewhere: FocusHandle,
    }

    impl Render for PanelHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(600.))
                .child(self.panel.clone())
                .child(div().size(px(10.)).track_focus(&self.elsewhere))
        }
    }

    /// Draw, then settle what the draw queued, such as a blur.
    fn draw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    }

    /// The panel in an active window, drawn once. Returns the window, the
    /// numeric field and the focus target outside the panel.
    fn open_panel(
        cx: &mut TestAppContext,
    ) -> (&mut VisualTestContext, Entity<InputState>, FocusHandle) {
        let (host, cx) = cx.add_window_view(|_, cx| PanelHost {
            panel: cx.new(DpiPanel::new),
            elsewhere: cx.focus_handle(),
        });
        // An inactive window reports an empty focus path, so no blur at all.
        cx.update(|window, _| window.activate_window());
        draw(cx);
        let (field, elsewhere) = cx.read(|cx| {
            let host = host.read(cx);
            let field = host
                .panel
                .read(cx)
                .numeric
                .as_ref()
                .expect("a mouse with a DPI range shows the field")
                .state
                .clone();
            (field, host.elsewhere.clone())
        });
        (cx, field, elsewhere)
    }

    /// Move focus to `handle` and draw, as a click on another control does.
    fn focus(cx: &mut VisualTestContext, handle: &FocusHandle) {
        cx.update(|window, cx| handle.focus(window, cx));
        draw(cx);
    }

    /// Focus the field and replace its text, as typing does.
    fn type_in(cx: &mut VisualTestContext, field: &Entity<InputState>, text: &'static str) {
        let field_focus = cx.read(|cx| field.focus_handle(cx));
        focus(cx, &field_focus);
        cx.update(|window, cx| field.update(cx, |state, cx| state.replace_all(text, window, cx)));
        cx.run_until_parked();
    }

    /// Send `event` from the field without a draw, as a late callback does.
    fn emit(cx: &mut VisualTestContext, field: &Entity<InputState>, event: InputEvent) {
        cx.update(|_, cx| field.update(cx, |_, cx| cx.emit(event)));
        cx.run_until_parked();
    }

    /// The DPI writes sent since the last call, by device.
    fn dpi_writes(receiver: &mut UnboundedReceiver<Command>) -> Vec<(DeviceKey, Dpi)> {
        std::iter::from_fn(|| receiver.try_recv().ok())
            .filter_map(|command| {
                if let Command::SetDpi(SetDpi { key, dpi, .. }) = command {
                    Some((key, dpi))
                } else {
                    None
                }
            })
            .collect()
    }

    fn selected_mouse(cx: &mut VisualTestContext) -> DeviceKey {
        cx.read(|cx| {
            AppState::try_read(cx)
                .and_then(AppState::current_record)
                .expect("a mouse is selected")
                .device_key()
        })
    }

    fn field_text(cx: &mut VisualTestContext, field: &Entity<InputState>) -> String {
        cx.read(|cx| field.read(cx).value().to_string())
    }

    /// GPUI moves focus before it draws and reports the blur after the draw.
    /// The render between them must keep the typed text, so the blur writes
    /// what the user typed, not the value the field showed before.
    #[gpui::test]
    fn leaving_the_field_writes_the_typed_dpi(cx: &mut TestAppContext) {
        let (mut receiver, _reads) = install_mice(cx);
        let (cx, field, elsewhere) = open_panel(cx);
        let mouse = selected_mouse(cx);
        assert_eq!(field_text(cx, &field), SAVED.to_string());

        type_in(cx, &field, "2300");
        focus(cx, &elsewhere);

        assert_eq!(dpi_writes(&mut receiver), [(mouse, Dpi::new(2300))]);
        assert_eq!(field_text(cx, &field), "2300");
    }

    /// Enter writes the typed DPI snapped to a supported step, and the field
    /// shows the snapped value. The blur after it has no edit left to write.
    #[gpui::test]
    fn enter_writes_the_snapped_dpi_once(cx: &mut TestAppContext) {
        let (mut receiver, _reads) = install_mice(cx);
        let (cx, field, elsewhere) = open_panel(cx);
        let mouse = selected_mouse(cx);

        type_in(cx, &field, "1733");
        emit(
            cx,
            &field,
            InputEvent::PressEnter {
                secondary: false,
                shift: false,
            },
        );

        assert_eq!(dpi_writes(&mut receiver), [(mouse, Dpi::new(1750))]);
        assert_eq!(field_text(cx, &field), "1750");

        focus(cx, &elsewhere);
        assert!(
            dpi_writes(&mut receiver).is_empty(),
            "a blur without a new edit writes nothing"
        );
    }

    /// A blur that lands after the selection moved to another mouse, before
    /// the panel drew again, must not write the typed DPI to that mouse.
    #[gpui::test]
    fn a_blur_after_a_device_switch_writes_nothing(cx: &mut TestAppContext) {
        let (mut receiver, _reads) = install_mice(cx);
        let (cx, field, _) = open_panel(cx);

        type_in(cx, &field, "3200");
        cx.update(|_, cx| {
            AppState::apply(cx, |state| {
                let other = device_at(state, |key| key != KNOWN_MOUSE_KEY);
                state.select_device(other)
            });
        });
        emit(cx, &field, InputEvent::Blur);

        assert!(
            dpi_writes(&mut receiver).is_empty(),
            "the other mouse keeps its DPI"
        );
    }
}
