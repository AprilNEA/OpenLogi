//! Exercise native DJI mapping and journal recovery without changing the user's configuration.

use std::path::PathBuf;

use openlogi_agent_core::peripherals::mapping::{EffectKey, FileJournal, MappingManager};
use openlogi_core::peripheral::{HidUsage, native_key};
use openlogi_device_registry::native_remap::DJI_MIC_3;
use openlogi_hid::native_mapping::{MappingBackend as _, MappingScope, NativeMapping};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(
        args.next()
            .ok_or("usage: native-mapping-check JOURNAL [--restore]")?,
    );
    let restore = match args.next() {
        None => false,
        Some(flag) if flag == "--restore" => true,
        Some(_) => return Err("only --restore is supported".into()),
    };
    if args.next().is_some() {
        return Err("too many arguments".into());
    }
    let key = EffectKey {
        scope: MappingScope {
            vendor_id: DJI_MIC_3.vendor_id,
            product_id: DJI_MIC_3.product_id,
        },
        source: HidUsage {
            page: DJI_MIC_3.source_page,
            usage: DJI_MIC_3.source_usage,
        }
        .into(),
    };
    let target = native_key(&openlogi_core::binding::Action::CustomShortcut(
        "F18".parse()?,
    ))?;
    let mut manager = MappingManager::new(NativeMapping, FileJournal::new(path.clone()))?;
    if restore {
        let status = manager
            .reconcile(key, "hardware-check", None, || Ok(()))
            .await?;
        println!("Recovery: {status:?}");
        return Ok(());
    }
    let before = NativeMapping.read(key.scope).await?;
    println!("Before: {}", serde_json::to_string(&before)?);
    let applied = manager
        .reconcile(key, "hardware-check", Some(target.into()), || Ok(()))
        .await;
    println!("Apply: {applied:?}");
    drop(manager);
    let mut recovered = MappingManager::new(NativeMapping, FileJournal::new(path))?;
    let restored = recovered
        .reconcile(key, "hardware-check", None, || Ok(()))
        .await;
    println!("Restore after manager restart: {restored:?}");
    restored?;
    applied?;
    let after = NativeMapping.read(key.scope).await?;
    if before != after {
        return Err("device lifetime or mappings changed during verification".into());
    }
    println!(
        "Original service mappings preserved. Button and application behavior remain unverified."
    );
    Ok(())
}
