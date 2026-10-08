//! Explicit backup selection, preview, and confirmation. Disk work happens in
//! user actions, never in render, and all replacement goes through AppState.

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::{Div, InteractiveElement as _, PathPromptOptions, StatefulInteractiveElement as _};
use gpui_component::{
    Selectable as _, alert::AlertVariant, collapsible::Collapsible, pagination::Pagination,
};
use openlogi_core::config::{ConfigError, RecoveryPlan, recovery_backups};

use super::{
    App, AppState, ButtonVariants, Context, Disableable, Entity, IconName, IntoElement,
    ParentElement, Render, SettingGroup, SettingItem, SettingPage, SharedString, Styled, Window,
    div, h_flex, v_flex,
};
use crate::ui::{
    components::control_button,
    theme::{self, Typography as _},
};

#[cfg(target_os = "macos")]
mod options;

const PREVIEW_PAGE_SIZE: usize = 8;

mod accessibility;
mod presentation;

use accessibility::{FocusScroll, alert, text};

enum RecoveryStage {
    Choose,
    Preview(RecoveryPlan),
    Confirm(RecoveryPlan),
    Loading,
    #[cfg(target_os = "macos")]
    Options(options::DeviceSelection),
}

enum RecoveryFailure {
    Preview(String),
    #[cfg(target_os = "macos")]
    Target(String),
    Restore(String),
}

impl From<ConfigError> for RecoveryFailure {
    fn from(error: ConfigError) -> Self {
        Self::Preview(error.to_string())
    }
}

pub(super) struct RecoveryView {
    backups: Vec<PathBuf>,
    stage: RecoveryStage,
    error: Option<RecoveryFailure>,
    details_open: bool,
    notice_page: usize,
    change_page: usize,
    request: u64,
    task: Option<gpui::Task<()>>,
}

impl RecoveryView {
    pub(super) fn new(cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            backups: Vec::new(),
            stage: RecoveryStage::Choose,
            error: None,
            details_open: false,
            notice_page: 0,
            change_page: 0,
            request: 0,
            task: None,
        };
        view.refresh(cx);
        view
    }

    fn run_work<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl std::future::Future<Output = Result<T, RecoveryFailure>> + Send + 'static,
        publish: impl FnOnce(&mut Self, T) + 'static,
    ) {
        self.notice_page = 0;
        self.change_page = 0;
        self.request += 1;
        let request = self.request;
        self.stage = RecoveryStage::Loading;
        self.error = None;
        self.details_open = false;
        let work = cx.background_executor().spawn(work);
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.request != request {
                    return;
                }
                match result {
                    Ok(result) => publish(this, result),
                    Err(error) => {
                        this.stage = RecoveryStage::Choose;
                        this.error = Some(error);
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let target = AppState::global(cx).read(cx).recovery_path();
        self.run_work(
            cx,
            async move {
                target
                    .and_then(|path| recovery_backups(&path))
                    .map_err(RecoveryFailure::from)
            },
            |this, backups| {
                this.backups = backups;
                this.stage = RecoveryStage::Choose;
            },
        );
    }

    fn preview(&mut self, source: &std::path::Path, cx: &mut Context<Self>) {
        let target = AppState::global(cx).read(cx).recovery_path();
        let source = source.to_path_buf();
        self.run_work(
            cx,
            async move {
                target
                    .and_then(|target| RecoveryPlan::prepare(&target, &source))
                    .map_err(RecoveryFailure::from)
            },
            |this, plan| this.stage = RecoveryStage::Preview(plan),
        );
    }

    fn browse(cx: &mut Context<Self>) {
        let result = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr!("recovery.choose_file")),
        });
        cx.spawn(async move |this, cx| {
            let selected = result.await;
            let _ = this.update(cx, |this, cx| match selected {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.first() {
                        this.preview(path, cx);
                    }
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    this.details_open = false;
                    this.error = Some(RecoveryFailure::Preview(error.to_string()));
                    cx.notify();
                }
                Err(error) => {
                    this.details_open = false;
                    this.error = Some(RecoveryFailure::Preview(error.to_string()));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn continue_to_confirmation(&mut self, cx: &mut Context<Self>) {
        if let RecoveryStage::Preview(plan) =
            std::mem::replace(&mut self.stage, RecoveryStage::Choose)
        {
            self.stage = RecoveryStage::Confirm(plan);
        }
        cx.notify();
    }

    fn restore(&mut self, cx: &mut Context<Self>) {
        let RecoveryStage::Confirm(plan) =
            std::mem::replace(&mut self.stage, RecoveryStage::Choose)
        else {
            return;
        };
        let state = AppState::global(cx);
        if let Err(error) = state.update(cx, |state, cx| {
            let result = state.begin_config_recovery(&plan);
            crate::state::StateEvents::from(crate::state::StateEvent::SettingsChanged).emit(cx);
            result
        }) {
            self.error = Some(RecoveryFailure::Restore(error.to_string()));
            cx.notify();
            return;
        }
        self.request += 1;
        self.task = None;
        self.details_open = false;
        self.stage = RecoveryStage::Loading;
        let (finished, completion) = tokio::sync::oneshot::channel();
        let mut completion = Some(completion);
        // Register on App, not this view: the last window may already be gone.
        // GPUI waits up to SHUTDOWN_TIMEOUT for the writer during graceful quit;
        // the atomic writer and recovery copy protect an interrupted shutdown.
        let quit_waiter = App::on_app_quit(cx, move |_| {
            let completion = completion.take();
            async move {
                if let Some(completion) = completion {
                    let _ = completion.await;
                }
            }
        });
        let work = cx.background_executor().spawn(async move {
            let result = plan.apply();
            let _ = finished.send(());
            result
        });
        // Completion and its quit observer outlive the settings view. The quit
        // wait depends only on disk work, never foreground entity updates.
        cx.spawn(async move |this, cx| {
            let result = work.await;
            drop(quit_waiter);
            state.update(cx, |state, cx| {
                state.finish_config_recovery(&result).emit(cx);
            });
            let _ = this.update(cx, |this, cx| {
                this.stage = RecoveryStage::Choose;
                this.error = result
                    .err()
                    .map(|error| RecoveryFailure::Restore(error.to_string()));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn chooser(&self, cx: &mut Context<Self>) -> Div {
        let pal = theme::palette(cx);
        let mut backups = v_flex().gap_2().w_full();
        if self.backups.is_empty() {
            backups = backups.child(
                text("recovery-no-backups", tr!("recovery.no_backups"))
                    .text_body()
                    .text_color(pal.text_muted),
            );
        }
        for (index, path) in self.backups.iter().enumerate() {
            let source = path.clone();
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            backups = backups.child(FocusScroll::new(
                SharedString::from(format!("backup-focus-{}", path.display())),
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .py_2()
                    .border_b_1()
                    .border_color(pal.border)
                    .child(
                        text(
                            SharedString::from(format!("backup-name-{}", path.display())),
                            name.clone(),
                        )
                        .flex_1()
                        .min_w_0()
                        .text_body(),
                    )
                    .child(
                        div()
                            .debug_selector(move || format!("recovery-backup-{index}"))
                            .child(
                                control_button(SharedString::from(format!(
                                    "preview-{}",
                                    path.display()
                                )))
                                .ghost()
                                .label(tr!("recovery.preview"))
                                .accessibility_label(format!("{}: {name}", tr!("recovery.preview")))
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.preview(&source, cx)),
                                ),
                            ),
                    ),
            ));
        }
        let recovery = v_flex()
            .gap_3()
            .child(section_heading(
                "recovery-backups",
                tr!("recovery.backups_title"),
                tr!("recovery.backups_description"),
                cx,
            ))
            .child(FocusScroll::new(
                "recovery-file-actions-focus",
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        control_button("choose-config-file")
                            .label(tr!("recovery.choose_file"))
                            .on_click(cx.listener(|_, _, _, cx| Self::browse(cx))),
                    )
                    .child(
                        control_button("refresh-backups")
                            .label(tr!("common.refresh"))
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
            ))
            .child(backups);
        let content = v_flex().gap_6().w_full();
        #[cfg(target_os = "macos")]
        let content = content.child(Self::options_entry(cx));
        content.child(recovery)
    }

    fn preview_change(
        &self,
        plan: &RecoveryPlan,
        change: &openlogi_core::config::ConfigChange,
        index: usize,
        cx: &App,
    ) -> impl IntoElement {
        let pal = theme::palette(cx);
        let mut before = presentation::value(change.before.as_ref(), &change.keys);
        let mut after = presentation::value(change.after.as_ref(), &change.keys);
        if before == after && change.before != change.after {
            before = change.before.as_ref().map_or(before, ToString::to_string);
            after = change.after.as_ref().map_or(after, ToString::to_string);
        }
        let device_context = presentation::context(plan, change);
        let title = presentation::title(change);
        let before = tr!("recovery.before", value => before);
        let after = if plan.is_options_import() {
            tr!("options_import.after", value => after)
        } else {
            tr!("recovery.after", value => after)
        };
        let row = v_flex()
            .debug_selector(move || format!("recovery-change-{index}"))
            .id(SharedString::from(change.path.clone()))
            .role(gpui::accesskit::Role::Group)
            .aria_label(format!("{device_context}. {title}. {before}. {after}"))
            .gap_1()
            .p_3()
            .rounded(pal.control_radius)
            .border_1()
            .border_color(pal.border)
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(device_context),
            )
            .child(div().text_subheading().child(title));
        let row = row
            .child(div().text_body().text_color(pal.text_muted).child(before))
            .child(div().text_body().child(after));
        row.when(self.details_open, |row| {
            row.child(
                text(
                    SharedString::from(format!("{}-details", change.path)),
                    format!(
                        "{}\n{} → {}",
                        change.path,
                        change
                            .before
                            .as_ref()
                            .map_or_else(|| "∅".into(), ToString::to_string),
                        change
                            .after
                            .as_ref()
                            .map_or_else(|| "∅".into(), ToString::to_string)
                    ),
                )
                .text_caption()
                .text_color(pal.text_muted),
            )
        })
    }

    fn preview_changes(&self, plan: &RecoveryPlan, cx: &mut Context<Self>) -> Div {
        let mut content = v_flex().gap_3().child(
            text(
                "recovery-change-count",
                tr!("options_import.changes", count => plan.changes().len()),
            )
            .text_subheading(),
        );
        for (index, change) in plan
            .changes()
            .iter()
            .enumerate()
            .skip(self.change_page * PREVIEW_PAGE_SIZE)
            .take(PREVIEW_PAGE_SIZE)
        {
            content = content.child(self.preview_change(plan, change, index, cx));
        }
        if plan.changes().len() > PREVIEW_PAGE_SIZE {
            content = content.child(FocusScroll::new(
                "recovery-change-page-focus",
                h_flex().child(
                    div()
                        .debug_selector(|| "recovery-change-pages".into())
                        .child(
                            Pagination::new("recovery-change-pages")
                                .current_page(self.change_page + 1)
                                .total_pages(plan.changes().len().div_ceil(PREVIEW_PAGE_SIZE))
                                .on_click(cx.listener(|this, page, _, cx| {
                                    this.change_page = page - 1;
                                    cx.notify();
                                })),
                        ),
                ),
            ));
        }
        content.child(FocusScroll::new(
            "recovery-details-focus",
            control_button("toggle-change-details")
                .ghost()
                .label(tr!("recovery.technical_details"))
                .selected(self.details_open)
                .toggled(self.details_open)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.details_open = !this.details_open;
                    cx.notify();
                })),
        ))
    }

    fn import_notices(&self, plan: &RecoveryPlan, cx: &mut Context<Self>) -> Div {
        let mut content = v_flex()
            .gap_2()
            .child(
                text(
                    "recovery-notice-count",
                    tr!("options_import.notes", count => plan.notices().len()),
                )
                .text_subheading(),
            )
            .child(
                text(
                    "recovery-profile-policy",
                    tr!("options_import.profile_policy"),
                )
                .text_body()
                .text_color(theme::palette(cx).text_muted),
            );
        for (index, notice) in plan
            .notices()
            .iter()
            .enumerate()
            .skip(self.notice_page * PREVIEW_PAGE_SIZE)
            .take(PREVIEW_PAGE_SIZE)
        {
            content = content.child(
                v_flex()
                    .debug_selector(move || format!("recovery-note-{index}"))
                    .id(SharedString::from(format!("notice-{index}")))
                    .role(gpui::accesskit::Role::Group)
                    .aria_label(format!(
                        "{}: {}. {}",
                        notice.profile(),
                        notice.slot(),
                        notice_text(notice.kind())
                    ))
                    .gap_1()
                    .py_2()
                    .child(div().text_body().child(notice_text(notice.kind())))
                    .child(
                        div()
                            .text_caption()
                            .text_color(theme::palette(cx).text_muted)
                            .child(format!("{} · {}", notice.profile(), notice.slot())),
                    ),
            );
        }
        if plan.notices().len() > PREVIEW_PAGE_SIZE {
            content = content.child(FocusScroll::new(
                "recovery-notice-page-focus",
                h_flex().child(
                    div()
                        .debug_selector(|| "recovery-notice-pages".into())
                        .child(
                            Pagination::new("recovery-notice-pages")
                                .current_page(self.notice_page + 1)
                                .total_pages(plan.notices().len().div_ceil(PREVIEW_PAGE_SIZE))
                                .on_click(cx.listener(|this, page, _, cx| {
                                    this.notice_page = page - 1;
                                    cx.notify();
                                })),
                        ),
                ),
            ));
        }
        content
    }

    fn technical_details(&self, message: &str, cx: &mut Context<Self>) -> Div {
        div().child(
            Collapsible::new()
                .open(self.details_open)
                .child(FocusScroll::new(
                    "recovery-error-focus",
                    h_flex().child(
                        div()
                            .debug_selector(|| "recovery-details-toggle".into())
                            .child(
                                control_button("toggle-recovery-details")
                                    .ghost()
                                    .icon(if self.details_open {
                                        IconName::ChevronUp
                                    } else {
                                        IconName::ChevronDown
                                    })
                                    .label(tr!("recovery.technical_details"))
                                    .toggled(self.details_open)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.details_open = !this.details_open;
                                        cx.notify();
                                    })),
                            ),
                    ),
                ))
                .content(
                    text("recovery-error-message", message.to_owned())
                        .debug_selector(|| "recovery-error-details".into())
                        .p_3()
                        .text_caption()
                        .text_color(theme::palette(cx).text_muted),
                ),
        )
    }

    fn import_pair(plan: &RecoveryPlan, source: &str, target: &str, cx: &App) -> Div {
        let pal = theme::palette(cx);
        v_flex()
            .gap_1()
            .p_3()
            .border_1()
            .border_color(pal.border)
            .rounded(pal.control_radius)
            .child(
                text("import-target-name", plan.device_name(target).to_owned()).text_subheading(),
            )
            .child(
                text(
                    "import-device-pair",
                    tr!("options_import.device_pair", source => source, target => target),
                )
                .text_body(),
            )
            .child(
                text("import-match-warning", tr!("options_import.match_warning"))
                    .text_caption()
                    .text_color(pal.text_muted),
            )
    }

    fn preview_body(&self, plan: &RecoveryPlan, cx: &mut Context<Self>) -> Div {
        let pal = theme::palette(cx);
        let description = if plan.is_options_import() {
            tr!("options_import.preview_description")
        } else {
            tr!("recovery.preview_description")
        };
        let source_name = plan
            .source()
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let mut content = v_flex()
            .gap_4()
            .w_full()
            .child(section_heading(
                "recovery-review",
                tr!("recovery.review_title"),
                description,
                cx,
            ))
            .child(
                text(
                    "recovery-source",
                    tr!("recovery.source", name => source_name),
                )
                .text_caption()
                .text_color(pal.text_muted),
            );
        if let Some((source, target)) = plan.import_devices() {
            content = content.child(Self::import_pair(plan, source, target, cx));
        }
        if let Some(error) = plan.current_error() {
            content = content
                .child(alert(
                    "recovery-current-invalid",
                    None,
                    tr!("recovery.current_invalid"),
                    AlertVariant::Warning,
                ))
                .child(self.technical_details(error, cx));
        }
        if plan.changes().is_empty() {
            content = content.child(text("recovery-no-changes", tr!("recovery.no_changes")));
        }
        content = content.child(self.preview_changes(plan, cx));
        if plan.is_options_import() {
            content = content.child(self.import_notices(plan, cx));
        }
        content.child(self.preview_footer(plan, cx))
    }

    fn preview_footer(&self, plan: &RecoveryPlan, cx: &mut Context<Self>) -> FocusScroll {
        let confirming = matches!(self.stage, RecoveryStage::Confirm(_));
        FocusScroll::new(
            "recovery-footer-focus",
            v_flex()
                .gap_3()
                .when(confirming, |footer| {
                    footer.child(alert(
                        "recovery-confirmation",
                        None,
                        if plan.is_options_import() {
                            tr!("options_import.confirm_description")
                        } else {
                            tr!("recovery.confirm_description")
                        },
                        AlertVariant::Info,
                    ))
                })
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            div().debug_selector(|| "recovery-cancel".into()).child(
                                control_button("cancel-recovery")
                                    .label(tr!("common.cancel"))
                                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                            ),
                        )
                        .child(div().debug_selector(|| "recovery-advance".into()).child(
                            if confirming {
                                control_button("confirm-recovery")
                                    .primary()
                                    .label(if plan.is_options_import() {
                                        tr!("options_import.import")
                                    } else {
                                        tr!("recovery.restore")
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| this.restore(cx)))
                            } else {
                                control_button("continue-recovery")
                                    .primary()
                                    .label(tr!("recovery.continue"))
                                    .disabled(
                                        plan.changes().is_empty() && plan.current_error().is_none(),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.continue_to_confirmation(cx);
                                    }))
                            },
                        )),
                ),
        )
        .reveal(confirming)
    }
}

impl Render for RecoveryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut body = match &self.stage {
            RecoveryStage::Choose => self.chooser(cx),
            RecoveryStage::Preview(plan) | RecoveryStage::Confirm(plan) => {
                self.preview_body(plan, cx)
            }
            RecoveryStage::Loading => loading_body().child(
                control_button("cancel-recovery-read")
                    .label(tr!("common.cancel"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.request += 1;
                        this.task = None;
                        this.stage = RecoveryStage::Choose;
                        cx.notify();
                    })),
            ),
            #[cfg(target_os = "macos")]
            RecoveryStage::Options(selection) => Self::options_devices(selection, cx),
        };
        if let Some(error) = &self.error {
            let (title, caption, message) = match error {
                RecoveryFailure::Preview(message) => (
                    tr!("recovery.preview_failed_title"),
                    tr!("recovery.preview_failed"),
                    message,
                ),
                #[cfg(target_os = "macos")]
                RecoveryFailure::Target(message) => (
                    tr!("options_import.target_failed_title"),
                    tr!("options_import.target_failed"),
                    message,
                ),
                RecoveryFailure::Restore(message) => (
                    tr!("recovery.failed_title"),
                    tr!("recovery.failed"),
                    message,
                ),
            };
            body = v_flex()
                .gap_5()
                .w_full()
                .child(
                    v_flex()
                        .gap_2()
                        .child(alert(
                            "recovery-error",
                            Some(title),
                            caption,
                            AlertVariant::Error,
                        ))
                        .child(self.technical_details(message, cx)),
                )
                .child(body);
        }
        body.w_full()
            .min_w_0()
            .text_body()
            .text_color(theme::palette(cx).text_primary)
    }
}

fn notice_text(kind: openlogi_core::optionsplus::NoticeKind) -> SharedString {
    use openlogi_core::optionsplus::NoticeKind;
    match kind {
        NoticeKind::UnsupportedAction => tr!("options_import.unsupported_action"),
        NoticeKind::Wheel => tr!("options_import.wheel"),
        NoticeKind::DeviceSetting => tr!("options_import.device_setting"),
        NoticeKind::VirtualDevice => tr!("options_import.virtual_device"),
        NoticeKind::Application => tr!("options_import.application"),
        NoticeKind::AppGesture => tr!("options_import.app_gesture"),
        NoticeKind::GestureTiming => tr!("options_import.gesture_timing"),
        NoticeKind::Navigation => tr!("options_import.navigation"),
        NoticeKind::InactiveProfile => tr!("options_import.inactive_profile"),
    }
}

pub(super) fn recovery_page(view: Entity<RecoveryView>) -> SettingPage {
    SettingPage::new(tr!("recovery.title"))
        .icon(IconName::HardDrive)
        .resettable(false)
        .group(
            SettingGroup::new().item(
                SettingItem::render(move |_, _, _| view.clone())
                    .keywords([tr!("recovery.title"), tr!("recovery.description")]),
            ),
        )
}

fn section_heading(
    id: &'static str,
    title: SharedString,
    description: SharedString,
    cx: &App,
) -> Div {
    v_flex()
        .gap_1()
        .child(text(id, title).text_subheading())
        .child(
            text(SharedString::from(format!("{id}-description")), description)
                .text_body()
                .text_color(theme::palette(cx).text_muted),
        )
}

pub(crate) fn loading_body() -> Div {
    v_flex()
        .p_4()
        .gap_3()
        .child(text("recovery-working", tr!("recovery.working")))
}

/// Shared completion frame: no window can keep editing the pre-recovery state.
pub(crate) fn restored_body(cx: &App) -> Div {
    let pal = theme::palette(cx);
    v_flex()
        .debug_selector(|| "recovery-complete".into())
        .size_full()
        .p_8()
        .gap_4()
        .items_center()
        .justify_center()
        .bg(pal.page)
        .text_color(pal.text_primary)
        .child(text("recovery-restored", tr!("recovery.restored")).text_title())
        .child(
            text(
                "recovery-restart-description",
                tr!("recovery.restart_description"),
            )
            .text_body()
            .text_center(),
        )
        .child(
            control_button("restart-after-recovery")
                .primary()
                .label(tr!("agent.relaunch_openlogi"))
                .on_click(|_, _, cx| cx.restart()),
        )
}

#[cfg(test)]
mod tests;
