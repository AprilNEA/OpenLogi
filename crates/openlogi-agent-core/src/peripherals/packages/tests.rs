use super::*;
use openlogi_core::peripheral::*;
use openlogi_plugin::{
    descriptor::Descriptor,
    runtime::{SessionContext, wire},
};
use std::{fs, path::Path};

const MANIFEST: &str =
    include_str!("../../../../../examples/plugins/counter-button/package/plugin.toml");
const DESCRIPTOR: &str =
    include_str!("../../../../../examples/plugins/counter-button/package/counter.device.toml");
const COMPONENT: &[u8] =
    include_bytes!("../../../../../examples/plugins/counter-button/package/driver.wasm");
const ALTERNATE: &[u8] =
    include_bytes!("../../../../openlogi-plugin/tests/fixtures/counter-boundaries.wasm");

struct Harness {
    _directory: tempfile::TempDir,
    paths: Paths,
    packages: Packages,
    sources: Sources,
}

impl Harness {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: directory.path().join(openlogi_core::paths::CONFIG_FILE),
            descriptors: directory.path().join("devices.d"),
            packages: directory.path().join("packages"),
            state: directory.path().join("state"),
        };
        fs::create_dir(&paths.descriptors).unwrap();
        let packages = Packages::load(&paths).unwrap();
        Self {
            _directory: directory,
            paths,
            packages,
            sources: Sources::new().unwrap(),
        }
    }

    fn config(&self) -> Config {
        ConfigFile::load_from_path(&self.paths.config).unwrap().0
    }

    async fn reload(&mut self) {
        self.sources
            .reload(
                &self.config(),
                &self.paths.descriptors,
                &self.paths.packages,
                self.packages.grants(),
            )
            .await;
    }

    async fn command(&mut self, command: PluginCommand) -> Result<(), PeripheralError> {
        let mut prepared = self
            .packages
            .prepare(command, &mut self.sources, &self.paths)
            .await?;
        self.packages.commit(&mut prepared, &self.paths)?;
        self.reload().await;
        Ok(())
    }

    async fn install(&mut self, version: &str, schema: u32, component: &[u8]) -> String {
        let staging = tempfile::tempdir().unwrap();
        write_package(staging.path(), version, schema, component);
        let digest = Package::read(staging.path()).unwrap().digest().to_owned();
        self.command(PluginCommand::Install {
            path: staging.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
        digest
    }

    fn enable(&self, digest: &str) -> PluginCommand {
        let package = self
            .sources
            .installed
            .iter()
            .find(|p| p.digest == digest)
            .unwrap();
        PluginCommand::Enable {
            digest: digest.into(),
            descriptors: package
                .descriptors
                .iter()
                .map(|(id, details)| (id.clone(), details.fingerprint.clone()))
                .collect(),
        }
    }

    async fn first_press(&self) -> usize {
        let plugin = self.sources.plugins.values().next().unwrap();
        let descriptor = &plugin.package.descriptors()[0];
        let selector = &descriptor.selectors()[0];
        let endpoint = Endpoint {
            id: EndpointId("test:counter".into()),
            parent: None,
            transport: Some(selector.transport),
            vendor_id: selector.vendor_id,
            product_id: selector.product_id,
            collection: Some(HidUsage {
                page: selector.usage_page.unwrap(),
                usage: selector.usage.unwrap(),
            }),
            interface: None,
            report_ids: vec![1],
            max_report_bytes: Some(3),
            max_output_report_bytes: None,
            max_feature_report_bytes: None,
        };
        let mut instance = plugin
            .component
            .attach(
                Arc::clone(&plugin.manifest),
                SessionContext {
                    session: SessionId {
                        endpoint: endpoint.id.clone(),
                        generation: 1,
                    },
                    model: descriptor.model().clone(),
                    endpoints: BTreeMap::from([(selector.role.clone(), endpoint)]),
                    permissions: plugin.manifest.permissions.clone(),
                    settings: BTreeMap::from([("minimum_count".into(), SettingValue::Integer(2))]),
                    native_keys: BTreeMap::new(),
                },
                BTreeMap::new(),
            )
            .await
            .unwrap();
        instance
            .event(wire::Event::Report(wire::DeviceReport {
                endpoint: "controls".into(),
                report: wire::Report {
                    kind: wire::ReportKind::Input,
                    id: 1,
                    payload: vec![1],
                },
            }))
            .await
            .unwrap()
            .updates
            .len()
    }
}

fn write_package(path: &Path, version: &str, schema: u32, component: &[u8]) {
    fs::write(
        path.join("plugin.toml"),
        MANIFEST
            .replace("version = \"0.1.0\"", &format!("version = \"{version}\""))
            .replace(
                "settings_schema = 1",
                &format!("settings_schema = {schema}"),
            ),
    )
    .unwrap();
    fs::write(path.join("counter.device.toml"), DESCRIPTOR).unwrap();
    fs::write(path.join("driver.wasm"), component).unwrap();
}

#[tokio::test]
async fn installation_is_inert_and_enable_requires_the_reviewed_descriptor_content() {
    let mut harness = Harness::new();
    let digest = harness.install("0.1.0", 1, COMPONENT).await;
    assert!(harness.config().plugins.is_empty());
    assert!(harness.sources.plugins.is_empty());
    assert_eq!(harness.sources.installed.len(), 1);
    let mut wrong = harness.enable(&digest);
    let PluginCommand::Enable { descriptors, .. } = &mut wrong else {
        unreachable!()
    };
    *descriptors.values_mut().next().unwrap() = "0".repeat(64);
    assert!(matches!(
        harness.command(wrong).await,
        Err(PeripheralError::PermissionDenied(_))
    ));
    assert!(harness.packages.grants().is_empty());
    harness.command(harness.enable(&digest)).await.unwrap();
    assert_eq!(harness.sources.plugins.len(), 1);
    let record = &harness.sources.installed[0];
    assert_eq!(record.selection, PluginSelectionStatus::Enabled);
    assert!(!record.active);
    assert!(record.descriptors.values().all(|d| d.granted));
}

#[tokio::test]
async fn invalid_active_edits_retain_the_source_but_revoked_or_missing_pins_never_execute() {
    let mut harness = Harness::new();
    let path = harness.paths.descriptors.join("external.device.toml");
    let external = DESCRIPTOR.replace(
        "dev.example.counter-button.test-device",
        "dev.example.external",
    );
    fs::write(&path, &external).unwrap();
    let digest = harness.install("0.1.0", 1, COMPONENT).await;
    harness.command(harness.enable(&digest)).await.unwrap();
    let accepted = Descriptor::parse(&external).unwrap();
    for edited in [
        external.replace("product_id = 1", "product_id = 2"),
        external.replace(
            "id = \"dev.example.counter-button\"",
            "id = \"dev.example.missing\"",
        ),
    ] {
        fs::write(&path, edited).unwrap();
        harness.reload().await;
        assert_eq!(
            harness
                .sources
                .catalog
                .entries()
                .find(|entry| entry.descriptor.id() == accepted.id())
                .unwrap()
                .descriptor,
            accepted
        );
        assert!(harness.sources.diagnostics.iter().any(|d| d.retained));
        assert_eq!(harness.sources.plugins.len(), 1);
    }
    let driver = harness.sources.installed[0].driver.clone();
    harness
        .command(PluginCommand::Disable {
            driver: driver.clone(),
        })
        .await
        .unwrap();
    assert!(harness.sources.plugins.is_empty());
    assert!(
        !harness
            .sources
            .catalog
            .entries()
            .any(|entry| entry.selection.driver == driver)
    );
    fs::write(&path, external).unwrap();
    harness.reload().await;
    harness.command(harness.enable(&digest)).await.unwrap();
    let package = Package::installed(&harness.paths.packages, &digest).unwrap();
    package.remove(&harness.paths.packages).unwrap();
    harness.reload().await;
    assert!(harness.sources.plugins.is_empty());
    assert!(
        !harness
            .sources
            .catalog
            .entries()
            .any(|entry| entry.selection.driver == driver)
    );
    assert!(
        harness.config().plugins[&driver].enabled,
        "missing bytes do not silently rewrite the user's selection"
    );
}

#[tokio::test]
async fn concurrent_config_edits_abort_activation_and_rollback_without_losing_the_edit() {
    let mut harness = Harness::new();
    let digest = harness.install("0.1.0", 1, COMPONENT).await;
    let mut prepared = harness
        .packages
        .prepare(
            harness.enable(&digest),
            &mut harness.sources,
            &harness.paths,
        )
        .await
        .unwrap();
    let (mut external, mut file) = ConfigFile::load_from_path(&harness.paths.config).unwrap();
    external.selected_device = Some("external".into());
    file.save(&external).unwrap();
    assert!(matches!(
        harness.packages.commit(&mut prepared, &harness.paths),
        Err(PeripheralError::ConfigConflict)
    ));
    assert_eq!(harness.config().selected_device, external.selected_device);
    assert!(harness.packages.grants().is_empty());
    let mut prepared = harness
        .packages
        .prepare(
            harness.enable(&digest),
            &mut harness.sources,
            &harness.paths,
        )
        .await
        .unwrap();
    harness
        .packages
        .commit(&mut prepared, &harness.paths)
        .unwrap();
    let (mut external, mut file) = ConfigFile::load_from_path(&harness.paths.config).unwrap();
    external.selected_device = Some("later-edit".into());
    file.save(&external).unwrap();
    assert!(matches!(
        harness.packages.rollback_failed_activation(&mut prepared),
        Err(PeripheralError::ConfigConflict)
    ));
    assert_eq!(harness.config().selected_device, external.selected_device);
}

#[tokio::test]
async fn actual_component_upgrade_migrates_settings_and_rollback_restores_the_old_algorithm() {
    let mut harness = Harness::new();
    let first = harness.install("0.1.0", 1, COMPONENT).await;
    harness.command(harness.enable(&first)).await.unwrap();
    assert_eq!(harness.first_press().await, 0);
    let descriptor = Descriptor::parse(DESCRIPTOR).unwrap();
    let (mut config, mut file) = ConfigFile::load_from_path(&harness.paths.config).unwrap();
    config.peripherals.push(PeripheralConfig {
        id: "counter".into(),
        enabled: true,
        scope: ConfigScope::Model(descriptor.model().clone()),
        descriptor: descriptor.id().clone(),
        capabilities: BTreeMap::from([(
            CapabilityId::try_new("extension/main").unwrap(),
            CapabilitySettings {
                version: 1,
                bindings: BTreeMap::new(),
                values: BTreeMap::from([("minimum_count".into(), SettingValue::Integer(2))]),
            },
        )]),
    });
    file.save(&config).unwrap();
    let second = harness.install("0.2.0", 2, ALTERNATE).await;
    harness.command(harness.enable(&second)).await.unwrap();
    assert_eq!(harness.first_press().await, 1);
    assert_eq!(harness.config().peripherals, config.peripherals);
    let driver = descriptor.driver().id.clone();
    assert_eq!(harness.config().plugins[&driver].settings_schema, 2);
    let unsupported = harness.install("0.3.0", 3, COMPONENT).await;
    assert!(matches!(
        harness.command(harness.enable(&unsupported)).await,
        Err(PeripheralError::PluginFault(_))
    ));
    assert_eq!(harness.config().plugins[&driver].digest, second);
    harness
        .command(PluginCommand::Rollback {
            driver: driver.clone(),
        })
        .await
        .unwrap();
    assert_eq!(harness.config().plugins[&driver].digest, first);
    assert_eq!(harness.first_press().await, 0);
    assert_eq!(harness.config().peripherals, config.peripherals);
}
