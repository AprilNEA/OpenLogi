//! `openlogi light` — discovery and manual control for standalone lights.
//!
//! The CLI intentionally uses the same Litra driver as the agent. It is a
//! small hardware-facing surface for validating discovery, the HID++ `0x1990`
//! read-back and the raw fallback before exercising the GPUI panel.

use anyhow::{Context, Result, anyhow};
use clap::{Args, Subcommand};
use openlogi_core::device::{LightState, StandaloneDevice};
use openlogi_hid::{LightCommand, LitraModel, find_litra};

#[derive(Debug, Subcommand)]
pub enum LightCmd {
    /// List recognized standalone lights and their advertised controls.
    List,
    /// Read a light's state back from the light itself.
    Status(DeviceArgs),
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
            Self::Status(args) => status(args.device.as_deref()).await,
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

async fn status(query: Option<&str>) -> Result<()> {
    let devices = standalone().await?;
    let device = select(&devices, query)?;
    let state = openlogi_hid::read_litra_state(&device.route(), litra_model(device)?)
        .await
        .context("failed to read the light")?;
    print_state(device, state.as_ref());
    Ok(())
}

/// Print what the light reports, or that it is reached only through its raw
/// report, which reads nothing back.
fn print_state(device: &StandaloneDevice, state: Option<&LightState>) {
    match state {
        Some(state) => {
            println!("{}:", device.display_name);
            for line in describe_state(state) {
                println!("  {line}");
            }
        }
        None => println!(
            "{}: reached only through its raw report, which reads nothing back",
            device.display_name
        ),
    }
}

/// One line per control of `state`.
fn describe_state(state: &LightState) -> Vec<String> {
    let mut lines = vec![format!("power: {}", if state.on { "on" } else { "off" })];
    if let (Some(range), Some(value)) = (state.capabilities.brightness, state.brightness) {
        let limit = state.brightness_limit.map_or_else(String::new, |limit| {
            format!(", limited to {limit} lm right now")
        });
        lines.push(format!(
            "brightness: {value} lm ({}–{} lm){limit}",
            range.min(),
            range.max()
        ));
    }
    if let (Some(range), Some(value)) = (state.capabilities.temperature, state.temperature_kelvin) {
        lines.push(format!(
            "temperature: {value} K ({}–{} K step {})",
            range.min(),
            range.max(),
            range.step()
        ));
    }
    lines
}

async fn set_power(query: Option<&str>, enabled: bool) -> Result<()> {
    let devices = standalone().await?;
    let device = select(&devices, query)?;
    apply(device, LightCommand::Power(enabled)).await
}

async fn set_brightness(args: BrightnessArgs) -> Result<()> {
    let command = brightness_command(args.percent, args.lumens)?;
    let devices = standalone().await?;
    let device = select(&devices, args.device.device.as_deref())?;
    apply(device, command).await
}

/// The command for `--percent` or `--lumens`.
///
/// Lumens are not checked here: the driver checks them against the range the
/// light reports over HID++, or the model's static range when only the raw
/// report reaches it, and a light can report a wider range than the static one.
fn brightness_command(percent: Option<u8>, lumens: Option<u16>) -> Result<LightCommand> {
    match (percent, lumens) {
        (Some(percent), None) => Ok(LightCommand::BrightnessPercent(percent)),
        (None, Some(lumens)) => Ok(LightCommand::BrightnessNative(lumens)),
        (None, None) => Err(anyhow!("pass either --percent or --lumens")),
        (Some(_), Some(_)) => unreachable!("clap enforces the argument conflict"),
    }
}

async fn set_temperature(args: TemperatureArgs) -> Result<()> {
    let devices = standalone().await?;
    let device = select(&devices, args.device.device.as_deref())?;
    apply(device, LightCommand::TemperatureKelvin(args.kelvin)).await
}

fn litra_model(device: &StandaloneDevice) -> Result<LitraModel> {
    find_litra(
        device.address.vendor_id,
        device.address.product_id,
        device.address.usage_page,
        device.address.usage_id,
    )
    .map(|descriptor| descriptor.model)
    .ok_or_else(|| {
        anyhow!(
            "unsupported light product {:04x}",
            device.address.product_id
        )
    })
}

async fn apply(device: &StandaloneDevice, command: LightCommand) -> Result<()> {
    let state = openlogi_hid::apply_litra(&device.route(), litra_model(device)?, command)
        .await
        .context("failed to write the light command")?;
    print_state(device, state.as_ref());
    Ok(())
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
    use super::{LightCommand, brightness_command, describe_state, select};
    use openlogi_core::device::{
        DeviceKind, LightCapabilities, LightState, LightValueRange, LightValueUnit,
        RawDeviceAddress, StandaloneDevice,
    };

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

    #[test]
    fn lumens_outside_the_static_range_reach_the_driver() {
        // The Glow's static range is 20–250 lm; a light that reports more is
        // checked by the driver against its own range.
        assert_eq!(
            brightness_command(None, Some(280)).expect("a native command"),
            LightCommand::BrightnessNative(280)
        );
        assert_eq!(
            brightness_command(Some(40), None).expect("a percent command"),
            LightCommand::BrightnessPercent(40)
        );
        brightness_command(None, None).expect_err("one of the two is required");
    }

    #[test]
    fn a_read_back_state_describes_each_control_and_the_limit() {
        let brightness =
            LightValueRange::new(20, 250, 1, LightValueUnit::Lumens).expect("valid range");
        let temperature =
            LightValueRange::new(2700, 6500, 100, LightValueUnit::Kelvin).expect("valid range");
        let mut state = LightState {
            capabilities: LightCapabilities {
                power: true,
                brightness: Some(brightness),
                temperature: Some(temperature),
                color: false,
                zones: false,
            },
            on: true,
            brightness: Some(136),
            temperature_kelvin: Some(4600),
            brightness_limit: None,
        };
        assert_eq!(
            describe_state(&state),
            [
                "power: on",
                "brightness: 136 lm (20–250 lm)",
                "temperature: 4600 K (2700–6500 K step 100)",
            ]
        );

        state.on = false;
        state.brightness_limit = Some(150);
        state.capabilities.temperature = None;
        state.temperature_kelvin = None;
        assert_eq!(
            describe_state(&state),
            [
                "power: off",
                "brightness: 136 lm (20–250 lm), limited to 150 lm right now",
            ]
        );
    }
}
