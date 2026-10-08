//! Options+ file selection and explicit device mapping. Reads run off the UI
//! thread; cancellation invalidates both the task and its publication identity.

use openlogi_core::{
    config::ConfigError,
    optionsplus::{OptionsSettings, read_database},
};

use super::*;

pub(super) struct DeviceSelection {
    path: PathBuf,
    sources: Vec<String>,
    targets: Vec<(String, String)>,
    selected: Option<String>,
}

impl RecoveryView {
    pub(super) fn options_entry(cx: &mut Context<Self>) -> Div {
        v_flex()
            .gap_3()
            .pb_5()
            .border_b_1()
            .border_color(theme::palette(cx).border)
            .child(section_heading(
                "options-import",
                tr!("options_import.title"),
                tr!("options_import.description"),
                cx,
            ))
            .child(FocusScroll::new(
                "options-file-actions-focus",
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        control_button("read-options-settings")
                            .primary()
                            .label(tr!("options_import.installed"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.load_installed_options(cx);
                            })),
                    )
                    .child(
                        control_button("choose-options-settings")
                            .label(tr!("options_import.choose_file"))
                            .on_click(cx.listener(|_, _, _, cx| Self::browse_options(cx))),
                    ),
            ))
    }

    fn load_installed_options(&mut self, cx: &mut Context<Self>) {
        match openlogi_core::paths::home_dir() {
            Ok(home) => self.load_options(
                home.join("Library/Application Support/LogiOptionsPlus/settings.db"),
                cx,
            ),
            Err(error) => {
                self.details_open = false;
                self.error = Some(RecoveryFailure::Preview(error.to_string()));
                cx.notify();
            }
        }
    }

    fn browse_options(cx: &mut Context<Self>) {
        let result = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr!("options_import.choose_file")),
        });
        cx.spawn(async move |this, cx| {
            let selected = result.await;
            let _ = this.update(cx, |this, cx| match selected {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.load_options(path, cx);
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

    pub(super) fn load_options(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let target = AppState::global(cx).read(cx).recovery_path();
        self.run_options(cx, async move {
            let target = target.map_err(|error| RecoveryFailure::Target(error.to_string()))?;
            let settings =
                OptionsSettings::parse(&read_database(&path).map_err(ConfigError::from)?)
                    .map_err(ConfigError::from)?;
            let config = RecoveryPlan::read_import_target(&target)
                .map_err(|error| RecoveryFailure::Target(error.to_string()))?;
            let sources = settings.devices().map(str::to_owned).collect();
            let targets = config
                .devices
                .iter()
                .filter(|(_, device)| openlogi_core::optionsplus::can_import_into(device))
                .map(|(key, device)| {
                    let label = device
                        .custom_name
                        .as_ref()
                        .or_else(|| {
                            device
                                .identity
                                .as_ref()
                                .map(|identity| &identity.display_name)
                        })
                        .unwrap_or(key);
                    (key.clone(), format!("{label} · {key}"))
                })
                .collect();
            Ok(RecoveryStage::Options(DeviceSelection {
                path,
                sources,
                targets,
                selected: None,
            }))
        });
    }

    fn preview_options(&mut self, target_device: String, cx: &mut Context<Self>) {
        let RecoveryStage::Options(selection) = &self.stage else {
            return;
        };
        let Some(source_device) = selection.selected.clone() else {
            return;
        };
        let source = selection.path.clone();
        let target = AppState::global(cx).read(cx).recovery_path();
        self.run_options(cx, async move {
            Ok(RecoveryStage::Preview(
                RecoveryPlan::prepare_options(
                    &target.map_err(|error| RecoveryFailure::Target(error.to_string()))?,
                    &source,
                    &source_device,
                    &target_device,
                )
                .map_err(|error| import_failure(&error))?,
            ))
        });
    }

    pub(super) fn run_options(
        &mut self,
        cx: &mut Context<Self>,
        work: impl std::future::Future<Output = Result<RecoveryStage, RecoveryFailure>> + Send + 'static,
    ) {
        self.run_work(cx, work, |this, stage| this.stage = stage);
    }

    pub(super) fn options_cancel(cx: &mut Context<Self>) -> impl IntoElement {
        FocusScroll::new(
            "options-cancel-focus",
            control_button("cancel-options-import")
                .label(tr!("common.cancel"))
                .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
        )
    }

    pub(super) fn options_devices(selection: &DeviceSelection, cx: &mut Context<Self>) -> Div {
        let mut body = v_flex().gap_3().child(section_heading(
            "options-match",
            tr!("options_import.match_devices"),
            tr!("options_import.select_source"),
            cx,
        ));
        if selection.sources.is_empty() {
            body = body.child(text("options-no-source", tr!("options_import.no_source")));
        }
        for source in &selection.sources {
            let key = source.clone();
            body = body.child(FocusScroll::new(
                SharedString::from(format!("options-source-focus-{source}")),
                div()
                    .debug_selector(move || format!("options-source-{key}"))
                    .child(
                        control_button(SharedString::from(format!("source-{source}")))
                            .label(SharedString::from(source.clone()))
                            .selected(selection.selected.as_ref() == Some(source))
                            .toggled(selection.selected.as_ref() == Some(source))
                            .on_click(cx.listener({
                                let source = source.clone();
                                move |this, _, _, cx| {
                                    if let RecoveryStage::Options(selection) = &mut this.stage {
                                        selection.selected = Some(source.clone());
                                    }
                                    cx.notify();
                                }
                            })),
                    ),
            ));
        }
        if selection.selected.is_some() {
            body = body.child(alert(
                "options-device-match",
                None,
                tr!("options_import.match_warning"),
                AlertVariant::Info,
            ));
            body = body.child(
                text("options-select-target", tr!("options_import.select_target"))
                    .pt_3()
                    .text_body(),
            );
            if selection.targets.is_empty() {
                body = body.child(text("options-no-target", tr!("options_import.no_target")));
            }
            for (target, label) in &selection.targets {
                let key = target.clone();
                body = body.child(FocusScroll::new(
                    SharedString::from(format!("options-target-focus-{target}")),
                    div()
                        .debug_selector(move || format!("options-target-{key}"))
                        .child(
                            control_button(SharedString::from(format!("target-{target}")))
                                .label(SharedString::from(label.clone()))
                                .on_click(cx.listener({
                                    let target = target.clone();
                                    move |this, _, _, cx| this.preview_options(target.clone(), cx)
                                })),
                        ),
                ));
            }
        }
        body.child(Self::options_cancel(cx))
    }
}

// ImportError identifies the source/conversion layer; config errors refer to the
// existing OpenLogi target. Keep that distinction out of string matching.
fn import_failure(error: &ConfigError) -> RecoveryFailure {
    match error {
        ConfigError::Import(_) => RecoveryFailure::Preview(error.to_string()),
        _ => RecoveryFailure::Target(error.to_string()),
    }
}
