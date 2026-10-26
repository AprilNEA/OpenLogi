//! Device-panel controls for HID++ `0x4521 DisableKeys`.

use gpui::{
    Context, ElementId, InteractiveElement as _, IntoElement, ParentElement, Render, Styled,
    Subscription, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{Disableable as _, Icon, h_flex, switch::Switch, v_flex};
use openlogi_core::config::DisableKey;
use openlogi_core::hid::DisableKeysMask;

use crate::state::{
    AppState, ConfigReconciliation, DeviceKey, DisableKeysError, DisableKeysLoad,
    DisableKeysPersistenceStatus, StateEvent,
};
use crate::ui::components::PanelCard;
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, Palette, Typography as _};

/// Long-lived Disable Keys panel in the generic Device tab.
pub struct DisableKeysPanel {
    _state_obs: Subscription,
}

impl DisableKeysPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = AppState::repaint_on(cx, |event| {
            matches!(event, StateEvent::DisableKeysChanged(_))
        });
        Self {
            _state_obs: state_obs,
        }
    }

    fn content(pal: Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(state) = AppState::try_read(cx) else {
            return status_line(tr!("keyboard.loading_disabled_key_state"), pal).into_any_element();
        };
        let Some(record) = state.current_record() else {
            return status_line(tr!("device.no_active_device"), pal).into_any_element();
        };
        let key = record.device_key();
        let online = record.online;
        let persistent = state.current_device_is_persistent();
        let enabled = state.disable_keys_controls_enabled(&key);
        let persistence = state.disable_keys_status(&key).cloned().unwrap_or_default();
        let error = state.disable_keys_error(&key);
        let recovery = recovery_content(&persistence, online, &key, pal).or_else(|| {
            config_recovery_content(
                state.disable_keys_config_reconciliation(),
                ReloadRecovery { online, key: &key },
                pal,
            )
        });
        let body = load_content(
            state.disable_keys_load_for(&key),
            online,
            persistent,
            enabled,
            error,
            &key,
            pal,
        );
        v_flex()
            .gap_3()
            .children(recovery)
            .child(body)
            .into_any_element()
    }
}

fn recovery_content(
    persistence: &DisableKeysPersistenceStatus,
    online: bool,
    key: &DeviceKey,
    pal: Palette,
) -> Option<gpui::AnyElement> {
    match persistence {
        DisableKeysPersistenceStatus::AppliedNotSaved { .. } => Some(
            v_flex()
                .gap_2()
                .child(status_line(
                    tr!("keyboard.applied_to_keyboard_but_not_saved"),
                    pal,
                ))
                .child(
                    retry_line(
                        device_control_id(key, "save-retry"),
                        tr!("keyboard.save_retry"),
                        {
                            let key = key.clone();
                            move |cx| AppState::retry_disable_keys_save(cx, &key)
                        },
                    )
                    .debug_selector(|| "disable-keys-save-retry".into()),
                )
                .into_any_element(),
        ),
        DisableKeysPersistenceStatus::SavedNotReloaded(_)
        | DisableKeysPersistenceStatus::SavedNotReloadedDetached(_)
        | DisableKeysPersistenceStatus::ReconcileRequired(_) => {
            let message = if matches!(
                persistence,
                DisableKeysPersistenceStatus::ReconcileRequired(_)
            ) {
                tr!("keyboard.disable_keys_refresh_failed")
            } else {
                tr!("keyboard.saved_but_agent_not_reloaded")
            };
            Some(reload_recovery_content(
                message,
                ReloadRecovery { online, key },
                pal,
            ))
        }
        DisableKeysPersistenceStatus::Applying(_)
        | DisableKeysPersistenceStatus::AwaitingReload(_)
        | DisableKeysPersistenceStatus::AwaitingReconciliationReload(_) => {
            Some(status_line(tr!("keyboard.applying_and_confirming"), pal).into_any_element())
        }
        DisableKeysPersistenceStatus::Idle => None,
    }
}

#[derive(Clone, Copy)]
struct ReloadRecovery<'a> {
    online: bool,
    key: &'a DeviceKey,
}

fn config_recovery_content(
    reconciliation: &ConfigReconciliation,
    recovery: ReloadRecovery<'_>,
    pal: Palette,
) -> Option<gpui::AnyElement> {
    match reconciliation {
        ConfigReconciliation::Confirmed => None,
        ConfigReconciliation::Awaiting(_) => {
            Some(status_line(tr!("keyboard.applying_and_confirming"), pal).into_any_element())
        }
        ConfigReconciliation::Required(error) => Some(reload_recovery_content(
            recovery_error_text(*error),
            recovery,
            pal,
        )),
    }
}

fn reload_recovery_content(
    message: gpui::SharedString,
    recovery: ReloadRecovery<'_>,
    pal: Palette,
) -> gpui::AnyElement {
    v_flex()
        .gap_2()
        .child(status_line(message, pal))
        .child(
            retry_line(
                device_control_id(recovery.key, "reload-retry"),
                tr!("keyboard.reload_retry"),
                {
                    let key = recovery.key.clone();
                    move |cx| AppState::retry_disable_keys_reload(cx, &key)
                },
            )
            .disabled(!recovery.online)
            .debug_selector(|| "disable-keys-reload-retry".into()),
        )
        .into_any_element()
}

fn load_content(
    load: DisableKeysLoad,
    online: bool,
    persistent: bool,
    enabled: bool,
    error: Option<DisableKeysError>,
    key: &DeviceKey,
    pal: Palette,
) -> gpui::AnyElement {
    match load {
        DisableKeysLoad::Unknown | DisableKeysLoad::Loading if !online => {
            status_line(tr!("device.offline"), pal)
                .debug_selector(|| "disable-keys-offline-unknown".into())
                .into_any_element()
        }
        DisableKeysLoad::Unknown | DisableKeysLoad::Loading => {
            status_line(tr!("keyboard.loading_disabled_key_state"), pal).into_any_element()
        }
        DisableKeysLoad::Failed(_) => {
            let message = tr!("keyboard.could_not_read_disabled_keys");
            retry_line(device_control_id(key, "read-retry"), message.clone(), {
                let key = key.clone();
                move |cx| AppState::retry_disable_keys_read(cx, key.clone())
            })
            .debug_selector(move || format!("disable-keys-read-retry:{message}"))
            .into_any_element()
        }
        DisableKeysLoad::Unsupported(_) => {
            let message = tr!("keyboard.disable_keys_unsupported");
            status_line(message.clone(), pal)
                .debug_selector(move || format!("disable-keys-unsupported:{message}"))
                .into_any_element()
        }
        DisableKeysLoad::Ready(snapshot) => {
            let known_disabled = snapshot.disabled & snapshot.supported & DisableKeysMask::KNOWN;
            let rows = DisableKey::ALL.into_iter().filter_map(|known_key| {
                let bit = known_key.mask();
                if !snapshot.supported.contains(bit) {
                    return None;
                }
                let checked = known_disabled.contains(bit);
                let desired = if checked {
                    known_disabled & !bit
                } else {
                    known_disabled | bit
                };
                Some(
                    h_flex()
                        .debug_selector(move || format!("disable-key-{}", bit.bits()))
                        .justify_between()
                        .items_center()
                        .gap_4()
                        .child(div().text_body().child(key_label(known_key)))
                        .child(
                            Switch::new((device_control_id(key, "key"), bit.bits().to_string()))
                                .accessibility_label(switch_label(known_key))
                                .checked(checked)
                                .disabled(!enabled)
                                .on_click(move |_, _, cx| {
                                    AppState::update_disable_keys(cx, desired);
                                }),
                        ),
                )
            });
            v_flex()
                .gap_3()
                .when(!online, |this| {
                    this.child(status_line(
                        tr!("keyboard.offline_showing_last_confirmed_snapshot"),
                        pal,
                    ))
                })
                .when(!persistent, |this| {
                    this.child(status_line(
                        tr!("keyboard.no_stable_identity_for_reconnect_policy"),
                        pal,
                    ))
                })
                .when_some(error, |this, error| {
                    let message = recovery_error_text(error);
                    this.child(
                        status_line(message.clone(), pal)
                            .debug_selector(move || format!("disable-keys-error:{message}")),
                    )
                })
                .children(rows)
                .into_any_element()
        }
    }
}

impl Render for DisableKeysPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        PanelCard::new(
            tr!("keyboard.disabled_keys"),
            Icon::empty().path("action-icons/keyboard.svg"),
            Self::content(pal, cx),
        )
    }
}

fn device_control_id(key: &DeviceKey, control: &'static str) -> ElementId {
    (
        ElementId::Name(format!("disable-keys:{key}").into()),
        control,
    )
        .into()
}

fn recovery_error_text(error: DisableKeysError) -> gpui::SharedString {
    match error {
        DisableKeysError::AgentUnavailable => tr!("keyboard.disable_keys_agent_unavailable"),
        DisableKeysError::ConnectionChanged => tr!("keyboard.disable_keys_connection_changed"),
        DisableKeysError::WriteFailed => tr!("keyboard.disable_keys_write_failed"),
        DisableKeysError::SaveFailed => tr!("keyboard.disable_keys_save_failed"),
        DisableKeysError::RefreshFailed => tr!("keyboard.disable_keys_refresh_failed"),
        DisableKeysError::ReloadFailed => tr!("keyboard.disable_keys_reload_failed"),
    }
}

fn key_label(key: DisableKey) -> gpui::SharedString {
    match key {
        DisableKey::CapsLock => tr!("keyboard.caps_lock"),
        DisableKey::NumLock => tr!("keyboard.num_lock"),
        DisableKey::ScrollLock => tr!("keyboard.scroll_lock"),
        DisableKey::Insert => tr!("keyboard.insert"),
        DisableKey::WindowsCommand if cfg!(target_os = "macos") => tr!("keyboard.command"),
        DisableKey::WindowsCommand => tr!("keyboard.windows"),
    }
}

/// Screen-reader name of a key's switch. The switch is on when the key is
/// disabled, so the name states the action instead of only the key.
fn switch_label(key: DisableKey) -> gpui::SharedString {
    tr!("keyboard.disable_key_switch", name => key_label(key).to_string())
}

#[cfg(test)]
mod tests;
