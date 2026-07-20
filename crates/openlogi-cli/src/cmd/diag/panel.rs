//! `openlogi diag panel` — arm the MX Master 4 Action Ring panel with the
//! reverse-engineered Options+ recipe and print each press/release.
//!
//! Proves OpenLogi can drive the panel: it enables `0x1b04`
//! `analyticsKeyEvents` on the panel CIDs (no diversion) and decodes the
//! events the panel emits. This is the empirical basis for the production
//! ring; keep the mouse moving so the (BTLE) link is hot while arming.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result};
use clap::Args;

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct PanelArgs {
    /// How long to listen, in seconds.
    #[arg(long, default_value_t = 45)]
    pub seconds: u64,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: PanelArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[0x1b04]).await?;
    println!("device: {name} ({route})");
    println!(
        "arming Action Ring panel (analytics on CIDs 0x01a0/0x0050/0x0051) — press it now, {}s window\n",
        args.seconds
    );

    let count = Arc::new(AtomicU32::new(0));
    let tally = Arc::clone(&count);
    openlogi_hid::hidden_features::watch_panel(&route, args.seconds, move |cid, event| {
        let n = tally.fetch_add(1, Ordering::Relaxed) + 1;
        let state = if event != 0 { "PRESS  " } else { "RELEASE" };
        println!("  [{n:>3}] panel cid=0x{cid:04x}  {state}  (event=0x{event:02x})");
    })
    .await
    .context("watch panel")?;

    let total = count.load(Ordering::Relaxed);
    println!("\n{total} panel event(s) captured.");
    if total == 0 {
        println!("none seen — was the mouse moving while arming? is Options+ holding the device?");
    }
    Ok(())
}
