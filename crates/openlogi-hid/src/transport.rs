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

#[cfg(target_os = "windows")]
use std::io;

#[cfg(target_os = "windows")]
use crate::windows_hid::NativeHidWriter;
#[cfg(target_os = "windows")]
use hidpp::channel::{LONG_REPORT_ID, LONG_REPORT_LENGTH, SHORT_REPORT_ID, SHORT_REPORT_LENGTH};

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
// Windows routes short vs long by report id over the composite channel
// (WindowsHidppChannel), so the long-only up-conversion path — and thus this
// helper — is only reached off Windows. Still compiled + unit-tested there.
#[cfg_attr(
    target_os = "windows",
    allow(
        dead_code,
        reason = "long-only up-conversion is the non-Windows AsyncHidChannel path"
    )
)]
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
    // On Windows the short (0x10) and long (0x11) HID++ report collections are
    // exposed as separate device interfaces, so the channel must open both and
    // route by report id (see WindowsHidppChannel). Elsewhere one node carries
    // both reports (or is long-only), handled by AsyncHidChannel.
    #[cfg(target_os = "windows")]
    {
        let raw = WindowsHidppChannel::open(dev, info.clone()).await?;
        let channel = match HidppChannel::from_raw_channel(raw).await {
            Ok(c) => Arc::new(c),
            Err(e) => {
                debug!(name = %info.name, error = ?e, "not a HID++ channel");
                return Ok(None);
            }
        };
        Ok(Some((info, channel)))
    }

    #[cfg(not(target_os = "windows"))]
    {
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
        // Logged once per actual open. The inventory watcher reuses channels across
        // ticks, so a steadily-connected device should log this on first sight (and
        // on reconnect) only — not every ~2s tick.
        debug!(name = %info.name, vid = format_args!("{:04x}", info.vendor_id), "opened HID++ channel");
        Ok(Some((info, channel)))
    }
}

#[cfg(target_os = "windows")]
struct HidEndpoint {
    reader: Mutex<DeviceReader>,
    writer: Mutex<DeviceWriter>,
    native_writer: Option<NativeHidWriter>,
}

#[cfg(target_os = "windows")]
impl HidEndpoint {
    fn new(reader: DeviceReader, writer: DeviceWriter, info: &DeviceInfo) -> Self {
        Self {
            reader: Mutex::new(reader),
            writer: Mutex::new(writer),
            native_writer: NativeHidWriter::new(info),
        }
    }

    async fn write_report(&self, src: &[u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        let mut writer = self.writer.lock().await;
        if let Err(e) = writer.write_output_report(src).await {
            if let Some(native_writer) = &self.native_writer {
                debug!(
                    error = %e,
                    report_id = format_args!("{:#04x}", src.first().copied().unwrap_or_default()),
                    len = src.len(),
                    "async-hid output report write failed; trying native Windows HID fallback"
                );
                native_writer.write_report(src)?;
                return Ok(src.len());
            }

            return Err(Box::new(e));
        }
        Ok(src.len())
    }
}

#[cfg(target_os = "windows")]
struct WindowsHidppChannel {
    info: DeviceInfo,
    short: Option<HidEndpoint>,
    long: Option<HidEndpoint>,
}

#[cfg(target_os = "windows")]
impl WindowsHidppChannel {
    async fn open(
        long_dev: async_hid::Device,
        long_info: DeviceInfo,
    ) -> Result<Self, async_hid::HidError> {
        let short_dev = find_windows_short_collection(&long_info).await?;
        let (long_reader, long_writer) = long_dev.open().await?;
        let long = Some(HidEndpoint::new(long_reader, long_writer, &long_info));

        let short = match short_dev {
            Some(dev) => {
                let short_info: DeviceInfo = (*dev).clone();
                match dev.open().await {
                    Ok((reader, writer)) => {
                        debug!(
                            name = %short_info.name,
                            pid = format_args!("{:04x}", short_info.product_id),
                            "paired Windows HID++ short collection"
                        );
                        Some(HidEndpoint::new(reader, writer, &short_info))
                    }
                    Err(e) => {
                        debug!(
                            name = %short_info.name,
                            pid = format_args!("{:04x}", short_info.product_id),
                            error = ?e,
                            "could not open Windows HID++ short collection"
                        );
                        None
                    }
                }
            }
            None => None,
        };

        debug!(
            name = %long_info.name,
            pid = format_args!("{:04x}", long_info.product_id),
            supports_short = short.is_some(),
            supports_long = long.is_some(),
            "opened Windows HID++ composite channel"
        );

        Ok(Self {
            info: long_info,
            short,
            long,
        })
    }
}

#[cfg(target_os = "windows")]
async fn find_windows_short_collection(
    long_info: &DeviceInfo,
) -> Result<Option<async_hid::Device>, async_hid::HidError> {
    // Pair the short collection to *this* long collection by physical interface,
    // not by vendor/product/name. Two identical Logitech devices share all three,
    // so an attribute match could splice one device's short handle onto another's
    // long handle. The grouping key (derived from the device path) is unique per
    // physical interface, so it always pairs the correct siblings. A node whose
    // path has an unexpected shape yields `None` and stays long-only.
    let Some(long_key) = grouping_key(long_info) else {
        return Ok(None);
    };
    let all: Vec<async_hid::Device> = HID_BACKEND.enumerate().await?.collect().await;
    Ok(all.into_iter().find(|d| {
        d.usage_page == 0xff00
            && d.usage_id == 0x0001
            && grouping_key(d).as_deref() == Some(long_key.as_str())
    }))
}

/// The device-path key shared by the short and long HID++ collections of one
/// physical interface. `None` for a non-path device id, which never occurs on
/// Windows (every id is a `UncPath`).
#[cfg(target_os = "windows")]
fn grouping_key(info: &DeviceInfo) -> Option<String> {
    match &info.id {
        async_hid::DeviceId::UncPath(p) => Some(normalize_collection_path(&p.to_string())),
        _ => None,
    }
}

/// Collapse a Windows HID interface path to a key that is equal for the short
/// (`&Col01`) and long (`&Col02`) collections of one physical interface and
/// distinct across different interfaces or physical devices.
///
/// A receiver path looks like
/// `\\?\HID#VID_046D&PID_C548&MI_02&Col01#7&348660ac&0&0000#{guid}`. The two
/// HID++ collections share everything except the `&Col0X` hardware-id token and
/// the trailing instance-id segment (`&0000` / `&0001`); stripping both yields a
/// shared key. Falls back to the whole lowercased path when the shape is
/// unexpected, so an unrecognized format simply never pairs — safe, as the node
/// then behaves as a long-only single handle.
#[cfg_attr(
    not(target_os = "windows"),
    allow(
        dead_code,
        reason = "pure path parser is exercised by host unit tests; its only runtime caller is the Windows-gated grouping_key"
    )
)]
fn normalize_collection_path(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    let segments: Vec<&str> = lower.split('#').collect();
    let (Some(hw), Some(inst)) = (segments.get(1), segments.get(2)) else {
        return lower;
    };
    let hw_key = hw
        .split('&')
        .filter(|s| !s.starts_with("col"))
        .collect::<Vec<_>>()
        .join("&");
    let inst_key = inst.rsplit_once('&').map_or(*inst, |(head, _)| head);
    format!("{hw_key}#{inst_key}")
}

#[cfg(target_os = "windows")]
#[async_trait]
impl RawHidChannel for WindowsHidppChannel {
    fn vendor_id(&self) -> u16 {
        self.info.vendor_id
    }

    fn product_id(&self) -> u16 {
        self.info.product_id
    }

    async fn write_report(&self, src: &[u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        let endpoint = match src.first().copied() {
            Some(SHORT_REPORT_ID) => self.short.as_ref(),
            Some(LONG_REPORT_ID) => self.long.as_ref(),
            _ => None,
        }
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "unsupported HID++ report id {:#04x}",
                    src.first().copied().unwrap_or_default()
                ),
            )
        })?;

        endpoint.write_report(src).await
    }

    async fn read_report(&self, buf: &mut [u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        match (&self.short, &self.long) {
            (Some(short), Some(long)) => {
                let mut short_buf = [0u8; SHORT_REPORT_LENGTH];
                let mut long_buf = [0u8; LONG_REPORT_LENGTH];
                let mut short_reader = short.reader.lock().await;
                let mut long_reader = long.reader.lock().await;
                // `select!` drops the losing read future, but no report is lost:
                // async-hid's win32 `IoBuffer` owns the in-flight OVERLAPPED read and
                // its buffer (not the future), so the pending operation survives the
                // drop, and the next `read_report` — re-locking this same endpoint —
                // resumes it and retrieves the report. This relies on reusing the
                // per-endpoint reader across calls; do not reopen readers per read.
                tokio::select! {
                    res = short_reader.read_input_report(&mut short_buf) => {
                        copy_report(&short_buf, res?, buf)
                    }
                    res = long_reader.read_input_report(&mut long_buf) => {
                        copy_report(&long_buf, res?, buf)
                    }
                }
            }
            (Some(endpoint), None) | (None, Some(endpoint)) => {
                let mut reader = endpoint.reader.lock().await;
                Ok(reader.read_input_report(buf).await?)
            }
            (None, None) => Err(Box::new(io::Error::new(
                io::ErrorKind::NotConnected,
                "no Windows HID++ endpoints are open",
            ))),
        }
    }

    fn supports_short_long_hidpp(&self) -> Option<(bool, bool)> {
        Some((self.short.is_some(), self.long.is_some()))
    }

    async fn get_report_descriptor(
        &self,
        _buf: &mut [u8],
    ) -> Result<usize, Box<dyn Error + Send + Sync>> {
        Err("get_report_descriptor is not implemented; pre-filter to HID++ usage pages".into())
    }
}

#[cfg(target_os = "windows")]
fn copy_report(
    src: &[u8],
    len: usize,
    dst: &mut [u8],
) -> Result<usize, Box<dyn Error + Send + Sync>> {
    if len > src.len() || len > dst.len() {
        return Err(Box::new(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("HID report length {len} exceeds buffer size"),
        )));
    }
    dst[..len].copy_from_slice(&src[..len]);
    Ok(len)
}

#[cfg(not(target_os = "windows"))]
pub(crate) struct AsyncHidChannel {
    reader: Mutex<DeviceReader>,
    writer: Mutex<DeviceWriter>,
    info: DeviceInfo,
    /// Whether the device exposes only the long HID++ report (a BLE-direct
    /// peripheral on macOS). Reported via `supports_short_long_hidpp` so the
    /// `hidpp` channel up-converts outgoing short messages to long.
    long_only: bool,
}

#[cfg(not(target_os = "windows"))]
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

#[cfg(not(target_os = "windows"))]
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
        let result = {
            let mut r = self.reader.lock().await;
            r.read_input_report(buf).await
        };
        match result {
            Ok(n) => Ok(n),
            // The device disconnected — there will never be another input
            // report, so this is the permanent-failure case of the
            // `RawHidChannel::read_report` contract: errors are retried by the
            // `hidpp` read loop (surfacing this one would busy-spin a core
            // until the inventory watcher evicts the channel), so park instead.
            // The contract guarantees every caller races this future against
            // the channel's close signal, which tears the read down on drop.
            Err(async_hid::HidError::Disconnected) => std::future::pending().await,
            Err(e) => Err(e.into()),
        }
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

    #[test]
    fn short_and_long_collections_of_one_interface_share_a_grouping_key() {
        // Real Bolt receiver paths: the short (Col01) and long (Col02) HID++
        // collections of interface MI_02 must collapse to the same key.
        let short = normalize_collection_path(
            r"\\?\HID#VID_046D&PID_C548&MI_02&Col01#7&348660ac&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}",
        );
        let long = normalize_collection_path(
            r"\\?\HID#VID_046D&PID_C548&MI_02&Col02#7&348660ac&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}",
        );
        assert_eq!(short, long);
        assert_eq!(short, "vid_046d&pid_c548&mi_02#7&348660ac&0");
    }

    #[test]
    fn distinct_interfaces_do_not_share_a_grouping_key() {
        // A different interface (MI_01) on the same receiver has its own instance
        // hash, so it must not pair with MI_02's HID++ collections.
        let mi01 = normalize_collection_path(
            r"\\?\HID#VID_046D&PID_C548&MI_01&Col02#7&1cc2d467&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}",
        );
        let mi02 = normalize_collection_path(
            r"\\?\HID#VID_046D&PID_C548&MI_02&Col02#7&348660ac&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}",
        );
        assert_ne!(mi01, mi02);
    }

    #[test]
    fn distinct_physical_receivers_do_not_share_a_grouping_key() {
        // Two receivers plugged in at once (here two identical Bolt receivers,
        // same VID/PID/interface/collection) must not cross-pair: each physical
        // device has a distinct instance hash, which the key preserves. This is
        // the multi-receiver scenario the single-interface tests don't cover.
        let recv_a = normalize_collection_path(
            r"\\?\HID#VID_046D&PID_C548&MI_02&Col01#7&348660ac&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}",
        );
        let recv_b = normalize_collection_path(
            r"\\?\HID#VID_046D&PID_C548&MI_02&Col01#7&9f1be20c&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}",
        );
        assert_ne!(recv_a, recv_b);

        // A Bolt + a Unifying receiver (different PID) must also stay distinct.
        let bolt = normalize_collection_path(
            r"\\?\HID#VID_046D&PID_C548&MI_02&Col02#7&348660ac&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}",
        );
        let unifying = normalize_collection_path(
            r"\\?\HID#VID_046D&PID_C52B&MI_02&Col02#7&1a2b3c4d&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}",
        );
        assert_ne!(bolt, unifying);
    }
}
