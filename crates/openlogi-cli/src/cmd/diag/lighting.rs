//! `openlogi diag lighting <RRGGBB>` — set a device's RGB LEDs to a solid
//! colour via HID++ `RgbEffects` (0x8071), `ColorLedEffects` (0x8070) or
//! `PerKeyLighting` (0x8080).
//!
//! Picks the first online device exposing one of those, so it reaches a wireless
//! mouse behind a receiver as well as a wired keyboard.

use anyhow::Result;
use clap::{Args, ValueEnum};
use openlogi_core::color::Rgb;
use openlogi_hid::LightingMethod;

use crate::cmd::diag::select_device;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Method {
    /// Walk 0x8071 → 0x8070 → 0x8080, taking the first exposed (default).
    Auto,
    /// Force 0x8071 RgbEffects (the per-cluster fixed-effect override).
    Rgb,
    /// Force 0x8070 ColorLedEffects (the per-zone fixed-effect override).
    Effects,
    /// Force 0x8080 PerKeyLighting (the per-key stream).
    Perkey,
}

impl From<Method> for LightingMethod {
    fn from(m: Method) -> Self {
        match m {
            Method::Auto => Self::Auto,
            Method::Rgb => Self::RgbEffects,
            Method::Effects => Self::Effects,
            Method::Perkey => Self::PerKey,
        }
    }
}

#[derive(Debug, Args)]
pub struct LightingArgs {
    /// Colour as `RRGGBB` hex (e.g. `ff0000` for red).
    #[arg(required_unless_present_any = ["info", "release_control"])]
    pub color: Option<String>,

    /// Dump the device's 0x8071 clusters and their effects, then exit without
    /// writing a colour.
    #[arg(long)]
    pub info: bool,

    /// Hand the 0x8071 clusters back to the device's own effect engine and
    /// exit, undoing the software control a colour write takes.
    #[arg(long, conflicts_with = "info")]
    pub release_control: bool,

    /// Run against the device whose name contains this string
    /// (case-insensitive). Useful when several lit devices are connected.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,

    /// Which HID++ lighting path to drive.
    #[arg(long, value_enum, default_value_t = Method::Auto)]
    pub method: Method,
}

pub async fn run(args: LightingArgs) -> Result<()> {
    // Parse before touching hardware so a typo fails fast rather than after a
    // device round-trip.
    let color = args
        .color
        .as_deref()
        .map(|c| c.trim_start_matches('#').parse::<Rgb>())
        .transpose()?;

    // The three features `set_keyboard_color` can drive — auto-skip devices with
    // no LEDs to paint.
    let (route, name) = select_device(args.device.as_deref(), &[0x8071, 0x8070, 0x8080]).await?;
    println!("device: {name} ({route})");

    if args.info {
        return print_rgb_info(&route).await;
    }

    if args.release_control {
        openlogi_hid::release_rgb_control(&route).await?;
        println!("released software control — the device drives its own RGB again");
        return Ok(());
    }

    let Some(color) = color else {
        // Unreachable via clap (`required_unless_present`), but the type is an
        // Option either way.
        anyhow::bail!("a colour is required unless --info is given");
    };
    let (r, g, b) = color.components();
    let method: LightingMethod = args.method.into();
    println!("setting {name} to #{r:02x}{g:02x}{b:02x} via {method:?}");
    openlogi_hid::set_keyboard_color_with(&route, method, r, g, b).await?;
    println!("done — {name} should now be solid #{r:02x}{g:02x}{b:02x}");
    Ok(())
}

/// Dump the `0x8071` cluster/effect table so a device whose LEDs don't all
/// respond can be told apart from one OpenLogi simply isn't addressing.
async fn print_rgb_info(route: &openlogi_hid::DeviceRoute) -> Result<()> {
    let (control, clusters) = openlogi_hid::dump_rgb_clusters(route).await?;
    println!(
        "  software control: all_clusters={} power_modes={}",
        control.all_clusters, control.power_modes
    );
    if clusters.is_empty() {
        println!("  no 0x8071 clusters reported");
        return Ok(());
    }
    for cluster in &clusters {
        println!(
            "  cluster {} location={:#06x} effect_persistency={} multiled_pattern={} effects={}",
            cluster.index,
            cluster.location,
            cluster.effect_persistency,
            cluster.multiled_pattern,
            cluster.effects.len()
        );
        for effect in &cluster.effects {
            println!(
                "    effect {:>2}  id={:#06x} caps={:#06x} period={}ms",
                effect.index, effect.effect_id, effect.effect_capabilities, effect.effect_period
            );
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "expect/unwrap are idiomatic in tests"
)]
mod color_validation_tests {
    use openlogi_core::color::RgbParseError;

    use super::{LightingArgs, Method, run};

    fn args(color: &str) -> LightingArgs {
        LightingArgs {
            color: Some(color.to_string()),
            info: false,
            release_control: false,
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
