//! `openlogi diag hidsniff` — passively hex-dump raw input reports from every
//! Logitech HID interface for a bounded window.
//!
//! Diagnosis tool for locating input that does NOT arrive via HID++ divert —
//! e.g. the MX Master 4 Action Ring panel, whose events are invisible on
//! `0x1b04` and suspected to flow through the Bolt receiver's touch-pad
//! collection (`MI_03`) instead. Read-only: opens interfaces for input
//! reports, writes nothing, and interfaces the OS holds exclusively (mouse /
//! keyboard boot collections) simply report their open error and are skipped.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_hid::{AsyncHidRead, HidBackend};
use clap::Args;
use futures_lite::StreamExt;

/// Per-interface cap on individually printed reports, so an unexpectedly
/// chatty collection (sensor stream) can't drown the interesting ones; counts
/// keep accumulating after the mute.
const PRINT_CAP: u64 = 300;

#[derive(Debug, Args)]
pub struct HidSniffArgs {
    /// Vendor ID to filter on, hex (default: Logitech).
    #[arg(long, default_value = "046d")]
    pub vid: String,

    /// How long to listen for reports, in seconds.
    #[arg(long, default_value_t = 30)]
    pub seconds: u64,
}

pub async fn run(args: HidSniffArgs) -> Result<()> {
    let vid = u16::from_str_radix(args.vid.trim_start_matches("0x"), 16)
        .context("--vid must be hex, e.g. 046d")?;

    let devices: Vec<async_hid::Device> = HidBackend::default()
        .enumerate()
        .await
        .context("HID enumeration failed")?
        .collect()
        .await;

    let started = Instant::now();
    let deadline = started + Duration::from_secs(args.seconds);
    let mut tasks = Vec::new();

    for (n, dev) in devices.iter().filter(|d| d.vendor_id == vid).enumerate() {
        println!(
            "iface #{n}: pid={:04x} usage={:#06x}/{:#04x} name={:?} id={:?}",
            dev.product_id, dev.usage_page, dev.usage_id, dev.name, dev.id
        );
        match dev.open().await {
            Ok((mut reader, _writer)) => {
                println!("  -> open OK, listening");
                let label = format!("#{n} pid={:04x} up={:#06x}", dev.product_id, dev.usage_page);
                let task_label = label.clone();
                let count = Arc::new(AtomicU64::new(0));
                let task_count = Arc::clone(&count);
                let task = tokio::spawn(async move {
                    let label = task_label;
                    // 1 KiB: Windows rejects reads whose buffer is smaller than
                    // the interface's max input report (seen on the Bolt
                    // receiver's Haptics collection, > 64 bytes).
                    let mut buf = [0u8; 1024];
                    loop {
                        let remain = deadline.saturating_duration_since(Instant::now());
                        if remain.is_zero() {
                            break;
                        }
                        match tokio::time::timeout(remain, reader.read_input_report(&mut buf)).await
                        {
                            Ok(Ok(len)) => {
                                let seen = task_count.fetch_add(1, Ordering::Relaxed) + 1;
                                if seen <= PRINT_CAP {
                                    let ms = started.elapsed().as_millis();
                                    let hex = super::hex_dump(&buf[..len]);
                                    println!("[{ms:>7}ms] {label} len={len}: {hex}");
                                } else if seen == PRINT_CAP + 1 {
                                    println!("[{label}] print cap reached — counting silently");
                                }
                            }
                            Ok(Err(e)) => {
                                println!("[{label}] read error: {e:?}");
                                break;
                            }
                            Err(_) => break, // deadline reached
                        }
                    }
                });
                tasks.push((label, count, task));
            }
            Err(e) => println!("  -> open failed: {e:?}"),
        }
    }

    if tasks.is_empty() {
        anyhow::bail!("no vid={vid:04x} interface could be opened");
    }

    println!(
        "\nlistening on {} interface(s) for {} s — exercise the control under test now\n",
        tasks.len(),
        args.seconds
    );
    for (label, count, task) in tasks {
        let _ = task.await;
        println!(
            "summary {label}: {} report(s)",
            count.load(Ordering::Relaxed)
        );
    }
    Ok(())
}
