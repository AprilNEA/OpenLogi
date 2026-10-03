use super::*;
use openlogi_core::peripheral::{EndpointId, HidUsage, Transport};

const MANIFEST: &str =
    include_str!("../../../../examples/plugins/counter-button/package/plugin.toml");
const COMPONENT: &[u8] =
    include_bytes!("../../../../examples/plugins/counter-button/package/driver.wasm");
const BOUNDARIES: &[u8] = include_bytes!("../../tests/fixtures/counter-boundaries.wasm");

fn context(generation: u64, manifest: &Manifest) -> SessionContext {
    let role = ControlId::try_new("controls").unwrap();
    let endpoint = Endpoint {
        id: EndpointId(format!("test:{generation}")),
        parent: None,
        transport: Some(Transport::UsbHid),
        vendor_id: 65535,
        product_id: 1,
        collection: Some(HidUsage {
            page: 0xff00,
            usage: 1,
        }),
        interface: None,
        report_ids: vec![1],
        max_report_bytes: Some(3),
        max_output_report_bytes: None,
        max_feature_report_bytes: None,
    };
    SessionContext {
        session: SessionId {
            endpoint: endpoint.id.clone(),
            generation,
        },
        model: ModelId::try_new("example.counter-button").unwrap(),
        endpoints: BTreeMap::from([(role, endpoint)]),
        permissions: manifest.permissions.clone(),
        settings: BTreeMap::from([("minimum_count".into(), SettingValue::Integer(2))]),
        native_keys: BTreeMap::new(),
    }
}

fn report(value: u8) -> wire::Event {
    wire::Event::Report(wire::DeviceReport {
        endpoint: "controls".into(),
        report: wire::Report {
            kind: wire::ReportKind::Input,
            id: 1,
            payload: vec![value],
        },
    })
}

#[tokio::test]
async fn real_component_state_is_isolated_and_replacement_changes_the_algorithm() {
    let runtime = Runtime::new().unwrap();
    let manifest = Arc::new(Manifest::parse(MANIFEST).unwrap());
    let compiled = runtime.compile(COMPONENT.to_vec()).await.unwrap();
    let mut first = compiled
        .attach(
            Arc::clone(&manifest),
            context(1, &manifest),
            BTreeMap::new(),
        )
        .await
        .unwrap();
    let mut second = compiled
        .attach(
            Arc::clone(&manifest),
            context(2, &manifest),
            BTreeMap::new(),
        )
        .await
        .unwrap();
    assert!(matches!(
        first.pending().as_slice(),
        [HostRequest::Subscribe { .. }]
    ));
    for value in [1, 1, 0] {
        assert!(first.event(report(value)).await.unwrap().updates.is_empty());
    }
    assert!(
        second.event(report(1)).await.unwrap().updates.is_empty(),
        "stores must not share their counter"
    );
    let mut denied = context(4, &manifest);
    denied.permissions.clear();
    assert!(
        matches!(
            compiled
                .attach(Arc::clone(&manifest), denied, BTreeMap::new())
                .await,
            Err(PeripheralError::PermissionDenied(_))
        ),
        "a second session must not inherit another store's endpoint grants"
    );
    let result = first.event(report(1)).await.unwrap();
    assert!(matches!(
        result.updates.as_slice(),
        [wire::Update::Input(wire::InputEvent {
            transition: wire::Transition::Trigger,
            ..
        })]
    ));
    first.detach(wire::DetachReason::Replaced).await.unwrap();
    let replacement = runtime.compile(BOUNDARIES.to_vec()).await.unwrap();
    let mut changed = replacement
        .attach(
            Arc::clone(&manifest),
            context(3, &manifest),
            BTreeMap::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        changed.event(report(1)).await.unwrap().updates.len(),
        1,
        "different guest bytes implement different counting logic without rebuilding the host"
    );
}

#[tokio::test]
async fn faults_revoke_one_store_and_leave_other_devices_running() {
    let runtime = Runtime::new().unwrap();
    let manifest = Arc::new(Manifest::parse(MANIFEST).unwrap());
    let component = runtime.compile(BOUNDARIES.to_vec()).await.unwrap();
    for byte in [250, 251, 252, 253, 254] {
        let mut bad = component
            .attach(
                Arc::clone(&manifest),
                context(u64::from(byte), &manifest),
                BTreeMap::new(),
            )
            .await
            .unwrap();
        let failure = bad
            .event(report(byte))
            .await
            .err()
            .expect("the guest must not escape its budget or grants");
        if byte == 252 {
            assert!(
                matches!(failure, PeripheralError::PermissionDenied(_)),
                "{failure}"
            );
        } else {
            assert!(
                matches!(failure, PeripheralError::ResourceLimit(_)),
                "{failure}"
            );
        }
        assert!(
            matches!(
                bad.event(report(1)).await,
                Err(PeripheralError::PluginFault(_))
            ),
            "a failed store cannot resume with partial input state"
        );
        let mut healthy = component
            .attach(
                Arc::clone(&manifest),
                context(1000 + u64::from(byte), &manifest),
                BTreeMap::new(),
            )
            .await
            .unwrap();
        assert_eq!(healthy.event(report(1)).await.unwrap().updates.len(), 1);
    }
    let untrusted =
        wat::parse_str(r#"(component (import "wasi:cli/environment@0.2.0" (instance)))"#).unwrap();
    let activation = async {
        let untrusted = runtime.compile(untrusted).await?;
        untrusted
            .attach(
                Arc::clone(&manifest),
                context(9, &manifest),
                BTreeMap::new(),
            )
            .await
    }
    .await;
    assert!(
        matches!(activation, Err(PeripheralError::PluginFault(_))),
        "the linker must not supply ambient imports"
    );
}

#[tokio::test]
async fn migration_has_no_device_grants_and_validates_the_returned_schema() {
    let runtime = Runtime::new().unwrap();
    let manifest = Arc::new(Manifest::parse(MANIFEST).unwrap());
    let component = runtime.compile(COMPONENT.to_vec()).await.unwrap();
    let settings = BTreeMap::from([("minimum_count".into(), SettingValue::Integer(3))]);
    assert_eq!(
        component
            .migrate(Arc::clone(&manifest), 1, settings.clone())
            .await
            .unwrap(),
        settings
    );
    assert!(matches!(
        component.migrate(manifest, 99, settings).await,
        Err(PeripheralError::PluginFault(_))
    ));
}
