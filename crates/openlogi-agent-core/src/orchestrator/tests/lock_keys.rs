//! The lock-key indicator: which lock-key changes the agent shows, and in
//! which language.

use std::sync::Arc;

use openlogi_core::config::Config;
use openlogi_core::device::LockKey;
use openlogi_ipc::{LockKeyChange, LockKeyObservation};

use crate::observable::ObservableState;
use crate::orchestrator::Orchestrator;

const CAPS_ON: LockKeyChange = LockKeyChange {
    key: LockKey::CapsLock,
    engaged: true,
};
const CAPS_OFF: LockKeyChange = LockKeyChange {
    key: LockKey::CapsLock,
    engaged: false,
};

fn wired(config: Config) -> (Orchestrator, Arc<ObservableState>) {
    let observable = Arc::new(ObservableState::new("test".to_owned()));
    (
        Orchestrator::new(config, Arc::clone(&observable)),
        observable,
    )
}

async fn indicator(observable: &ObservableState) -> LockKeyObservation {
    observable.lock_keys().observe(0).await
}

#[tokio::test]
async fn a_lock_key_shows_in_the_configured_language() {
    let mut config = Config::default();
    config.app_settings.language = Some("ja".to_owned());
    let (orch, observable) = wired(config);

    orch.announce_lock_key(CAPS_OFF);

    let shown = indicator(&observable).await;
    assert_eq!(shown.shown, Some(CAPS_OFF));
    assert_eq!(shown.language.as_deref(), Some("ja"));
}

#[tokio::test]
async fn nothing_shows_with_the_indicator_turned_off() {
    let mut config = Config::default();
    config.app_settings.lock_key_notifications = false;
    let (orch, observable) = wired(config);

    orch.announce_lock_key(CAPS_ON);

    let current = indicator(&observable).await;
    assert_eq!(current.shown, None);
    assert_eq!(current.generation, 1, "the cell never moved");
}

#[tokio::test]
async fn num_and_scroll_lock_show_only_where_the_platform_gives_them_meaning() {
    for key in [LockKey::NumLock, LockKey::ScrollLock] {
        let (orch, observable) = wired(Config::default());
        let change = LockKeyChange { key, engaged: true };

        orch.announce_lock_key(change);

        let shown = indicator(&observable).await.shown;
        if cfg!(target_os = "macos") {
            assert_eq!(shown, None, "{key:?} has no meaning on macOS");
        } else {
            assert_eq!(shown, Some(change));
        }
    }
}

#[tokio::test]
async fn turning_the_indicator_on_again_shows_the_next_change() {
    let mut config = Config::default();
    config.app_settings.lock_key_notifications = false;
    let (mut orch, observable) = wired(config.clone());
    orch.announce_lock_key(CAPS_ON);

    config.app_settings.lock_key_notifications = true;
    orch.replace_config(config);
    orch.announce_lock_key(CAPS_OFF);

    assert_eq!(indicator(&observable).await.shown, Some(CAPS_OFF));
}
