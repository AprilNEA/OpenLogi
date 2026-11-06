//! Probes whether key reporting needs the keepalive pump, or whether a plain
//! divert delivers notifications on its own.
//!
//! Opens ONE handle (`0xff43:0x1a02` — reads mirror across handles; a live
//! probe proved every node of the keypad shares the same device), diverts both
//! page buttons, then:
//!   WINDOW 1 (30 s): NO keepalive. Press page buttons AND grid keys.
//!   WINDOW 2 (30 s): re-divert, then 1 s keepalive pump. Press again.
//! Every report is logged with a timestamp so counts are unambiguous, and both
//! buttons are restored to native reporting before exit.
//!
//! Quit any running OpenLogi agent/GUI first.

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

/// A `SetCIDReporting` write for `cid`: `reporting` 0x03 = divert (persisted),
/// 0x01 = back to native keystrokes (persisted).
fn set_reporting(cid: u16, reporting: u8) -> Vec<u8> {
    let mut buf = vec![0u8; 20];
    buf[0] = 0x11;
    buf[1] = 0xff;
    buf[2] = 0x0b; // 0x1b04 at index 11
    buf[3] = 0x3a; // fn 3, sw 1
    [buf[4], buf[5]] = cid.to_be_bytes();
    buf[6] = reporting;
    buf
}

async fn open_keypad() -> Result<(async_hid::DeviceReader, async_hid::DeviceWriter), Box<dyn Error>>
{
    static BACKEND: OnceLock<HidBackend> = OnceLock::new();
    let backend = BACKEND.get_or_init(HidBackend::default);
    let mut stream = backend
        .enumerate()
        .await
        .map_err(|e| format!("enumerate: {e}"))?;
    let mut found = None;
    while let Some(d) = stream.next().await {
        if d.vendor_id == KEY_VID
            && d.product_id == KEY_PID
            && d.usage_page == 0xff43
            && d.usage_id == 0x1a02
        {
            found = Some(d);
            break;
        }
    }
    let dev = found.ok_or("keypad 1a02 node not found")?;
    let (r, w) = dev.open().await.map_err(|e| format!("open: {e}"))?;
    Ok((r, w))
}

async fn window(
    reader: &Mutex<async_hid::DeviceReader>,
    _writer: &Mutex<async_hid::DeviceWriter>,
    label: &str,
    ms: u64,
) {
    println!(">> WINDOW {label}: press page buttons AND grid keys ({ms} ms) <<");
    let start = Instant::now();
    let mut notes = 0usize;
    let mut keepalive_acks = 0usize;
    let mut other = 0usize;
    while start.elapsed() < Duration::from_millis(ms) {
        let mut buf = [0u8; 64];
        let n = {
            let mut r = reader.lock().await;
            match tokio::time::timeout(Duration::from_millis(200), r.read_input_report(&mut buf))
                .await
            {
                Ok(Ok(n)) => n,
                Ok(Err(e)) => {
                    println!("    read error: {e}");
                    break;
                }
                Err(_) => 0,
            }
        };
        if n == 0 {
            continue;
        }
        let t = start.elapsed().as_secs_f32();
        let is_ka = n == 20
            && buf[0] == 0x11
            && buf[2] == 0x04
            && buf[3] == 0x1c
            && (buf[4] == 0x0b && buf[5] == 0xb8);
        if is_ka {
            keepalive_acks += 1;
            continue;
        }
        // A notification carries a feature index != keepalive.
        let line = format!("    {t:6.1}s  {}", hex(&buf[..n]));
        if buf[1] == 0xff {
            notes += 1;
            println!("NOTE  {line}");
        } else {
            other += 1;
            println!("other {line}");
        }
    }
    println!(
        "<< WINDOW {label}: {notes} notifications, {keepalive_acks} keepalive acks, {other} other"
    );
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let (reader, writer) = open_keypad().await?;
    let reader = Mutex::new(reader);
    let writer = Arc::new(Mutex::new(writer));

    for cid in [0x01a1u16, 0x01a2] {
        writer
            .lock()
            .await
            .write_output_report(&set_reporting(cid, 0x03))
            .await
            .map_err(|e| e.to_string())?;
        println!("diverted {cid:#06x}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    window(&reader, &writer, "1 — NO keepalive", 30_000).await;

    for cid in [0x01a1u16, 0x01a2] {
        writer
            .lock()
            .await
            .write_output_report(&set_reporting(cid, 0x03))
            .await
            .map_err(|e| e.to_string())?;
    }
    println!("re-diverted, starting keepalive pump");

    let ka_writer = Arc::clone(&writer);
    let pump = tokio::spawn(async move {
        loop {
            let _ = ka_writer
                .lock()
                .await
                .write_output_report(&keepalive())
                .await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    window(&reader, &writer, "2 — WITH keepalive", 30_000).await;
    pump.abort();

    // Leave the device as found: both page buttons back to native.
    for cid in [0x01a1u16, 0x01a2] {
        writer
            .lock()
            .await
            .write_output_report(&set_reporting(cid, 0x01))
            .await
            .map_err(|e| e.to_string())?;
        println!("undiverted {cid:#06x}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("probe done");
    Ok(())
}
