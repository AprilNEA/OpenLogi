//! `openlogi diag lighting <RRGGBB>` — set a wired RGB keyboard to a solid
//! colour via HID++ `PerKeyLighting` (0x8080).
//!
//! Targets the first online direct-attached (USB) Logitech device — i.e. a
//! wired G-series keyboard — by VID/PID, so it isn't tied to one model.

use anyhow::{Result, anyhow};
use clap::{Args, ValueEnum};
use openlogi_core::color::Rgb;
use openlogi_hid::{DeviceRoute, LightingMethod};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Method {
    /// Prefer 0x8070 ColorLedEffects, fall back to 0x8080 per-key (default).
    Auto,
    /// Force 0x8070 ColorLedEffects (the fixed-effect onboard override).
    Effects,
    /// Force 0x8080 PerKeyLighting (the per-key stream).
    Perkey,
}

impl From<Method> for LightingMethod {
    fn from(m: Method) -> Self {
        match m {
            Method::Auto => Self::Auto,
            Method::Effects => Self::Effects,
            Method::Perkey => Self::PerKey,
        }
    }
}

#[derive(Debug, Args)]
pub struct LightingArgs {
    /// Colour as `RRGGBB` hex (e.g. `ff0000` for red).
    pub color: String,

    /// Run against the wired device whose name contains this string
    /// (case-insensitive). Useful when several keyboards are connected.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,

    /// Which HID++ lighting path to drive.
    #[arg(long, value_enum, default_value_t = Method::Auto)]
    pub method: Method,
}

pub async fn run(args: LightingArgs) -> Result<()> {
    let color: Rgb = args.color.trim_start_matches('#').parse()?;
    let (r, g, b) = color.components();

    let device_query = args.device;
    let needle = device_query.as_deref().map(str::to_lowercase);

    let inventories = openlogi_hid::enumerate().await?;
    let (route, name) = inventories
        .iter()
        .find_map(|inv| {
            // Direct (USB-wired) devices carry no receiver UID — that's the
            // wired keyboard. Bolt/Unifying receivers (mice) are skipped.
            if inv.receiver.unique_id.is_some() {
                return None;
            }
            let paired = inv.paired.iter().find(|p| p.online)?;
            let name = paired.codename.clone().unwrap_or_else(|| {
                format!(
                    "{:04x}:{:04x}",
                    inv.receiver.vendor_id, inv.receiver.product_id
                )
            });
            if let Some(ref n) = needle
                && !name.to_lowercase().contains(n.as_str())
            {
                return None;
            }
            let route = DeviceRoute::Direct {
                vendor_id: inv.receiver.vendor_id,
                product_id: inv.receiver.product_id,
            };
            Some((route, name))
        })
        .ok_or_else(|| match &device_query {
            Some(q) => anyhow!("no wired device matches `--device {q}`"),
            None => {
                anyhow!("no wired (direct-USB) Logitech device found — is the keyboard plugged in?")
            }
        })?;

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
