//! Characterizes the MX Creative Keypad LCD panel: brightness range and image
//! writes.
//!
//! Open questions this answers before any device-layer design:
//!
//! 1. What range does `0x8040` BrightnessControl advertise? The reference
//!    driver (`node-logitech-mx-creative-console`, MIT) clamps writes to
//!    `>= 1` because "0 resets the device" — if `get_info` reports `min = 0`,
//!    the GUI slider must not offer it.
//! 2. Does the vendor image write work on the single `ff43:1a02` node, as
//!    the reference describes it: 4095-byte report `0x14`,
//!    `14 ff 02 2b [part] 01 00 01 00 [x] [y] [w] [h] 00 [len] [jpeg…]`
//!    with big-endian u16 fields, `part` = index | 0x20, | 0x80 on the first
//!    packet, | 0x40 on the last; continuation packets carry the JPEG from
//!    byte 5. The panel is 480×480 and key `n` (0-based, row-major) is the
//!    118×118 tile at (23 + col·158, 6 + row·158).
//! 3. What, if anything, does the device answer to an image write?
//!
//! The probe paints each key a distinct solid colour (key 1 red … key 9
//! white-ish), reading every report the node emits for a few seconds after.
//! Pass `--brightness-only` to skip the drawing. Quit any running OpenLogi
//! agent first. Unplugging the keypad restores its own panel.
//!
//! ## Results (verified 2026-10-14, MX Keypad `046d:c354`, macOS)
//!
//! - `0x8040` sits at index `0x0f`. `get_info` answers `00 64 65 10 00 00`:
//!   max 100, 101 steps, min **0**, capabilities `TRANSIENT` (bit 4) — the
//!   device does not persist the level itself. Level read back: 70.
//! - Image writes work exactly as the reference describes, on the single
//!   `ff43:1a02` node, with no handshake or keepalive: all nine solid tiles
//!   landed edge to edge in row-major order, confirming the 118×118 tiles at
//!   (23 + col·158, 6 + row·158). A ~1 KiB JPEG fits one packet.
//! - Every tile write is acknowledged on report `0x13`:
//!   `13 ff 02 2b c1 00 01 …` (the part byte echoed without its `0x20`
//!   marker). The first write also drew one `13 ff 02 10 c5 a0` notification
//!   whose meaning is unknown.
//! - The `02` in byte 2 is not a HID++ feature index (index 2 is `0x0003`
//!   here): it is the vendor channel that also carries the grid-key reports.
//!
//! The probe's own min-0 reading is the open hazard: the reference clamps
//! writes to >= 1 because 0 "resets the device". Not tried here.

use std::error::Error;
use std::io::Cursor;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use async_hid::{AsyncHidRead as _, AsyncHidWrite as _, HidBackend};
use futures_lite::StreamExt as _;
use image::codecs::jpeg::JpegEncoder;
use image::{ExtendedColorType, ImageEncoder as _};

const KEY_VID: u16 = 0x046d;
const KEY_PID: u16 = 0xc354;
/// `0x8040` BrightnessControl's index in this keypad's feature table (the
/// recorded fixture's table; the probe verifies it with a root lookup).
const BRIGHTNESS_FEATURE: u16 = 0x8040;
const SWID: u8 = 0x0b;

const PACKET_LEN: usize = 4095;
const FIRST_HEADER_LEN: usize = 20;
const NEXT_HEADER_LEN: usize = 5;
const KEY_SIZE: u16 = 118;

static BACKEND: OnceLock<HidBackend> = OnceLock::new();

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

async fn open_keypad() -> Result<(async_hid::DeviceReader, async_hid::DeviceWriter), Box<dyn Error>>
{
    let backend = BACKEND.get_or_init(HidBackend::default);
    let mut stream = backend
        .enumerate()
        .await
        .map_err(|e| format!("enumerate: {e}"))?;
    while let Some(d) = stream.next().await {
        if d.vendor_id == KEY_VID
            && d.product_id == KEY_PID
            && d.usage_page == 0xff43
            && d.usage_id == 0x1a02
        {
            return Ok(d.open().await.map_err(|e| format!("open: {e}"))?);
        }
    }
    Err("keypad ff43:1a02 node not found".into())
}

/// Send a short HID++ request and wait for its reply (same feature index and
/// function/swid byte), printing anything else that arrives meanwhile.
async fn request(
    reader: &mut async_hid::DeviceReader,
    writer: &mut async_hid::DeviceWriter,
    feature_index: u8,
    function: u8,
    params: [u8; 3],
) -> Result<Vec<u8>, Box<dyn Error>> {
    let fn_swid = (function << 4) | SWID;
    let report = [
        0x10,
        0xff,
        feature_index,
        fn_swid,
        params[0],
        params[1],
        params[2],
    ];
    writer
        .write_output_report(&report)
        .await
        .map_err(|e| format!("write: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let mut buf = [0u8; 64];
        let Ok(read) = tokio::time::timeout(
            Duration::from_millis(200),
            reader.read_input_report(&mut buf),
        )
        .await
        else {
            continue;
        };
        let n = read.map_err(|e| format!("read: {e}"))?;
        let reply = &buf[..n];
        if n >= 4 && reply[1] == 0xff && reply[2] == feature_index && reply[3] == fn_swid {
            return Ok(reply.to_vec());
        }
        if n >= 4 && reply[1] == 0xff && reply[2] == 0xff {
            return Err(format!("HID++ error: {}", hex(reply)).into());
        }
        println!("    (unrelated) {}", hex(reply));
    }
    Err("no reply within 2 s".into())
}

fn solid_jpeg(rgb: [u8; 3]) -> Result<Vec<u8>, Box<dyn Error>> {
    let pixels: Vec<u8> = std::iter::repeat_n(rgb, usize::from(KEY_SIZE) * usize::from(KEY_SIZE))
        .flatten()
        .collect();
    let mut out = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut out, 90).write_image(
        &pixels,
        u32::from(KEY_SIZE),
        u32::from(KEY_SIZE),
        ExtendedColorType::Rgb8,
    )?;
    Ok(out.into_inner())
}

fn part_byte(index: u8, first: bool, last: bool) -> u8 {
    index | 0x20 | if first { 0x80 } else { 0 } | if last { 0x40 } else { 0 }
}

/// The reference driver's packetization, written out independently.
fn image_packets(
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    jpeg: &[u8],
) -> Result<Vec<Vec<u8>>, Box<dyn Error>> {
    let mut packets = Vec::new();
    let first_len = jpeg.len().min(PACKET_LEN - FIRST_HEADER_LEN);
    let mut first = vec![0u8; PACKET_LEN];
    first[..5].copy_from_slice(&[
        0x14,
        0xff,
        0x02,
        0x2b,
        part_byte(1, true, first_len == jpeg.len()),
    ]);
    first[5..7].copy_from_slice(&0x0100u16.to_be_bytes());
    first[7..9].copy_from_slice(&0x0100u16.to_be_bytes());
    first[9..11].copy_from_slice(&x.to_be_bytes());
    first[11..13].copy_from_slice(&y.to_be_bytes());
    first[13..15].copy_from_slice(&w.to_be_bytes());
    first[15..17].copy_from_slice(&h.to_be_bytes());
    let total = u16::try_from(jpeg.len()).map_err(|_| "tile JPEG exceeds u16 length")?;
    first[18..20].copy_from_slice(&total.to_be_bytes());
    first[FIRST_HEADER_LEN..FIRST_HEADER_LEN + first_len].copy_from_slice(&jpeg[..first_len]);
    packets.push(first);
    let mut offset = first_len;
    let mut part = 2u8;
    while offset < jpeg.len() {
        let len = (jpeg.len() - offset).min(PACKET_LEN - NEXT_HEADER_LEN);
        let last = offset + len == jpeg.len();
        let mut packet = vec![0u8; PACKET_LEN];
        packet[..5].copy_from_slice(&[0x14, 0xff, 0x02, 0x2b, part_byte(part, false, last)]);
        packet[NEXT_HEADER_LEN..NEXT_HEADER_LEN + len].copy_from_slice(&jpeg[offset..offset + len]);
        packets.push(packet);
        offset += len;
        part += 1;
    }
    Ok(packets)
}

async fn drain(reader: &mut async_hid::DeviceReader, ms: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {
        let mut buf = [0u8; 64];
        if let Ok(Ok(n)) = tokio::time::timeout(
            Duration::from_millis(100),
            reader.read_input_report(&mut buf),
        )
        .await
        {
            let shown = n.min(24);
            println!(
                "    {:5.2}s  len {n:2}  {}",
                start.elapsed().as_secs_f32(),
                hex(&buf[..shown])
            );
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let brightness_only = std::env::args().any(|arg| arg == "--brightness-only");
    let (mut reader, mut writer) = open_keypad().await?;

    // Root getFeature(0x8040) — confirms the index rather than trusting it.
    let [hi, lo] = BRIGHTNESS_FEATURE.to_be_bytes();
    let root = request(&mut reader, &mut writer, 0x00, 0, [hi, lo, 0]).await?;
    let index = root[4];
    println!("0x8040 BrightnessControl at feature index {index:#04x}");
    let info = request(&mut reader, &mut writer, index, 0, [0; 3]).await?;
    println!("  get_info      {}", hex(&info));
    let level = request(&mut reader, &mut writer, index, 1, [0; 3]).await?;
    println!("  get_brightness {}", hex(&level));
    // Same layout `hidpp`'s `BrightnessInfo::from_payload` decodes.
    println!(
        "  -> max {}, steps {}, caps {:#04x}, min {}; level {}",
        u16::from_be_bytes([info[4], info[5]]),
        u16::from_be_bytes([info[10], info[6]]),
        info[7],
        u16::from_be_bytes([info[8], info[9]]),
        u16::from_be_bytes([level[4], level[5]]),
    );
    if brightness_only {
        return Ok(());
    }

    let colours: [[u8; 3]; 9] = [
        [0xff, 0x00, 0x00],
        [0x00, 0xff, 0x00],
        [0x00, 0x00, 0xff],
        [0xff, 0xff, 0x00],
        [0x00, 0xff, 0xff],
        [0xff, 0x00, 0xff],
        [0xff, 0x80, 0x00],
        [0x80, 0x00, 0xff],
        [0xe0, 0xe0, 0xe0],
    ];
    for (key, rgb) in (0u16..).zip(colours) {
        let (row, col) = (key / 3, key % 3);
        let (x, y) = (23 + col * 158, 6 + row * 158);
        let jpeg = solid_jpeg(rgb)?;
        let packets = image_packets(x, y, KEY_SIZE, KEY_SIZE, &jpeg)?;
        println!(
            "key {} at ({x},{y}): {} B jpeg in {} packet(s), first header {}",
            key + 1,
            jpeg.len(),
            packets.len(),
            hex(&packets[0][..FIRST_HEADER_LEN])
        );
        for packet in &packets {
            writer
                .write_output_report(packet)
                .await
                .map_err(|e| format!("image write: {e}"))?;
        }
        drain(&mut reader, 300).await;
    }
    println!("reports after the last write:");
    drain(&mut reader, 2_000).await;
    println!(
        "probe done — check the keypad: keys 1..9 should read red, green, blue, yellow, cyan, magenta, orange, violet, light grey"
    );
    Ok(())
}
