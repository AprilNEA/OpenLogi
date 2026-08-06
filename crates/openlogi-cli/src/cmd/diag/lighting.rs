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
    pub color: String,

    /// Run against the device whose name contains this string
    /// (case-insensitive). Useful when several lit devices are connected.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,

    /// Which HID++ lighting path to drive.
    #[arg(long, value_enum, default_value_t = Method::Auto)]
    pub method: Method,
}

pub async fn run(args: LightingArgs) -> Result<()> {
    let color: Rgb = args.color.trim_start_matches('#').parse()?;
    let (r, g, b) = color.components();

    // The three features `set_keyboard_color` can drive — auto-skip devices with
    // no LEDs to paint.
    let (route, name) = select_device(args.device.as_deref(), &[0x8071, 0x8070, 0x8080]).await?;

    let method: LightingMethod = args.method.into();
    println!("setting {name} ({route}) to #{r:02x}{g:02x}{b:02x} via {method:?}");
    openlogi_hid::set_keyboard_color_with(&route, method, r, g, b).await?;
    println!("done — {name} should now be solid #{r:02x}{g:02x}{b:02x}");
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
            color: color.to_string(),
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
