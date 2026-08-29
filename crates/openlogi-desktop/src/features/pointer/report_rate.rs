//! Device report-rate (polling frequency) controls.

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, Render, Styled, Subscription, Window, div,
};
use gpui_component::{
    Selectable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use openlogi_core::hid::ReportRateHz;

use crate::state::{AppState, DeviceKey, DeviceRecord, ReportRateStatus, StateEvent};
use crate::ui::components::PresetChip;
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, Palette, Typography as _};

pub struct ReportRatePanel {
    _state_obs: Subscription,
}

struct ReportRatePanelSnapshot {
    device_key: DeviceKey,
    rate: ReportRateHz,
    status: ReportRateStatus,
    reachable: bool,
}

impl ReportRatePanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = cx.subscribe(
            &AppState::global(cx),
            |_panel, _, event: &StateEvent, cx| {
                let relevant = match event {
                    StateEvent::InventoryChanged | StateEvent::DeviceSelected(_) => true,
                    StateEvent::ReportRateChanged(key) => AppState::try_read(cx)
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
            _state_obs: state_obs,
        }
    }
}

impl Render for ReportRatePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = report_rate_panel_snapshot(cx);
        let pal = theme::palette(cx);

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
                            .child(tr!("Report rate")),
                    )
                    .child(
                        div()
                            .text_body()
                            .text_color(pal.text_primary)
                            .child(format!("{} Hz", snapshot.rate)),
                    ),
            )
            .child(rate_body(&snapshot, pal))
    }
}

fn rate_body(snapshot: &ReportRatePanelSnapshot, pal: Palette) -> AnyElement {
    match &snapshot.status {
        ReportRateStatus::Ready(info) => {
            let chips: Vec<_> = info
                .capabilities
                .values()
                .iter()
                .enumerate()
                .map(|(idx, rate)| rate_chip(idx, *rate, *rate == snapshot.rate))
                .collect();
            v_flex()
                .gap_2()
                .child(h_flex().gap_2().flex_wrap().children(chips))
                .child(div().text_caption().text_color(pal.text_muted).child(tr!(
                    "Supported: %{min}–%{max} Hz",
                    min => info.capabilities.min(),
                    max => info.capabilities.max()
                )))
                .into_any_element()
        }
        ReportRateStatus::Unknown | ReportRateStatus::Loading if !snapshot.reachable => {
            status_line(tr!("Device offline — report rate unavailable."), pal).into_any_element()
        }
        ReportRateStatus::Unknown | ReportRateStatus::Loading => {
            status_line(tr!("Reading supported report rates…"), pal).into_any_element()
        }
        ReportRateStatus::Failed(_) => {
            let key = snapshot.device_key.clone();
            retry_line(
                "report-rate-retry",
                tr!("Couldn't read report rate — click to retry."),
                pal,
                move |cx| {
                    AppState::retry_report_rate_read(cx, key.clone());
                },
            )
            .into_any_element()
        }
        ReportRateStatus::Unsupported(_) => status_line(
            tr!("This device does not support adjustable report rate."),
            pal,
        )
        .into_any_element(),
    }
}

fn rate_chip(idx: usize, rate: ReportRateHz, active: bool) -> impl IntoElement {
    PresetChip::new(("report-rate-chip", idx))
        .selected(active)
        .child(
            Button::new(("report-rate-apply", idx))
                .compact()
                .ghost()
                .label(format!("{rate} Hz"))
                .selected(active)
                .on_click(move |_event, _window, cx| {
                    AppState::update(cx, |state, cx| {
                        let key = state.current_record().map(DeviceRecord::device_key);
                        state.commit_report_rate(rate);
                        if let Some(key) = key {
                            cx.emit(StateEvent::ReportRateChanged(key));
                        }
                    });
                }),
        )
}

fn report_rate_panel_snapshot(cx: &mut Context<ReportRatePanel>) -> ReportRatePanelSnapshot {
    AppState::try_read(cx)
        .and_then(|s| {
            let record = s.current_record()?;
            let device_key = record.device_key();
            Some(ReportRatePanelSnapshot {
                status: s.report_rate_status_for(&device_key),
                device_key,
                rate: s.report_rate(),
                reachable: record.route.is_some(),
            })
        })
        .unwrap_or_else(|| ReportRatePanelSnapshot {
            device_key: DeviceKey::default(),
            rate: ReportRateHz::new(1000),
            status: ReportRateStatus::Unsupported(tr!("No active device").to_string()),
            reachable: false,
        })
}
