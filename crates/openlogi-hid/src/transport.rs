//! `RawHidChannel` implementation over `async-hid`.
//!
//! `hidpp` derives short/long-report support by reading the HID report
//! descriptor, but `async-hid 0.4` only exposes descriptors on Linux. We avoid
//! that path by pre-filtering to the Logitech HID++ vendor collections at
//! enumeration time (see [`HIDPP_LONG_COLLECTIONS`]) and reporting support
//! straight from [`AsyncHidChannel::supports_short_long_hidpp`]: USB / receiver
//! collections carry both reports; BLE-direct collections are long-only, and the
//! `hidpp` channel up-converts outgoing short messages to long for them.

use std::{
    error::Error,
    sync::{Arc, LazyLock},
};

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
/// - `0xFF43 / 0x0602` — wired G-series gaming keyboards (e.g. the G513): a
///   distinct vendor collection on the same `0xFF43` page. Carries both report
///   widths, so it is not long-only.
///
/// `long_only` marks a transport that exposes *only* the long report — no
/// short-report (`0x10`) collection — so short HID++ requests must be
/// up-converted to long (handled by the `hidpp` channel). BLE-direct devices on
/// macOS are long-only; USB / receiver / wired-keyboard devices carry both.
/// Keeping the flag in this table means a new long-only transport is a
/// single-line addition here, with no second site to update.
///
/// Filtering on these pairs gives us one HID node per physical HID++ device on
/// every supported OS, without reading report descriptors (`async-hid 0.4`
/// only exposes those on Linux).
const HIDPP_LONG_COLLECTIONS: [(u16, u16, bool); 3] = [
    (0xff00, 0x0002, false),
    (0xff43, 0x0202, true),
    (0xff43, 0x0602, false),
];

/// Whether `(usage_page, usage_id)` is one of the HID++ long-report collections.
fn is_hidpp_long_collection(usage_page: u16, usage_id: u16) -> bool {
    HIDPP_LONG_COLLECTIONS
        .iter()
        .any(|&(page, usage, _)| (page, usage) == (usage_page, usage_id))
}

/// Whether the matched HID++ collection exposes only the long report, so short
/// requests must be re-framed as long (done in the `hidpp` channel). `false` for
/// pages not in [`HIDPP_LONG_COLLECTIONS`].
fn is_long_only_collection(usage_page: u16, usage_id: u16) -> bool {
    HIDPP_LONG_COLLECTIONS
        .iter()
        .any(|&(page, usage, long_only)| long_only && (page, usage) == (usage_page, usage_id))
}

/// Process-wide HID backend, created once and reused for every enumeration.
///
/// async-hid's macOS backend wraps an `IOHIDManager`; `HidBackend::default()`
/// builds, schedules, and (on drop) cancels one. The inventory watcher
/// enumerates every ~2 s, so building a fresh backend per call spun up and tore
/// down an `IOHIDManager` on every tick — needless churn that kept the process
/// busy and its heap dirty around the clock (issue #99). Reusing one long-lived
/// backend is the usage async-hid intends, and keeps the device set warm between
/// polls. `HidBackend` is `Arc`-backed, so this is shared, not copied.
///
/// `enumerate` is also reached from `open_route_writer`, so the inventory
/// watcher and a (rare) lighting write can enumerate through this one backend
/// concurrently. That is sound: async-hid declares the backend `Send + Sync`,
/// `enumerate` only reads a snapshot (`IOHIDManagerCopyDevices`), and sharing a
/// single long-lived `IOHIDManager` across threads is the model hidapi uses too.
static HID_BACKEND: LazyLock<HidBackend> = LazyLock::new(HidBackend::default);

pub(crate) async fn enumerate_hidpp_devices() -> Result<Vec<async_hid::Device>, async_hid::HidError>
{
    let all: Vec<async_hid::Device> = HID_BACKEND.enumerate().await?.collect().await;

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

/// Open the raw HID writer for a directly-attached (USB) device, for sending
/// reports the HID++ wrapper can't model — e.g. the 64-byte `0x12` lighting
/// frames G-series keyboards use. Returns `None` for Bolt routes or when no
/// matching node is connected.
pub(crate) async fn open_route_writer(
    route: &crate::route::DeviceRoute,
) -> Result<Option<DeviceWriter>, async_hid::HidError> {
    let crate::route::DeviceRoute::Direct {
        vendor_id,
        product_id,
    } = route
    else {
        return Ok(None);
    };
    let candidates = enumerate_hidpp_devices().await?;
    for dev in candidates {
        if dev.vendor_id == *vendor_id && dev.product_id == *product_id {
            let (_reader, writer) = dev.open().await?;
            return Ok(Some(writer));
        }
    }
    Ok(None)
}

pub(crate) async fn open_hidpp_channel(
    dev: async_hid::Device,
) -> Result<Option<(DeviceInfo, Arc<HidppChannel>)>, async_hid::HidError> {
    // `Device: Deref<Target = DeviceInfo>` — clone the deref'd value so we can
    // keep using `dev` (which `to_device_info` would consume).
    let info: DeviceInfo = (*dev).clone();
    let (reader, writer) = dev.open().await?;
    // BLE-direct devices expose only the long HID++ report; flag the channel so
    // it advertises short-unsupported and the `hidpp` channel up-converts shorts.
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
    /// Whether the device exposes only the long HID++ report (a BLE-direct
    /// peripheral on macOS). Reported via `supports_short_long_hidpp` so the
    /// `hidpp` channel up-converts outgoing short messages to long.
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
        w.write_output_report(src).await?;
        Ok(src.len())
    }

    async fn read_report(&self, buf: &mut [u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        let mut r = self.reader.lock().await;
        Ok(r.read_input_report(buf).await?)
    }

    fn supports_short_long_hidpp(&self) -> Option<(bool, bool)> {
        // USB / receiver collections carry both reports; BLE-direct collections
        // are long-only (no short report on macOS), where the `hidpp` channel
        // up-converts outgoing short messages to long.
        Some((!self.long_only, true))
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
    fn matches_usb_ble_and_keyboard_hidpp_collections() {
        assert!(is_hidpp_long_collection(0xff00, 0x0002)); // USB / receiver / BT-classic
        assert!(is_hidpp_long_collection(0xff43, 0x0202)); // BLE-direct (Lift, Signature)
        assert!(is_hidpp_long_collection(0xff43, 0x0602)); // wired G-series keyboard (G513)
        assert!(!is_hidpp_long_collection(0x0001, 0x0002)); // generic-desktop mouse
        assert!(!is_hidpp_long_collection(0xff43, 0x0002)); // page right, usage wrong
    }

    #[test]
    fn only_ble_collection_is_long_only() {
        assert!(is_long_only_collection(0xff43, 0x0202)); // BLE-direct → short-unsupported
        assert!(!is_long_only_collection(0xff00, 0x0002)); // USB / receiver carries both reports
        assert!(!is_long_only_collection(0xff43, 0x0602)); // wired G-series keyboard carries both
        assert!(!is_long_only_collection(0x0001, 0x0002)); // not a HID++ collection at all
    }
}
