use super::*;
use openlogi_core::peripheral::{HidUsage, Transport};

#[test]
fn a_model_scoped_replacement_cannot_discard_its_disabled_rule() {
    use openlogi_core::peripheral::{ModelId, PeripheralConfig};
    let source = DriverSource::Descriptor("test.toml".into());
    let entry = registration("replacement", "", source.clone());
    let endpoints = vec![endpoint()];
    let session = SessionId {
        endpoint: endpoints[0].id.clone(),
        generation: 1,
    };
    let mut previous = entry.record(session.clone(), endpoints.clone()).unwrap();
    previous.model = ModelId::try_new("example.previous").unwrap();
    let mut config = openlogi_core::config::Config::ephemeral();
    config.peripherals.push(PeripheralConfig {
        id: "disabled-rule".into(),
        enabled: false,
        scope: ConfigScope::Model(previous.model.clone()),
        descriptor: entry.descriptor.id().clone(),
        capabilities: BTreeMap::new(),
    });
    let mut catalog = Catalog::default();
    catalog
        .replace_source(&source, vec![entry.clone()])
        .unwrap();
    let context = SelectionContext {
        session: &session,
        current: Some(&previous),
        builtin_owner: None,
    };
    assert!(
        matches!(
            catalog.select_with_config(&endpoints, &config, context),
            Err(PeripheralError::InvalidSettings(_))
        ),
        "a mismatched model scope must fail before the replacement starts"
    );
    config.peripherals[0].scope = ConfigScope::Model(entry.descriptor.model().clone());
    let selected = catalog
        .select_with_config(&endpoints, &config, context)
        .unwrap()
        .unwrap();
    let record = selected
        .registration
        .record(session, endpoints.clone())
        .unwrap();
    assert!(!config.peripheral_rule(&record).unwrap().unwrap().enabled);
}

fn registration(id: &str, constraint: &str, source: DriverSource) -> Registration {
    let descriptor = Descriptor::parse(&format!(
        r#"
schema = 1
id = "example.{id}"
revision = "1.0.0"
model = "example.test"
name = "Test"
driver = {{ id = "example.driver", parameters = 1 }}
platforms = ["macos", "linux", "windows"]
identity = {{ strategy = "unverified", default_scope = "session" }}
[[matches]]
role = "buttons"
transport = "usb-hid"
vendor_id = 65535
product_id = 1
usage_page = 12
usage = 1
{constraint}
"#
    ))
    .unwrap();
    let selection = DriverSelection {
        descriptor: descriptor.id().clone(),
        driver: descriptor.driver().id.clone(),
        digest: None,
        source,
    };
    Registration {
        descriptor,
        selection,
    }
}

fn endpoint() -> Endpoint {
    Endpoint {
        id: EndpointId("hid:1/consumer".into()),
        parent: Some(EndpointId("hid:1".into())),
        transport: Some(Transport::UsbHid),
        vendor_id: 65535,
        product_id: 1,
        collection: Some(HidUsage { page: 12, usage: 1 }),
        interface: Some(2),
        report_ids: vec![1],
        max_report_bytes: Some(3),
        max_output_report_bytes: None,
        max_feature_report_bytes: None,
    }
}

#[tokio::test]
async fn compiled_and_loaded_models_use_the_same_selection_and_capability_projection() {
    use openlogi_device_registry::native_remap::NativeRemapDevice;
    let facts = NativeRemapDevice {
        descriptor_id: "example.media-button-usb",
        model_id: "example.media-button",
        name: "Media button",
        vendor_id: 65535,
        product_id: 1,
        usage_page: 12,
        usage: 1,
        control_id: "media",
        label: "Media button",
        label_zh_cn: "媒体键",
        source_page: 12,
        source_usage: 233,
        recommended_key: "F18",
    };
    let descriptor = Descriptor::native(&facts).unwrap();
    let compiled = Registration {
        selection: DriverSelection {
            descriptor: descriptor.id().clone(),
            driver: descriptor.driver().id.clone(),
            digest: None,
            source: DriverSource::Builtin,
        },
        descriptor,
    };
    let mut catalog = Catalog::default();
    catalog
        .replace_source(&DriverSource::Builtin, vec![compiled])
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("devices");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(
        directory.join("example.device.toml"),
        include_str!("../../../../../examples/descriptors/media-button.device.toml"),
    )
    .unwrap();
    let mut loaded = super::super::sources::Sources::new().unwrap();
    loaded
        .reload(
            &openlogi_core::config::Config::ephemeral(),
            &directory,
            &root.path().join("packages"),
            &BTreeMap::new(),
        )
        .await;
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    let endpoints = [endpoint()];
    let select = |catalog: &Catalog| {
        let selected = catalog
            .select(&endpoints, Platform::Macos, None, None)
            .unwrap()
            .unwrap();
        selected
            .registration
            .record(
                SessionId {
                    endpoint: endpoints[0].id.clone(),
                    generation: 1,
                },
                vec![endpoints[0].clone()],
            )
            .unwrap()
    };
    let native = select(&catalog);
    let external = select(&loaded.catalog);
    assert_eq!(native.driver.driver, external.driver.driver);
    assert_eq!(native.capabilities, external.capabilities);
    assert_eq!(native.scopes, external.scopes);
    assert!(matches!(
        external.driver.source,
        DriverSource::Descriptor(_)
    ));
}

#[test]
fn specificity_and_incomparability_do_not_depend_on_load_order() {
    let source = DriverSource::Descriptor("test.toml".into());
    let broad = registration("broad", "", source.clone());
    let interface = registration("interface", "interface = 2", source.clone());
    let report = registration("report", "report_id = 1", source.clone());
    let endpoints = [endpoint()];
    for entries in [
        vec![broad.clone(), interface.clone()],
        vec![interface.clone(), broad.clone()],
    ] {
        let mut catalog = Catalog::default();
        catalog.replace_source(&source, entries).unwrap();
        let selected = catalog
            .select(&endpoints, Platform::Macos, None, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            selected.registration.descriptor.id(),
            interface.descriptor.id()
        );
    }
    for entries in [
        vec![interface.clone(), report.clone()],
        vec![report.clone(), interface.clone()],
    ] {
        let mut catalog = Catalog::default();
        catalog.replace_source(&source, entries).unwrap();
        assert!(matches!(
            catalog.select(&endpoints, Platform::Macos, None, None),
            Err(PeripheralError::DriverConflict(_))
        ));
    }
    let mut unknown = endpoint();
    unknown.interface = None;
    let mut catalog = Catalog::default();
    catalog.replace_source(&source, vec![interface]).unwrap();
    assert!(
        catalog
            .select(&[unknown], Platform::Macos, None, None)
            .unwrap()
            .is_none()
    );
}

#[test]
fn builtin_ownership_and_rejected_updates_preserve_the_active_source() {
    let mut catalog = Catalog::default();
    let builtin = registration("builtin", "", DriverSource::Builtin);
    catalog
        .replace_source(&DriverSource::Builtin, vec![builtin.clone()])
        .unwrap();
    let external_source = DriverSource::Descriptor("override.toml".into());
    let external = registration("external", "interface = 2", external_source.clone());
    catalog
        .replace_source(&external_source, vec![external.clone()])
        .unwrap();
    let endpoints = [endpoint()];
    assert_eq!(
        catalog
            .select(&endpoints, Platform::Macos, None, None)
            .unwrap()
            .unwrap()
            .registration
            .selection,
        builtin.selection
    );
    assert_eq!(
        catalog
            .select(
                &endpoints,
                Platform::Macos,
                None,
                Some(&builtin.selection.driver)
            )
            .unwrap()
            .unwrap()
            .registration
            .selection,
        builtin.selection,
        "loading another descriptor for the same protocol cannot replace its compiled owner"
    );
    assert_eq!(
        catalog
            .select(
                &endpoints,
                Platform::Macos,
                Some(external.descriptor.id()),
                None
            )
            .unwrap()
            .unwrap()
            .registration
            .selection,
        external.selection
    );
    let generation = catalog.generation();
    catalog
        .replace_source(&external_source, vec![builtin])
        .expect_err("external source cannot replace a builtin ID");
    assert_eq!(catalog.generation(), generation);
    assert_eq!(catalog.entries().count(), 2);
    assert!(matches!(
        catalog.select(
            &endpoints,
            Platform::Macos,
            Some(&DescriptorId::try_new("example.missing").unwrap()),
            None
        ),
        Err(PeripheralError::DriverUnavailable(_))
    ));
}

#[test]
fn claims_are_all_or_nothing_and_a_stale_release_cannot_clear_a_successor() {
    let endpoint = EndpointId("hid:1".into());
    let second = EndpointId("hid:2".into());
    let first = SessionId {
        endpoint: endpoint.clone(),
        generation: 1,
    };
    let successor = SessionId {
        generation: 2,
        ..first.clone()
    };
    let other = SessionId {
        endpoint: second.clone(),
        generation: 3,
    };
    let mut claims = Claims::default();
    claims
        .acquire(&first, std::slice::from_ref(&endpoint))
        .unwrap();
    claims
        .acquire(&other, &[second.clone(), endpoint.clone()])
        .expect_err("overlap cannot partially claim a group");
    claims
        .acquire(&successor, std::slice::from_ref(&second))
        .unwrap();
    claims.release(&first);
    claims
        .acquire(&successor, std::slice::from_ref(&endpoint))
        .unwrap();
    claims.release(&first);
    claims
        .acquire(&other, &[endpoint])
        .expect_err("old cleanup must leave successor ownership intact");
}

#[test]
fn an_ambiguous_losing_descriptor_does_not_block_the_selected_owner() {
    let source = DriverSource::Descriptor("broad.toml".into());
    let broad = registration("broad", "", source.clone());
    let builtin = registration("builtin", "interface = 2", DriverSource::Builtin);
    let mut sibling = endpoint();
    sibling.id = EndpointId("hid:1/other".into());
    sibling.interface = Some(3);
    let endpoints = [endpoint(), sibling];
    let mut catalog = Catalog::default();
    catalog
        .replace_source(&source, vec![broad.clone()])
        .unwrap();
    catalog
        .replace_source(&DriverSource::Builtin, vec![builtin.clone()])
        .unwrap();
    assert_eq!(
        catalog
            .select(&endpoints, Platform::Macos, None, None)
            .unwrap()
            .unwrap()
            .registration
            .selection,
        builtin.selection
    );
    assert!(matches!(
        catalog.select(
            &endpoints,
            Platform::Macos,
            Some(broad.descriptor.id()),
            None
        ),
        Err(PeripheralError::DriverConflict(_))
    ));
}

#[test]
fn scope_precedence_and_unavailable_explicit_selection_are_shared_by_adapters() {
    use openlogi_core::{
        config::Config,
        peripheral::{IdentityEvidence, PeripheralConfig, PhysicalDeviceId},
    };
    let source = DriverSource::Descriptor("scopes.toml".into());
    let entries = ["model", "physical", "session"].map(|id| registration(id, "", source.clone()));
    let session = SessionId {
        endpoint: endpoint().id,
        generation: 42,
    };
    let endpoints = [endpoint()];
    let mut record = entries[0]
        .record(session.clone(), endpoints.to_vec())
        .unwrap();
    record.physical = Some(PhysicalDeviceId {
        key: "unit-one".into(),
        evidence: IdentityEvidence::Protocol,
    });
    let mut config = Config::ephemeral();
    for (entry, scope) in entries.iter().zip([
        ConfigScope::Model(record.model.clone()),
        ConfigScope::Physical("unit-one".into()),
        ConfigScope::Session(session.clone()),
    ]) {
        config.peripherals.push(PeripheralConfig {
            id: entry.descriptor.id().to_string(),
            enabled: true,
            scope,
            descriptor: entry.descriptor.id().clone(),
            capabilities: BTreeMap::new(),
        });
    }
    let mut catalog = Catalog::default();
    catalog.replace_source(&source, entries.to_vec()).unwrap();
    for expected in entries.iter().rev() {
        let selected = catalog
            .select_with_config(
                &endpoints,
                &config,
                SelectionContext {
                    session: &session,
                    current: Some(&record),
                    builtin_owner: None,
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(selected.registration.selection, expected.selection);
        config.peripherals.pop();
    }
    config.peripherals.push(PeripheralConfig {
        id: "missing-selection".into(),
        enabled: true,
        scope: ConfigScope::Model(record.model.clone()),
        descriptor: DescriptorId::try_new("example.missing").unwrap(),
        capabilities: BTreeMap::new(),
    });
    assert!(matches!(
        catalog.select_with_config(
            &endpoints,
            &config,
            SelectionContext {
                session: &session,
                current: Some(&record),
                builtin_owner: None,
            }
        ),
        Err(PeripheralError::DriverUnavailable(_))
    ));
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one table-driven scenario checks compiled and file-loaded protocol registrations"
)]
async fn compiled_protocols_and_loaded_descriptors_share_selection_and_validation() {
    use openlogi_device_registry::driver::BuiltinDriver;
    use openlogi_plugin::selector::Selector;
    let litra = &openlogi_device_registry::litra::LITRA_DEVICES[0];
    let cases = [
        (
            BuiltinDriver::Hidpp,
            Transport::UsbHid,
            0x046d,
            0xc548,
            Some(0xff00),
            Some(2),
        ),
        (
            BuiltinDriver::Litra,
            Transport::UsbHid,
            litra.vendor_id,
            litra.product_id,
            Some(litra.usage_page),
            Some(litra.usage_id),
        ),
        (BuiltinDriver::Camera, Transport::Uvc, 65535, 1, None, None),
    ];
    for (driver, transport, vendor_id, product_id, usage_page, usage) in cases {
        let selector = Selector {
            role: ControlId::try_new("controls").unwrap(),
            transport,
            vendor_id,
            product_id,
            usage_page,
            usage,
            interface: None,
            report_id: None,
        };
        let compiled = Descriptor::protocol(driver, selector).unwrap();
        let transport_name = if transport == Transport::Uvc {
            "uvc"
        } else {
            "usb-hid"
        };
        let collection = usage_page
            .zip(usage)
            .map_or_else(String::new, |(page, usage)| {
                format!("usage_page = {page}\nusage = {usage}")
            });
        let external = format!(
            r#"
schema = 1
id = "example.protocol"
revision = "1.0.0"
model = "{}"
name = "External protocol descriptor"
driver = {{ id = "{}", parameters = 1 }}
platforms = ["macos", "linux", "windows"]
identity = {{ strategy = "builtin", default_scope = "physical" }}
[[matches]]
role = "controls"
transport = "{transport_name}"
vendor_id = {vendor_id}
product_id = {product_id}
{collection}
"#,
            compiled.model(),
            driver.id()
        );
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("protocol.device.toml"), external).unwrap();
        let mut sources = super::super::sources::Sources::new().unwrap();
        let mut builtins: BTreeMap<_, _> = sources
            .catalog
            .entries()
            .map(|entry| (entry.descriptor.id().clone(), entry.clone()))
            .collect();
        builtins.insert(
            compiled.id().clone(),
            Registration {
                selection: DriverSelection {
                    descriptor: compiled.id().clone(),
                    driver: compiled.driver().id.clone(),
                    digest: None,
                    source: DriverSource::Builtin,
                },
                descriptor: compiled.clone(),
            },
        );
        sources
            .catalog
            .replace_source(&DriverSource::Builtin, builtins.into_values().collect())
            .unwrap();
        sources
            .reload(
                &openlogi_core::config::Config::ephemeral(),
                root.path(),
                &root.path().join("packages"),
                &BTreeMap::new(),
            )
            .await;
        assert!(sources.diagnostics.is_empty(), "{:?}", sources.diagnostics);
        let endpoints = [Endpoint {
            transport: Some(transport),
            vendor_id,
            product_id,
            collection: usage_page
                .zip(usage)
                .map(|(page, usage)| HidUsage { page, usage }),
            ..endpoint()
        }];
        let owner = &compiled.driver().id;
        let default = sources
            .catalog
            .select(&endpoints, Platform::Macos, None, Some(owner))
            .unwrap()
            .unwrap();
        assert_eq!(default.registration.descriptor, compiled);
        let selected = sources
            .catalog
            .select(
                &endpoints,
                Platform::Macos,
                Some(&DescriptorId::try_new("example.protocol").unwrap()),
                Some(owner),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            selected
                .registration
                .descriptor
                .protocol_parameters()
                .unwrap(),
            driver
        );
        assert!(matches!(
            selected.registration.selection.source,
            DriverSource::Descriptor(_)
        ));
    }
}

#[test]
fn a_late_compiled_identity_collision_rejects_the_whole_observation() {
    use openlogi_device_registry::driver::BuiltinDriver;
    use openlogi_plugin::selector::Selector;
    let camera = openlogi_core::camera::Camera {
        name: "Synthetic camera".into(),
        unique_id: "synthetic".into(),
        serial_number: None,
        vendor_id: 65535,
        product_id: 1,
        max_resolution: None,
        max_fps: None,
    };
    let descriptor = Descriptor::protocol(
        BuiltinDriver::Camera,
        Selector {
            role: ControlId::try_new("camera").unwrap(),
            transport: Transport::Uvc,
            vendor_id: camera.vendor_id,
            product_id: camera.product_id,
            usage_page: None,
            usage: None,
            interface: None,
            report_id: None,
        },
    )
    .unwrap();
    let source = DriverSource::Descriptor("early.device.toml".into());
    let mut sources = super::super::sources::Sources::new().unwrap();
    sources
        .catalog
        .replace_source(
            &source,
            vec![Registration {
                selection: DriverSelection {
                    descriptor: descriptor.id().clone(),
                    driver: descriptor.driver().id.clone(),
                    digest: None,
                    source: source.clone(),
                },
                descriptor,
            }],
        )
        .unwrap();
    let generation = sources.catalog.generation();
    assert!(matches!(
        sources.observe(&[], &[camera]),
        Err(PeripheralError::DriverConflict(_))
    ));
    assert_eq!(
        sources.catalog.generation(),
        generation,
        "rejected observations cannot partly activate their compiled registrations"
    );
}
