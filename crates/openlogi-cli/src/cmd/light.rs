//! `openlogi light` — discovery and manual control for standalone lights.
//!
//! The CLI intentionally uses the same raw-HID driver as the agent. It is a
//! small hardware-facing surface for validating discovery and report encoding
//! before exercising the GPUI panel.

use anyhow::{Context, Result, anyhow};
use clap::{Args, Subcommand};
use openlogi_core::device::{LightValueUnit, StandaloneDevice};
use openlogi_hid::{DeviceRoute, LightCommand, LitraModel};

#[derive(Debug, Subcommand)]
pub enum LightCmd {
    /// List recognized standalone lights and their advertised controls.
    List,
    /// Turn a light on.
    On(DeviceArgs),
    /// Turn a light off.
    Off(DeviceArgs),
    /// Set normalized brightness or native lumens.
    Brightness(BrightnessArgs),
    /// Set colour temperature in Kelvin.
    Temperature(TemperatureArgs),
}

#[derive(Debug, Args)]
pub struct DeviceArgs {
    /// Case-insensitive substring of the light name or identity.
    #[arg(long)]
    device: Option<String>,
}

#[derive(Debug, Args)]
pub struct BrightnessArgs {
    #[command(flatten)]
    device: DeviceArgs,
    /// Normalized brightness from 0 to 100 percent.
    #[arg(long, conflicts_with = "lumens", value_parser = clap::value_parser!(u8).range(0..=100))]
    percent: Option<u8>,
    /// Native brightness in lumens.
    #[arg(long, conflicts_with = "percent")]
    lumens: Option<u16>,
}

#[derive(Debug, Args)]
pub struct TemperatureArgs {
    #[command(flatten)]
    device: DeviceArgs,
    /// Colour temperature in Kelvin.
    #[arg(long)]
    kelvin: u16,
}

impl LightCmd {
    pub async fn run(self) -> Result<()> {
        match self {
            Self::List => list().await,
            Self::On(args) => set_power(args.device.as_deref(), true).await,
            Self::Off(args) => set_power(args.device.as_deref(), false).await,
            Self::Brightness(args) => set_brightness(args).await,
            Self::Temperature(args) => set_temperature(args).await,
        }
    }
}

async fn standalone() -> Result<Vec<StandaloneDevice>> {
    openlogi_hid::enumerate_standalone()
        .await
        .context("failed to enumerate standalone HID devices")
}

async fn list() -> Result<()> {
    let devices = standalone().await?;
    if devices.is_empty() {
        println!("No supported standalone lights found.");
        return Ok(());
    }
    for device in devices {
        let address = &device.address;
        println!(
            "{} — {} ({:04x}:{:04x} usage {:04x}:{:04x})",
            device.display_name,
            address.identity,
            address.vendor_id,
            address.product_id,
            address.usage_page,
            address.usage_id,
        );
        if let Some(caps) = device.light_capabilities {
            if let Some(range) = caps.brightness {
                println!(
                    "  brightness: {}–{} {:?}",
                    range.min(),
                    range.max(),
                    range.unit()
                );
            }
            if let Some(range) = caps.temperature {
                println!(
                    "  temperature: {}–{} K step {}",
                    range.min(),
                    range.max(),
                    range.step()
                );
            }
            println!("  power: {}", if caps.power { "yes" } else { "no" });
        }
    }
    Ok(())
}

async fn set_power(query: Option<&str>, enabled: bool) -> Result<()> {
    let devices = standalone().await?;
    let device = select(&devices, query)?;
    apply(device, LightCommand::Power(enabled)).await
}

async fn set_brightness(args: BrightnessArgs) -> Result<()> {
    let devices = standalone().await?;
    let device = select(&devices, args.device.device.as_deref())?;
    let caps = device
        .light_capabilities
        .ok_or_else(|| anyhow!("selected light did not advertise capabilities"))?;
    let range = caps
        .brightness
        .ok_or_else(|| anyhow!("selected light does not support brightness"))?;
    let command = match (args.percent, args.lumens) {
        (Some(percent), None) => LightCommand::BrightnessPercent(percent),
        (None, Some(lumens)) => {
            if range.unit() != LightValueUnit::Lumens || !range.contains(lumens) {
                return Err(anyhow!(
                    "lumens must be in the supported range {}..={} with step {}",
                    range.min(),
                    range.max(),
                    range.step()
                ));
            }
            LightCommand::BrightnessNative(lumens)
        }
        (None, None) => return Err(anyhow!("pass either --percent or --lumens")),
        (Some(_), Some(_)) => unreachable!("clap enforces the argument conflict"),
    };
    apply(device, command).await
}

async fn set_temperature(args: TemperatureArgs) -> Result<()> {
    let devices = standalone().await?;
    let device = select(&devices, args.device.device.as_deref())?;
    apply(device, LightCommand::TemperatureKelvin(args.kelvin)).await
}

async fn apply(device: &StandaloneDevice, command: LightCommand) -> Result<()> {
    let model = LitraModel::from_product_id(device.address.product_id).ok_or_else(|| {
        anyhow!(
            "unsupported light product {:04x}",
            device.address.product_id
        )
    })?;
    let route = DeviceRoute::RawHid {
        vendor_id: device.address.vendor_id,
        product_id: device.address.product_id,
        usage_page: device.address.usage_page,
        usage_id: device.address.usage_id,
        identity: device.address.identity.clone(),
    };
    openlogi_hid::apply_litra(&route, model, command)
        .await
        .context("failed to write the light command")
}

fn select<'a>(
    devices: &'a [StandaloneDevice],
    query: Option<&str>,
) -> Result<&'a StandaloneDevice> {
    let Some(query) = query else {
        return match devices {
            [] => Err(anyhow!("no supported standalone light found")),
            [device] => Ok(device),
            _ => Err(anyhow!(
                "multiple standalone lights found; select one with --device"
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
        return Err(anyhow!("no standalone light matches --device {query}"));
    };
    if matches.next().is_some() {
        return Err(anyhow!(
            "multiple standalone lights match --device {query}; use a more specific value"
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
        select(&devices, None).expect_err("two lights and no --device must be ambiguous");
        assert_eq!(
            select(&devices, Some("beam"))
                .expect("matching device")
                .display_name,
            "Litra Beam"
        );
        select(&devices, Some("litra"))
            .expect_err("a --device query matching both lights must be ambiguous");
        assert_eq!(
            select(&devices[..1], None)
                .expect("single device")
                .display_name,
            "Litra Glow"
        );
    }
}
