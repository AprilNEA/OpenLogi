//! `RawHidChannel` implementation over `async-hid`.
//!
//! `hidpp` derives short/long-report support by reading the HID report
//! descriptor, but `async-hid 0.4` only exposes descriptors on Linux. We avoid
//! that path by pre-filtering to the Logitech HID++ vendor collections at
//! enumeration time (see [`HIDPP_LONG_COLLECTIONS`]) and reporting support
//! straight from the channel: USB / receiver collections carry both reports;
//! BLE-direct collections are long-only, and the `hidpp` channel up-converts
//! outgoing short messages to long for them.
//!
//! # Per-platform handle model
//!
//! On macOS (`IOHIDManager`) and Linux (`hidraw`) a single OS handle for the
//! `0xff00/0x0002` collection accepts **both** HID++ report ids — short (`0x10`)
//! and long (`0x11`) — so one [`AsyncHidChannel`] is all we need.
//!
//! Windows is different: the receiver exposes the short and long HID++ reports
//! as **two separate top-level collections → two separate device handles**
//! (`0xff00/0x0001` = short, `0xff00/0x0002` = long). A handle rejects any report
//! id that is not its own with `ERROR_INVALID_FUNCTION` *before* the device sees
//! it, so HID++ 1.0 receiver-register access (pairing / device enumeration, which
//! is short-report) fails on a long-only handle. On Windows we therefore pair the
//! two collections into one [`WinDualChannel`] that routes each write by report id
//! and merges reads from both handles. See `HIDPP_SHORT_COLLECTION`.

#[cfg(windows)]
use std::collections::HashMap;
use std::{error::Error, sync::Arc};

use async_hid::{AsyncHidRead, AsyncHidWrite, DeviceInfo, DeviceReader, DeviceWriter, HidBackend};
use futures_lite::StreamExt as _;
#[cfg(windows)]
use hidpp::channel::SHORT_REPORT_ID;
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
/// up-converted to long (handled by the `hidpp` channel). BLE-direct devices are
/// long-only; USB / receiver devices carry both. Keeping the flag in this table
/// means a new long-only transport is a single-line addition here, with no second
/// site to update.
///
/// Filtering on these pairs gives us one HID node per physical HID++ device on
/// every supported OS, without reading report descriptors (`async-hid 0.4`
/// only exposes those on Linux).
const HIDPP_LONG_COLLECTIONS: [(u16, u16, bool); 2] =
    [(0xff00, 0x0002, false), (0xff43, 0x0202, true)];

/// The short-report (`0x10`) companion collection for the USB / receiver page.
///
/// On Windows this is a *separate* device handle from its long-report sibling
/// `0xff00/0x0002` (see the module docs); we open both and pair them. There is no
/// short companion for the long-only BLE-direct page, so a node may legitimately
/// have no short handle.
#[cfg(windows)]
const HIDPP_SHORT_COLLECTION: (u16, u16) = (0xff00, 0x0001);

/// Whether `(usage_page, usage_id)` is one of the HID++ long-report collections.
fn is_hidpp_long_collection(usage_page: u16, usage_id: u16) -> bool {
    HIDPP_LONG_COLLECTIONS
        .iter()
        .any(|&(page, usage, _)| (page, usage) == (usage_page, usage_id))
}

/// Whether the matched HID++ collection exposes only the long report, so short
/// requests must be re-framed as long (done in the `hidpp` channel). `false` for
/// pages not in [`HIDPP_LONG_COLLECTIONS`].
///
/// On non-Windows this drives the channel's short/long advertisement directly. On
/// Windows long-only is instead derived from the *absence* of a paired short
/// handle, and this only labels the diagnostic log line in `enumerate_hidpp_devices`.
fn is_long_only_collection(usage_page: u16, usage_id: u16) -> bool {
    HIDPP_LONG_COLLECTIONS
        .iter()
        .any(|&(page, usage, long_only)| long_only && (page, usage) == (usage_page, usage_id))
}

/// A physical HID++ node to open into a channel.
///
/// On most platforms this is a single OS handle that carries both report ids. On
/// Windows it pairs the long handle with its short companion (when present) so
/// the resulting channel can speak both short and long HID++ — see the module
/// docs. Derefs to the underlying [`DeviceInfo`] (of the long handle on Windows)
/// so callers can filter on `vendor_id` / `product_id` before paying the
/// channel-open cost.
#[cfg(not(windows))]
pub(crate) struct HidppNode {
    dev: async_hid::Device,
}

#[cfg(windows)]
pub(crate) struct HidppNode {
    /// The long-report (`0x11`) handle, `0xff00/0x0002` (or the long-only BLE page).
    long: async_hid::Device,
    /// The short-report (`0x10`) handle, `0xff00/0x0001`. `None` for long-only
    /// transports (BLE-direct) that expose no short collection.
    short: Option<async_hid::Device>,
}

impl std::ops::Deref for HidppNode {
    type Target = DeviceInfo;

    fn deref(&self) -> &Self::Target {
        // `async_hid::Device: Deref<Target = DeviceInfo>`, so `&self.<field>`
        // deref-coerces to `&DeviceInfo` at this return site.
        #[cfg(not(windows))]
        {
            &self.dev
        }
        #[cfg(windows)]
        {
            &self.long
        }
    }
}

#[cfg(not(windows))]
pub(crate) async fn enumerate_hidpp_devices() -> Result<Vec<HidppNode>, async_hid::HidError> {
    let backend = HidBackend::default();
    let all: Vec<async_hid::Device> = backend.enumerate().await?.collect().await;

    log_logitech_nodes(&all);

    Ok(all
        .into_iter()
        .filter(|d| {
            d.vendor_id == LOGITECH_VID && is_hidpp_long_collection(d.usage_page, d.usage_id)
        })
        .map(|dev| HidppNode { dev })
        .collect())
}

/// Windows enumeration pairs each long-report collection with its short-report
/// sibling on the same physical interface (matched by [`grouping_key`]), so the
/// channel can issue both `0x10` and `0x11` reports.
#[cfg(windows)]
pub(crate) async fn enumerate_hidpp_devices() -> Result<Vec<HidppNode>, async_hid::HidError> {
    let backend = HidBackend::default();
    let all: Vec<async_hid::Device> = backend.enumerate().await?.collect().await;

    log_logitech_nodes(&all);

    // Index every short companion by grouping key, then attach it to its long
    // sibling. We move (not clone) `Device`s because they are not `Clone`.
    let mut shorts: HashMap<String, async_hid::Device> = HashMap::new();
    let mut longs: Vec<async_hid::Device> = Vec::new();
    for d in all {
        if d.vendor_id != LOGITECH_VID {
            continue;
        }
        if is_hidpp_long_collection(d.usage_page, d.usage_id) {
            longs.push(d);
        } else if (d.usage_page, d.usage_id) == HIDPP_SHORT_COLLECTION {
            if let Some(key) = grouping_key(&d) {
                shorts.insert(key, d);
            }
        }
    }

    Ok(longs
        .into_iter()
        .map(|long| {
            let short = grouping_key(&long).and_then(|k| shorts.remove(&k));
            if short.is_none() && is_long_only_collection(long.usage_page, long.usage_id) {
                debug!(name = %long.name, "long-only transport; no short companion expected");
            } else if short.is_none() {
                debug!(name = %long.name, "no short companion found; short HID++ requests will fail");
            }
            HidppNode { long, short }
        })
        .collect())
}

/// One-time visibility into what the OS reports for Logitech nodes, so a
/// transport that uses an unexpected vendor page (e.g. a new BLE mouse) can be
/// diagnosed from `OPENLOGI_LOG=debug` without a rebuild.
fn log_logitech_nodes(all: &[async_hid::Device]) {
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
}

#[cfg(not(windows))]
pub(crate) async fn open_hidpp_channel(
    node: HidppNode,
) -> Result<Option<(DeviceInfo, Arc<HidppChannel>)>, async_hid::HidError> {
    // `Device: Deref<Target = DeviceInfo>` — clone the deref'd value so we can
    // keep using `node.dev` (which `to_device_info` would consume).
    let info: DeviceInfo = (*node.dev).clone();
    let (reader, writer) = node.dev.open().await?;
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

/// Windows: open the long handle and (when present) its short companion, then
/// wrap them in a [`WinDualChannel`] that routes writes by report id and merges
/// reads. A node with no short companion behaves exactly like the single-handle
/// long-only path.
#[cfg(windows)]
pub(crate) async fn open_hidpp_channel(
    node: HidppNode,
) -> Result<Option<(DeviceInfo, Arc<HidppChannel>)>, async_hid::HidError> {
    let info: DeviceInfo = (*node.long).clone();
    let (long_reader, long_writer) = node.long.open().await?;
    let short = match node.short {
        Some(s) => Some(s.open().await?),
        None => None,
    };
    let raw = WinDualChannel::new(long_reader, long_writer, short, info.clone());
    let channel = match HidppChannel::from_raw_channel(raw).await {
        Ok(c) => Arc::new(c),
        Err(e) => {
            debug!(name = %info.name, error = ?e, "not a HID++ channel");
            return Ok(None);
        }
    };
    Ok(Some((info, channel)))
}

/// Single-handle channel: one OS handle carries both HID++ report ids (macOS /
/// Linux). Retained on non-Windows targets only.
#[cfg(not(windows))]
pub(crate) struct AsyncHidChannel {
    reader: Mutex<DeviceReader>,
    writer: Mutex<DeviceWriter>,
    info: DeviceInfo,
    /// Whether the device exposes only the long HID++ report (a BLE-direct
    /// peripheral). Reported via `supports_short_long_hidpp` so the `hidpp`
    /// channel up-converts outgoing short messages to long.
    long_only: bool,
}

#[cfg(not(windows))]
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

#[cfg(not(windows))]
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
        // are long-only, where the `hidpp` channel up-converts outgoing short
        // messages to long.
        Some((!self.long_only, true))
    }

    async fn get_report_descriptor(
        &self,
        _buf: &mut [u8],
    ) -> Result<usize, Box<dyn Error + Send + Sync>> {
        Err("get_report_descriptor is not implemented; pre-filter to HID++ usage pages".into())
    }
}

/// Largest HID++ input report is the long report (20 bytes incl. id); 64 is a
/// comfortable ceiling for the merged-read scratch buffer.
#[cfg(windows)]
const WIN_READ_BUF_LEN: usize = 64;

/// Windows dual-handle channel: short (`0x10`) and long (`0x11`) HID++ reports
/// live on separate device handles. Outgoing writes are routed by leading report
/// id; incoming reads are serviced from whichever handle produces a report first.
#[cfg(windows)]
pub(crate) struct WinDualChannel {
    long_reader: Mutex<DeviceReader>,
    long_writer: Mutex<DeviceWriter>,
    /// The short handle, split into its reader/writer. `None` for long-only
    /// transports (BLE-direct), which the channel then reports as short-unsupported.
    short_reader: Option<Mutex<DeviceReader>>,
    short_writer: Option<Mutex<DeviceWriter>>,
    info: DeviceInfo,
}

#[cfg(windows)]
impl WinDualChannel {
    pub(crate) fn new(
        long_reader: DeviceReader,
        long_writer: DeviceWriter,
        short: Option<(DeviceReader, DeviceWriter)>,
        info: DeviceInfo,
    ) -> Self {
        let (short_reader, short_writer) = match short {
            Some((r, w)) => (Some(Mutex::new(r)), Some(Mutex::new(w))),
            None => (None, None),
        };
        Self {
            long_reader: Mutex::new(long_reader),
            long_writer: Mutex::new(long_writer),
            short_reader,
            short_writer,
            info,
        }
    }
}

#[cfg(windows)]
#[async_trait]
impl RawHidChannel for WinDualChannel {
    fn vendor_id(&self) -> u16 {
        self.info.vendor_id
    }

    fn product_id(&self) -> u16 {
        self.info.product_id
    }

    async fn write_report(&self, src: &[u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        // Route by report id: short (0x10) reports must go to the short handle,
        // everything else (long 0x11) to the long handle. A short report with no
        // short handle would be rejected by Windows, but that path is unreachable:
        // a long-only node reports short-unsupported, so the `hidpp` channel
        // up-converts shorts to long before they reach us.
        if src.first() == Some(&SHORT_REPORT_ID) {
            if let Some(writer) = &self.short_writer {
                let mut w = writer.lock().await;
                w.write_output_report(src).await?;
                return Ok(src.len());
            }
        }
        let mut w = self.long_writer.lock().await;
        w.write_output_report(src).await?;
        Ok(src.len())
    }

    async fn read_report(&self, buf: &mut [u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        // Responses to short requests arrive on the short handle and long on the
        // long handle, so a single read must service both. Race the two handles
        // and return whichever yields a report first.
        //
        // Cancellation-safety: `or` drops the losing future once the winner
        // resolves, but that does NOT drop the in-flight read. async-hid's win32
        // `IoBuffer` keeps its `pending` flag, OS `OVERLAPPED`, and target buffer
        // inside the `DeviceReader` (which lives on in the `Mutex`), not in the
        // future. So a cancelled read leaves `pending = true` with the overlapped
        // `ReadFile` still filling that buffer; the next call re-enters `read()`,
        // sees `pending`, and resumes via `GetOverlappedResult` instead of issuing
        // a second `ReadFile` (its `start_io` even asserts `!pending`). The report
        // is therefore delivered to a subsequent call, never dropped. Verified
        // against async-hid 0.4 (`backend/win32/buffer.rs`) and empirically: a
        // Bolt `enumerate()` drains its burst of short device-arrival events and
        // reads every paired slot without loss.
        let read_long = async {
            let mut tmp = [0u8; WIN_READ_BUF_LEN];
            let mut r = self.long_reader.lock().await;
            let n = r.read_input_report(&mut tmp).await?;
            Ok::<(usize, [u8; WIN_READ_BUF_LEN]), Box<dyn Error + Send + Sync>>((n, tmp))
        };

        let (n, tmp) = match &self.short_reader {
            Some(short_reader) => {
                let read_short = async {
                    let mut tmp = [0u8; WIN_READ_BUF_LEN];
                    let mut r = short_reader.lock().await;
                    let n = r.read_input_report(&mut tmp).await?;
                    Ok::<(usize, [u8; WIN_READ_BUF_LEN]), Box<dyn Error + Send + Sync>>((n, tmp))
                };
                futures_lite::future::or(read_long, read_short).await?
            }
            None => read_long.await?,
        };

        let n = n.min(buf.len());
        buf[..n].copy_from_slice(&tmp[..n]);
        Ok(n)
    }

    fn supports_short_long_hidpp(&self) -> Option<(bool, bool)> {
        // Short support is exactly "do we hold a short handle?". Without one the
        // node is long-only and the `hidpp` channel up-converts shorts to long.
        Some((self.short_reader.is_some(), true))
    }

    async fn get_report_descriptor(
        &self,
        _buf: &mut [u8],
    ) -> Result<usize, Box<dyn Error + Send + Sync>> {
        Err("get_report_descriptor is not implemented; pre-filter to HID++ usage pages".into())
    }
}

/// A key shared by the short and long HID++ collections of the same physical
/// Windows interface, used to pair them in [`enumerate_hidpp_devices`].
///
/// Windows HID interface paths look like
/// `\\?\HID#VID_046D&PID_C548&MI_02&Col01#7&348660ac&0&0000#{guid}`. The two
/// HID++ collections of one receiver share everything except the `&Col0X`
/// hardware-id token and the trailing instance-id segment (`&0000` / `&0001`);
/// stripping both yields a key that is equal for the pair and distinct across
/// physical interfaces. Returns `None` for non-`UncPath` ids (other platforms).
#[cfg(windows)]
fn grouping_key(dev: &async_hid::Device) -> Option<String> {
    match &dev.id {
        async_hid::DeviceId::UncPath(p) => Some(normalize_collection_path(&p.to_string())),
        _ => None,
    }
}

/// See [`grouping_key`]. Splits the `#`-delimited path into
/// `[prefix, hardware_id, instance_id, class_guid]`, drops the `&col..` token
/// from the hardware id and the trailing collection-index segment from the
/// instance id. Falls back to the whole (lowercased) path when the shape is
/// unexpected, so an unrecognized format simply never pairs (safe: the node then
/// behaves as a long-only single handle).
#[cfg(windows)]
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
        assert!(is_long_only_collection(0xff43, 0x0202)); // BLE-direct → short-unsupported
        assert!(!is_long_only_collection(0xff00, 0x0002)); // USB / receiver carries both reports
        assert!(!is_long_only_collection(0x0001, 0x0002)); // not a HID++ collection at all
    }

    #[cfg(windows)]
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

    #[cfg(windows)]
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

    #[cfg(windows)]
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
