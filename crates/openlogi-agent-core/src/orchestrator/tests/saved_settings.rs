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
use openlogi_hid::{HidppOperation, WriteError};
use openlogi_ipc::ConfigReloadError;
use tokio::sync::Mutex;

use super::*;
use crate::orchestrator::{ConfigReload, DeviceSettings, SettingError};
use crate::receiver_access::ExclusiveAccessReason;
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

/// A keyboard at `route` in `registry` with `0x40a2` Fn inversion, which
/// keeps the state written and starts with Fn lock off. Counts its writes.
async fn publish_fn_lock_keyboard(
    registry: &openlogi_hid::ChannelRegistry,
    route: DeviceRoute,
) -> Arc<AtomicUsize> {
    const FN_INVERSION: u8 = 5;
    let writes = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&writes);
    // Inversion on: the bare F-keys run their special functions.
    let inversion = std::sync::Mutex::new(1u8);
    openlogi_device::test_support::publish_scripted_channel(
        registry,
        "fn-lock-keyboard",
        route,
        move |request| {
            let mut response = vec![0; 20];
            response[..4].copy_from_slice(request.get(..4)?);
            response[0] = 0x11;
            let mut inversion = inversion.lock().unwrap();
            match (request[2], request[3] >> 4) {
                (0, 1) => response[4] = 4,
                (0, 0) => {
                    response[4] = if request[4..6] == [0x40, 0xa2] {
                        FN_INVERSION
                    } else {
                        0
                    }
                }
                (FN_INVERSION, 0) => response[4..6].copy_from_slice(&[*inversion, 1]),
                (FN_INVERSION, 1) => {
                    counted.fetch_add(1, Ordering::SeqCst);
                    *inversion = request[4];
                    response[4..6].copy_from_slice(&[*inversion, 1]);
                }
                _ => return None,
            }
            Some(response)
        },
    )
    .await;
    writes
}

/// A Fn-lock change a newer write superseded before its turn writes and
/// saves nothing, even when the keyboard already has the state it asked for:
/// the newer write decides the state, and the file must not say otherwise.
#[tokio::test]
async fn a_superseded_fn_lock_is_not_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    let agent = Agent::new(&path, true);
    let shared = agent.shared().await;
    let writes = publish_fn_lock_keyboard(&shared.channel_registry, agent.route.clone()).await;
    let before = std::fs::read_to_string(&path).unwrap();
    let lease = shared
        .receiver_access
        .acquire_exclusive(ExclusiveAccessReason::Pairing)
        .await;

    let (older, newer) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        // The keyboard starts with Fn lock off, which the older change asks.
        let settings = agent.settings.clone();
        let route = agent.route.clone();
        let older = tokio::spawn(async move { settings.set_fn_lock(&route, false).await });
        while shared.fn_lock_order.requested(&agent.route) == 0 {
            assert!(
                !older.is_finished(),
                "the older change ended before its write: {:?}",
                older.await
            );
            tokio::task::yield_now().await;
        }
        let mut newer = std::pin::pin!(shared.set_fn_lock(&agent.route, true));
        assert!(futures_lite::future::poll_once(&mut newer).await.is_none());
        drop(lease);
        (older.await.unwrap(), newer.await)
    })
    .await
    .expect("both Fn-lock writes finish");

    let error = older.unwrap_err();
    assert!(
        matches!(
            error,
            SettingError::Write(WriteError::WriteSuperseded {
                operation: HidppOperation::WriteFnLock,
            })
        ),
        "{error:?}"
    );
    assert!(newer.unwrap().fn_lock);
    assert_eq!(writes.load(Ordering::SeqCst), 1, "only the newer write");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    assert_eq!(agent.reloads.load(Ordering::SeqCst), 0);
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
