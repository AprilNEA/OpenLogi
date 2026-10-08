//! Capability controls for devices without a specialized model editor.

use std::{collections::BTreeMap, rc::Rc};

use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div,
};
use gpui_component::{
    Disableable as _,
    button::{Button, ButtonVariants as _},
    input::InputState,
    switch::Switch,
    v_flex,
};
use openlogi_core::{
    binding::{Action, KeyCombo},
    peripheral::{
        ApplicationStatus, Capability, CapabilityId, CapabilityRecord, ConnectionStatus, ControlId,
        InputRemapCapability, OperationStatus, PeripheralConfig, PeripheralError, PeripheralRecord,
        ScopeKind, SessionId, SettingAccess, SettingField, SettingType, SettingValue, TargetKind,
        Trigger, VerificationStatus,
    },
};

use crate::{
    features::binding_editor,
    services::ipc::PeripheralOperation,
    state::{AppState, StateEvent},
    ui::{
        components::{control_button, control_input},
        theme::{self, Typography as _},
    },
};

pub struct PeripheralPanel {
    editor: Option<Editor>,
    error: Option<String>,
    _state: Subscription,
}

#[derive(Clone)]
enum EditTarget {
    Binding(ControlId),
    Field(String, SettingType),
}

struct Editor {
    session: SessionId,
    capability: CapabilityId,
    target: EditTarget,
    input: Entity<InputState>,
    _input: Subscription,
}

pub(crate) fn localized_label(labels: &BTreeMap<String, String>) -> String {
    let locale = rust_i18n::locale();
    labels
        .get::<str>(&locale)
        .or_else(|| labels.get("en"))
        .cloned()
        .unwrap_or_default()
}

pub(crate) fn peripheral_error(error: &PeripheralError) -> SharedString {
    match error {
        PeripheralError::Offline => tr!("peripheral.offline"),
        PeripheralError::WriteFailed(_) | PeripheralError::ReadbackMismatch => {
            tr!("peripheral.write_failed", detail => error.to_string())
        }
        PeripheralError::ExternalModification => tr!("peripheral.external_change"),
        PeripheralError::MappingScopeConflict => tr!("peripheral.scope_conflict"),
        PeripheralError::Suspended => tr!("peripheral.suspended"),
        _ => error.to_string().into(),
    }
}

pub(crate) fn operation_label(status: &OperationStatus) -> SharedString {
    match &status.application {
        ApplicationStatus::Disabled => tr!("peripheral.disabled"),
        ApplicationStatus::Pending => tr!("peripheral.pending"),
        ApplicationStatus::Applied
            if status.verification == VerificationStatus::WaitingForPress =>
        {
            tr!("peripheral.waiting_for_press")
        }
        ApplicationStatus::Applied => tr!("peripheral.applied"),
        ApplicationStatus::RestorePending => tr!("peripheral.restore_pending"),
        ApplicationStatus::Restored => tr!("peripheral.restored"),
        ApplicationStatus::Failed(error) => peripheral_error(error),
    }
}

impl PeripheralPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            editor: None,
            error: None,
            _state: AppState::observe_panel(
                cx,
                |event| {
                    matches!(
                        event,
                        StateEvent::PeripheralsChanged | StateEvent::AgentChanged
                    )
                },
                |panel, cx| {
                    let current = AppState::try_read(cx)
                        .and_then(AppState::current_record)
                        .and_then(|record| record.extension());
                    if panel.editor.as_ref().is_some_and(|editor| {
                        current.is_none_or(|record| record.session != editor.session)
                    }) {
                        panel.editor = None;
                        panel.error = None;
                    }
                },
            ),
        }
    }

    fn edit(
        &mut self,
        record: &PeripheralRecord,
        capability: &CapabilityId,
        target: EditTarget,
        initial: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
        let subscription = cx.observe(&input, |_, _, cx| cx.notify());
        self.editor = Some(Editor {
            session: record.session.clone(),
            capability: capability.clone(),
            target,
            input,
            _input: subscription,
        });
        self.error = None;
        cx.notify();
    }

    fn submit(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(editor) = &self.editor else {
            return;
        };
        let Some(record) = AppState::try_read(cx)
            .and_then(AppState::current_record)
            .and_then(|r| r.extension())
            .cloned()
        else {
            return;
        };
        if record.session != editor.session {
            self.editor = None;
            cx.notify();
            return;
        }
        let capability = editor.capability.clone();
        match &editor.target {
            EditTarget::Binding(control) => match text.parse::<KeyCombo>() {
                Ok(combo) => AppState::apply(cx, |state| {
                    state.commit_peripheral_binding(
                        &record,
                        &capability,
                        control.clone(),
                        Some(Action::CustomShortcut(combo)),
                    )
                }),
                Err(error) => {
                    self.error = Some(error.to_string());
                    cx.notify();
                    return;
                }
            },
            EditTarget::Field(key, value_type) => {
                let value = match value_type {
                    SettingType::Integer { .. } => text
                        .parse::<i64>()
                        .map(SettingValue::Integer)
                        .map_err(|e| e.to_string()),
                    SettingType::Number { .. } => text
                        .parse::<f64>()
                        .map(SettingValue::Number)
                        .map_err(|e| e.to_string()),
                    SettingType::Text { .. } | SettingType::Enum(_) => Ok(SettingValue::Text(text)),
                    SettingType::Boolean => text
                        .parse::<bool>()
                        .map(SettingValue::Boolean)
                        .map_err(|e| e.to_string()),
                };
                match value {
                    Ok(value) => AppState::apply(cx, |state| {
                        state.commit_peripheral_value(&record, &capability, key.clone(), value)
                    }),
                    Err(error) => {
                        self.error = Some(error);
                        cx.notify();
                        return;
                    }
                }
            }
        }
        if AppState::try_read(cx).is_some_and(|state| state.peripheral_issue().is_none()) {
            self.editor = None;
        }
        cx.notify();
    }
}

impl Render for PeripheralPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let Some(state) = AppState::try_read(cx) else {
            return div();
        };
        let Some(record) = state.current_record().and_then(|r| r.extension()).cloned() else {
            return div();
        };
        let resolved = state
            .peripheral_rule(&record)
            .map(Option::<&PeripheralConfig>::cloned);
        let enabled = resolved
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .is_some_and(|r| r.enabled);
        let issue = resolved
            .as_ref()
            .err()
            .or_else(|| state.peripheral_issue())
            .map(peripheral_error);
        let rule = resolved.ok().flatten();
        let busy = state.peripheral_busy();
        let toggle_record = record.clone();
        let scope = rule.as_ref().map_or_else(
            || record.scopes.first().copied().unwrap_or(ScopeKind::Session),
            |rule| rule.scope.kind(),
        );
        let mut panel = v_flex()
            .gap_4()
            .p_5()
            .w_full()
            .max_w(gpui::px(680.))
            .child(div().text_heading().child(tr!("peripheral.controls")))
            .child(
                div()
                    .text_body()
                    .text_color(pal.text_muted)
                    .child(match scope {
                        ScopeKind::Model => tr!("peripheral.model_scope"),
                        ScopeKind::Physical => tr!("peripheral.physical_scope"),
                        ScopeKind::Session => tr!("peripheral.session_scope"),
                    }),
            )
            .child(
                Switch::new("peripheral-enabled")
                    .label(tr!("peripheral.enable"))
                    .checked(enabled)
                    .disabled(busy)
                    .on_click(move |enabled, _, cx| {
                        AppState::apply(cx, |state| {
                            state.commit_peripheral_enabled(&toggle_record, *enabled)
                        });
                    }),
            );
        if let Some(issue) = issue {
            panel = panel.child(div().text_body().child(issue));
        }
        if let Some(error) = &self.error {
            panel = panel.child(div().text_body().child(error.clone()));
        }
        if let Some(error) = &record.driver_error {
            panel = panel.child(div().text_body().child(peripheral_error(error)));
        }
        if record.driver_error.is_some() {
            let session = record.session.clone();
            panel = panel.child(
                control_button("peripheral-retry")
                    .label(tr!("peripheral.retry"))
                    .disabled(busy || record.connection != ConnectionStatus::Online)
                    .on_click(move |_, _, cx| {
                        AppState::apply(cx, |state| {
                            state.manage_peripheral(PeripheralOperation::Retry(session.clone()))
                        });
                    }),
            );
        }
        for capability in &record.capabilities {
            panel = panel.child(Self::render_capability(
                CapabilityView {
                    record: &record,
                    capability,
                    rule: rule.as_ref(),
                    busy,
                },
                cx,
            ));
        }
        if let Some(editor) = &self.editor {
            panel = panel.child(Self::render_editor(editor, &record, cx));
        }
        panel
    }
}

#[derive(Clone, Copy)]
struct CapabilityView<'a> {
    record: &'a PeripheralRecord,
    capability: &'a CapabilityRecord,
    rule: Option<&'a PeripheralConfig>,
    busy: bool,
}

impl PeripheralPanel {
    fn render_capability(view: CapabilityView<'_>, cx: &mut Context<Self>) -> gpui::Div {
        let CapabilityView {
            record,
            capability,
            rule,
            busy,
        } = view;
        let pal = theme::palette(cx);
        let mut panel = v_flex().gap_4();
        let settings = rule
            .as_ref()
            .and_then(|r| r.capabilities.get(&capability.id));
        let unavailable = capability.version != 1
            || capability.unavailable.is_some()
            || settings.is_some_and(|settings| settings.version != capability.version);
        if unavailable {
            panel = panel.child(div().text_body().child(tr!("peripheral.unsupported_capability", id => capability.id.to_string(), version => settings.map_or(capability.version, |s| s.version).to_string())));
            return panel;
        }
        match &capability.capability {
            Capability::InputRemap(input) => panel = Self::render_input(view, input, cx),
            Capability::Extension(fields) => panel = Self::render_fields(view, fields, cx),
            _ => {}
        }
        for status in record
            .operations
            .iter()
            .filter(|s| s.capability == capability.id)
        {
            panel = panel.child(
                div()
                    .text_body()
                    .text_color(pal.text_muted)
                    .child(operation_label(status)),
            );
            if status.application
                == ApplicationStatus::Failed(PeripheralError::ExternalModification)
                && let Some(rule) = &rule
            {
                let rule = rule.id.clone();
                panel = panel.child(
                    control_button("resolve-mapping")
                        .label(tr!("peripheral.resolve_mapping"))
                        .disabled(busy)
                        .on_click(move |_, _, cx| {
                            AppState::apply(cx, |state| {
                                state.manage_peripheral(PeripheralOperation::Resolve(rule.clone()))
                            });
                        }),
                );
            }
        }
        panel
    }

    fn render_input(
        view: CapabilityView<'_>,
        input: &InputRemapCapability,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let CapabilityView {
            record,
            capability,
            rule,
            busy,
        } = view;
        let settings = rule.and_then(|r| r.capabilities.get(&capability.id));
        let pal = theme::palette(cx);
        let mut panel = v_flex().gap_4();
        for control in &input.controls {
            let id = SharedString::from(format!("{}-{}", capability.id, control.id));
            let action = settings.and_then(|s| s.bindings.get(&control.id));
            let initial = match action {
                Some(Action::CustomShortcut(combo)) => combo.rendered_label(),
                _ => control
                    .recommended_key
                    .as_ref()
                    .map(KeyCombo::rendered_label)
                    .unwrap_or_default(),
            };
            let edit_record = record.clone();
            let cap = capability.id.clone();
            let target = control.id.clone();
            let remove_record = record.clone();
            let remove_cap = cap.clone();
            let remove_control = target.clone();
            let label = action.map_or_else(
                || tr!("peripheral.choose_key"),
                crate::ui::action::localized_action_label,
            );
            panel = panel.child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_subheading()
                            .child(localized_label(&control.labels)),
                    )
                    .child(
                        gpui_component::h_flex()
                            .gap_2()
                            .child(
                                control_button(id.clone())
                                    .debug_selector(move || id.to_string())
                                    .label(label)
                                    .disabled(busy)
                                    .on_click(cx.listener(move |panel, _, window, cx| {
                                        panel.edit(
                                            &edit_record,
                                            &cap,
                                            EditTarget::Binding(target.clone()),
                                            initial.clone(),
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!(
                                    "clear-{}-{}",
                                    capability.id, control.id
                                )))
                                .ghost()
                                .label(tr!("peripheral.clear_binding"))
                                .disabled(action.is_none() || busy)
                                .on_click(move |_, _, cx| {
                                    AppState::apply(cx, |state| {
                                        state.commit_peripheral_binding(
                                            &remove_record,
                                            &remove_cap,
                                            remove_control.clone(),
                                            None,
                                        )
                                    });
                                }),
                            ),
                    ),
            );
        }
        if input
            .controls
            .iter()
            .any(|c| c.trigger == Trigger::ShortPress)
        {
            panel = panel.child(
                div()
                    .text_body()
                    .text_color(pal.text_muted)
                    .child(tr!("peripheral.short_press")),
            );
        }
        panel
    }

    fn render_fields(
        view: CapabilityView<'_>,
        fields: &BTreeMap<String, SettingField>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let CapabilityView {
            record,
            capability,
            rule,
            busy,
        } = view;
        let settings = rule.and_then(|r| r.capabilities.get(&capability.id));
        let pal = theme::palette(cx);
        let mut panel = v_flex().gap_4();
        for (key, field) in fields {
            let value = settings
                .and_then(|s| s.values.get(key))
                .or_else(|| capability.values.get(key))
                .or(field.default.as_ref());
            let text = value.map(setting_text).unwrap_or_default();
            let edit_record = record.clone();
            let cap = capability.id.clone();
            let target = EditTarget::Field(key.clone(), field.value_type.clone());
            let detail = match &field.value_type {
                SettingType::Integer { minimum, maximum } => {
                    format!("{minimum} – {maximum}")
                }
                SettingType::Number {
                    minimum,
                    maximum,
                    unit,
                } => format!("{minimum} – {maximum} {unit}"),
                SettingType::Enum(tags) => tags.join(", "),
                SettingType::Boolean => "true / false".into(),
                SettingType::Text { .. } => String::new(),
            };
            panel = panel.child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_subheading()
                            .child(localized_label(&field.labels)),
                    )
                    .child(
                        control_button(SharedString::from(format!(
                            "field-{}-{key}",
                            capability.id
                        )))
                        .label(if text.is_empty() {
                            tr!("peripheral.edit_value")
                        } else {
                            text.clone().into()
                        })
                        .disabled(busy || field.access == SettingAccess::ReadOnly)
                        .on_click(cx.listener(
                            move |panel, _, window, cx| {
                                panel.edit(
                                    &edit_record,
                                    &cap,
                                    target.clone(),
                                    text.clone(),
                                    window,
                                    cx,
                                );
                            },
                        )),
                    )
                    .child(
                        div()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(detail),
                    ),
            );
        }
        panel
    }

    fn render_editor(
        editor: &Editor,
        record: &PeripheralRecord,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let pal = theme::palette(cx);
        let view = cx.entity().downgrade();
        let input = editor.input.clone();
        let mut area = v_flex()
            .gap_2()
            .p_3()
            .border_1()
            .border_color(pal.border)
            .rounded(pal.card_radius);
        match &editor.target {
            EditTarget::Binding(control) => {
                area = area.child(binding_editor::shortcut_picker(
                    &input,
                    move |text, _, cx| {
                        let _ = view.update(cx, |view, cx| view.submit(text, cx));
                    },
                ));
                let contract = record
                    .capabilities
                    .iter()
                    .find(|c| c.id == editor.capability);
                if matches!(contract.map(|c| &c.capability), Some(Capability::InputRemap(input)) if input.targets == TargetKind::Action)
                {
                    let cap = editor.capability.clone();
                    let control = control.clone();
                    let record = record.clone();
                    let pick: binding_editor::PickFn = Rc::new(move |action, _, cx| {
                        AppState::apply(cx, |state| {
                            state.commit_peripheral_binding(
                                &record,
                                &cap,
                                control.clone(),
                                Some(action),
                            )
                        });
                    });
                    area = area.child(binding_editor::editor_scroll_list(
                        "peripheral-actions",
                        binding_editor::action_rows("peripheral-action", None, &pick, pal),
                    ));
                }
            }
            EditTarget::Field(..) => {
                let read = input.clone();
                area = area.child(control_input(&input)).child(
                    control_button("peripheral-field-save")
                        .label(tr!("common.save"))
                        .on_click(cx.listener(move |panel, _, _, cx| {
                            panel.submit(read.read(cx).value().to_string(), cx);
                        })),
                );
            }
        }
        area.child(
            Button::new("peripheral-editor-close")
                .ghost()
                .label(tr!("common.cancel"))
                .on_click(cx.listener(|panel, _, _, cx| {
                    panel.editor = None;
                    panel.error = None;
                    cx.notify();
                })),
        )
    }
}

fn setting_text(value: &SettingValue) -> String {
    match value {
        SettingValue::Boolean(value) => value.to_string(),
        SettingValue::Integer(value) => value.to_string(),
        SettingValue::Number(value) => value.to_string(),
        SettingValue::Text(value) => value.clone(),
    }
}

#[cfg(test)]
mod tests;
