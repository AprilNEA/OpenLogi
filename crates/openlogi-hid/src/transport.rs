//! `RawHidChannel` implementation over `async-hid`.
//!
//! The published `hidpp 0.2` derives short/long-report support by reading the
//! HID report descriptor, but `async-hid 0.4` only exposes descriptors on
//! Linux. We avoid the path entirely by pre-filtering to the Logitech HID++
//! long-report usage page at enumeration time, then returning a hardcoded
//! `Some((true, true))` from `supports_short_long_hidpp`.

use std::{error::Error, sync::Arc};

use async_hid::{AsyncHidRead, AsyncHidWrite, DeviceInfo, DeviceReader, DeviceWriter, HidBackend};
use futures_lite::StreamExt as _;
use hidpp::{
    async_trait,
    channel::{HidppChannel, RawHidChannel},
};
use tokio::sync::Mutex;
use tracing::debug;

/// Logitech HID vendor ID.
const LOGITECH_VID: u16 = 0x046d;
/// HID++ long-report vendor collections, as `(usage_page, usage_id, long_only)`.
///
/// Logitech exposes its HID++ long-report (report id `0x11`) under a
/// vendor-defined HID collection, but the page differs by transport:
///
/// - `0xFF00 / 0x0002` — USB, Logi Bolt / Unifying receivers, and
///   Bluetooth-*classic* devices (MX Master over BT).
/// - `0xFF43 / 0x0202` — Bluetooth-*Low-Energy* directly-paired devices
///   (e.g. the Logitech Lift / Signature mice). Same HID++ protocol, just a
///   different vendor page on the BLE HID report descriptor.
///
/// `long_only` marks a transport that exposes *only* the long report — no
/// short-report (`0x10`) collection — so short HID++ requests must be
/// up-converted to long (see [`short_as_long`]). BLE-direct devices on macOS
/// are long-only; USB / receiver devices carry both. Keeping the flag in this
/// table means a new long-only transport is a single-line addition here, with
/// no second site to update.
///
/// Filtering on these pairs gives us one HID node per physical HID++ device on
/// every supported OS, without reading report descriptors (`async-hid 0.4`
/// only exposes those on Linux).
const HIDPP_LONG_COLLECTIONS: [(u16, u16, bool); 2] =
    [(0xff00, 0x0002, false), (0xff43, 0x0202, true)];

/// HID++ short / long report IDs and the long report's on-wire length
/// (report id + 19 payload bytes). Mirrors `hidpp`'s private constants.
const SHORT_REPORT_ID: u8 = 0x10;
const LONG_REPORT_ID: u8 = 0x11;
const LONG_REPORT_LEN: usize = 20;

/// Whether `(usage_page, usage_id)` is one of the HID++ long-report collections.
fn is_hidpp_long_collection(usage_page: u16, usage_id: u16) -> bool {
    HIDPP_LONG_COLLECTIONS
        .iter()
        .any(|&(page, usage, _)| (page, usage) == (usage_page, usage_id))
}

/// Whether the matched HID++ collection exposes only the long report, so short
/// requests must be re-framed as long (see [`short_as_long`]). `false` for
/// pages not in [`HIDPP_LONG_COLLECTIONS`].
fn is_long_only_collection(usage_page: u16, usage_id: u16) -> bool {
    HIDPP_LONG_COLLECTIONS
        .iter()
        .any(|&(page, usage, long_only)| long_only && (page, usage) == (usage_page, usage_id))
}

/// Re-frame a short HID++ report (`0x10`, 7 bytes) as a long one (`0x11`, 20
/// bytes) for a transport that only exposes the long report.
///
/// `hidpp 0.2` always pings with — and often sends — short reports, but a
/// BLE-direct device exposes only the long report on macOS, so a short
/// `IOHIDDeviceSetReport` fails with `kIOReturnNotFound`. The header bytes
/// (device / feature / function / sw id) sit at the same offsets in both
/// widths; only the trailing payload is zero-padded. The device answers with a
/// long report, which `hidpp` parses and matches by header regardless of width.
///
/// Returns `None` (pass the report through unchanged) when `src` isn't a short
/// report or wouldn't fit the long frame.
fn short_as_long(src: &[u8]) -> Option<[u8; LONG_REPORT_LEN]> {
    if src.first() != Some(&SHORT_REPORT_ID) || src.len() > LONG_REPORT_LEN {
        return None;
    }
    let mut long = [0u8; LONG_REPORT_LEN];
    long[0] = LONG_REPORT_ID;
    long[1..src.len()].copy_from_slice(&src[1..]);
    Some(long)
}

pub(crate) async fn enumerate_hidpp_devices() -> Result<Vec<async_hid::Device>, async_hid::HidError>
{
    let backend = HidBackend::default();
    let all: Vec<async_hid::Device> = backend.enumerate().await?.collect().await;

    // One-time visibility into what the OS actually reports for Logitech nodes,
    // so a transport that uses an unexpected vendor page (e.g. a new BLE mouse)
    // can be diagnosed from `OPENLOGI_LOG=debug` without a rebuild.
    for d in all.iter().filter(|d| d.vendor_id == LOGITECH_VID) {
        debug!(
            name = %d.name,
            pid = format_args!("{:04x}", d.product_id),
            usage_page = format_args!("{:#06x}", d.usage_page),
            usage_id = format_args!("{:#06x}", d.usage_id),
            matched = is_hidpp_long_collection(d.usage_page, d.usage_id),
            "logitech HID node"
        );
    }

    Ok(all
        .into_iter()
        .filter(|d| {
            d.vendor_id == LOGITECH_VID && is_hidpp_long_collection(d.usage_page, d.usage_id)
        })
        .collect())
}

pub(crate) async fn open_hidpp_channel(
    dev: async_hid::Device,
) -> Result<Option<(DeviceInfo, Arc<HidppChannel>)>, async_hid::HidError> {
    // `Device: Deref<Target = DeviceInfo>` — clone the deref'd value so we can
    // keep using `dev` (which `to_device_info` would consume).
    let info: DeviceInfo = (*dev).clone();
    let (reader, writer) = dev.open().await?;
    // BLE-direct devices expose only the long HID++ report; flag the channel so
    // outgoing short requests get up-converted to long (see `write_report`).
    let long_only = is_long_only_collection(info.usage_page, info.usage_id);
    let raw = AsyncHidChannel::new(reader, writer, info.clone(), long_only);
    let channel = match HidppChannel::from_raw_channel(raw).await {
        Ok(c) => Arc::new(c),
        Err(e) => {
            debug!(name = %info.name, error = ?e, "not a HID++ channel");
            return Ok(None);
        }
    };
    Ok(Some((info, channel)))
}

pub(crate) struct AsyncHidChannel {
    reader: Mutex<DeviceReader>,
    writer: Mutex<DeviceWriter>,
    info: DeviceInfo,
    /// When set, outgoing short HID++ reports are rewritten as long ones — the
    /// device (a BLE-direct peripheral) only exposes the long report on macOS.
    long_only: bool,
}

impl AsyncHidChannel {
    pub(crate) fn new(
        reader: DeviceReader,
        writer: DeviceWriter,
        info: DeviceInfo,
        long_only: bool,
    ) -> Self {
        Self {
            reader: Mutex::new(reader),
            writer: Mutex::new(writer),
            info,
            long_only,
        }
    }
}

#[async_trait]
impl RawHidChannel for AsyncHidChannel {
    fn vendor_id(&self) -> u16 {
        self.info.vendor_id
    }

    fn product_id(&self) -> u16 {
        self.info.product_id
    }

    async fn write_report(&self, src: &[u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        let mut w = self.writer.lock().await;
        match self.long_only.then(|| short_as_long(src)).flatten() {
            Some(long) => w.write_output_report(&long).await?,
            None => w.write_output_report(src).await?,
        }
        Ok(src.len())
    }

    async fn read_report(&self, buf: &mut [u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        let mut r = self.reader.lock().await;
        Ok(r.read_input_report(buf).await?)
    }

    fn supports_short_long_hidpp(&self) -> Option<(bool, bool)> {
        Some((true, true))
    }

    async fn get_report_descriptor(
        &self,
        _buf: &mut [u8],
    ) -> Result<usize, Box<dyn Error + Send + Sync>> {
        Err("get_report_descriptor is not implemented; pre-filter to HID++ usage pages".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_both_usb_and_ble_hidpp_collections() {
        assert!(is_hidpp_long_collection(0xff00, 0x0002)); // USB / receiver / BT-classic
        assert!(is_hidpp_long_collection(0xff43, 0x0202)); // BLE-direct (Lift, Signature)
        assert!(!is_hidpp_long_collection(0x0001, 0x0002)); // generic-desktop mouse
        assert!(!is_hidpp_long_collection(0xff43, 0x0002)); // page right, usage wrong
    }

    #[test]
    fn only_ble_collection_is_long_only() {
        assert!(is_long_only_collection(0xff43, 0x0202)); // BLE-direct → up-convert short→long
        assert!(!is_long_only_collection(0xff00, 0x0002)); // USB / receiver carries both reports
        assert!(!is_long_only_collection(0x0001, 0x0002)); // not a HID++ collection at all
    }

    #[test]
    fn upconverts_short_report_preserving_header_and_padding() {
        // [report id, device, feature, func|sw, p0, p1, p2]
        let short = [SHORT_REPORT_ID, 0xff, 0x00, 0x1e, 0xaa, 0xbb, 0xcc];
        let Some(long) = short_as_long(&short) else {
            panic!("short report should up-convert");
        };

        assert_eq!(long[0], LONG_REPORT_ID);
        assert_eq!(&long[1..7], &short[1..]); // header + payload copied verbatim
        assert!(long[7..].iter().all(|&b| b == 0)); // remainder zero-padded
        assert_eq!(long.len(), LONG_REPORT_LEN);
    }

    #[test]
    fn passes_through_non_short_reports() {
        // Already a long report — leave it alone.
        let long_in = [LONG_REPORT_ID, 0xff, 0x00, 0x1e];
        assert!(short_as_long(&long_in).is_none());
        // Empty / unknown frames are passed through too.
        assert!(short_as_long(&[]).is_none());
    }
}
