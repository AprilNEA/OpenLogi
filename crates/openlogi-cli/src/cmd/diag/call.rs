//! `openlogi diag call` — send one raw call to ANY HID++ 2.0 feature by ID.
//!
//! Generic reverse-engineering probe: resolves the feature at runtime and
//! hex-dumps the 16-byte response. `InvalidFunctionId` marks the end of a
//! feature's function table; `InvalidArgument` means the function exists but
//! wants different arguments.

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct CallArgs {
    /// Feature ID, hex (e.g. 1e02).
    #[arg(long, value_name = "HEX")]
    pub feature: String,

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

pub async fn run(args: CallArgs) -> Result<()> {
    let feature = u16::from_str_radix(args.feature.trim_start_matches("0x"), 16)
        .context("--feature must be hex, e.g. 1e02")?;
    if args.function > 0x0f {
        bail!("function must be 0-15 (got {})", args.function);
    }
    let call_args = super::fsb::parse_args(args.args.as_deref())?;

    let (route, name) = select_device(args.device.as_deref(), &[feature]).await?;
    println!("device: {name} ({route})");
    println!(
        "feature 0x{feature:04x} fn={} args={:02x} {:02x} {:02x}",
        args.function, call_args[0], call_args[1], call_args[2]
    );

    match openlogi_hid::hidden_features::raw_feature_call(&route, feature, args.function, call_args)
        .await
    {
        Ok(Some(payload)) => {
            let hex = super::hex_dump(&payload);
            println!("response: {hex}");
        }
        Ok(None) => println!("feature not exposed by this device"),
        Err(e) => println!("error: {e}"),
    }
    Ok(())
}
