//! `RawHidChannel` implementation over `async-hid`.
//!
//! The published `hidpp 0.2` derives short/long-report support by reading the
//! HID report descriptor, but `async-hid 0.4` only exposes descriptors on
//! Linux. We avoid the path entirely by pre-filtering to the Logitech HID++
//! long-report usage page at enumeration time, then returning a hardcoded
//! `Some((true, true))` from `supports_short_long_hidpp`.
//!
//! Bluetooth-direct devices add one wrinkle: they expose *only* the long HID++
//! report (`0x11`) — there is no short (`0x10`) output report — so any short
//! frame `hidpp` emits (e.g. the protocol-version ping) is repacked as a long
//! report on the way out. See [`AsyncHidChannel::write_report`].

use std::{error::Error, sync::Arc};

use async_hid::{AsyncHidRead, AsyncHidWrite, DeviceInfo, DeviceReader, DeviceWriter, HidBackend};
use futures_lite::StreamExt as _;
use hidpp::{
    async_trait,
    channel::{HidppChannel, LONG_REPORT_ID, LONG_REPORT_LENGTH, RawHidChannel, SHORT_REPORT_ID},
};
use tokio::sync::Mutex;
use tracing::debug;

/// Logitech HID vendor ID.
const LOGITECH_VID: u16 = 0x046d;
/// HID++ long-report usage page / usage for receivers and USB-wired devices.
/// Filtering on this pair gives us one HID node per physical HID++ device.
const HIDPP_USAGE_PAGE: u16 = 0xff00;
const HIDPP_LONG_USAGE_ID: u16 = 0x0002;
/// HID++ long-report usage page / usage a device exposes when paired directly
/// over Bluetooth (LE). Logitech moves the HID++ vendor collection to page
/// `0xff43` with usage `0x0202` there — the MX Master 3, for instance, presents
/// only generic mouse/keyboard collections on page `0xff00` and its HID++
/// channel on `0xff43`. Filtering on `0xff00` alone misses every BT-direct
/// device. (Matches Solaar's Bluetooth HID++ report mapping.)
pub(crate) const HIDPP_BLE_USAGE_PAGE: u16 = 0xff43;
const HIDPP_BLE_LONG_USAGE_ID: u16 = 0x0202;

/// Whether a HID interface is the HID++ long-report node we drive devices
/// through — either the receiver/USB collection (`0xff00`/`0x0002`) or the
/// Bluetooth-direct one (`0xff43`/`0x0202`).
fn is_hidpp_long_node(d: &async_hid::DeviceInfo) -> bool {
    d.vendor_id == LOGITECH_VID
        && ((d.usage_page == HIDPP_USAGE_PAGE && d.usage_id == HIDPP_LONG_USAGE_ID)
            || (d.usage_page == HIDPP_BLE_USAGE_PAGE && d.usage_id == HIDPP_BLE_LONG_USAGE_ID))
}

pub(crate) async fn enumerate_hidpp_devices() -> Result<Vec<async_hid::Device>, async_hid::HidError>
{
    let backend = HidBackend::default();
    Ok(backend
        .enumerate()
        .await?
        .filter(|d| is_hidpp_long_node(d))
        .collect()
        .await)
}

/// A cheap presence signature of the currently-connected HID++ nodes:
/// `(vendor_id, product_id, usage_page)` per node, sorted. Enumerating the HID
/// registry does *not* open any device, so this is safe to poll frequently —
/// unlike [`enumerate_hidpp_devices`] callers that open each channel, which on
/// a Bluetooth-direct device renegotiates the BLE link and jitters the pointer.
pub(crate) async fn present_device_keys() -> Result<Vec<(u16, u16, u16)>, async_hid::HidError> {
    let backend = HidBackend::default();
    let mut keys: Vec<(u16, u16, u16)> = backend
        .enumerate()
        .await?
        .filter(|d| is_hidpp_long_node(d))
        .map(|d| (d.vendor_id, d.product_id, d.usage_page))
        .collect()
        .await;
    keys.sort_unstable();
    Ok(keys)
}

pub(crate) async fn open_hidpp_channel(
    dev: async_hid::Device,
) -> Result<Option<(DeviceInfo, Arc<HidppChannel>)>, async_hid::HidError> {
    // `Device: Deref<Target = DeviceInfo>` — clone the deref'd value so we can
    // keep using `dev` (which `to_device_info` would consume).
    let info: DeviceInfo = (*dev).clone();
    let (reader, writer) = dev.open().await?;
    let raw = AsyncHidChannel::new(reader, writer, info.clone());
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
}

impl AsyncHidChannel {
    pub(crate) fn new(reader: DeviceReader, writer: DeviceWriter, info: DeviceInfo) -> Self {
        Self {
            reader: Mutex::new(reader),
            writer: Mutex::new(writer),
            info,
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

    async fn write_report(&self, src: &[u8]) -> Result<usize, Box<dyn Error>> {
        let mut w = self.writer.lock().await;
        // Bluetooth-direct devices expose only the long HID++ report (`0x11`):
        // there is no short (`0x10`) output report, and writing one makes macOS
        // return `kIOReturnNotFound`. `hidpp 0.2` still emits short frames (the
        // protocol-version ping in `determine_version`, the Root feature
        // queries), so repack any short report as a long one — same 3-byte
        // header, payload zero-extended to the long length. The device replies
        // with a long report either way, which the read path already handles.
        if self.info.usage_page == HIDPP_BLE_USAGE_PAGE && src.first() == Some(&SHORT_REPORT_ID) {
            w.write_output_report(&repack_short_as_long(src)).await?;
            return Ok(src.len());
        }
        w.write_output_report(src).await?;
        Ok(src.len())
    }

    async fn read_report(&self, buf: &mut [u8]) -> Result<usize, Box<dyn Error>> {
        let mut r = self.reader.lock().await;
        Ok(r.read_input_report(buf).await?)
    }

    fn supports_short_long_hidpp(&self) -> Option<(bool, bool)> {
        Some((true, true))
    }

    async fn get_report_descriptor(&self, _buf: &mut [u8]) -> Result<usize, Box<dyn Error>> {
        Err("get_report_descriptor is not implemented; pre-filter to HID++ usage pages".into())
    }
}

/// Repack a short HID++ report (`0x10`, 7 bytes) as a long one (`0x11`, 20
/// bytes) for transports that only carry the long report, e.g. Bluetooth-direct
/// devices. The 3-byte header (`device_index, feature_index, func/sw`) and the
/// short payload are preserved at the same offsets; the rest is left zeroed.
/// Reports that aren't a short HID++ frame are returned padded but otherwise
/// untouched.
fn repack_short_as_long(src: &[u8]) -> [u8; LONG_REPORT_LENGTH] {
    let mut long = [0u8; LONG_REPORT_LENGTH];
    let n = src.len().min(LONG_REPORT_LENGTH);
    long[..n].copy_from_slice(&src[..n]);
    long[0] = LONG_REPORT_ID;
    long
}

#[cfg(test)]
mod tests {
    use super::*;
    use hidpp::channel::SHORT_REPORT_LENGTH;

    #[test]
    fn repack_promotes_report_id_and_preserves_header_payload() {
        // A short protocol-version ping: id, device_index, feature_index,
        // func/sw nibble, then the 3-byte payload.
        let short = [SHORT_REPORT_ID, 0xff, 0x00, 0x11, 0xaa, 0xbb, 0xcc];
        assert_eq!(short.len(), SHORT_REPORT_LENGTH);

        let long = repack_short_as_long(&short);

        assert_eq!(long.len(), LONG_REPORT_LENGTH);
        assert_eq!(long[0], LONG_REPORT_ID, "report id upgraded to long");
        assert_eq!(&long[1..7], &short[1..7], "header + payload preserved");
        assert!(long[7..].iter().all(|&b| b == 0), "tail zero-padded");
    }
}
