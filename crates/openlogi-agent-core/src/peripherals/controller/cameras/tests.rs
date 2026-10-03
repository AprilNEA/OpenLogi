use super::*;
use openlogi_core::config::Config;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

fn camera(id: &str) -> Camera {
    Camera {
        name: id.into(),
        unique_id: id.into(),
        serial_number: None,
        vendor_id: 0xffff,
        product_id: 1,
        max_resolution: None,
        max_fps: None,
    }
}

fn reconcile(cameras: &mut Cameras, handle: &Handle, generation: &mut u64) {
    let mut sources = crate::peripherals::sources::Sources::new().unwrap();
    let discovery = handle.cameras.borrow().clone();
    sources
        .observe(
            &[],
            discovery
                .result
                .as_ref()
                .and_then(|r| r.as_ref().ok())
                .map_or(&[], Vec::as_slice),
        )
        .unwrap();
    reconcile_catalog(cameras, handle, generation, &sources.catalog);
}

fn reconcile_catalog(
    cameras: &mut Cameras,
    handle: &Handle,
    generation: &mut u64,
    catalog: &crate::peripherals::catalog::Catalog,
) {
    let (_, gate) = openlogi_hid::device_io_channel();
    cameras.reconcile(
        Context {
            handle,
            desired: &handle.desired.borrow().clone(),
            gate: &gate,
            catalog,
        },
        generation,
    );
}

#[tokio::test]
async fn an_external_camera_descriptor_keeps_the_attachment_and_native_settings() {
    use crate::peripherals::catalog::Registration;
    use openlogi_core::peripheral::{ConfigScope, DriverSelection, DriverSource, PeripheralConfig};
    let descriptor = openlogi_plugin::descriptor::Descriptor::parse(
        r#"
schema = 1
id = "example.camera"
revision = "1.0.0"
model = "example.camera"
name = "External camera descriptor"
driver = { id = "org.openlogi.uvc", parameters = 1 }
platforms = ["macos", "linux", "windows"]
identity = { strategy = "builtin", default_scope = "physical" }
[[matches]]
role = "camera"
transport = "uvc"
vendor_id = 65535
product_id = 1
"#,
    )
    .unwrap();
    let source = DriverSource::Descriptor("camera.toml".into());
    let registration = Registration {
        selection: DriverSelection {
            descriptor: descriptor.id().clone(),
            driver: descriptor.driver().id.clone(),
            digest: None,
            source: source.clone(),
        },
        descriptor,
    };
    let mut sources = crate::peripherals::sources::Sources::new().unwrap();
    let device = camera("same-camera");
    sources.observe(&[], std::slice::from_ref(&device)).unwrap();
    sources
        .catalog
        .replace_source(&source, vec![registration.clone()])
        .unwrap();
    let mut config = Config::ephemeral();
    config.set_camera_controls(
        &device.config_key(),
        CameraControls(BTreeMap::from([("brightness".into(), 42)])),
    );
    let handle = Handle::new(&config);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let capture = Arc::clone(&calls);
    let mut cameras = Cameras {
        io: Arc::new(move |_, settings, check, _| {
            check()?;
            capture.lock().unwrap().push(settings.cloned());
            Ok(())
        }),
        ..Cameras::default()
    };
    handle.discover_cameras(Ok(vec![device]));
    let mut generation = 0;
    reconcile_catalog(&mut cameras, &handle, &mut generation, &sources.catalog);
    let completed = cameras.next().await;
    cameras.complete(completed, &handle);
    let original = cameras.records().next().unwrap();
    config.peripherals.push(PeripheralConfig {
        id: "camera-selection".into(),
        enabled: true,
        scope: ConfigScope::Session(original.session.clone()),
        descriptor: registration.descriptor.id().clone(),
        capabilities: BTreeMap::new(),
    });
    handle.reload(&config);
    reconcile_catalog(&mut cameras, &handle, &mut generation, &sources.catalog);
    let completed = cameras.next().await;
    cameras.complete(completed, &handle);
    reconcile_catalog(&mut cameras, &handle, &mut generation, &sources.catalog);
    let record = cameras.records().next().unwrap();
    assert_eq!(record.session, original.session);
    assert_eq!(record.driver, registration.selection);
    assert_eq!(&record.model, registration.descriptor.model());
    assert!(
        cameras.work.is_empty(),
        "a descriptor model change is not a new attachment"
    );
    config.peripherals[0].enabled = false;
    handle.reload(&config);
    reconcile_catalog(&mut cameras, &handle, &mut generation, &sources.catalog);
    assert!(cameras.work.is_empty());
    assert_eq!(
        cameras.records().next().unwrap().operations[0].application,
        ApplicationStatus::Disabled
    );
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .all(|settings| settings.as_ref().unwrap().0["brightness"] == 42)
    );
}

#[tokio::test]
async fn unavailable_and_disabled_selections_do_not_read_or_write_the_camera() {
    use openlogi_core::peripheral::{ConfigScope, DescriptorId, PeripheralConfig};
    let mut config = Config::default();
    let handle = Handle::new(&config);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    let mut cameras = Cameras {
        io: Arc::new(move |_, _, check, _| {
            check()?;
            count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }),
        ..Cameras::default()
    };
    handle.discover_cameras(Ok(vec![camera("selected-camera")]));
    let mut generation = 0;
    reconcile(&mut cameras, &handle, &mut generation);
    let completion = cameras.next().await;
    cameras.complete(completion, &handle);
    let record = cameras.records().next().unwrap();
    config.peripherals.push(PeripheralConfig {
        id: "camera-driver".into(),
        enabled: true,
        scope: ConfigScope::Session(record.session),
        descriptor: DescriptorId::try_new("example.unavailable").unwrap(),
        capabilities: BTreeMap::new(),
    });
    handle.reload(&config);
    reconcile(&mut cameras, &handle, &mut generation);
    assert!(cameras.work.is_empty());
    assert!(matches!(
        cameras.records().next().unwrap().driver_error,
        Some(PeripheralError::DriverUnavailable(_))
    ));
    config.peripherals[0].descriptor = record.driver.descriptor;
    config.peripherals[0].enabled = false;
    handle.reload(&config);
    reconcile(&mut cameras, &handle, &mut generation);
    assert!(cameras.work.is_empty());
    assert_eq!(
        cameras.records().next().unwrap().operations[0].application,
        ApplicationStatus::Disabled
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    config.peripherals[0].enabled = true;
    handle.reload(&config);
    reconcile(&mut cameras, &handle, &mut generation);
    let completion = cameras.next().await;
    cameras.complete(completion, &handle);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "restoring the selection resumes the native adapter"
    );
}

#[tokio::test]
async fn restores_legacy_settings_before_gui_migration_and_prefers_canonical_settings() {
    let camera = camera("legacy-port");
    let mut config = Config::default();
    config.set_camera_controls(
        "camera-legacy-port",
        CameraControls(BTreeMap::from([("brightness".into(), 42)])),
    );
    let handle = Handle::new(&config);
    let observed = Arc::new(Mutex::new(Vec::new()));
    let capture = Arc::clone(&observed);
    let mut cameras = Cameras {
        io: Arc::new(move |_, settings, check, state| {
            check()?;
            capture.lock().unwrap().push(settings.cloned());
            *state = Some(CameraState::default());
            Ok(())
        }),
        ..Cameras::default()
    };
    handle.discover_cameras(Ok(vec![camera.clone()]));
    let mut generation = 0;
    reconcile(&mut cameras, &handle, &mut generation);
    let completion = cameras.next().await;
    cameras.complete(completion, &handle);
    config.set_camera_controls(
        &camera.config_key(),
        CameraControls(BTreeMap::from([("brightness".into(), 17)])),
    );
    handle.reload(&config);
    reconcile(&mut cameras, &handle, &mut generation);
    let completion = cameras.next().await;
    cameras.complete(completion, &handle);
    let values: Vec<_> = observed
        .lock()
        .unwrap()
        .iter()
        .map(|settings| settings.as_ref().unwrap().0["brightness"])
        .collect();
    assert_eq!(values, vec![42, 17]);
    assert!(
        config.camera_controls("camera-legacy-port").is_some(),
        "read-side selection must not delete migration input"
    );
}

#[tokio::test]
async fn a_panicked_camera_task_leaves_another_cameras_pending_work_intact() {
    let handle = Handle::new(&Config::default());
    let (release, wait) = mpsc::channel();
    let wait = Mutex::new(wait);
    let mut cameras = Cameras {
        io: Arc::new(move |camera, _, check, state| {
            assert_ne!(
                camera.unique_id, "failed",
                "synthetic camera worker failure"
            );
            wait.lock().unwrap().recv().unwrap();
            check()?;
            *state = Some(CameraState::default());
            Ok(())
        }),
        ..Cameras::default()
    };
    handle.discover_cameras(Ok(vec![camera("failed"), camera("healthy")]));
    let mut generation = 0;
    reconcile(&mut cameras, &handle, &mut generation);
    let failed = cameras.next().await;
    assert!(
        failed.is_err(),
        "the failed worker must complete before the blocked worker"
    );
    cameras.complete(failed, &handle);
    let healthy = cameras
        .entries
        .values()
        .find(|entry| entry.record.name == "healthy")
        .unwrap();
    assert!(healthy.pending.is_some());
    assert_eq!(
        healthy.record.operations[0].application,
        ApplicationStatus::Pending
    );
    release.send(()).unwrap();
    let completed = cameras.next().await;
    cameras.complete(completed, &handle);
    let healthy = cameras
        .entries
        .values()
        .find(|entry| entry.record.name == "healthy")
        .unwrap();
    assert!(healthy.pending.is_none());
    assert_eq!(
        healthy.record.capabilities[0].evidence,
        CapabilityEvidence::Probed
    );
    reconcile(&mut cameras, &handle, &mut generation);
    assert!(
        cameras.work.is_empty(),
        "unchanged failed settings do not cause an automatic retry loop"
    );
}

#[tokio::test]
async fn reconnect_rejects_old_readback_and_metadata_and_wake_reconcile_the_current_session() {
    let handle = Handle::new(&Config::default());
    let (release, wait) = mpsc::channel();
    let wait = Mutex::new(wait);
    let first = AtomicBool::new(true);
    let mut cameras = Cameras {
        io: Arc::new(move |_, _, _, state| {
            if first.swap(false, Ordering::SeqCst) {
                wait.lock().unwrap().recv().unwrap();
            }
            *state = Some(CameraState::default());
            Ok(())
        }),
        ..Cameras::default()
    };
    let mut generation = 0;
    let mut device = camera("same-os-address");
    handle.discover_cameras(Ok(vec![device.clone()]));
    reconcile(&mut cameras, &handle, &mut generation);
    let old = cameras.records().next().unwrap().session;
    handle.discover_cameras(Ok(Vec::new()));
    reconcile(&mut cameras, &handle, &mut generation);
    device.name = "Reconnected camera".into();
    handle.discover_cameras(Ok(vec![device]));
    reconcile(&mut cameras, &handle, &mut generation);
    let replacement = cameras.records().next().unwrap();
    assert!(replacement.session.generation > old.generation);
    assert_eq!(replacement.name, "Reconnected camera");
    release.send(()).unwrap();
    let completed = cameras.next().await;
    cameras.complete(completed, &handle);
    let record = cameras.records().next().unwrap();
    assert!(
        matches!(&record.capabilities[0].capability, Capability::Camera(camera) if camera.state.is_none())
    );
    reconcile(&mut cameras, &handle, &mut generation);
    let completed = cameras.next().await;
    cameras.complete(completed, &handle);
    assert_eq!(
        cameras.records().next().unwrap().capabilities[0].evidence,
        CapabilityEvidence::Probed
    );
    handle.wake();
    reconcile(&mut cameras, &handle, &mut generation);
    assert_eq!(cameras.work.len(), 1);
    cameras.stop().await;
}
