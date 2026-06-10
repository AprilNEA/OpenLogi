//! `openlogi diag lighting <RRGGBB>` — set a wired RGB keyboard to a solid
//! colour via HID++ `PerKeyLighting` (0x8080).
//!
//! Targets the first online direct-attached (USB) Logitech device — i.e. a
//! wired G-series keyboard — by VID/PID, so it isn't tied to one model.

use anyhow::{Result, anyhow};
use clap::{Args, ValueEnum};
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
    let hex = args.color.trim_start_matches('#');
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(anyhow!("color must be exactly 6 hex digits, e.g. ff0000"));
    }
    let rgb = u32::from_str_radix(hex, 16)
        .map_err(|_| anyhow!("color must be 6 hex digits, e.g. ff0000"))?;
    let r = ((rgb >> 16) & 0xff) as u8;
    let g = ((rgb >> 8) & 0xff) as u8;
    let b = (rgb & 0xff) as u8;

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
