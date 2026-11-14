//! Generic HID++ probe for the MX Creative Dialpad (`046d:bc00`, Bluetooth LE).
//!
//! Used to reverse engineer feature `0x4610` MultiRoller and the dialpad's
//! `0x1b04` buttons against real hardware. Subcommands:
//!
//! ```text
//! mx_dialpad_probe nodes                       # list the device's HID nodes
//! mx_dialpad_probe call <feature> <fn> [b..]   # one request, print the reply
//! mx_dialpad_probe listen <secs>               # log every input report
//! mx_dialpad_probe divert-listen <secs>        # divert both rollers + 4 buttons,
//!                                              # log, then restore native
//! ```
//!
//! `<feature>` is the HID++ feature id in hex (resolved to its index through
//! root `0x0000`), `<fn>` the function number, `b..` hex parameter bytes.
//! Quit any running OpenLogi agent/GUI first.

use std::error::Error;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use async_hid::{AsyncHidRead as _, AsyncHidWrite as _, HidBackend};
use futures_lite::StreamExt as _;

const VID: u16 = 0x046d;
const PID: u16 = 0xbc00;
/// Software id stamped into the low nibble of every request — distinct from
/// Options+ so its traffic is told apart from ours.
const SW_ID: u8 = 0x0a;
/// The dialpad's four keys: which control each one reports, as the route
/// owner knows it. Empty for an unregistered product.
fn dialpad_cids() -> Vec<u16> {
    openlogi_core::hid::DeviceRoute::Direct {
        vendor_id: VID,
        product_id: PID,
    }
    .dialpad()
    .map(|dialpad| dialpad.keys.iter().map(|key| key.cid).collect())
    .unwrap_or_default()
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn backend() -> &'static HidBackend {
    static BACKEND: OnceLock<HidBackend> = OnceLock::new();
    BACKEND.get_or_init(HidBackend::default)
}

async fn nodes() -> Result<Vec<async_hid::Device>, Box<dyn Error>> {
    // `PROBE_PID=c354` points the probe at another device (e.g. the keypad).
    let pid = std::env::var("PROBE_PID")
        .ok()
        .and_then(|p| u16::from_str_radix(&p, 16).ok())
        .unwrap_or(PID);
    nodes_of(pid).await
}

async fn nodes_of(pid: u16) -> Result<Vec<async_hid::Device>, Box<dyn Error>> {
    let mut stream = backend()
        .enumerate()
        .await
        .map_err(|e| format!("enumerate: {e}"))?;
    let mut out = Vec::new();
    while let Some(d) = stream.next().await {
        if d.vendor_id == VID && d.product_id == pid {
            out.push(d);
        }
    }
    Ok(out)
}

struct Hidpp {
    reader: async_hid::DeviceReader,
    writer: async_hid::DeviceWriter,
}

impl Hidpp {
    async fn open() -> Result<Self, Box<dyn Error>> {
        let dev = nodes()
            .await?
            .into_iter()
            .find(|d| d.usage_page == 0xff43)
            .ok_or("dialpad HID++ (0xff43) node not found")?;
        let (reader, writer) = dev.open().await.map_err(|e| format!("open: {e}"))?;
        Ok(Self { reader, writer })
    }

    /// Send one long request and return the matching reply (or HID++ error).
    async fn request(&mut self, index: u8, func: u8, params: &[u8]) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; 20];
        buf[0] = 0x11;
        buf[1] = 0xff;
        buf[2] = index;
        buf[3] = (func << 4) | SW_ID;
        buf[4..4 + params.len()].copy_from_slice(params);
        self.writer
            .write_output_report(&buf)
            .await
            .map_err(|e| format!("write: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let mut rx = [0u8; 64];
            let n = match tokio::time::timeout(
                Duration::from_millis(200),
                self.reader.read_input_report(&mut rx),
            )
            .await
            {
                Ok(Ok(n)) => n,
                Ok(Err(e)) => return Err(format!("read: {e}")),
                Err(_) => continue,
            };
            let r = &rx[..n];
            if n >= 4 && r[2] == index && r[3] == buf[3] {
                return Ok(r.to_vec());
            }
            if n >= 5 && r[2] == 0xff && r[3] == index && r[4] == buf[3] {
                return Err(format!("HID++ error {:#04x}: {}", r[5], hex(r)));
            }
            println!("  (async) {}", hex(r));
        }
        Err("timeout".into())
    }

    async fn feature_index(&mut self, feature: u16) -> Result<u8, String> {
        let r = self.request(0x00, 0, &feature.to_be_bytes()).await?;
        if r[4] == 0 {
            return Err(format!("feature {feature:#06x} not present"));
        }
        Ok(r[4])
    }

    async fn listen(&mut self, secs: u64) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(secs) {
            let mut rx = [0u8; 64];
            match tokio::time::timeout(
                Duration::from_millis(200),
                self.reader.read_input_report(&mut rx),
            )
            .await
            {
                Ok(Ok(n)) if n > 0 => {
                    println!("{:7.2}s  {}", start.elapsed().as_secs_f32(), hex(&rx[..n]));
                }
                Ok(Err(e)) => {
                    println!("read error: {e}");
                    return;
                }
                _ => {}
            }
        }
    }
}

fn parse_hex_u8(s: &str) -> Result<u8, Box<dyn Error>> {
    Ok(u8::from_str_radix(s.trim_start_matches("0x"), 16)?)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("nodes") => {
            let pid = args.get(1).map_or(Ok(PID), |p| {
                u16::from_str_radix(p.trim_start_matches("0x"), 16)
            })?;
            for d in nodes_of(pid).await? {
                println!(
                    "usage_page={:#06x} usage={:#06x} id={:?}",
                    d.usage_page, d.usage_id, d.id
                );
            }
        }
        Some("call") => {
            let feature = u16::from_str_radix(args[1].trim_start_matches("0x"), 16)?;
            let func: u8 = args[2].parse()?;
            let params = args[3..]
                .iter()
                .map(|s| parse_hex_u8(s))
                .collect::<Result<Vec<_>, _>>()?;
            let mut h = Hidpp::open().await?;
            let index = h.feature_index(feature).await?;
            println!("feature {feature:#06x} @ index {index}");
            match h.request(index, func, &params).await {
                Ok(r) => println!("reply  {}", hex(&r)),
                Err(e) => println!("error  {e}"),
            }
        }
        Some("listen") => {
            let secs = args.get(1).map_or(Ok(20), |s| s.parse())?;
            let mut h = Hidpp::open().await?;
            println!(">> listening {secs}s: turn the dial / roller, press buttons <<");
            h.listen(secs).await;
        }
        Some("divert-listen") => {
            let secs = args.get(1).map_or(Ok(20), |s| s.parse())?;
            let mut h = Hidpp::open().await?;
            let roller = h.feature_index(0x4610).await?;
            let reprog = h.feature_index(0x1b04).await?;
            let caps = h.request(roller, 0, &[]).await?;
            println!("multi-roller caps {}", hex(&caps));
            let count = caps[4].max(1);
            for id in 0..count {
                println!(
                    "roller {id} setMode(diverted) {:?}",
                    h.request(roller, 3, &[id, 1]).await.map(|r| hex(&r))
                );
            }
            for cid in dialpad_cids() {
                let [hi, lo] = cid.to_be_bytes();
                // SetCidReporting: divert (bit0 valid, bit1 divert) = 0x03.
                println!(
                    "cid {cid:#06x} divert {:?}",
                    h.request(reprog, 3, &[hi, lo, 0x03]).await.map(|r| hex(&r))
                );
            }
            println!(">> diverted; turn the dial / roller, press buttons for {secs}s <<");
            h.listen(secs).await;
            for id in 0..count {
                println!(
                    "roller {id} setMode(native) {:?}",
                    h.request(roller, 3, &[id, 0]).await.map(|r| hex(&r))
                );
            }
            for cid in dialpad_cids() {
                let [hi, lo] = cid.to_be_bytes();
                println!(
                    "cid {cid:#06x} native {:?}",
                    h.request(reprog, 3, &[hi, lo, 0x02]).await.map(|r| hex(&r))
                );
            }
        }
        _ => {
            eprintln!(
                "usage: mx_dialpad_probe nodes | call <feature> <fn> [bytes..] | listen <secs> | divert-listen <secs>"
            );
        }
    }
    Ok(())
}
