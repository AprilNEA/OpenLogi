//! Execute the example component with synthetic reports and report worker round-trip latency.

use std::{collections::BTreeMap, sync::Arc, time::Instant};

use openlogi_core::peripheral::{Endpoint, EndpointId, HidUsage, SessionId};
use openlogi_plugin::{
    descriptor::Descriptor,
    manifest::{Manifest, Permission, validate_values},
    runtime::{Runtime, SessionContext, wire},
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = Arc::new(Manifest::parse(include_str!(
        "../../../examples/plugins/counter-button/package/plugin.toml"
    ))?);
    let descriptor = Descriptor::parse(include_str!(
        "../../../examples/plugins/counter-button/package/counter.device.toml"
    ))?;
    let selector = descriptor.selectors().first().ok_or("missing selector")?;
    let Permission::Hid {
        report_ids,
        max_report_bytes,
        ..
    } = manifest
        .permissions
        .first()
        .ok_or("missing HID permission")?
    else {
        return Err("the example requires HID reports".into());
    };
    let report_id = *report_ids.first().ok_or("missing report ID")?;
    let endpoint = Endpoint {
        id: EndpointId("synthetic:runtime-probe".into()),
        parent: None,
        transport: Some(selector.transport),
        vendor_id: selector.vendor_id,
        product_id: selector.product_id,
        collection: Some(HidUsage {
            page: selector.usage_page.ok_or("missing usage page")?,
            usage: selector.usage.ok_or("missing usage")?,
        }),
        interface: selector.interface,
        report_ids: report_ids.clone(),
        max_report_bytes: Some(*max_report_bytes),
        max_output_report_bytes: None,
        max_feature_report_bytes: None,
    };
    let context = SessionContext {
        session: SessionId {
            endpoint: endpoint.id.clone(),
            generation: 1,
        },
        model: descriptor.model().clone(),
        endpoints: BTreeMap::from([(selector.role.clone(), endpoint)]),
        permissions: manifest.permissions.clone(),
        settings: validate_values(&manifest.settings, &BTreeMap::new(), true)?,
        native_keys: BTreeMap::new(),
    };
    let start = Instant::now();
    let runtime = Runtime::new()?;
    let init = start.elapsed();
    let start = Instant::now();
    let component = runtime
        .compile(
            include_bytes!("../../../examples/plugins/counter-button/package/driver.wasm").to_vec(),
        )
        .await?;
    let compile = start.elapsed();
    let start = Instant::now();
    let mut session = component
        .attach(
            Arc::clone(&manifest),
            context,
            manifest.validate_descriptor(&descriptor)?,
        )
        .await?;
    let attach = start.elapsed();
    let _subscriptions = session.pending();
    let mut latency = Vec::new();
    let mut triggers = 0;
    for index in 0..1000 {
        let event = wire::Event::Report(wire::DeviceReport {
            endpoint: selector.role.to_string(),
            report: wire::Report {
                kind: wire::ReportKind::Input,
                id: report_id,
                payload: vec![u8::from(index % 2 == 0)],
            },
        });
        let start = Instant::now();
        let update = session.event(event).await?;
        latency.push(start.elapsed());
        triggers += update.updates.len();
    }
    session.detach(wire::DetachReason::Shutdown).await?;
    latency.sort_unstable();
    println!(
        "synthetic_reports=1000 triggers={triggers} init_us={} compile_us={} attach_us={} event_p50_us={} event_p95_us={}",
        init.as_micros(),
        compile.as_micros(),
        attach.as_micros(),
        latency[500].as_micros(),
        latency[950].as_micros()
    );
    Ok(())
}
