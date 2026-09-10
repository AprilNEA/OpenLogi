//! Device-panel controls for HID++ `0x4521 DisableKeys`.

use gpui::{
    Context, IntoElement, ParentElement, Render, Styled, Subscription, Window, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{Disableable as _, Icon, button::Button, h_flex, switch::Switch, v_flex};
use openlogi_core::config::DisableKey;
use openlogi_core::hid::DisableKeysMask;

use crate::state::{
    AppState, DeviceKey, DisableKeysLoad, DisableKeysPersistenceStatus, StateEvent,
};
use crate::ui::components::PanelCard;
use crate::ui::theme::{self, Palette, Typography as _};

/// Long-lived Disable Keys panel in the generic Device tab.
pub struct DisableKeysPanel {
    _state_obs: Subscription,
}

impl DisableKeysPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = cx.subscribe(&AppState::global(cx), |_, _, event: &StateEvent, cx| {
            let relevant = match event {
                StateEvent::InventoryChanged | StateEvent::DeviceSelected(_) => true,
                StateEvent::DisableKeysChanged(key) => AppState::try_read(cx)
                    .and_then(AppState::current_record)
                    .is_some_and(|record| record.device_key() == *key),
                _ => false,
            };
            if relevant {
                cx.notify();
            }
        });
        Self {
            _state_obs: state_obs,
        }
    }

    fn content(pal: Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(state) = AppState::try_read(cx) else {
            return status_text(tr!("keyboard.loading_disabled_key_state"), pal).into_any_element();
        };
        let Some(record) = state.current_record() else {
            return status_text(tr!("device.no_active_device"), pal).into_any_element();
        };
        let key = record.device_key();
        let online = record.online;
        let persistent = state.current_device_is_persistent();
        let enabled = state.disable_keys_controls_enabled(&key);
        let persistence = state.disable_keys_status(&key).cloned().unwrap_or_default();
        let error = state.disable_keys_error(&key).map(str::to_owned);
        let recovery = recovery_content(&persistence, online, &key, pal);
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
            h_flex()
                .gap_2()
                .items_center()
                .child(status_text(
                    tr!("keyboard.applied_to_keyboard_but_not_saved"),
                    pal,
                ))
                .child(
                    Button::new("disable-keys-save-retry")
                        .label(tr!("keyboard.save_retry"))
                        .on_click({
                            let key = key.clone();
                            move |_, _, cx| AppState::retry_disable_keys_save(cx, key.clone())
                        }),
                )
                .into_any_element(),
        ),
        DisableKeysPersistenceStatus::SavedNotReloaded(_)
        | DisableKeysPersistenceStatus::SavedNotReloadedDetached(_) => Some(
            h_flex()
                .gap_2()
                .items_center()
                .child(status_text(
                    tr!("keyboard.saved_but_agent_not_reloaded"),
                    pal,
                ))
                .child(
                    Button::new("disable-keys-reload-retry")
                        .label(tr!("keyboard.reload_retry"))
                        .disabled(!online)
                        .on_click({
                            let key = key.clone();
                            move |_, _, cx| {
                                AppState::retry_disable_keys_reload(cx, key.clone());
                            }
                        }),
                )
                .into_any_element(),
        ),
        DisableKeysPersistenceStatus::Applying(_)
        | DisableKeysPersistenceStatus::AwaitingReload(_) => {
            Some(status_text(tr!("keyboard.applying_and_confirming"), pal).into_any_element())
        }
        DisableKeysPersistenceStatus::Idle => None,
    }
}

fn load_content(
    load: DisableKeysLoad,
    online: bool,
    persistent: bool,
    enabled: bool,
    error: Option<String>,
    key: &DeviceKey,
    pal: Palette,
) -> gpui::AnyElement {
    match load {
        DisableKeysLoad::Unknown | DisableKeysLoad::Loading => {
            status_text(tr!("keyboard.loading_disabled_key_state"), pal).into_any_element()
        }
        DisableKeysLoad::Failed(message) => v_flex()
            .gap_2()
            .child(status_text(
                format!(
                    "{}: {message}",
                    tr!("keyboard.could_not_read_disabled_keys")
                ),
                pal,
            ))
            .child(
                Button::new("disable-keys-read-retry")
                    .label(tr!("keyboard.retry"))
                    .on_click({
                        let key = key.clone();
                        move |_, _, cx| AppState::retry_disable_keys_read(cx, key.clone())
                    }),
            )
            .into_any_element(),
        DisableKeysLoad::Unsupported(message) => {
            status_text(format!("{}: {message}", tr!("common.unavailable")), pal).into_any_element()
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
                        .justify_between()
                        .items_center()
                        .gap_4()
                        .child(div().text_body().child(key_label(known_key)))
                        .child(
                            Switch::new(("disable-key", bit.bits() as usize))
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
                    this.child(status_text(
                        tr!("keyboard.offline_showing_last_confirmed_snapshot"),
                        pal,
                    ))
                })
                .when(!persistent, |this| {
                    this.child(status_text(
                        tr!("keyboard.no_stable_identity_for_reconnect_policy"),
                        pal,
                    ))
                })
                .when_some(error, |this, error| this.child(status_text(error, pal)))
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

fn status_text(text: impl Into<gpui::SharedString>, pal: Palette) -> gpui::Div {
    div()
        .text_caption()
        .text_color(pal.text_muted)
        .child(text.into())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_rows_are_exact_and_platform_system_key_is_stable() {
        let supported = DisableKeysMask::CAPS_LOCK | DisableKeysMask::WINDOWS_COMMAND;
        let rows: Vec<_> = DisableKey::ALL
            .into_iter()
            .filter(|key| supported.contains(key.mask()))
            .collect();
        assert_eq!(rows, vec![DisableKey::CapsLock, DisableKey::WindowsCommand]);
        assert!(!key_label(DisableKey::WindowsCommand).is_empty());
    }
}
