//! Persisting a change: the rollback on a read-only config and the agent reload that follows.

use super::*;
use openlogi_core::config::{ConfigFile, MouseProfileTarget};

#[test]
fn read_only_config_rolls_back_mutations_and_does_not_reload_agent() {
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        persistence: ConfigPersistence::ReadOnly("invalid config".into()),
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });

    let _ = state.commit_thumbwheel_sensitivity(ThumbwheelSensitivity::from_rounded(50.0));
    let _ = state.commit_smooth_scroll(true);
    let _ = state.commit_vertical_scroll_sensitivity(VerticalScrollSensitivity::from_rounded(7.0));
    let _ = state.commit_mouse_profile_target(MouseProfileTarget::Focused);

    assert_eq!(
        state.app_settings().thumbwheel_sensitivity,
        ThumbwheelSensitivity::DEFAULT
    );
    assert!(!state.app_settings().smooth_scroll);
    assert_eq!(
        state.app_settings().vertical_scroll_sensitivity,
        VerticalScrollSensitivity::DEFAULT
    );
    assert_eq!(
        state.app_settings().mouse_profile_target,
        MouseProfileTarget::Pointer
    );
    assert_eq!(state.config_issue(), Some("invalid config"));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn mouse_profile_target_persists_and_reloads_only_when_changed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    let (config, file) = ConfigFile::load_from_path(&path).unwrap();
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources {
        persistence: ConfigPersistence::UserFile(file),
        ..Sources::in_memory(config, &resolver, commands)
    });
    // A file-backed AppState reloads once at startup, independently of edits.
    assert!(matches!(
        receiver.try_recv(),
        Ok(crate::services::ipc::Command::ReloadConfig(_))
    ));
    assert!(receiver.try_recv().is_err());

    for target in [MouseProfileTarget::Focused, MouseProfileTarget::Pointer] {
        assert_eq!(
            state.commit_mouse_profile_target(target),
            [StateEvent::SettingsChanged]
        );
        assert_eq!(state.app_settings().mouse_profile_target, target);
        assert_eq!(
            Config::load_from_path(&path)
                .unwrap()
                .app_settings
                .mouse_profile_target,
            target
        );
        assert!(matches!(
            receiver.try_recv(),
            Ok(crate::services::ipc::Command::ReloadConfig(_))
        ));
        assert!(receiver.try_recv().is_err());

        let _ = state.commit_mouse_profile_target(target);
        assert!(
            receiver.try_recv().is_err(),
            "unchanged selection must not reload"
        );
    }
}

#[test]
fn smooth_scroll_change_reloads_the_agent_once() {
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands));

    let _ = state.commit_smooth_scroll(true);

    assert!(state.app_settings().smooth_scroll);
    assert!(matches!(
        receiver.try_recv(),
        Ok(crate::services::ipc::Command::ReloadConfig(_))
    ));

    let _ = state.commit_smooth_scroll(true);
    assert!(receiver.try_recv().is_err());
}

/// A live language switch runs inside `AppState::update`, and the menu rebuild
/// it schedules reads the same entity (the Device menu lists devices). Rebuilt
/// synchronously that read is re-entrant and panics ("cannot read … while it is
/// already being updated"), which crashed 0.8.0 on every language change —
/// announcing the switch must defer the rebuild until the update returns the
/// lease.
#[gpui::test]
fn language_switch_rebuilds_menus_after_the_state_update(cx: &mut gpui::TestAppContext) {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock();
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands));

    cx.update(|cx| {
        AppState::set_global(cx.new(|_| state), cx);
        AppState::apply(cx, |state| state.commit_language(Some("zh-CN".into())));
    });

    cx.read(|cx| {
        assert_eq!(
            AppState::try_read(cx).and_then(AppState::language),
            Some("zh-CN")
        );
    });
}

#[test]
fn agent_reload_error_stays_visible_until_a_successful_confirmation() {
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands));
    assert_eq!(
        state.apply_config_reload_result(Err(openlogi_ipc::ConfigReloadError {
            message: "agent rejected config".into(),
        })),
        [StateEvent::SettingsChanged]
    );
    assert_eq!(state.config_issue(), Some("agent rejected config"));
    assert_eq!(
        state.apply_config_reload_result(Ok(())),
        [StateEvent::SettingsChanged]
    );
    assert_eq!(state.config_issue(), None);
}
