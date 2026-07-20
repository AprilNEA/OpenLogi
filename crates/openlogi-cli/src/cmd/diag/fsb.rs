//! `openlogi diag fsb` — raw-probe the `0x19c0 ForceSensingButton` feature
//! (the MX Master 4's Action Ring panel).
//!
//! The feature's function map is undocumented; this sends one function call
//! with caller-chosen argument bytes and hex-dumps the response so the map
//! can be reverse-engineered on real hardware. An `InvalidFunction` HID++
//! error is the expected "past the end of the table" response.

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct FsbArgs {
    /// Function ID to call (0-15).
    #[arg(long, default_value_t = 0)]
    pub function: u8,

    /// Three comma-separated hex argument bytes, e.g. `01,00,00`.
    #[arg(long, value_name = "AA,BB,CC")]
    pub args: Option<String>,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: FsbArgs) -> Result<()> {
    if args.function > 0x0f {
        bail!("function must be 0-15 (got {})", args.function);
    }
    let call_args = parse_args(args.args.as_deref())?;

    let (route, name) = select_device(args.device.as_deref(), &[0x19c0]).await?;
    println!("device: {name} ({route})");
    println!(
        "0x19c0 fn={} args={:02x} {:02x} {:02x}",
        args.function, call_args[0], call_args[1], call_args[2]
    );

    match openlogi_hid::hidden_features::force_button_raw_call(&route, args.function, call_args)
        .await
    {
        Ok(payload) => {
            let hex: String = payload.iter().map(|b| format!("{b:02x} ")).collect();
            println!("response: {hex}");
        }
        Err(e) => println!("error: {e}"),
    }
    Ok(())
}

pub(crate) fn parse_args(raw: Option<&str>) -> Result<[u8; 3]> {
    let Some(raw) = raw else { return Ok([0; 3]) };
    let parts: Vec<u8> = raw
        .split(',')
        .map(|p| u8::from_str_radix(p.trim().trim_start_matches("0x"), 16))
        .collect::<Result<_, _>>()
        .context("--args must be hex bytes like 01,00,00")?;
    if parts.len() != 3 {
        bail!("--args needs exactly 3 bytes (got {})", parts.len());
    }
    Ok([parts[0], parts[1], parts[2]])
}
