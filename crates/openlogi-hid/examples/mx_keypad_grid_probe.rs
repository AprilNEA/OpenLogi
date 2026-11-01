//! Characterizes the MX Creative Keypad grid keys (the deferred part of v1).
//!
//! Open question this answers: do the nine LCD grid keys emit anything WITHOUT
//! the keepalive pump, and on which of the device's HID interfaces do their
//! reports arrive? (Earlier runs saw `13 …` vendor reports only while a ping
//! pump ran, but only the `ff43:1a02` handle was read, so "another interface
//! is the real carrier" was never excluded.)
//!
//! The probe is a passive observer: it opens EVERY node of `046d:c354` and
//! polls all of them, logging every report tagged with its node — no
//! diversion, no setting writes, only root pings during window 2.
//!   WINDOW 1 (60 s): nothing written. Press each grid key ONCE, in order.
//!   WINDOW 2 (60 s): 1 s ping pump on the HID++ node. Press each key once
//!     again. Same reports in both windows ⇒ keepalive is not needed;
//!     reports only under the pump ⇒ the pump is required.
//!
//! `--map` mode answers the remaining question instead: which physical key is
//! which index in the `13 ff 02 00 [seq] 01 [key]` report? Press each of the
//! nine keys ONCE in a declared order and the program prints the press order
//! live, then the index table. No windows, no pump, no timing — it stops as
//! soon as all nine have been seen.
//!
//! Quit any running OpenLogi agent/GUI first, and keep a focused text editor
//! open during both windows to see whether grid keys also inject keystrokes.
//!
//! ## Results (verified 2026-10)
//!
//! - Delivery is passive: windows 1 and 2 saw the identical stream. The
//!   earlier "keepalive pump required" reading was an artifact of watching a
//!   single handle; the pump is not part of this design.
//! - Every node of the device mirrors the same stream, on one HID++ channel.
//!   The session therefore reads it wherever the channel reads, deduplicating
//!   frames rather than trusting per-node arrival.
//! - A grid event is the first seven bytes of a **32-byte** report
//!   `13 ff 02 00 [seq] 01 [id, …]`: the NUL-terminated id list names every
//!   key held at that moment, `id` being the 1-based grid index. A press
//!   names the key; its release names none. The node carries more than the
//!   20 bytes of a long HID++ message, so a reader must not truncate at 20.
//! - Mapping run: pressing the nine keys in physical row-major order printed
//!   the index sequence `1..=9`. The pinning lives in
//!   `openlogi_core::binding::GridKey`; this file only verifies it.
//! - Cross-checked 2026-10 against the reference decoder
//!   (`node-logitech-mx-creative-console`,
//!   `packages/core/src/services/input/mx-creative-keypad.ts`): the payload
//!   is the same — `ff 02 00 [counter] 01 [id, …]`, ids NUL-terminated, so a
//!   chord arrives as one frame naming every held key — and so is edge
//!   detection: diff the held-id sets of successive frames. An earlier
//!   reading of this capture took `seq` parity as press/release with the
//!   release re-encoding the id; a live agent capture (2026-10-15: press
//!   `… c8 01 08`, release `… c9 01 00`, the counter wrapping `cf`→`c0`)
//!   refuted it — `seq` is a rolling counter with no edge of its own.

use std::collections::BTreeMap;
use std::error::Error;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use async_hid::{AsyncHidRead as _, AsyncHidWrite as _, HidBackend};
use futures_lite::StreamExt as _;
use tokio::sync::Mutex;

const KEY_VID: u16 = 0x046d;
const KEY_PID: u16 = 0xc354;

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Root Ping with the 3000 ms inactivity payload — same pump as the other
/// probe; the device answers it and stays report-active while it runs.
fn keepalive() -> Vec<u8> {
    let mut buf = vec![0u8; 20];
    buf[0] = 0x11;
    buf[1] = 0xff;
    buf[2] = 0x04; // feature 0x0008 at index 4
    buf[3] = 0x1a; // fn 1, sw 1
    buf[4] = 0x0b;
    buf[5] = 0xb8; // 3000 ms
    buf
}

struct Node {
    tag: String,
    reader: async_hid::DeviceReader,
}

/// Poll every node round-robin for 60 s, printing and counting each report.
/// `pump` sends the keepalive once per second when present.
async fn window(
    nodes: &mut [Node],
    pump: Option<&Arc<Mutex<async_hid::DeviceWriter>>>,
    label: &str,
) {
    let start = Instant::now();
    let mut last_ping = start;
    let mut counts: BTreeMap<(String, u8), usize> = BTreeMap::new();
    while start.elapsed() < Duration::from_secs(60) {
        if let Some(w) = pump
            && last_ping.elapsed() >= Duration::from_millis(950)
        {
            last_ping = Instant::now();
            let _ = w.lock().await.write_output_report(&keepalive()).await;
        }
        for node in nodes.iter_mut() {
            let mut buf = [0u8; 64];
            match tokio::time::timeout(
                Duration::from_millis(5),
                node.reader.read_input_report(&mut buf),
            )
            .await
            {
                Ok(Ok(0)) | Err(_) => {}
                Ok(Ok(n)) => {
                    *counts.entry((node.tag.clone(), buf[0])).or_insert(0) += 1;
                    println!(
                        "  {} @ {:5.1}s {}",
                        node.tag,
                        start.elapsed().as_secs_f32(),
                        hex(&buf[..n])
                    );
                }
                Ok(Err(e)) => println!("  read error on {}: {e}", node.tag),
            }
        }
    }
    println!("<< WINDOW {label}: (node, report id) counts:");
    for ((node, id), c) in &counts {
        println!("   {node} 0x{id:02x} = {c}");
    }
}

/// A grid press is `13 ff 02 00 [seq] 01 [key]` naming the held key; its
/// release names none, so it decodes to `None` here. Every node mirrors the
/// same stream, so presses are identified by `(seq, key)` and not trusted
/// per node.
fn grid_event(buf: &[u8]) -> Option<(u8, u8)> {
    if buf.len() >= 7 && buf[0] == 0x13 && buf[1] == 0xff && buf[2] == 0x02 && buf[3] == 0x00 {
        let (seq, key) = (buf[4], buf[6]);
        if (1..=9).contains(&key) {
            Some((seq, key))
        } else {
            None
        }
    } else {
        None
    }
}

/// Poll every node round-robin and print the press order until all nine key
/// indices have been seen once — no writes, no deadline pressure.
async fn map_mode(nodes: &mut [Node]) {
    let start = Instant::now();
    let mut seen: std::collections::BTreeSet<(u8, u8)> = std::collections::BTreeSet::new();
    let mut order: Vec<u8> = Vec::new();
    println!(">> MAP mode: press each of the nine grid keys ONCE in your declared order.");
    println!("   No rush — 5 min budget, stops when all nine are seen.");
    while start.elapsed() < Duration::from_secs(300) && order.len() < 9 {
        for node in nodes.iter_mut() {
            let mut buf = [0u8; 64];
            match tokio::time::timeout(
                Duration::from_millis(5),
                node.reader.read_input_report(&mut buf),
            )
            .await
            {
                Ok(Ok(0)) | Err(_) => {}
                Ok(Ok(n)) => {
                    if let Some((seq, key)) = grid_event(&buf[..n])
                        && seen.insert((seq, key))
                        && !order.contains(&key)
                    {
                        order.push(key);
                        println!("  press #{:>1} -> key index {}", order.len(), key);
                    }
                }
                Ok(Err(e)) => println!("  read error on {}: {e}", node.tag),
            }
        }
    }
    println!("<< MAP result: press order = {order:?}");
    if order.len() == 9
        && order
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            == 9
    {
        println!("   a complete 1..=9 permutation — the index table is pinned");
    } else {
        println!("   INCOMPLETE — press all nine once (or rerun)");
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    static BACKEND: OnceLock<HidBackend> = OnceLock::new();
    let backend = BACKEND.get_or_init(HidBackend::default);
    let mut stream = backend
        .enumerate()
        .await
        .map_err(|e| format!("enumerate: {e}"))?;

    println!("== MX Creative Keypad nodes ==");
    let mut devs: Vec<async_hid::Device> = Vec::new();
    while let Some(d) = stream.next().await {
        if d.vendor_id == KEY_VID && d.product_id == KEY_PID {
            println!(
                "  page={:#06x} usage={:#06x} name={:?}",
                d.usage_page, d.usage_id, d.name
            );
            devs.push(d);
        }
    }
    if devs.is_empty() {
        return Err("no 046d:c354 nodes found".into());
    }

    let mut nodes = Vec::new();
    let mut hidpp_writer = None;
    for (i, d) in devs.iter().enumerate() {
        let tag = format!("{:#06x}/{:#06x}#{}", d.usage_page, d.usage_id, i);
        match d.open().await {
            Ok((reader, writer)) => {
                if d.usage_page == 0xff43 {
                    hidpp_writer = Some(Arc::new(Mutex::new(writer)));
                }
                println!("  opened {tag}");
                nodes.push(Node { tag, reader });
            }
            Err(e) => println!("  SKIP {tag}: {e}"),
        }
    }
    if std::env::args().any(|a| a == "--map") {
        map_mode(&mut nodes).await;
        println!("\nmap done (no device state was changed)");
        return Ok(());
    }

    let Some(pump_writer) = hidpp_writer else {
        return Err("no writable HID++ (ff43) node — window 2 would be meaningless".into());
    };

    println!(
        "\n!! GET READY — window 1 opens in 15 s: press all nine grid keys ONCE in a SHUFFLED order !!"
    );
    for remaining in (1..=15u64).rev() {
        println!("   ... starting in {remaining} s");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    println!("\n>> WINDOW 1 — NO keepalive (60 s): press each grid key ONCE, shuffled order <<");
    window(&mut nodes, None, "1 — no keepalive").await;

    println!("\n>> WINDOW 2 — WITH 1 s ping pump (60 s): press each grid key ONCE again <<");
    window(&mut nodes, Some(&pump_writer), "2 — keepalive").await;

    println!("\nprobe done (no device state was changed)");
    Ok(())
}
