//! Device settings the agent changes and saves itself: the device, the saved
//! file and the reloaded orchestrator agree, a file another writer changed
//! is not overwritten, and an offline device's settings are only saved.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use openlogi_core::config::{ConfigError, DisableKey, SmartShift, WheelMode};
use openlogi_core::hid::{
    DisableKeysMask, SmartShiftAutoDisengage, SmartShiftMode, SmartShiftStatus,
};
use openlogi_hid::HidppOperation;
use openlogi_ipc::ConfigReloadError;
use tokio::sync::Mutex;

use super::*;
use crate::orchestrator::{ConfigReload, DeviceSettings, SettingError};
use crate::test_support::publish_dpi_mouse;

const KEY: &str = "unit:12345678";

/// A reload that loads the file into the orchestrator, as the agent's does,
/// and counts itself.
#[derive(Clone)]
struct Reload {
    orchestrator: Arc<Mutex<Orchestrator>>,
    path: PathBuf,
    count: Arc<AtomicUsize>,
}

impl ConfigReload for Reload {
    async fn reload_config_file(&self) -> Result<(), ConfigReloadError> {
        let config = Config::load_from_path(&self.path).map_err(|error| ConfigReloadError {
            message: error.to_string(),
        })?;
        self.orchestrator.lock().await.reload_config(config);
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct Agent {
    orchestrator: Arc<Mutex<Orchestrator>>,
    settings: DeviceSettings<Reload>,
    reloads: Arc<AtomicUsize>,
    route: DeviceRoute,
}

impl Agent {
    /// An agent that knows one mouse with a physical identity, saving to
    /// `path`. Another mouse's settings are in the file already, and an
    /// offline mouse is known by the route it was last seen on.
    fn new(path: &Path, online: bool) -> Self {
        let mouse = AgentDevice {
            unit_id: [0x12, 0x34, 0x56, 0x78],
            ..dev(KEY, 1, online)
        };
        let route = mouse.route.clone().unwrap();
        let mut config = Config::default();
        config.set_dpi("unit:87654321", Dpi::new(2600));
        config
            .devices
            .entry(KEY.to_string())
            .or_default()
            .links
            .insert(stable_id(&mouse).route_key(), LinkConfig::default());
        config.save_to_path(path).unwrap();
        let mut orchestrator = orchestrator(Config::load_from_path(path).unwrap());
        orchestrator.devices = vec![mouse];
        orchestrator.rebuild();
        let shared = orchestrator.shared();
        let orchestrator = Arc::new(Mutex::new(orchestrator));
        let reloads = Arc::new(AtomicUsize::new(0));
        let reload = Reload {
            orchestrator: Arc::clone(&orchestrator),
            path: path.to_path_buf(),
            count: Arc::clone(&reloads),
        };
        let settings =
            DeviceSettings::new(Arc::clone(&orchestrator), shared, reload).saving_to(path.into());
        Self {
            orchestrator,
            settings,
            reloads,
            route,
        }
    }

    async fn shared(&self) -> crate::orchestrator::SharedHandles {
        self.orchestrator.lock().await.shared()
    }

    async fn device_dpi(&self) -> Dpi {
        self.shared()
            .await
            .device(&self.route)
            .run(HidppOperation::ReadDpiCapabilities, |channel| async move {
                openlogi_hid::get_dpi_info_on(&channel).await
            })
            .await
            .unwrap()
            .current
    }
}

#[tokio::test]
async fn an_online_dpi_is_written_saved_and_reloaded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    let agent = Agent::new(&path, true);
    publish_dpi_mouse(
        &agent.shared().await.channel_registry,
        agent.route.clone(),
        || true,
    )
    .await;

    agent
        .settings
        .set_dpi(&agent.route, Dpi::new(1600))
        .await
        .unwrap();

    assert_eq!(agent.device_dpi().await, Dpi::new(1600));
    let saved = Config::load_from_path(&path).unwrap();
    assert_eq!(saved.dpi(KEY), Some(Dpi::new(1600)));
    assert_eq!(saved.dpi("unit:87654321"), Some(Dpi::new(2600)));
    assert_eq!(agent.reloads.load(Ordering::SeqCst), 1);
    assert_eq!(
        agent.orchestrator.lock().await.config.dpi(KEY),
        Some(Dpi::new(1600))
    );
}

/// A DPI the mouse refuses is not saved, so the agent does not re-send it
/// at every reconnect.
#[tokio::test]
async fn a_refused_dpi_is_not_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    let agent = Agent::new(&path, true);
    publish_dpi_mouse(
        &agent.shared().await.channel_registry,
        agent.route.clone(),
        || false,
    )
    .await;
    let before = std::fs::read_to_string(&path).unwrap();

    let error = agent
        .settings
        .set_dpi(&agent.route, Dpi::new(1600))
        .await
        .unwrap_err();

    assert!(matches!(error, SettingError::Write(_)), "{error:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    assert_eq!(agent.reloads.load(Ordering::SeqCst), 0);
}

/// Another writer saves while the mouse takes the DPI: the save is refused
/// and keeps the other writer's file, and nothing is reloaded.
#[tokio::test]
async fn a_file_changed_after_it_was_read_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    let agent = Agent::new(&path, true);
    let external = "schema_version = 11\nselected_device = \"external\"\n";
    let writer = path.clone();
    publish_dpi_mouse(
        &agent.shared().await.channel_registry,
        agent.route.clone(),
        move || {
            std::fs::write(&writer, external).unwrap();
            true
        },
    )
    .await;

    let error = agent
        .settings
        .set_dpi(&agent.route, Dpi::new(1600))
        .await
        .unwrap_err();

    assert!(
        matches!(error, SettingError::NotSaved(ConfigError::Conflict { .. })),
        "{error:?}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), external);
    assert_eq!(agent.reloads.load(Ordering::SeqCst), 0);
    assert_eq!(agent.device_dpi().await, Dpi::new(1600));
}

/// No channel serves the offline mouse, so any write to it would fail the
/// change: its DPI and wheel are saved under its identity and reloaded, for
/// the reapply to write when it reconnects.
#[tokio::test]
async fn an_offline_device_is_only_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    let agent = Agent::new(&path, false);
    let wheel = SmartShiftStatus {
        mode: SmartShiftMode::Free,
        auto_disengage: SmartShiftAutoDisengage::try_from(20).unwrap(),
        tunable_torque: None,
    };

    agent
        .settings
        .set_dpi(&agent.route, Dpi::new(1600))
        .await
        .unwrap();
    agent
        .settings
        .set_smartshift(&agent.route, wheel)
        .await
        .unwrap();
    agent
        .settings
        .set_enabled(&agent.route, false)
        .await
        .unwrap();

    let saved = Config::load_from_path(&path).unwrap();
    assert_eq!(saved.dpi(KEY), Some(Dpi::new(1600)));
    let saved_wheel: SmartShift = saved.smartshift(KEY).unwrap();
    assert_eq!(saved_wheel.mode, WheelMode::Free);
    assert_eq!(u8::from(saved_wheel.auto_disengage), 20);
    assert!(!saved.device_enabled(KEY));
    assert_eq!(saved.devices.len(), 2, "{:?}", saved.devices.keys());
    assert_eq!(agent.reloads.load(Ordering::SeqCst), 3);
    let orchestrator = agent.orchestrator.lock().await;
    assert_eq!(orchestrator.config.dpi(KEY), Some(Dpi::new(1600)));
    assert!(!orchestrator.config.device_enabled(KEY));
}

/// An offline keyboard's Fn lock and disabled keys are saved for the
/// reapply to write when it reconnects; a disabled key this version does not
/// know is not saved.
#[tokio::test]
async fn an_offline_keyboards_fn_lock_and_disabled_keys_are_only_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    let agent = Agent::new(&path, false);
    let unknown = DisableKeysMask::from_bits_retain(0x80);

    agent
        .settings
        .set_fn_lock(&agent.route, true)
        .await
        .unwrap();
    agent
        .settings
        .set_disabled_keys(&agent.route, DisableKeysMask::CAPS_LOCK | unknown)
        .await
        .unwrap();

    let saved = Config::load_from_path(&path).unwrap();
    assert_eq!(saved.fn_lock(KEY), Some(true));
    assert_eq!(
        saved.disabled_keys(KEY),
        Some(&BTreeSet::from([DisableKey::CapsLock]))
    );
    assert_eq!(agent.reloads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn an_unknown_route_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    let agent = Agent::new(&path, true);
    let before = std::fs::read_to_string(&path).unwrap();
    let elsewhere = DeviceRoute::Bolt {
        receiver_uid: "AA00".to_string(),
        slot: 2,
    };

    let error = agent
        .settings
        .set_enabled(&elsewhere, false)
        .await
        .unwrap_err();

    assert!(matches!(error, SettingError::UnknownDevice(_)), "{error:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}
