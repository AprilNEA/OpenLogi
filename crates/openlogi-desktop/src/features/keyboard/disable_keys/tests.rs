use std::{collections::BTreeSet, sync::Arc};

use gpui::{
    AppContext as _, Entity, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, TestAppContext, point,
    px,
};
use openlogi_core::config::{Config, ConfigFile};
use openlogi_core::device::{Capabilities, DeviceKind};
use openlogi_core::hid::DisableKeysState;

use super::*;
use crate::services::assets::AssetResolver;
use crate::services::ipc::Command;
use crate::services::ipc::ConfigReloadContext;
use crate::state::{ConfigPersistence, Load, Sources};

struct RestoreLocale(String);

impl Drop for RestoreLocale {
    fn drop(&mut self) {
        rust_i18n::set_locale(&self.0);
    }
}

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

#[test]
fn switch_name_states_that_on_disables_the_key() {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    let _restore = RestoreLocale(rust_i18n::locale().to_string());
    rust_i18n::set_locale("en");
    assert_eq!(
        switch_label(DisableKey::CapsLock).as_ref(),
        "Disable Caps Lock"
    );
    assert_ne!(
        switch_label(DisableKey::NumLock),
        key_label(DisableKey::NumLock)
    );
    rust_i18n::set_locale("ru");
    assert_eq!(
        switch_label(DisableKey::Insert).as_ref(),
        "Отключить Insert"
    );
}

fn install_keyboard(cx: &mut TestAppContext) -> tokio::sync::mpsc::UnboundedReceiver<Command> {
    install_keyboard_with_config(cx, Config::ephemeral(), ConfigPersistence::MemoryOnly)
}

fn install_keyboard_with_config(
    cx: &mut TestAppContext,
    config: Config,
    persistence: ConfigPersistence,
) -> tokio::sync::mpsc::UnboundedReceiver<Command> {
    cx.update(|cx| {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut inventory = crate::state::tests::direct_inventory([1, 2, 3, 4]);
        inventory.paired[0].kind = DeviceKind::Keyboard;
        inventory.paired[0].capabilities = Some(Capabilities {
            disable_keys: true,
            ..Capabilities::default()
        });
        let runtime: Arc<dyn swr_core::Runtime> = Arc::new(swr_gpui::GpuiRuntime::new(cx));
        let client = swr_core::SwrClient::builder().build(runtime.clone());
        let state = cx.new(|_| {
            let mut state = AppState::new(Sources {
                config,
                inventories: &[inventory],
                standalone: &[],
                resolver: &AssetResolver::new(),
                cameras: &[],
                persistence,
                ipc_commands: sender,
            });
            state.connect_device_reads(client, runtime);
            state
        });
        AppState::set_global(state, cx);
        AppState::load_current_device_reads(cx);
        receiver
    })
}

fn answer_initial_read(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    cx: &mut TestAppContext,
) -> Vec<Command> {
    cx.run_until_parked();
    let mut replied = false;
    let mut other_requests = Vec::new();
    while let Ok(command) = receiver.try_recv() {
        if let Command::ReadDisableKeys(read) = command {
            read.reply
                .send(Ok(DisableKeysState {
                    supported: DisableKeysMask::CAPS_LOCK,
                    disabled: DisableKeysMask::EMPTY,
                }))
                .unwrap();
            replied = true;
        } else {
            // Keep unrelated reads alive so dropping their reply channels does
            // not schedule retries during the interaction under test.
            other_requests.push(command);
        }
    }
    assert!(replied, "the ordinary read path must supply the snapshot");
    cx.run_until_parked();
    other_requests
}

#[gpui::test]
fn general_external_reload_requires_latest_revision_receipt_then_fresh_read(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(openlogi_core::paths::CONFIG_FILE);
    let (config, file) = ConfigFile::load_from_path(&path).unwrap();
    let mut receiver = install_keyboard_with_config(cx, config, ConfigPersistence::UserFile(file));
    let _other_requests = answer_initial_read(&mut receiver, cx);
    let key = cx.read(|cx| {
        let state = AppState::try_read(cx).unwrap();
        assert!(state.disable_keys_controls_enabled(&state.current_record().unwrap().device_key()));
        state.current_record().unwrap().device_key()
    });
    cx.update(|cx| AppState::apply(cx, |state| state.commit_smooth_scroll(true)));
    assert!(matches!(receiver.try_recv(), Ok(Command::ReloadConfig(_))));

    let mut external = Config::load_from_path(&path).unwrap();
    external.app_settings.animate_desktop_switch = false;
    external.save_to_path(&path).unwrap(); // R2, already loaded by the agent.
    external.set_disabled_keys(key.as_str(), BTreeSet::from([DisableKey::CapsLock]));
    external.save_to_path(&path).unwrap(); // R3, adopted by the GUI below.
    let external_text = std::fs::read_to_string(&path).unwrap();
    cx.update(|cx| {
        AppState::apply(cx, |state| {
            state.apply_saved_config_reload_result(Ok(
                openlogi_ipc::SavedConfigVerdict::FileChangedAfterSave,
            ))
        });
    });
    cx.read(|cx| {
        let state = AppState::try_read(cx).unwrap();
        assert!(matches!(state.disable_keys_load_for(&key), Load::Loading));
        assert!(!state.disable_keys_controls_enabled(&key));
    });
    cx.update(|cx| AppState::update_disable_keys(cx, DisableKeysMask::CAPS_LOCK));
    cx.run_until_parked();
    let mut fresh_read = None;
    let mut receipt = None;
    while let Ok(command) = receiver.try_recv() {
        match command {
            Command::SetDisableKeys(_) => panic!("stale cache must not permit a write"),
            Command::ReadDisableKeys(read) => {
                assert!(
                    fresh_read.replace(read).is_none(),
                    "one fresh read is required"
                );
            }
            Command::ReloadConfig(reload) => {
                assert_eq!(
                    reload.revision,
                    openlogi_core::config::ConfigRevision::of(&external_text)
                );
                let ConfigReloadContext::Reconciled(id) = reload.contexts[0].context() else {
                    panic!("revision-specific reconciliation");
                };
                assert!(receipt.replace(id).is_none());
            }
            _ => panic!("only a reload and a read are needed"),
        }
    }
    fresh_read
        .expect("external verdict requests a fresh read")
        .reply
        .send(Ok(DisableKeysState {
            supported: DisableKeysMask::CAPS_LOCK,
            disabled: DisableKeysMask::EMPTY,
        }))
        .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(
            !AppState::try_read(cx)
                .unwrap()
                .disable_keys_controls_enabled(&key)
        );
    });
    cx.update(|cx| {
        AppState::apply(cx, |state| {
            state.apply_reconciled_config_reload_result(
                receipt.expect("reload for R3"),
                Ok(openlogi_ipc::SavedConfigVerdict::Accepted),
            )
        });
    });
    answer_post_receipt_read(&mut receiver, &key, cx);
    cx.read(|cx| {
        let state = AppState::try_read(cx).unwrap();
        assert!(state.disable_keys_controls_enabled(&key));
        let Load::Ready(snapshot) = state.disable_keys_load_for(&key) else {
            panic!("fresh response must replace the stale snapshot");
        };
        assert_eq!(snapshot.disabled, DisableKeysMask::CAPS_LOCK);
    });
    assert_eq!(std::fs::read_to_string(&path).unwrap(), external_text);
}

fn answer_post_receipt_read(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    key: &DeviceKey,
    cx: &mut TestAppContext,
) {
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(
            !AppState::try_read(cx)
                .unwrap()
                .disable_keys_controls_enabled(key)
        );
    });
    let Ok(Command::ReadDisableKeys(read)) = receiver.try_recv() else {
        panic!("receipt must retire the earlier read");
    };
    read.reply
        .send(Ok(DisableKeysState {
            supported: DisableKeysMask::CAPS_LOCK,
            disabled: DisableKeysMask::CAPS_LOCK,
        }))
        .unwrap();
    cx.run_until_parked();
}

#[gpui::test]
fn general_refresh_failure_offers_a_working_reload_retry(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(theme::register_builtin_themes);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(openlogi_core::paths::CONFIG_FILE);
    let (config, file) = ConfigFile::load_from_path(&path).unwrap();
    let mut receiver = install_keyboard_with_config(cx, config, ConfigPersistence::UserFile(file));
    let _other_requests = answer_initial_read(&mut receiver, cx);
    let valid = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, "[malformed").unwrap();
    cx.update(|cx| {
        AppState::apply(cx, |state| {
            state.apply_saved_config_reload_result(Ok(
                openlogi_ipc::SavedConfigVerdict::FileChangedAfterSave,
            ))
        });
    });
    cx.run_until_parked();
    let Ok(Command::ReadDisableKeys(read)) = receiver.try_recv() else {
        panic!("refresh invalidates the earlier snapshot");
    };
    read.reply.send(Ok(DisableKeysState::default())).unwrap();
    cx.run_until_parked();
    assert!(
        receiver.try_recv().is_err(),
        "a failed refresh cannot request an exact revision"
    );
    let (_, visual) = cx.add_window_view(|_, cx| PanelHarness(cx.new(DisableKeysPanel::new)));
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let retry = visual
        .debug_bounds("disable-keys-reload-retry")
        .expect("shared recovery must not be a dead end");
    std::fs::write(&path, &valid).unwrap();
    visual.simulate_click(retry.center(), Modifiers::default());
    visual.run_until_parked();
    let mut reload = None;
    let mut reads = Vec::new();
    while let Ok(command) = receiver.try_recv() {
        match command {
            Command::ReloadConfig(request) => {
                assert!(reload.replace(request).is_none());
            }
            Command::ReadDisableKeys(read) => reads.push(read),
            _ => panic!("recovery never resends a hardware write"),
        }
    }
    let reload = reload.expect("the retry requests confirmation of the repaired file");
    assert_eq!(
        reload.revision,
        openlogi_core::config::ConfigRevision::of(&valid)
    );
    assert!(matches!(
        reload.contexts[0].context(),
        ConfigReloadContext::Reconciled(_)
    ));
    visual.read(|cx| {
        let state = AppState::try_read(cx).unwrap();
        let key = state.current_record().unwrap().device_key();
        assert!(!state.disable_keys_controls_enabled(&key));
        assert!(matches!(
            state.disable_keys_config_reconciliation(),
            ConfigReconciliation::Awaiting(_)
        ));
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("disable-keys-reload-retry").is_none());
}

#[gpui::test]
fn unavailable_agent_recovery_uses_the_current_language(cx: &mut TestAppContext) {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    let _restore = RestoreLocale(rust_i18n::locale().to_string());
    cx.update(gpui_component::init);
    cx.update(theme::register_builtin_themes);
    let mut receiver = install_keyboard(cx);
    let _other_requests = answer_initial_read(&mut receiver, cx);
    drop(receiver);
    cx.update(|cx| AppState::update_disable_keys(cx, DisableKeysMask::CAPS_LOCK));
    let (_, visual) = cx.add_window_view(|_, cx| DisableKeysPanel::new(cx));

    for (language, expected, selector, previous) in [
        (
            "ru",
            "Фоновая служба недоступна.",
            "disable-keys-error:Фоновая служба недоступна.",
            "disable-keys-error:The background service is unavailable.",
        ),
        (
            "en",
            "The background service is unavailable.",
            "disable-keys-error:The background service is unavailable.",
            "disable-keys-error:Фоновая служба недоступна.",
        ),
    ] {
        visual.update(|_, cx| {
            AppState::apply(cx, |state| state.commit_language(Some(language.into())));
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual.debug_bounds(selector).is_some(),
            "the live panel must render the current-language error"
        );
        assert!(
            visual.debug_bounds(previous).is_none(),
            "the old-language error must disappear"
        );
        visual.read(|cx| {
            let state = AppState::try_read(cx).unwrap();
            let key = state.current_record().unwrap().device_key();
            assert_eq!(
                recovery_error_text(state.disable_keys_error(&key).unwrap()).as_ref(),
                expected
            );
        });
    }
}

struct LoadHarness {
    key: DeviceKey,
    disabled: DisableKeysMask,
    enabled: bool,
    supported: DisableKeysMask,
}

impl Render for LoadHarness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().w(px(300.)).child(load_content(
            DisableKeysLoad::Ready(Arc::new(DisableKeysState {
                supported: self.supported,
                disabled: self.disabled,
            })),
            true,
            true,
            self.enabled,
            None,
            &self.key,
            theme::palette(cx),
        ))
    }
}

struct OfflineLoadHarness {
    load: DisableKeysLoad,
}

impl Render for OfflineLoadHarness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().w(px(300.)).child(load_content(
            self.load.clone(),
            false,
            true,
            false,
            None,
            &DeviceKey::from("unit:01020304"),
            theme::palette(cx),
        ))
    }
}

#[gpui::test]
fn offline_unknown_and_loading_do_not_present_an_endless_spinner(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(theme::register_builtin_themes);
    for load in [DisableKeysLoad::Unknown, DisableKeysLoad::Loading] {
        let (_, visual) = cx.add_window_view(|_, _| OfflineLoadHarness { load });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual
                .debug_bounds("disable-keys-offline-unknown")
                .is_some(),
            "offline Unknown and Loading must render the existing offline status"
        );
    }
}

#[gpui::test]
fn switch_focus_is_stable_but_not_shared_between_devices(cx: &mut TestAppContext) {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    let _restore = RestoreLocale(rust_i18n::locale().to_string());
    rust_i18n::set_locale("en");
    cx.update(gpui_component::init);
    cx.update(theme::register_builtin_themes);
    let _receiver = install_keyboard(cx);
    let (view, visual) = cx.add_window_view(|_, _| LoadHarness {
        key: DeviceKey::from("unit:01020304"),
        disabled: DisableKeysMask::EMPTY,
        enabled: true,
        supported: DisableKeysMask::CAPS_LOCK | DisableKeysMask::NUM_LOCK,
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.simulate_click(point(px(280.), px(10.)), Modifiers::default());
    let first = visual.update(|window, cx| window.focused(cx).expect("Caps Lock focused"));

    view.update(visual, |view, cx| {
        view.disabled = DisableKeysMask::CAPS_LOCK;
        cx.notify();
    });
    rust_i18n::set_locale("ru");
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.simulate_click(point(px(280.), px(10.)), Modifiers::default());
    let same = visual.update(|window, cx| window.focused(cx).unwrap());
    assert_eq!(
        first, same,
        "checked state and locale must not reset identity"
    );

    view.update(visual, |view, cx| {
        view.key = DeviceKey::from("unit:05060708");
        cx.notify();
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.simulate_click(point(px(280.), px(10.)), Modifiers::default());
    let second = visual.update(|window, cx| window.focused(cx).unwrap());
    assert_ne!(
        first, second,
        "another keyboard must not inherit focus identity"
    );

    let num_lock = visual.debug_bounds("disable-key-2").unwrap();
    visual.simulate_click(
        point(num_lock.right() - px(10.), num_lock.center().y),
        Modifiers::default(),
    );
    let other_key = visual.update(|window, cx| window.focused(cx).unwrap());
    assert_ne!(
        second, other_key,
        "each key on the same device has its own identity"
    );
}

#[test]
fn all_recovery_errors_have_russian_product_messages() {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    let _restore = RestoreLocale(rust_i18n::locale().to_string());
    rust_i18n::set_locale("ru");
    for (error, expected) in [
        (
            DisableKeysError::AgentUnavailable,
            "Фоновая служба недоступна.",
        ),
        (
            DisableKeysError::ConnectionChanged,
            "Соединение изменилось. Повторите чтение.",
        ),
        (
            DisableKeysError::WriteFailed,
            "Клавиатура не подтвердила изменение.",
        ),
        (
            DisableKeysError::SaveFailed,
            "Не удалось сохранить подтверждённую настройку.",
        ),
        (
            DisableKeysError::RefreshFailed,
            "Не удалось перечитать конфигурацию перед сохранением.",
        ),
        (
            DisableKeysError::ReloadFailed,
            "Фоновая служба не смогла перечитать конфигурацию.",
        ),
    ] {
        assert_eq!(recovery_error_text(error).as_ref(), expected);
    }
}

#[gpui::test]
fn disabled_switch_does_not_write_or_take_focus(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(theme::register_builtin_themes);
    let mut receiver = install_keyboard(cx);
    let _other_requests = answer_initial_read(&mut receiver, cx);
    let (_, visual) = cx.add_window_view(|_, _| LoadHarness {
        key: DeviceKey::from("unit:01020304"),
        disabled: DisableKeysMask::EMPTY,
        enabled: false,
        supported: DisableKeysMask::CAPS_LOCK,
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.simulate_click(point(px(280.), px(10.)), Modifiers::default());
    assert!(visual.update(|window, cx| window.focused(cx).is_none()));
    assert!(
        receiver.try_recv().is_err(),
        "disabled controls must not emit commands"
    );
}

struct RecoveryHarness {
    status: DisableKeysPersistenceStatus,
    key: DeviceKey,
    online: bool,
}

impl Render for RecoveryHarness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().w(px(600.)).children(recovery_content(
            &self.status,
            self.online,
            &self.key,
            theme::palette(cx),
        ))
    }
}

fn take_reload(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
) -> crate::services::ipc::DisableKeysRequestContext {
    let Command::ReloadConfig(mut request) = receiver.try_recv().unwrap() else {
        panic!("expected configuration reload, not a hardware write");
    };
    assert_eq!(request.contexts.len(), 1);
    let ConfigReloadContext::DisableKeys(context) = request.contexts.remove(0).context() else {
        panic!("expected Disable Keys reload context");
    };
    context
}

fn take_save_retry_reload(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
) -> (
    crate::services::ipc::DisableKeysRequestContext,
    Vec<crate::services::ipc::ReadDisableKeys>,
) {
    let mut context = None;
    let mut reads = Vec::new();
    while let Ok(command) = receiver.try_recv() {
        match command {
            Command::ReloadConfig(reload) => {
                let ConfigReloadContext::DisableKeys(receipt) = reload.contexts[0].context() else {
                    panic!("Save Retry retains its device transaction");
                };
                assert!(context.replace(receipt).is_none());
            }
            Command::ReadDisableKeys(read) => reads.push(read),
            _ => panic!("Save Retry must not repeat a hardware write"),
        }
    }
    (context.expect("one saved reload"), reads)
}

struct PanelHarness(Entity<DisableKeysPanel>);

impl Render for PanelHarness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().tab_group().w(px(600.)).child(self.0.clone())
    }
}

fn exercise_save_retry(cx: &mut TestAppContext, keypress: Option<&str>) {
    cx.update(gpui_component::init);
    cx.update(theme::register_builtin_themes);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(openlogi_core::paths::CONFIG_FILE);
    Config::default().save_to_path(&path).unwrap();
    let (config, file) = ConfigFile::load_from_path(&path).unwrap();
    let mut receiver = install_keyboard_with_config(cx, config, ConfigPersistence::UserFile(file));
    let _other_requests = answer_initial_read(&mut receiver, cx);
    cx.update(|cx| AppState::update_disable_keys(cx, DisableKeysMask::CAPS_LOCK));
    let Command::SetDisableKeys(write) = receiver.try_recv().unwrap() else {
        panic!("expected initial hardware write");
    };
    let key = write.context.key.clone();

    // A real concurrent file edit makes the confirmed hardware write fail to
    // persist. The rendered Save Retry must merge it, not repeat the HID write.
    let mut external = Config::load_from_path(&path).unwrap();
    external.keyboard.bindings.insert(
        "f1".parse().unwrap(),
        openlogi_core::binding::Action::TypeText("external binding".into()),
    );
    external.save_to_path(&path).unwrap();
    cx.update(|cx| {
        AppState::apply(cx, |state| {
            state.apply_disable_keys_write_result(
                write.context,
                Ok(DisableKeysState {
                    supported: DisableKeysMask::CAPS_LOCK,
                    disabled: DisableKeysMask::CAPS_LOCK,
                }),
            )
        });
    });
    cx.read(|cx| {
        assert!(matches!(
            AppState::try_read(cx).unwrap().disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::AppliedNotSaved { .. })
        ));
    });
    assert!(receiver.try_recv().is_err());
    let (_, visual) = cx.add_window_view(|_, cx| PanelHarness(cx.new(DisableKeysPanel::new)));
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let button = visual
        .debug_bounds("disable-keys-save-retry")
        .expect("live Save Retry button");
    if let Some(keypress) = keypress {
        visual.update(|window, cx| {
            window.focus_next(cx);
            assert!(window.focused(cx).is_some());
            window.draw(cx).clear(cx);
        });
        let keystroke = Keystroke::parse(keypress).unwrap();
        visual.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        visual.simulate_event(KeyUpEvent { keystroke });
    } else {
        visual.simulate_click(button.center(), Modifiers::default());
    }
    let (reload, _refresh_reads) = take_save_retry_reload(&mut receiver);
    assert_eq!(reload.key, key);
    assert!(
        receiver.try_recv().is_err(),
        "Save Retry must not repeat a hardware write"
    );
    let saved = Config::load_from_path(&path).unwrap();
    assert_eq!(
        saved.disabled_keys(key.as_str()),
        Some(&BTreeSet::from([DisableKey::CapsLock]))
    );
    assert_eq!(saved.keyboard.bindings, external.keyboard.bindings);
    visual.read(|cx| {
        let state = AppState::try_read(cx).unwrap();
        assert_eq!(state.keyboard_bindings(), &external.keyboard.bindings);
        assert!(matches!(
            state.disable_keys_status(&key),
            Some(DisableKeysPersistenceStatus::AwaitingReload(_))
        ));
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual.debug_bounds("disable-keys-save-retry").is_none(),
        "successful save must remove the retry action"
    );
}

#[gpui::test]
fn save_retry_click_preserves_external_binding_without_second_write(cx: &mut TestAppContext) {
    exercise_save_retry(cx, None);
}

#[gpui::test]
fn save_retry_enter_preserves_external_binding_without_second_write(cx: &mut TestAppContext) {
    exercise_save_retry(cx, Some("enter"));
}

#[gpui::test]
fn save_retry_space_preserves_external_binding_without_second_write(cx: &mut TestAppContext) {
    exercise_save_retry(cx, Some("space"));
}

#[gpui::test]
fn shared_reload_retry_is_inert_offline_and_retries_only_reload(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(theme::register_builtin_themes);
    let mut receiver = install_keyboard(cx);
    let _other_requests = answer_initial_read(&mut receiver, cx);
    cx.update(|cx| AppState::update_disable_keys(cx, DisableKeysMask::CAPS_LOCK));
    let Command::SetDisableKeys(write) = receiver.try_recv().unwrap() else {
        panic!("expected initial hardware write");
    };
    let key = write.context.key.clone();
    cx.update(|cx| {
        AppState::apply(cx, |state| {
            state.apply_disable_keys_write_result(
                write.context,
                Ok(DisableKeysState {
                    supported: DisableKeysMask::CAPS_LOCK,
                    disabled: DisableKeysMask::CAPS_LOCK,
                }),
            )
        });
    });
    let reload = take_reload(&mut receiver);
    cx.update(|cx| {
        AppState::apply(cx, |state| {
            state.apply_disable_keys_reload_result(
                reload,
                Err(openlogi_ipc::ConfigReloadError {
                    message: "scripted diagnostic that is not UI copy".into(),
                }),
            )
        });
    });
    let status = cx.read(|cx| {
        AppState::try_read(cx)
            .unwrap()
            .disable_keys_status(&key)
            .unwrap()
            .clone()
    });
    let (view, visual) = cx.add_window_view(|_, _| RecoveryHarness {
        status,
        key: key.clone(),
        online: false,
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let button = visual.debug_bounds("disable-keys-reload-retry").unwrap();
    visual.simulate_click(button.center(), Modifiers::default());
    assert!(
        receiver.try_recv().is_err(),
        "offline retry must not send a command"
    );

    view.update(visual, |view, cx| {
        view.online = true;
        cx.notify();
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let button = visual.debug_bounds("disable-keys-reload-retry").unwrap();
    visual.simulate_click(button.center(), Modifiers::default());
    assert_eq!(take_reload(&mut receiver).key, key);
    assert!(
        receiver.try_recv().is_err(),
        "retry must not repeat the hardware write"
    );
}

#[gpui::test]
fn shared_read_retry_is_localized_and_restarts_the_failed_read(cx: &mut TestAppContext) {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    let _restore = RestoreLocale(rust_i18n::locale().to_string());
    rust_i18n::set_locale("ru");
    cx.update(gpui_component::init);
    cx.update(theme::register_builtin_themes);
    let mut receiver = install_keyboard(cx);
    let mut unrelated = Vec::new();
    for _ in 0..3 {
        cx.run_until_parked();
        let mut answered = false;
        while let Ok(command) = receiver.try_recv() {
            if let Command::ReadDisableKeys(read) = command {
                read.reply
                    .send(Err(openlogi_core::hid::WriteError::AgentUnavailable))
                    .unwrap();
                answered = true;
            } else {
                unrelated.push(command);
            }
        }
        assert!(answered, "each bounded read attempt must be answered");
    }
    cx.run_until_parked();
    let (_, visual) = cx.add_window_view(|_, cx| DisableKeysPanel::new(cx));
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let retry = visual
        .debug_bounds(
            "disable-keys-read-retry:Не удалось прочитать настройки клавиш. Нажмите для повтора.",
        )
        .unwrap();
    visual.simulate_click(retry.center(), Modifiers::default());
    visual.run_until_parked();
    let Command::ReadDisableKeys(read) = receiver.try_recv().unwrap() else {
        panic!("the shared retry must issue a read, not a write");
    };
    read.reply
        .send(Ok(DisableKeysState {
            supported: DisableKeysMask::CAPS_LOCK,
            disabled: DisableKeysMask::EMPTY,
        }))
        .unwrap();
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("disable-key-1").is_some());
    assert!(visual.debug_bounds("disable-keys-read-retry:Не удалось прочитать настройки клавиш. Нажмите для повтора.").is_none());
    assert!(receiver.try_recv().is_err());
}

#[gpui::test]
fn unsupported_read_has_localized_status_without_retry(cx: &mut TestAppContext) {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    let _restore = RestoreLocale(rust_i18n::locale().to_string());
    rust_i18n::set_locale("ru");
    cx.update(gpui_component::init);
    cx.update(theme::register_builtin_themes);
    let mut receiver = install_keyboard(cx);
    let mut unrelated = Vec::new();
    cx.run_until_parked();
    let mut answered = false;
    while let Ok(command) = receiver.try_recv() {
        if let Command::ReadDisableKeys(read) = command {
            read.reply
                .send(Err(openlogi_core::hid::WriteError::FeatureUnsupported {
                    feature_hex: 0x4521,
                }))
                .unwrap();
            answered = true;
        } else {
            unrelated.push(command);
        }
    }
    assert!(answered);
    cx.run_until_parked();
    let (_, visual) = cx.add_window_view(|_, cx| DisableKeysPanel::new(cx));
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let status = visual
        .debug_bounds("disable-keys-unsupported:Это устройство не поддерживает отключение клавиш.")
        .unwrap();
    assert!(visual.debug_bounds("disable-key-1").is_none());
    visual.simulate_click(status.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(
        receiver.try_recv().is_err(),
        "unsupported status must not retry a permanent failure"
    );
}
