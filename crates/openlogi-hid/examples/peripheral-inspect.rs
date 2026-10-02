//! Read discovery and existing native mappings without changing devices.

use openlogi_device_registry::native_remap::NATIVE_REMAP_DEVICES;
use openlogi_hid::native_mapping::{MappingBackend as _, MappingScope, NativeMapping};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let devices = openlogi_hid::peripheral::discover(&openlogi_hid::host::device_io_gate()).await?;
    for device in devices {
        for model in NATIVE_REMAP_DEVICES {
            if device.endpoint.vendor_id == model.vendor_id
                && device.endpoint.product_id == model.product_id
            {
                println!("{}: {:?}", device.name, device.endpoint);
                let evidence = NativeMapping
                    .read(MappingScope {
                        vendor_id: model.vendor_id,
                        product_id: model.product_id,
                    })
                    .await?;
                println!("{}", serde_json::to_string_pretty(&evidence)?);
            }
        }
    }
    Ok(())
}
