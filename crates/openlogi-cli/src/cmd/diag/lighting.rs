//! `openlogi diag lighting <RRGGBB>` — set an online RGB device to a solid
//! colour via the same HID++ lighting write path the GUI uses.

use anyhow::{Result, anyhow};
use clap::{Args, ValueEnum};
use openlogi_hid::LightingMethod;

use super::select_device;

const LIGHTING_FEATURES: &[u16] = &[0x8070, 0x8071, 0x8080, 0x8081];

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Method {
    /// Prefer effect engines, then fall back to 0x8080 per-key (default).
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

    /// Run against the online device whose name contains this string
    /// (case-insensitive). Useful when several devices are connected.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,

    /// Which HID++ lighting path to drive.
    #[arg(long, value_enum, default_value_t = Method::Auto)]
    pub method: Method,
}

pub async fn run(args: LightingArgs) -> Result<()> {
    let hex = args.color.trim_start_matches('#');
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(anyhow!("color must be exactly 6 hex digits, e.g. ff0000"));
    }
    let rgb = u32::from_str_radix(hex, 16)
        .map_err(|_| anyhow!("color must be 6 hex digits, e.g. ff0000"))?;
    let r = ((rgb >> 16) & 0xff) as u8;
    let g = ((rgb >> 8) & 0xff) as u8;
    let b = (rgb & 0xff) as u8;

    let device_query = args.device.as_deref();
    let (route, name) = select_device(device_query, LIGHTING_FEATURES)
        .await
        .map_err(|e| match device_query {
            Some(q) => anyhow!("no lighting-capable online device matches `--device {q}`: {e}"),
            None => anyhow!("no lighting-capable online device found: {e}"),
        })?;

    let method: LightingMethod = args.method.into();
    println!("setting {name} ({route}) to #{r:02x}{g:02x}{b:02x} via {method:?}");
    openlogi_hid::set_keyboard_color_with(&route, method, r, g, b).await?;
    println!("done — {name} should now be solid #{r:02x}{g:02x}{b:02x}");
    Ok(())
}
