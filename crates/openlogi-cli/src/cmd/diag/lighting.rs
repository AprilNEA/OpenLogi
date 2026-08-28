//! `openlogi diag lighting` — list advertised effects or apply one.
//!
//! `--list` prints `0x8070` zones and firmware ids for every online lighting
//! device. `--effect <id>` writes a catalog prefab (volatile). A positional
//! `RRGGBB` still forces a solid colour via 0x8070 / 0x8081 / 0x8080.

use anyhow::{Result, anyhow};
use clap::{Args, ValueEnum};
use openlogi_core::color::Rgb;
use openlogi_core::config::Lighting;
use openlogi_core::device::DeviceKind;
use openlogi_core::hid::LightingEffect;
use openlogi_hid::{DeviceRoute, LightingApply, LightingMethod};

use super::{online_devices, select_device};

const COLOR_LED_EFFECTS: u16 = 0x8070;
const PER_KEY_LIGHTING_V2: u16 = 0x8081;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Method {
    /// Prefer 0x8070 ColorLedEffects, fall back to 0x8081 then 0x8080
    /// per-key (default).
    Auto,
    /// Force 0x8070 ColorLedEffects (the fixed-effect onboard override).
    Effects,
    /// Force 0x8080 PerKeyLighting (the raw per-key stream).
    Perkey,
    /// Force 0x8081 PerKeyLighting2 (the zone-addressed successor).
    Perkeyv2,
}

impl From<Method> for LightingMethod {
    fn from(m: Method) -> Self {
        match m {
            Method::Auto => Self::Auto,
            Method::Effects => Self::Effects,
            Method::Perkey => Self::PerKey,
            Method::Perkeyv2 => Self::PerKeyV2,
        }
    }
}

#[derive(Debug, Args)]
pub struct LightingArgs {
    /// Colour as `RRGGBB` hex (e.g. `ff0000` for red).
    #[arg(conflicts_with_all = ["list", "effect"])]
    pub color: Option<String>,

    /// Print zones and advertised firmware effects for every online lighting
    /// device.
    #[arg(long, conflicts_with = "effect")]
    pub list: bool,

    /// Apply a catalog effect id (`solid`, `color_cycle`, `breathing`, …).
    #[arg(long, value_name = "ID", conflicts_with = "list")]
    pub effect: Option<String>,

    /// Run against the device whose name contains this string
    /// (case-insensitive).
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,

    /// Which HID++ lighting path to drive for a solid `RRGGBB` write.
    #[arg(long, value_enum, default_value_t = Method::Auto)]
    pub method: Method,
}

pub async fn run(args: LightingArgs) -> Result<()> {
    if args.list {
        return list_devices(args.device.as_deref()).await;
    }
    if let Some(effect) = args.effect.as_deref() {
        return apply_effect(effect, args.device.as_deref()).await;
    }
    let color = args
        .color
        .as_deref()
        .ok_or_else(|| anyhow!("pass an RRGGBB colour, --list, or --effect <id>"))?;
    apply_solid(color, args.device.as_deref(), args.method).await
}

async fn list_devices(device: Option<&str>) -> Result<()> {
    let needle = device.map(str::to_lowercase);
    let devices = online_devices().await?;
    let mut listed = 0usize;
    for candidate in devices {
        if let Some(ref needle) = needle
            && !candidate.name.to_lowercase().contains(needle.as_str())
        {
            continue;
        }
        let mouse = matches!(candidate.kind, DeviceKind::Mouse | DeviceKind::Trackball);
        match openlogi_hid::read_lighting_info(&candidate.route, mouse, true, true).await {
            Ok(info) => {
                print_lighting(&candidate.name, &candidate.route, &info);
                listed += 1;
            }
            Err(openlogi_hid::WriteError::FeatureUnsupported { .. }) => {}
            Err(error) => {
                tracing::warn!(
                    device = %candidate.name,
                    route = %candidate.route,
                    error = %error,
                    "lighting probe failed"
                );
            }
        }
    }
    if listed == 0 {
        return Err(anyhow!("no online lighting device found (0x8070 / 0x8081)"));
    }
    Ok(())
}

fn print_lighting(name: &str, route: &DeviceRoute, info: &openlogi_core::hid::LightingInfo) {
    println!("{name} ({route})");
    println!(
        "  mouse={} per-key={} screen-sampler={} audio={}",
        info.mouse, info.per_key, info.screen_sampler, info.audio_visualizer
    );
    if info.zones.is_empty() {
        println!("  zones: (none)");
    }
    for zone in &info.zones {
        println!(
            "  zone {} ({:?}): {:?}",
            zone.index, zone.location, zone.firmware_effects
        );
    }
    let prefabs: Vec<_> = info
        .available_prefabs()
        .into_iter()
        .map(|prefab| prefab.effect.id())
        .collect();
    println!("  catalog: {}", prefabs.join(", "));
}

async fn apply_effect(id: &str, device: Option<&str>) -> Result<()> {
    let effect = LightingEffect::from_id(id).ok_or_else(|| {
        anyhow!("unknown lighting effect `{id}` — try `openlogi diag lighting --list`")
    })?;
    let (route, name) = select_device(device, &[COLOR_LED_EFFECTS, PER_KEY_LIGHTING_V2]).await?;
    let lighting = Lighting {
        enabled: true,
        effect,
        ..Lighting::default()
    };
    println!("setting {name} ({route}) to effect `{}`", effect.id());
    match openlogi_hid::apply_lighting(&route, &lighting).await? {
        LightingApply::Firmware => {
            println!("done — firmware owns the LEDs (volatile until unplug/re-apply)");
        }
        LightingApply::Host => {
            println!(
                "host effect `{id}` needs the running agent Lighting tab; firmware was not changed"
            );
        }
    }
    Ok(())
}

async fn apply_solid(color: &str, device: Option<&str>, method: Method) -> Result<()> {
    let color: Rgb = color.trim_start_matches('#').parse()?;
    let (r, g, b) = color.components();
    let (route, name) = select_device(device, &[COLOR_LED_EFFECTS, PER_KEY_LIGHTING_V2]).await?;
    let method: LightingMethod = method.into();
    println!("setting {name} ({route}) to #{r:02x}{g:02x}{b:02x} via {method:?}");
    openlogi_hid::set_keyboard_color_with(&route, method, r, g, b).await?;
    println!("done — {name} should now be solid #{r:02x}{g:02x}{b:02x}");
    Ok(())
}

#[cfg(test)]
mod color_validation_tests {
    use openlogi_core::color::RgbParseError;

    use super::{LightingArgs, Method, run};

    fn args(color: &str) -> LightingArgs {
        LightingArgs {
            color: Some(color.to_string()),
            list: false,
            effect: None,
            device: None,
            method: Method::Auto,
        }
    }

    /// Invalid colours are rejected before any device I/O, so `run` is safe to
    /// call in-process here. Valid colours proceed to hardware enumeration and
    /// are deliberately not exercised.
    #[tokio::test]
    async fn rejects_malformed_colors_before_touching_hardware() {
        for bad in ["zzz", "ff000", "ff00001", "gg0000", ""] {
            let err = run(args(bad)).await.unwrap_err();
            assert!(
                err.downcast_ref::<RgbParseError>().is_some(),
                "{bad:?} should fail Rgb parsing, got: {err}"
            );
        }
    }

    #[tokio::test]
    async fn hash_prefix_is_stripped_before_validation() {
        // `#zzzzzz` still fails, and the rejected input the error reports is
        // `zzzzzz` — proving the `#` is stripped rather than counted toward
        // the 6-digit length.
        let err = run(args("#zzzzzz")).await.unwrap_err();
        let parse = err
            .downcast_ref::<RgbParseError>()
            .expect("Rgb parse error");
        assert_eq!(
            parse.to_string(),
            r#"invalid RGB color "zzzzzz": expected 6 hex digits ("RRGGBB", no '#')"#
        );
    }
}
