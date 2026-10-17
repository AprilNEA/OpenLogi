//! The report-rate row of the pointer-tuning card.
//!
//! One pill per rate the mouse lists for its current connection, the
//! selected pill being the rate it reports at now. The list is the mouse's
//! own (HID++ `0x8061` lists rates per connection type, `0x8060` lists
//! intervals), so the row never offers a rate the mouse would refuse. A pick
//! goes through [`AppState::commit_report_rate`], which persists it and writes
//! the mouse; the read-back then replaces the optimistic pick.

use gpui::{AnyElement, App, IntoElement, ParentElement, Styled, div};
use gpui_component::{Selectable as _, button::Button, h_flex, v_flex};
use openlogi_core::hid::ReportRate;

use crate::state::{AppState, DeviceKey, ReportRateLoad};
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{Palette, Typography as _};

/// What the row needs about the selected device.
pub(super) struct ReportRateFacts {
    key: DeviceKey,
    load: ReportRateLoad,
    reachable: bool,
}

impl ReportRateFacts {
    /// The selected device's report-rate facts, or `None` when nothing is
    /// selected or the device reports no report-rate control.
    pub(super) fn current(cx: &App) -> Option<Self> {
        let state = AppState::try_read(cx)?;
        if !state.current_report_rate_supported() {
            return None;
        }
        let record = state.current_record()?;
        Some(Self {
            key: record.device_key(),
            load: state.current_report_rate_load(),
            reachable: record.route.is_some(),
        })
    }
}

/// The row, or `None` to hide it: no control, a mouse that turned out not to
/// have one, or an offline mouse (the DPI row above already says it is
/// offline, and a rate list read over another connection would mislead).
pub(super) fn report_rate_section(
    facts: Option<ReportRateFacts>,
    pal: Palette,
) -> Option<AnyElement> {
    let ReportRateFacts {
        key,
        load,
        reachable,
    } = facts?;
    let body: AnyElement = match load {
        ReportRateLoad::Ready(info) => h_flex()
            .gap_2()
            .flex_wrap()
            .children(
                info.supported
                    .iter()
                    .map(|&rate| rate_pill(rate, rate == info.current)),
            )
            .into_any_element(),
        ReportRateLoad::Unknown | ReportRateLoad::Loading if !reachable => return None,
        ReportRateLoad::Unknown | ReportRateLoad::Loading => {
            status_line(tr!("pointer.reading_report_rate"), pal).into_any_element()
        }
        // Clickable in place, like the DPI retry: reselecting is a no-op for a
        // single-device gallery.
        ReportRateLoad::Failed(_) => retry_line(
            "report-rate-retry",
            tr!("pointer.couldnt_read_report_rate_click_to_retry"),
            pal,
            move |cx| AppState::apply(cx, |state| state.retry_report_rate_read(&key)),
        )
        .into_any_element(),
        ReportRateLoad::Unsupported(_) => return None,
    };
    Some(
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("pointer.report_rate")),
            )
            .child(body)
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("pointer.report_rate_description")),
            )
            .into_any_element(),
    )
}

/// One rate pill. Clicking it writes `rate` to the selected mouse.
fn rate_pill(rate: ReportRate, selected: bool) -> impl IntoElement {
    Button::new(("report-rate", usize::from(rate.into_inner())))
        .compact()
        .label(tr!("pointer.report_rate_hz", rate => rate))
        .selected(selected)
        .on_click(move |_event, _window, cx| {
            AppState::apply(cx, |state| state.commit_report_rate(rate));
        })
}
