//! Choosing one standalone device for a subcommand's `--device` query.

use anyhow::{Result, anyhow};
use openlogi_core::device::StandaloneDevice;

/// The one standalone `noun` among `devices` that `query` names by display
/// name or identity, or the only one when there is no query.
pub(crate) fn select<'a>(
    devices: &'a [StandaloneDevice],
    query: Option<&str>,
    noun: &str,
) -> Result<&'a StandaloneDevice> {
    let Some(query) = query else {
        return match devices {
            [] => Err(anyhow!("no supported standalone {noun} found")),
            [device] => Ok(device),
            _ => Err(anyhow!(
                "multiple standalone {noun}s found; select one with --device"
            )),
        };
    };
    let query = query.to_ascii_lowercase();
    let mut matches = devices.iter().filter(|device| {
        device.display_name.to_ascii_lowercase().contains(&query)
            || device
                .address
                .identity
                .to_ascii_lowercase()
                .contains(&query)
    });
    let Some(device) = matches.next() else {
        return Err(anyhow!("no standalone {noun} matches --device {query}"));
    };
    if matches.next().is_some() {
        return Err(anyhow!(
            "multiple standalone {noun}s match --device {query}; use a more specific value"
        ));
    }
    Ok(device)
}

#[cfg(test)]
mod tests {
    use super::select;
    use openlogi_core::device::{DeviceKind, RawDeviceAddress, StandaloneDevice};

    fn device(name: &str) -> StandaloneDevice {
        StandaloneDevice {
            address: RawDeviceAddress {
                vendor_id: 0x046d,
                product_id: 0xc900,
                usage_page: 0xff43,
                usage_id: 0x0202,
                identity: "serial:test".into(),
            },
            display_name: name.into(),
            manufacturer: Some("Logi".into()),
            serial_number: Some("test".into()),
            unit_id: [0; 4],
            kind: DeviceKind::Light,
            online: true,
            capabilities: None,
            light_capabilities: None,
            driver_id: "litra".into(),
            registry_model_id: None,
        }
    }

    #[test]
    fn selection_requires_disambiguation_and_supports_name_queries() {
        let devices = vec![device("Litra Glow"), device("Litra Beam")];
        select(&devices, None, "light").expect_err("two lights and no --device must be ambiguous");
        assert_eq!(
            select(&devices, Some("beam"), "light")
                .expect("matching device")
                .display_name,
            "Litra Beam"
        );
        select(&devices, Some("litra"), "light")
            .expect_err("a --device query matching both lights must be ambiguous");
        assert_eq!(
            select(&devices[..1], None, "light")
                .expect("single device")
                .display_name,
            "Litra Glow"
        );
    }
}
