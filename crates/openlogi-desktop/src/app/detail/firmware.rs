//! Device firmware: live version, explicit installation, and agent-owned progress.

use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement as _, ParentElement as _, RenderOnce,
    SharedString, StyleRefinement, Styled as _, Window, div, prelude::FluentBuilder as _, rems,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{Cancel, Confirm, DialogFooter},
    h_flex,
    message_scroller::{MessageScroller, MessageScrollerState},
    progress::Progress,
    v_flex,
};
use openlogi_core::hid::{
    FirmwareBatteryCharge, FirmwareOffer, FirmwareStatus, FirmwareUpdate, FirmwareUpdateFailure,
    FirmwareUpdatePhase, FirmwareUpdateRefusal, FirmwareVersion,
};

use crate::state::{AgentLink, AppState, DeviceKey, FirmwareStartFailure, Load};
use crate::ui::components::{PanelCard, control_button};
use crate::ui::theme::{Palette, Typography as _};

/// Firmware facts projected for the selected Device tab.
pub(super) struct FirmwareFacts {
    key: DeviceKey,
    name: String,
    load: Load<FirmwareStatus>,
    reachable: bool,
    busy: bool,
    starting: bool,
    update: Option<FirmwareUpdate>,
    refusal: Option<FirmwareStartFailure>,
}

impl FirmwareFacts {
    /// Read presentation facts without starting any device work.
    pub(super) fn current(cx: &App) -> Option<Self> {
        let state = AppState::try_read(cx)?;
        let record = state.current_record()?;
        let route = record.route.as_ref()?;
        if matches!(route, openlogi_core::hid::DeviceRoute::RawHid { .. }) {
            return None;
        }
        Some(Self {
            key: record.device_key(),
            name: record.display_name.clone(),
            load: state.current_firmware_load(),
            reachable: record.online && matches!(state.agent_link(), AgentLink::Ready(_)),
            busy: state.firmware_busy(),
            starting: state.current_firmware_starting(),
            update: state.current_firmware_update().cloned(),
            refusal: state.current_firmware_start_failure().cloned(),
        })
    }
}

/// Compose the firmware section, hiding devices without firmware support.
pub(super) fn firmware_card(
    facts: Option<FirmwareFacts>,
    pal: Palette,
    cx: &App,
) -> Option<AnyElement> {
    let facts = facts?;
    if matches!(facts.load, Load::Unsupported(_)) && facts.update.is_none() {
        return None;
    }
    let mut body = v_flex().w_full().min_w_0().gap_3();
    match &facts.load {
        Load::Ready(status) => {
            body = body.child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_body()
                            .text_color(pal.text_primary)
                            .child(tr!("firmware.running", version => status.running.to_string())),
                    )
                    .when(status.offer == FirmwareOffer::UpToDate, |row| {
                        row.child(
                            div()
                                .debug_selector(|| "firmware-current-check".into())
                                .child(
                                    Icon::new(IconName::CircleCheck).text_color(cx.theme().success),
                                ),
                        )
                    }),
            );
        }
        Load::Failed(_)
            if facts
                .update
                .as_ref()
                .is_none_or(|job| job.phase.is_finished()) =>
        {
            let key = facts.key.clone();
            body = body.child(caption(tr!("firmware.read_failed"), pal)).child(
                control_button("firmware-read-retry")
                    .label(tr!("common.try_again"))
                    .disabled(!facts.reachable || facts.busy)
                    .on_click(move |_, _, cx| {
                        AppState::apply(cx, |state| state.retry_firmware_read(&key));
                    }),
            );
        }
        Load::Unknown | Load::Loading if facts.update.is_none() && !facts.starting => {
            body = body.child(caption(
                if facts.reachable {
                    tr!("firmware.reading")
                } else {
                    tr!("firmware.offline")
                },
                pal,
            ));
        }
        _ => {}
    }
    if facts.starting {
        body = body.child(progress_line(&FirmwareUpdatePhase::Downloading, pal, cx));
    } else if let Some(update) = &facts.update {
        body = body.child(progress_line(&update.phase, pal, cx));
        if !update.events.is_empty() {
            body = body.child(FirmwareActivityLog {
                update: update.clone(),
                key: facts.key.clone(),
                pal,
            });
        }
    }
    if let Some(refusal) = &facts.refusal {
        body = body.child(caption(start_failure_text(refusal), pal));
    }
    body = body.children(offer_controls(&facts, pal));
    if facts.busy
        && !facts.starting
        && facts
            .update
            .as_ref()
            .is_none_or(|job| job.phase.is_finished())
    {
        body = body.child(caption(tr!("firmware.another_update"), pal));
    } else if !facts.reachable
        && !matches!(facts.load, Load::Unknown | Load::Loading)
        && facts
            .update
            .as_ref()
            .is_none_or(|job| job.phase.is_finished())
    {
        body = body.child(caption(tr!("firmware.offline"), pal));
    }
    Some(
        PanelCard::new(
            tr!("firmware.title"),
            Icon::new(IconName::Info),
            body.debug_selector(|| "firmware-card".into()),
        )
        .into_any_element(),
    )
}

fn offer_controls(facts: &FirmwareFacts, pal: Palette) -> Option<AnyElement> {
    if facts.starting {
        return None;
    }
    let Load::Ready(status) = &facts.load else {
        return None;
    };
    let install = match status.installation_target() {
        Some(target) if status.offer == FirmwareOffer::UpToDate => {
            FirmwareInstallation::Reinstall(target)
        }
        Some(target) => FirmwareInstallation::Update(target),
        None => return Some(caption(tr!("firmware.unavailable"), pal).into_any_element()),
    };
    let key = facts.key.clone();
    let name = facts.name.clone();
    Some(
        v_flex()
            .gap_3()
            .child(
                caption(
                    match install {
                        FirmwareInstallation::Update(target) => {
                            tr!("firmware.available", version => target.to_string())
                        }
                        FirmwareInstallation::Reinstall(_) => tr!("firmware.up_to_date"),
                    },
                    pal,
                )
                .debug_selector(|| "firmware-offer-status".into()),
            )
            .child(
                control_button("firmware-update")
                    .debug_selector(|| "firmware-update".into())
                    .outline()
                    .label(tr!("firmware.update_dialog"))
                    .disabled(!facts.reachable || facts.busy)
                    .on_click(move |_, window, cx| {
                        open_update_confirmation(window, cx, key.clone(), name.clone(), install);
                    }),
            )
            .into_any_element(),
    )
}

#[derive(Clone, Copy)]
enum FirmwareInstallation {
    Update(FirmwareVersion),
    Reinstall(FirmwareVersion),
}

impl FirmwareInstallation {
    fn target(self) -> FirmwareVersion {
        match self {
            Self::Update(target) | Self::Reinstall(target) => target,
        }
    }
}

fn caption(text: SharedString, pal: Palette) -> gpui::Div {
    div().text_caption().text_color(pal.text_muted).child(text)
}

fn progress_line(phase: &FirmwareUpdatePhase, pal: Palette, cx: &App) -> AnyElement {
    let text = phase_text(phase);
    v_flex()
        .gap_2()
        .debug_selector(|| "firmware-progress".into())
        .child(
            caption(text.clone(), pal)
                .when(matches!(phase, FirmwareUpdatePhase::Failed(_)), |line| {
                    line.text_color(cx.theme().danger)
                }),
        )
        .when(!phase.is_finished(), |column| {
            let progress = Progress::new("firmware-progress-bar").accessibility_label(text);
            column.child(match phase {
                FirmwareUpdatePhase::Transferring { percent } => {
                    progress.value(f32::from(*percent))
                }
                _ => progress.loading(true),
            })
        })
        .into_any_element()
}

fn phase_text(phase: &FirmwareUpdatePhase) -> SharedString {
    match phase {
        FirmwareUpdatePhase::Downloading => tr!("firmware.downloading"),
        FirmwareUpdatePhase::Preparing => tr!("firmware.preparing"),
        FirmwareUpdatePhase::Transferring { percent } => {
            tr!("firmware.transferring", percent => percent)
        }
        FirmwareUpdatePhase::Restarting => tr!("firmware.restarting"),
        FirmwareUpdatePhase::Succeeded => tr!("firmware.succeeded"),
        FirmwareUpdatePhase::Failed(error) => failure_text(error),
    }
}

#[derive(gpui::IntoElement)]
struct FirmwareActivityLog {
    update: FirmwareUpdate,
    key: DeviceKey,
    pal: Palette,
}

#[derive(Clone, PartialEq, Eq)]
struct ActivityLine {
    timestamp_millis: u64,
    text: SharedString,
    error: bool,
}

fn activity_lines(update: &FirmwareUpdate) -> Vec<ActivityLine> {
    let mut lines = Vec::new();
    for (index, event) in update.events.iter().enumerate() {
        let mut add = |text, error| {
            lines.push(ActivityLine {
                timestamp_millis: event.timestamp_millis,
                text,
                error,
            });
        };
        if index == 0
            && let Some(source) = &update.source
        {
            add(
                tr!("firmware.activity_device", name => source.device_name.clone(), family => source.firmware_line.clone(), version => update.target.to_string()),
                false,
            );
            add(
                tr!("firmware.activity_url", url => source.package_url.clone()),
                false,
            );
        }
        if matches!(event.phase, FirmwareUpdatePhase::Preparing) && update.source.is_some() {
            add(
                tr!("firmware.activity_downloaded", version => update.target.to_string()),
                false,
            );
        }
        let error = matches!(event.phase, FirmwareUpdatePhase::Failed(_));
        let message = phase_text(&event.phase);
        add(
            if error {
                tr!("firmware.activity_error", message => message)
            } else {
                message
            },
            error,
        );
    }
    lines
}

impl RenderOnce for FirmwareActivityLog {
    fn render(self, window: &mut Window, cx: &mut App) -> impl gpui::IntoElement {
        let id: gpui::ElementId = SharedString::from(format!(
            "firmware-log-{}-{}",
            self.key.as_str(),
            self.update.id
        ))
        .into();
        let lines = activity_lines(&self.update);
        let scroller = window.use_keyed_state((id.clone(), "scroll"), cx, |_, cx| {
            MessageScrollerState::new(lines.len(), cx)
        });
        let cached = window.use_keyed_state((id.clone(), "rows"), cx, |_, _| lines.clone());
        let previous = cached.read(cx);
        if *previous != lines {
            let appending = lines.starts_with(previous);
            let previous_len = previous.len();
            scroller.update(cx, |state, cx| {
                if appending {
                    state.append(lines.len() - previous_len, cx);
                } else if lines.len() == previous_len {
                    state.remeasure(cx);
                } else {
                    state.reset(lines.len(), cx);
                }
            });
            cached.update(cx, |cached, _| cached.clone_from(&lines));
        }
        let pal = self.pal;
        let count = lines.len();
        v_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .gap_1()
            .child(caption(tr!("firmware.activity_log"), pal))
            .child(
                div()
                    .debug_selector(|| "firmware-activity-log".into())
                    .w_full()
                    .min_w_0()
                    .h(rems(12.6))
                    .flex_shrink_0()
                    .bg(pal.control)
                    .border_1()
                    .border_color(pal.border)
                    .rounded(pal.control_radius)
                    .overflow_hidden()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_caption()
                    .child(
                        MessageScroller::new(id, scroller, move |index, _, cx| {
                            let line = &lines[index];
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .items_start()
                                .gap_2()
                                .when(index == 0, |row| {
                                    row.pt_2()
                                        .debug_selector(|| "firmware-log-first-row".into())
                                })
                                .when(index + 1 == count, |row| {
                                    row.pb_2().debug_selector(|| "firmware-log-last-row".into())
                                })
                                .child(
                                    div()
                                        .w(rems(4.5))
                                        .flex_shrink_0()
                                        .text_color(pal.text_muted)
                                        .child(activity_time(line.timestamp_millis)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .when(line.error, |message| {
                                            message.debug_selector(|| "firmware-log-error".into())
                                        })
                                        .text_color(if line.error {
                                            cx.theme().danger
                                        } else {
                                            pal.text_primary
                                        })
                                        .child(line.text.clone()),
                                )
                        })
                        // Measured rows own vertical insets, so the list's
                        // scroll extent and native tail anchor agree.
                        .with_list_style(StyleRefinement::default().py_0())
                        .with_row_style(StyleRefinement::default().px_2().pb_0())
                        .with_jump_button_label(tr!("firmware.activity_latest")),
                    ),
            )
    }
}

fn activity_time(timestamp_millis: u64) -> String {
    let seconds = timestamp_millis / 1000 % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn failure_text(error: &FirmwareUpdateFailure) -> SharedString {
    match error {
        FirmwareUpdateFailure::Download(_) => tr!("firmware.download_failed"),
        FirmwareUpdateFailure::ImageRejected => tr!("firmware.image_rejected"),
        FirmwareUpdateFailure::WrongDevice => tr!("firmware.wrong_device"),
        FirmwareUpdateFailure::BatteryLow { percent } => {
            tr!("firmware.battery_low", percent => percent)
        }
        FirmwareUpdateFailure::Unsupported => tr!("firmware.unsupported"),
        FirmwareUpdateFailure::DeviceRefused { .. } => tr!("firmware.device_refused"),
        FirmwareUpdateFailure::Protocol(_) | FirmwareUpdateFailure::Device(_) => {
            tr!("firmware.interrupted")
        }
        FirmwareUpdateFailure::NotActivated => tr!("firmware.not_activated"),
    }
}

fn start_failure_text(error: &FirmwareStartFailure) -> SharedString {
    match error {
        FirmwareStartFailure::NotSent | FirmwareStartFailure::Unavailable => {
            tr!("firmware.agent_unavailable")
        }
        FirmwareStartFailure::Refused(FirmwareUpdateRefusal::Busy) => {
            tr!("firmware.another_update")
        }
        FirmwareStartFailure::Refused(FirmwareUpdateRefusal::NoUpdate) => tr!("firmware.no_update"),
        FirmwareStartFailure::Refused(FirmwareUpdateRefusal::DeviceNotFound) => {
            tr!("firmware.offline")
        }
        FirmwareStartFailure::Refused(FirmwareUpdateRefusal::Unreadable) => {
            tr!("firmware.read_failed")
        }
        FirmwareStartFailure::Refused(FirmwareUpdateRefusal::Stopping) => {
            tr!("firmware.agent_stopping")
        }
    }
}

fn open_update_confirmation(
    window: &mut Window,
    cx: &mut App,
    key: DeviceKey,
    name: String,
    installation: FirmwareInstallation,
) {
    let target = installation.target();
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let charge = AppState::try_read(cx)
            .map_or(FirmwareBatteryCharge::Unknown, |state| state.firmware_battery_charge(&key));
        let warning = match charge {
            FirmwareBatteryCharge::Low => Some(tr!("firmware.battery_below_minimum", minimum => FirmwareBatteryCharge::minimum_percent())),
            FirmwareBatteryCharge::Unknown => Some(tr!("firmware.battery_unknown")),
            FirmwareBatteryCharge::Sufficient => None,
        };
        alert
            .title(match installation {
                FirmwareInstallation::Update(_) => tr!("firmware.confirm_title", name => name.clone(), version => target.to_string()),
                FirmwareInstallation::Reinstall(_) => tr!("firmware.reinstall_title", name => name.clone()),
            })
            .description(
                v_flex()
                    .gap_3()
                    .debug_selector(|| "firmware-confirmation".into())
                    .when(matches!(installation, FirmwareInstallation::Reinstall(_)), |column| column.child(
                        div().debug_selector(|| "firmware-already-latest".into())
                            .child(tr!("firmware.already_latest", version => target.to_string())),
                    ))
                    .child(tr!("firmware.confirm_description")),
            )
            .footer(
                v_flex()
                    .w_full()
                    .gap_3()
                    .when_some(warning, |column, warning| column.child(
                        div()
                            .debug_selector(|| "firmware-battery-warning".into())
                            .text_color(cx.theme().danger)
                            .child(warning),
                    ))
                    .child(
                        DialogFooter::new()
                            .child(
                                Button::new("firmware-confirm-cancel")
                                    .label(tr!("common.cancel"))
                                    .on_click(|_, window, cx| window.dispatch_action(Box::new(Cancel), cx)),
                            )
                            .child(
                                Button::new("firmware-confirm-install")
                                    .debug_selector(|| "firmware-confirm-install".into())
                                    .primary()
                                    .label(match installation {
                                        FirmwareInstallation::Update(_) => tr!("firmware.install"),
                                        FirmwareInstallation::Reinstall(_) => tr!("firmware.reinstall"),
                                    })
                                    .disabled(!charge.is_sufficient())
                                    .on_click(|_, window, cx| window.dispatch_action(Box::new(Confirm { secondary: false }), cx)),
                            ),
                    ),
            )
            .on_ok({
                let key = key.clone();
                move |_, _, cx| {
                    let mut submitted = false;
                    AppState::apply(cx, |state| {
                        let events = state.start_firmware_update(&key, target);
                        submitted = !events.is_empty();
                        events
                    });
                    submitted
                }
            })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::i18n::LOCALE_LOCK;
    use crate::services::ipc::Command;
    use crate::state::tests::{firmware_current_state, firmware_ready_state};
    use crate::ui::theme;
    use gpui::{
        AppContext as _, Context, Modifiers, Render, ScrollDelta, ScrollHandle, ScrollWheelEvent,
        StatefulInteractiveElement as _, TestAppContext, VisualTestContext, point, px, size,
    };
    use openlogi_core::hid::{FirmwareUpdateEvent, FirmwareUpdateSource};

    struct CardHost;

    struct ActivityHost(FirmwareUpdate);

    fn activity_update() -> FirmwareUpdate {
        let phases = [
            FirmwareUpdatePhase::Downloading,
            FirmwareUpdatePhase::Preparing,
        ]
        .into_iter()
        .chain(
            (0..=90)
                .step_by(10)
                .map(|percent| FirmwareUpdatePhase::Transferring { percent }),
        );
        FirmwareUpdate {
            request: openlogi_core::hid::FirmwareRequest(1),
            source: Some(FirmwareUpdateSource {
                device_name: "MX Master 4".into(),
                firmware_line: "RBM27".into(),
                package_url: "https://firmware.example.test/releases/27.3.19/firmware.zip".into(),
            }),
            route: openlogi_core::hid::DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb042,
            },
            target: FirmwareVersion {
                number: 27,
                revision: 3,
                build: 19,
            },
            id: 42,
            phase: FirmwareUpdatePhase::Transferring { percent: 90 },
            events: phases
                .enumerate()
                .map(|(index, phase)| FirmwareUpdateEvent {
                    timestamp_millis: u64::try_from(index).unwrap() * 1000,
                    phase,
                })
                .collect(),
        }
    }

    #[test]
    fn firmware_activity_includes_device_version_and_requested_package_url() {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let lines = activity_lines(&activity_update());
        assert_eq!(
            lines[0].text,
            "Device: MX Master 4 · RBM27 · firmware 27.3.19"
        );
        assert_eq!(
            lines[1].text,
            "Package URL: https://firmware.example.test/releases/27.3.19/firmware.zip"
        );
        assert_eq!(lines[3].text, "Firmware 27.3.19 downloaded and verified.");
    }

    struct NestedActivityHost {
        update: FirmwareUpdate,
        page: ScrollHandle,
    }

    impl Render for NestedActivityHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
            v_flex()
                .id("activity-test-page")
                .h_full()
                .w_full()
                .overflow_y_scroll()
                .track_scroll(&self.page)
                .child(div().h(px(180.)).flex_shrink_0())
                .child(FirmwareActivityLog {
                    update: self.update.clone(),
                    key: DeviceKey::from("nested-activity-test"),
                    pal: theme::palette(cx),
                })
                .child(div().h(px(400.)).flex_shrink_0())
        }
    }

    #[gpui::test]
    fn firmware_activity_keeps_precise_wheel_scrolling_inside_the_log(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        cx.update(gpui_component::init);
        let page = ScrollHandle::new();
        let (_, cx) = cx.add_window_view(|_, _| NestedActivityHost {
            page: page.clone(),
            update: activity_update(),
        });
        cx.simulate_resize(size(px(360.), px(400.)));
        redraw(cx);
        page.set_offset(point(px(0.), px(-100.)));
        redraw(cx);
        let bounds = cx.debug_bounds("firmware-activity-log").unwrap();
        let before = page.offset();
        cx.simulate_event(ScrollWheelEvent {
            position: bounds.center(),
            delta: ScrollDelta::Pixels(point(px(1.), px(40.))),
            ..Default::default()
        });
        redraw(cx);
        assert_eq!(
            page.offset(),
            before,
            "scrolling upward in the log must not move the page"
        );
        cx.simulate_event(ScrollWheelEvent {
            position: bounds.center(),
            delta: ScrollDelta::Pixels(point(px(-1.), px(-20.))),
            ..Default::default()
        });
        redraw(cx);
        assert_eq!(
            page.offset(),
            before,
            "scrolling downward in the log must not move the page"
        );
    }

    impl Render for ActivityHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
            div().w_full().child(FirmwareActivityLog {
                update: self.0.clone(),
                key: DeviceKey::from("activity-test"),
                pal: theme::palette(cx),
            })
        }
    }

    #[gpui::test]
    fn firmware_activity_scrolls_follows_new_entries_and_keeps_manual_scroll_position(
        cx: &mut TestAppContext,
    ) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        cx.update(gpui_component::init);
        let (host, cx) = cx.add_window_view(|_, _| ActivityHost(activity_update()));
        cx.simulate_resize(size(px(360.), px(400.)));
        redraw(cx);
        let bounds = cx.debug_bounds("firmware-activity-log").unwrap();
        let expected_height = cx.update(|window, _| window.pixel_snap(window.rem_size() * 12.6));
        assert_eq!(bounds.size.height, expected_height);
        assert!(cx.debug_bounds("firmware-log-last-row").unwrap().bottom() <= bounds.bottom());
        cx.simulate_event(ScrollWheelEvent {
            position: bounds.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(100.))),
            ..Default::default()
        });
        redraw(cx);
        let first = cx.debug_bounds("firmware-log-first-row").unwrap().top();
        assert!(
            cx.debug_bounds("firmware-log-last-row")
                .is_none_or(|last| last.bottom() > bounds.bottom()),
            "wheel scrolling exposes earlier entries"
        );
        host.update(cx, |host, cx| {
            host.0.phase = FirmwareUpdatePhase::Transferring { percent: 100 };
            host.0.events.push(FirmwareUpdateEvent {
                timestamp_millis: 21_000,
                phase: host.0.phase.clone(),
            });
            cx.notify();
        });
        redraw(cx);
        assert_eq!(
            cx.debug_bounds("firmware-log-first-row").unwrap().top(),
            first,
            "new entries do not interrupt reading older lines"
        );
        cx.simulate_event(ScrollWheelEvent {
            position: bounds.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(-1000.))),
            ..Default::default()
        });
        redraw(cx);
        host.update(cx, |host, cx| {
            host.0.phase =
                FirmwareUpdatePhase::Failed(FirmwareUpdateFailure::DeviceRefused { code: 0x83 });
            host.0.events.push(FirmwareUpdateEvent {
                timestamp_millis: 22_000,
                phase: host.0.phase.clone(),
            });
            cx.notify();
        });
        redraw(cx);
        let error = cx.debug_bounds("firmware-log-error").unwrap();
        assert!(
            error.bottom() <= bounds.bottom(),
            "following the bottom reveals a new terminal error"
        );
        assert!(error.top() >= bounds.top());
    }

    #[test]
    fn firmware_activity_timestamps_are_fixed_width_utc() {
        assert_eq!(activity_time(0), "00:00:00");
        assert_eq!(activity_time(45_296_789), "12:34:56");
        assert_eq!(activity_time(86_401_000), "00:00:01");
    }

    #[gpui::test]
    fn firmware_activity_follows_when_content_first_grows_past_the_viewport(
        cx: &mut TestAppContext,
    ) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        cx.update(gpui_component::init);
        let mut update = activity_update();
        update.source = None;
        update.events.truncate(1);
        update.phase = FirmwareUpdatePhase::Downloading;
        let (host, cx) = cx.add_window_view(|_, _| ActivityHost(update));
        cx.simulate_resize(size(px(360.), px(400.)));
        redraw(cx);
        let bounds = cx.debug_bounds("firmware-activity-log").unwrap();
        for (index, phase) in [FirmwareUpdatePhase::Preparing]
            .into_iter()
            .chain(
                (0..=100)
                    .step_by(10)
                    .map(|percent| FirmwareUpdatePhase::Transferring { percent }),
            )
            .enumerate()
        {
            host.update(cx, |host, cx| {
                host.0.phase = phase.clone();
                host.0.events.push(FirmwareUpdateEvent {
                    timestamp_millis: u64::try_from(index + 1).unwrap() * 1000,
                    phase,
                });
                cx.notify();
            });
            redraw(cx);
            let last = cx.debug_bounds("firmware-log-last-row").unwrap();
            assert!(last.top() >= bounds.top());
            assert!(
                last.bottom() <= bounds.bottom(),
                "tail following must survive the first overflow and every subsequent append"
            );
        }
    }

    impl Render for CardHost {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> impl gpui::IntoElement {
            div()
                .w_96()
                .children(firmware_card(
                    FirmwareFacts::current(cx),
                    theme::palette(cx),
                    cx,
                ))
                .children(gpui_component::Root::render_dialog_layer(window, cx))
        }
    }

    fn draw(
        cx: &mut TestAppContext,
    ) -> (
        &mut VisualTestContext,
        tokio::sync::mpsc::UnboundedReceiver<Command>,
    ) {
        draw_state(cx, firmware_ready_state())
    }

    fn draw_state(
        cx: &mut TestAppContext,
        (state, received): (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>),
    ) -> (
        &mut VisualTestContext,
        tokio::sync::mpsc::UnboundedReceiver<Command>,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_reduce_motion(true);
            let state = cx.new(|_| state);
            AppState::set_global(state, cx);
        });
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| CardHost);
            gpui_component::Root::new(view, window, cx)
        });
        cx.simulate_resize(size(px(800.), px(600.)));
        redraw(cx);
        (cx, received)
    }

    fn redraw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn update_button(cx: &mut VisualTestContext) {
        let bounds = cx.debug_bounds("firmware-update").expect("update button");
        cx.simulate_click(bounds.center(), Modifiers::none());
        redraw(cx);
    }

    fn set_charge(cx: &mut VisualTestContext, percentage: Option<u8>) {
        cx.update(|_, cx| {
            let mut inventory = crate::state::tests::direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
            inventory.paired[0].battery = Some(openlogi_core::device::BatteryInfo {
                percentage,
                level: openlogi_core::device::BatteryLevel::Good,
                status: openlogi_core::device::BatteryStatus::Discharging,
            });
            AppState::apply(cx, |state| {
                state.refresh_inventories(
                    &[inventory],
                    &[],
                    &crate::services::assets::AssetResolver::new(),
                    &[],
                )
            });
        });
        redraw(cx);
    }

    #[gpui::test]
    fn firmware_confirmation_blocks_low_charge_for_pointer_and_keyboard(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let (cx, mut received) = draw_state(cx, firmware_current_state());
        set_charge(cx, Some(29));
        update_button(cx);
        assert!(cx.debug_bounds("firmware-battery-warning").is_some());
        let buttons = cx.debug_bounds("firmware-confirm-install").unwrap();
        let warning = cx.debug_bounds("firmware-battery-warning").unwrap();
        assert!(
            warning.bottom() <= buttons.top(),
            "warning is above the buttons"
        );
        let install = cx.debug_bounds("firmware-confirm-install").unwrap();
        cx.simulate_click(install.center(), Modifiers::none());
        redraw(cx);
        assert!(received.try_recv().is_err());
        cx.simulate_keystrokes("enter");
        redraw(cx);
        assert!(received.try_recv().is_err());
        assert!(cx.debug_bounds("firmware-confirmation").is_some());
        cx.simulate_keystrokes("escape");
        redraw(cx);
        assert!(cx.debug_bounds("firmware-confirmation").is_none());
    }

    #[gpui::test]
    fn firmware_confirmation_tracks_charge_changes_and_allows_exactly_thirty_percent(
        cx: &mut TestAppContext,
    ) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let (cx, mut received) = draw(cx);
        set_charge(cx, Some(30));
        update_button(cx);
        assert!(cx.debug_bounds("firmware-battery-warning").is_none());

        set_charge(cx, Some(29));
        assert!(cx.debug_bounds("firmware-battery-warning").is_some());
        cx.simulate_keystrokes("enter");
        redraw(cx);
        assert!(
            received.try_recv().is_err(),
            "a drop while open blocks submission"
        );

        set_charge(cx, None);
        assert!(cx.debug_bounds("firmware-battery-warning").is_some());
        cx.simulate_keystrokes("enter");
        redraw(cx);
        assert!(
            received.try_recv().is_err(),
            "missing charge is not sufficient charge"
        );

        set_charge(cx, Some(30));
        assert!(cx.debug_bounds("firmware-battery-warning").is_none());
        let install = cx.debug_bounds("firmware-confirm-install").unwrap();
        cx.simulate_click(install.center(), Modifiers::none());
        redraw(cx);
        assert!(matches!(
            received.try_recv().unwrap(),
            Command::StartFirmwareUpdate(_)
        ));
        assert!(received.try_recv().is_err());
        assert!(cx.debug_bounds("firmware-confirmation").is_none());
    }

    #[gpui::test]
    fn firmware_confirmation_cancels_with_escape_and_enter_submits_once(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let (cx, mut received) = draw(cx);
        assert!(
            cx.debug_bounds("firmware-current-check").is_none(),
            "an older version does not claim to be current"
        );
        update_button(cx);
        assert!(cx.debug_bounds("firmware-confirmation").is_some());
        assert!(
            received.try_recv().is_err(),
            "opening confirmation does not start a flash"
        );
        cx.simulate_keystrokes("escape");
        redraw(cx);
        assert!(cx.debug_bounds("firmware-confirmation").is_none());
        assert!(received.try_recv().is_err());
        update_button(cx);
        cx.simulate_keystrokes("enter");
        redraw(cx);
        assert!(cx.debug_bounds("firmware-confirmation").is_none());
        assert!(matches!(
            received.try_recv().unwrap(),
            Command::StartFirmwareUpdate(_)
        ));
        assert!(received.try_recv().is_err());
        assert!(
            cx.debug_bounds("firmware-progress").is_some(),
            "admission stays visibly pending until observed"
        );
    }

    #[gpui::test]
    fn current_firmware_shows_check_and_reinstall_requires_explicit_confirmation(
        cx: &mut TestAppContext,
    ) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let (cx, mut received) = draw_state(cx, firmware_current_state());
        assert!(cx.debug_bounds("firmware-current-check").is_some());
        assert!(cx.debug_bounds("firmware-offer-status").is_some());
        assert_eq!(tr!("firmware.up_to_date"), "Firmware is up to date");
        update_button(cx);
        assert!(cx.debug_bounds("firmware-already-latest").is_some());
        assert!(
            received.try_recv().is_err(),
            "the latest-firmware notice itself does not start a flash"
        );
        cx.simulate_keystrokes("escape");
        redraw(cx);
        assert!(cx.debug_bounds("firmware-confirmation").is_none());
        assert!(received.try_recv().is_err());
        update_button(cx);
        cx.simulate_keystrokes("enter");
        redraw(cx);
        assert!(matches!(
            received.try_recv().unwrap(),
            Command::StartFirmwareUpdate(_)
        ));
        assert!(received.try_recv().is_err());
        assert!(cx.debug_bounds("firmware-progress").is_some());
        assert!(
            cx.debug_bounds("firmware-update").is_none(),
            "an admitted reinstall cannot be submitted twice"
        );
    }

    #[gpui::test]
    fn an_offline_current_device_cannot_open_reinstallation(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let (cx, mut received) = draw_state(cx, firmware_current_state());
        cx.update(|_, cx| {
            let mut inventory = crate::state::tests::direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
            inventory.paired[0].online = false;
            AppState::apply(cx, |state| {
                state.refresh_inventories(
                    &[inventory],
                    &[],
                    &crate::services::assets::AssetResolver::new(),
                    &[],
                )
            });
        });
        redraw(cx);
        assert!(cx.debug_bounds("firmware-current-check").is_some());
        update_button(cx);
        assert!(cx.debug_bounds("firmware-confirmation").is_none());
        assert!(received.try_recv().is_err());
    }

    #[gpui::test]
    fn an_offline_device_keeps_the_version_but_cannot_open_installation(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let (cx, mut received) = draw(cx);
        cx.update(|_, cx| {
            let mut inventory = crate::state::tests::direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
            inventory.paired[0].online = false;
            AppState::apply(cx, |state| {
                state.refresh_inventories(
                    &[inventory],
                    &[],
                    &crate::services::assets::AssetResolver::new(),
                    &[],
                )
            });
        });
        redraw(cx);
        assert!(cx.debug_bounds("firmware-card").is_some());
        update_button(cx);
        assert!(cx.debug_bounds("firmware-confirmation").is_none());
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn failures_do_not_claim_that_the_old_firmware_is_running() {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        assert_eq!(
            failure_text(&FirmwareUpdateFailure::NotActivated),
            "Couldn't confirm the new firmware after restart. Reconnect to check its firmware version."
        );
        assert_eq!(
            start_failure_text(&FirmwareStartFailure::Refused(FirmwareUpdateRefusal::Busy)),
            "Another firmware update is in progress."
        );
    }
}
