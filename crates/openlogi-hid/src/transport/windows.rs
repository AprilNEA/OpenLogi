use std::ops::DerefMut;
#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[cfg(target_os = "windows")]
use std::{error::Error, io};

use async_hid::{AsyncHidRead, HidError, HidResult};
#[cfg(target_os = "windows")]
use async_hid::{AsyncHidWrite, DeviceInfo, DeviceReader, DeviceWriter};
use futures_concurrency::future::Race as _;
#[cfg(target_os = "windows")]
use futures_lite::StreamExt as _;
use hidpp::channel::{LONG_REPORT_ID, SHORT_REPORT_ID};
#[cfg(target_os = "windows")]
use hidpp::{async_trait, channel::RawHidChannel};
#[cfg(target_os = "windows")]
use tokio::sync::Mutex;
#[cfg(target_os = "windows")]
use tracing::debug;

#[cfg(target_os = "windows")]
use openlogi_device::DeviceIoGate;

#[cfg(target_os = "windows")]
use super::windows_hid::{NativeHidWriter, ReportLengths};

#[cfg(target_os = "windows")]
use super::HID_BACKEND;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReportEndpoint {
    Short,
    Long,
    /// The vendor collection at this index of the channel's
    /// [`VendorCollection`]s.
    Vendor(usize),
}

/// The collection a report with `report_id` is written to, given the report
/// ids of the channel's vendor collections, in order.
///
/// Windows splits a device's top-level collections into separate handles, and
/// a handle takes only the reports its own collection declares: the MX
/// Creative Keypad's long handle refuses its image report `0x14` with
/// `ERROR_INVALID_FUNCTION` (#2281). Report `0x10` goes to the short
/// collection, a report a vendor collection carries goes to that collection,
/// and every other report to the long handle — the node the device was matched
/// on, which carries the long HID++ report.
fn endpoint_for_report_id(
    report_id: u8,
    vendor_report_ids: impl IntoIterator<Item = u8>,
) -> ReportEndpoint {
    if report_id == SHORT_REPORT_ID {
        return ReportEndpoint::Short;
    }
    vendor_report_ids
        .into_iter()
        .position(|id| id == report_id)
        .map_or(ReportEndpoint::Long, ReportEndpoint::Vendor)
}

/// Logitech's vendor usage page whose usage id names the one report a
/// collection carries: BLE-direct devices, wired G-series keyboards and the
/// MX Creative Keypad.
const LOGITECH_VENDOR_PAGE: u16 = 0xff43;

/// A top-level collection's usage page and usage id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CollectionUsage {
    page: u16,
    id: u16,
}

#[cfg(target_os = "windows")]
impl From<&DeviceInfo> for CollectionUsage {
    fn from(info: &DeviceInfo) -> Self {
        Self {
            page: info.usage_page,
            id: info.usage_id,
        }
    }
}

impl CollectionUsage {
    /// The report a [`LOGITECH_VENDOR_PAGE`] collection carries, decoded from
    /// its usage id: the low byte sets one bit, and bit `n` is report
    /// `0x10 + n`.
    ///
    /// - `0x0202`, `0x0602` and `0x1a02` carry the 20-byte long report `0x11`.
    /// - `0x0604` carries the 64-byte very-long report `0x12`: the wired
    ///   G-series collection that OpenRGB opens as a second handle on Windows
    ///   for exactly that report.
    /// - The MX Creative Keypad's `0x1a08` and `0x1a10` decode to its vendor
    ///   input `0x13` (grid keys, image acknowledgements) and its vendor
    ///   output `0x14` (LCD images).
    ///
    /// `None` off that page, or when the low byte does not set exactly one bit.
    fn vendor_report_id(self) -> Option<u8> {
        let [_, report_bit] = self.id.to_be_bytes();
        if self.page != LOGITECH_VENDOR_PAGE || report_bit.count_ones() != 1 {
            return None;
        }
        u8::try_from(report_bit.trailing_zeros())
            .ok()
            .map(|bit| SHORT_REPORT_ID + bit)
    }

    /// The report this collection carries when it sits beside the matched
    /// `long` collection and carries a report that neither the short nor the
    /// long handle does: the only siblings the channel opens.
    fn vendor_report_beside(self, long: Self) -> Option<u8> {
        if long.page != LOGITECH_VENDOR_PAGE || self == long {
            return None;
        }
        self.vendor_report_id().filter(|&id| id > LONG_REPORT_ID)
    }
}

/// Read the next input report from whichever of `readers` delivers one first.
///
/// Each handle reads into its own scratch buffer, sized like `buf`, and the
/// winner's report is copied out. Dropping the losing reads loses no report:
/// async-hid's win32 `IoBuffer` owns each in-flight OVERLAPPED read and its
/// buffer (not the future), so the pending operation survives the drop, and
/// the next call — re-locking these same per-endpoint readers — resumes it.
/// This relies on reusing each reader across calls; do not reopen readers per
/// read. `Race` polls `readers` in order, so the caller rotates them.
async fn read_first<G, R>(readers: &mut [G], buf: &mut [u8]) -> HidResult<usize>
where
    G: DerefMut<Target = R>,
    R: AsyncHidRead,
{
    if let [reader] = readers {
        return reader.read_input_report(buf).await;
    }
    let mut scratch = vec![vec![0u8; buf.len()]; readers.len()];
    let (winner, read) = readers
        .iter_mut()
        .zip(&mut scratch)
        .enumerate()
        .map(|(index, (reader, scratch))| async move {
            (index, reader.read_input_report(scratch).await)
        })
        .collect::<Vec<_>>()
        .race()
        .await;
    let len = read?;
    copy_report(scratch.get(winner).map_or(&[], Vec::as_slice), len, buf)
}

fn copy_report(src: &[u8], len: usize, dst: &mut [u8]) -> HidResult<usize> {
    let (Some(report), Some(dst)) = (src.get(..len), dst.get_mut(..len)) else {
        return Err(HidError::message(format!(
            "HID report length {len} exceeds buffer size"
        )));
    };
    dst.copy_from_slice(report);
    Ok(len)
}

/// One collection's output path: the async-hid writer, with the native Win32
/// fallback beside it.
#[cfg(target_os = "windows")]
struct ReportWriter {
    writer: Mutex<DeviceWriter>,
    native_writer: Option<NativeHidWriter>,
}

#[cfg(target_os = "windows")]
impl ReportWriter {
    fn new(writer: DeviceWriter, info: &DeviceInfo) -> Self {
        Self {
            writer: Mutex::new(writer),
            native_writer: NativeHidWriter::new(info),
        }
    }

    async fn write_report(&self, src: &[u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        let mut writer = self.writer.lock().await;
        if let Err(e) = writer.write_output_report(src).await {
            // The native fallback works around async-hid write quirks on a
            // *live* device; reopening a device that is gone can only fail, and
            // its `NativeHidError` would replace the typed `Disconnected`
            // that `is_permanent_disconnect` needs to retire the channel.
            if matches!(e, async_hid::HidError::Disconnected) {
                return Err(Box::new(e));
            }
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

/// A HID++ collection the channel both reads and writes: the short or the long
/// handle.
#[cfg(target_os = "windows")]
struct HidEndpoint {
    reader: Mutex<DeviceReader>,
    writer: ReportWriter,
}

#[cfg(target_os = "windows")]
impl HidEndpoint {
    fn new(reader: DeviceReader, writer: DeviceWriter, info: &DeviceInfo) -> Self {
        Self {
            reader: Mutex::new(reader),
            writer: ReportWriter::new(writer, info),
        }
    }
}

/// A sibling collection that carries one vendor report neither HID++ handle
/// does ([`CollectionUsage::vendor_report_beside`]), opened only in the
/// directions it declares.
#[cfg(target_os = "windows")]
struct VendorCollection {
    report_id: u8,
    reader: Option<Mutex<DeviceReader>>,
    writer: Option<ReportWriter>,
}

#[cfg(target_os = "windows")]
impl VendorCollection {
    /// Open `dev`, which carries `report_id`, for the report kinds it
    /// declares. async-hid sizes a reader's buffer by the collection's input
    /// report length, so a collection without input reports gets no reader.
    /// `None` when `dev` can be neither read nor written.
    async fn open(dev: &async_hid::Device, report_id: u8) -> Option<Self> {
        let info: &DeviceInfo = dev;
        let lengths = match ReportLengths::query(info) {
            Ok(lengths) => lengths,
            Err(e) => {
                debug!(
                    name = %info.name,
                    usage_id = format_args!("{:#06x}", info.usage_id),
                    error = %e,
                    "could not query Windows vendor collection"
                );
                return None;
            }
        };
        let reader = if lengths.input > 0 {
            match dev.open_readable().await {
                Ok(reader) => Some(Mutex::new(reader)),
                Err(e) => {
                    debug!(
                        name = %info.name,
                        usage_id = format_args!("{:#06x}", info.usage_id),
                        error = ?e,
                        "could not open Windows vendor collection for reading"
                    );
                    None
                }
            }
        } else {
            None
        };
        let writer = if lengths.output > 0 {
            match dev.open_writeable().await {
                Ok(writer) => Some(ReportWriter::new(writer, info)),
                Err(e) => {
                    debug!(
                        name = %info.name,
                        usage_id = format_args!("{:#06x}", info.usage_id),
                        error = ?e,
                        "could not open Windows vendor collection for writing"
                    );
                    None
                }
            }
        } else {
            None
        };
        if reader.is_none() && writer.is_none() {
            return None;
        }
        debug!(
            name = %info.name,
            usage_id = format_args!("{:#06x}", info.usage_id),
            report_id = format_args!("{report_id:#04x}"),
            input_len = lengths.input,
            output_len = lengths.output,
            "paired Windows vendor report collection"
        );
        Some(Self {
            report_id,
            reader,
            writer,
        })
    }
}

#[cfg(target_os = "windows")]
pub(super) struct WindowsHidppChannel {
    info: DeviceInfo,
    short: Option<HidEndpoint>,
    long: HidEndpoint,
    /// Sibling collections that carry the device's other vendor reports, such
    /// as the MX Creative Keypad's grid input `0x13` and image output `0x14`.
    vendor: Vec<VendorCollection>,
    /// Rotates which handle [`read_first`] polls first, so a busy handle cannot
    /// starve the others.
    read_turn: AtomicUsize,
    /// Cleared the first time either endpoint reports a permanently dead
    /// handle. Read by [`RawHidChannel::is_connected`], which is what lets
    /// `inventory::ledger` evict this channel — the node-vanish path never
    /// fires for a cabled device whose receiver keeps the HID node enumerated.
    connected: AtomicBool,
    device_io: DeviceIoGate,
}

#[cfg(target_os = "windows")]
impl WindowsHidppChannel {
    pub(super) async fn open(
        long_dev: &async_hid::Device,
        long_info: DeviceInfo,
        device_io: DeviceIoGate,
    ) -> Result<Self, async_hid::HidError> {
        let siblings = interface_siblings(&long_info).await?;
        let (long_reader, long_writer) = long_dev.open().await?;
        let long = HidEndpoint::new(long_reader, long_writer, &long_info);
        let short = match short_collection(&long_info, &siblings) {
            Some(dev) => {
                let short_info: &DeviceInfo = dev;
                match dev.open().await {
                    Ok((reader, writer)) => {
                        debug!(
                            name = %short_info.name,
                            pid = format_args!("{:04x}", short_info.product_id),
                            "paired Windows HID++ short collection"
                        );
                        Some(HidEndpoint::new(reader, writer, short_info))
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
        let vendor = open_vendor_collections(&long_info, &siblings).await;
        debug!(
            name = %long_info.name,
            pid = format_args!("{:04x}", long_info.product_id),
            supports_short = short.is_some(),
            supports_long = true,
            vendor_reports = ?vendor.iter().map(|c| c.report_id).collect::<Vec<_>>(),
            "opened Windows HID++ composite channel"
        );
        Ok(Self {
            info: long_info,
            short,
            long,
            vendor,
            read_turn: AtomicUsize::new(0),
            connected: AtomicBool::new(true),
            device_io,
        })
    }

    fn mark_disconnected(&self) {
        if self.connected.swap(false, Ordering::AcqRel) {
            debug!(name = %self.info.name, "HID channel disconnected");
        }
    }

    /// Honour the permanent-failure half of the [`RawHidChannel::read_report`]
    /// contract: an `Err` is retried by the `hidpp` read loop, so a condition
    /// that will never clear has to park instead of surfacing — otherwise the
    /// loop busy-spins a core until something evicts the channel. Sound because
    /// the read loop always races this future against the channel's close
    /// signal in a `select!`.
    async fn park_disconnected<T>(&self) -> T {
        self.mark_disconnected();
        std::future::pending().await
    }

    /// The writer for a report with `report_id` ([`endpoint_for_report_id`]).
    fn writer_for(&self, report_id: u8) -> io::Result<&ReportWriter> {
        let unsupported = |reason: &str| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                format!("report id {report_id:#04x} {reason}"),
            )
        };
        match endpoint_for_report_id(report_id, self.vendor.iter().map(|c| c.report_id)) {
            ReportEndpoint::Short => self
                .short
                .as_ref()
                .map(|short| &short.writer)
                .ok_or_else(|| unsupported("needs the short collection, which this device lacks")),
            ReportEndpoint::Long => Ok(&self.long.writer),
            ReportEndpoint::Vendor(index) => self
                .vendor
                .get(index)
                .and_then(|collection| collection.writer.as_ref())
                .ok_or_else(|| unsupported("rides a collection that declares no output report")),
        }
    }
}

/// Whether an already-boxed transport error means the handle is permanently
/// dead. [`ReportWriter::write_report`] boxes the `async_hid` error before the
/// channel sees it, so that path can only recognise the variant by downcast.
#[cfg(target_os = "windows")]
fn is_permanent_disconnect(error: &(dyn Error + Send + Sync + 'static)) -> bool {
    error
        .downcast_ref::<async_hid::HidError>()
        .is_some_and(|e| matches!(e, async_hid::HidError::Disconnected))
}

/// The other collections of the physical interface `long_info` belongs to.
///
/// Pairs by physical interface, not by vendor/product/name. Two identical
/// Logitech devices share all three, so an attribute match could splice one
/// device's short handle onto another's long handle. The grouping key (derived
/// from the device path) is unique per physical interface, so it always pairs
/// the correct siblings. A node whose path has an unexpected shape has no
/// siblings and stays a single long handle.
#[cfg(target_os = "windows")]
async fn interface_siblings(
    long_info: &DeviceInfo,
) -> Result<Vec<async_hid::Device>, async_hid::HidError> {
    let Some(long_key) = grouping_key(long_info) else {
        return Ok(Vec::new());
    };
    let all: Vec<async_hid::Device> = HID_BACKEND.enumerate().await?.collect().await;
    Ok(all
        .into_iter()
        .filter(|d| d.id != long_info.id && grouping_key(d).as_deref() == Some(long_key.as_str()))
        .collect())
}

/// The short (`0x10`) HID++ collection among `siblings`, if the matched long
/// collection has one.
#[cfg(target_os = "windows")]
fn short_collection<'a>(
    long_info: &DeviceInfo,
    siblings: &'a [async_hid::Device],
) -> Option<&'a async_hid::Device> {
    // A long-only collection gets no short sibling, as on every other OS: the
    // `hidpp` channel re-frames its short requests as long. The G733 headset's
    // HID++ interface carries a `0xFF00 / 0x0001` collection beside its
    // `0xFF43 / 0x0202` one. Pairing it as the short endpoint sent every short
    // request to a collection no other OS uses for HID++ — the likely reason
    // the headset never enumerated on Windows (#1957), while Linux, which
    // treats the interface as long-only, reads its battery.
    if super::is_long_only_collection(long_info.usage_page, long_info.usage_id) {
        return None;
    }
    siblings
        .iter()
        .find(|d| d.usage_page == 0xff00 && d.usage_id == 0x0001)
}

/// Open every sibling that carries a vendor report neither HID++ handle does.
///
/// macOS and Linux deliver every collection of an interface through one
/// handle, so the matched node reads and writes all of them there. Windows
/// gives each collection its own handle, and a report reaches the device only
/// through the collection that declares it.
#[cfg(target_os = "windows")]
async fn open_vendor_collections(
    long_info: &DeviceInfo,
    siblings: &[async_hid::Device],
) -> Vec<VendorCollection> {
    let long = CollectionUsage::from(long_info);
    let mut collections = Vec::new();
    for dev in siblings {
        let usage = CollectionUsage::from(&**dev);
        let Some(report_id) = usage.vendor_report_beside(long) else {
            continue;
        };
        if let Some(collection) = VendorCollection::open(dev, report_id).await {
            collections.push(collection);
        }
    }
    collections
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
pub(super) fn normalize_collection_path(path: &str) -> String {
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
        self.device_io.ensure_allowed()?;
        let report_id = src.first().copied().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "empty HID output report")
        })?;
        let writer = self.writer_for(report_id)?;
        self.device_io.ensure_allowed()?;
        let result = writer.write_report(src).await;
        if let Err(e) = &result
            && is_permanent_disconnect(e.as_ref())
        {
            self.mark_disconnected();
        }
        result
    }

    async fn read_report(&self, buf: &mut [u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        // Every scratch buffer in `read_first` is sized like `buf`, which the
        // `hidpp` read loop sizes for the widest input report: the MX Creative
        // Keypad's 32-byte grid frames, which a 20-byte HID++ long buffer
        // would truncate.
        let mut readers = Vec::with_capacity(2 + self.vendor.len());
        if let Some(short) = &self.short {
            readers.push(short.reader.lock().await);
        }
        readers.push(self.long.reader.lock().await);
        for collection in &self.vendor {
            if let Some(reader) = &collection.reader {
                readers.push(reader.lock().await);
            }
        }
        let turn = self.read_turn.fetch_add(1, Ordering::Relaxed) % readers.len();
        readers.rotate_left(turn);
        match read_first(&mut readers, buf).await {
            Ok(len) => Ok(len),
            Err(async_hid::HidError::Disconnected) => self.park_disconnected().await,
            Err(e) => Err(e.into()),
        }
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    fn supports_short_long_hidpp(&self) -> Option<(bool, bool)> {
        Some((self.short.is_some(), true))
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
    use std::future::Future;

    use super::*;

    /// Logitech's very-long HID++ lighting report.
    const VERY_LONG_REPORT_ID: u8 = 0x12;
    /// The MX Creative Keypad's vendor input: grid keys and image
    /// acknowledgements.
    const KEYPAD_INPUT_REPORT_ID: u8 = 0x13;
    /// The MX Creative Keypad's vendor output: LCD images.
    const KEYPAD_IMAGE_REPORT_ID: u8 = 0x14;

    fn vendor(id: u16) -> CollectionUsage {
        CollectionUsage {
            page: LOGITECH_VENDOR_PAGE,
            id,
        }
    }

    #[test]
    fn report_ids_select_their_windows_endpoint() {
        assert_eq!(
            endpoint_for_report_id(SHORT_REPORT_ID, []),
            ReportEndpoint::Short
        );
        assert_eq!(
            endpoint_for_report_id(LONG_REPORT_ID, []),
            ReportEndpoint::Long
        );
        assert_eq!(
            endpoint_for_report_id(VERY_LONG_REPORT_ID, []),
            ReportEndpoint::Long
        );
        // Without a vendor collection the long handle stays the only target.
        assert_eq!(
            endpoint_for_report_id(KEYPAD_IMAGE_REPORT_ID, []),
            ReportEndpoint::Long
        );
    }

    /// #2281: the keypad's image write went to the matched `0x1a02` node, which
    /// refuses report `0x14`; it belongs to the `0x1a10` sibling.
    #[test]
    fn a_vendor_report_selects_the_collection_that_carries_it() {
        let keypad = [KEYPAD_INPUT_REPORT_ID, KEYPAD_IMAGE_REPORT_ID];
        assert_eq!(
            endpoint_for_report_id(KEYPAD_IMAGE_REPORT_ID, keypad),
            ReportEndpoint::Vendor(1)
        );
        assert_eq!(
            endpoint_for_report_id(KEYPAD_INPUT_REPORT_ID, keypad),
            ReportEndpoint::Vendor(0)
        );
        // The HID++ reports keep their handles.
        assert_eq!(
            endpoint_for_report_id(LONG_REPORT_ID, keypad),
            ReportEndpoint::Long
        );
        assert_eq!(
            endpoint_for_report_id(SHORT_REPORT_ID, keypad),
            ReportEndpoint::Short
        );
    }

    #[test]
    fn a_vendor_usage_decodes_to_the_report_it_carries() {
        let cases = [
            (0x0202, Some(LONG_REPORT_ID)),
            (0x0602, Some(LONG_REPORT_ID)),
            (0x0604, Some(VERY_LONG_REPORT_ID)),
            (0x1a02, Some(LONG_REPORT_ID)),
            (0x1a08, Some(KEYPAD_INPUT_REPORT_ID)),
            (0x1a10, Some(KEYPAD_IMAGE_REPORT_ID)),
            // No report bit, or more than one.
            (0x1a00, None),
            (0x1a0a, None),
        ];
        for (usage_id, report_id) in cases {
            assert_eq!(
                vendor(usage_id).vendor_report_id(),
                report_id,
                "usage {usage_id:#06x}"
            );
        }
        // Only the vendor page encodes a report in its usage.
        let receiver_long = CollectionUsage {
            page: 0xff00,
            id: 0x0002,
        };
        assert_eq!(receiver_long.vendor_report_id(), None);
    }

    #[test]
    fn only_siblings_with_a_report_beyond_hidpp_are_vendor_collections() {
        let keypad = vendor(0x1a02);
        assert_eq!(
            vendor(0x1a08).vendor_report_beside(keypad),
            Some(KEYPAD_INPUT_REPORT_ID)
        );
        assert_eq!(
            vendor(0x1a10).vendor_report_beside(keypad),
            Some(KEYPAD_IMAGE_REPORT_ID)
        );
        assert_eq!(
            vendor(0x0604).vendor_report_beside(vendor(0x0602)),
            Some(VERY_LONG_REPORT_ID)
        );
        // The matched node itself, and siblings the HID++ handles already
        // cover, are not reopened.
        assert_eq!(keypad.vendor_report_beside(keypad), None);
        assert_eq!(vendor(0x0601).vendor_report_beside(vendor(0x0602)), None);
        // The keypad's consumer-control and system-control collections.
        let consumer = CollectionUsage {
            page: 0x000c,
            id: 0x0001,
        };
        let system = CollectionUsage {
            page: 0x0001,
            id: 0x0080,
        };
        assert_eq!(consumer.vendor_report_beside(keypad), None);
        assert_eq!(system.vendor_report_beside(keypad), None);
        // A receiver's long collection has no vendor siblings.
        let receiver_long = CollectionUsage {
            page: 0xff00,
            id: 0x0002,
        };
        assert_eq!(vendor(0x0604).vendor_report_beside(receiver_long), None);
    }

    /// A handle that delivers `report` on every read, or never when it is
    /// `None`.
    struct ScriptedReader(Option<&'static [u8]>);

    impl AsyncHidRead for ScriptedReader {
        fn read_input_report<'a>(
            &'a mut self,
            buf: &'a mut [u8],
        ) -> impl Future<Output = HidResult<usize>> + Send + 'a {
            let report = self.0;
            async move {
                let Some(report) = report else {
                    return std::future::pending().await;
                };
                let len = report.len().min(buf.len());
                buf[..len].copy_from_slice(&report[..len]);
                Ok(len)
            }
        }
    }

    /// #2281: the keypad's grid frames and image acknowledgements arrive on a
    /// sibling collection while its long handle idles.
    #[test]
    fn a_report_on_any_handle_is_read_while_the_others_idle() {
        const ACK: &[u8] = &[0x13, 0xff, 0x02, 0x2b, 0xc1, 0x00, 0x01];
        for position in 0..3 {
            let mut handles = [
                ScriptedReader(None),
                ScriptedReader(None),
                ScriptedReader(None),
            ];
            handles[position] = ScriptedReader(Some(ACK));
            let mut readers: Vec<&mut ScriptedReader> = handles.iter_mut().collect();
            let mut buf = [0u8; 64];
            let len = futures_lite::future::block_on(read_first(&mut readers, &mut buf))
                .expect("the scripted handle delivers its report");
            assert_eq!(&buf[..len], ACK, "report on handle {position}");
        }
    }

    #[test]
    fn a_single_handle_reads_straight_into_the_buffer() {
        const LONG: &[u8] = &[0x11, 0xff, 0x00, 0x1a];
        let mut handle = ScriptedReader(Some(LONG));
        let mut readers = [&mut handle];
        let mut buf = [0u8; 64];
        let len = futures_lite::future::block_on(read_first(&mut readers, &mut buf))
            .expect("the handle delivers its report");
        assert_eq!(&buf[..len], LONG);
    }

    #[test]
    fn a_report_longer_than_the_buffer_is_an_error() {
        let mut buf = [0u8; 4];
        let error = copy_report(&[0u8; 8], 8, &mut buf).expect_err("8 bytes do not fit in 4");
        assert!(matches!(error, HidError::Message(_)), "{error:?}");
    }

    /// `ReportWriter::write_report` boxes the `async_hid` error before the
    /// channel sees it, so the permanent-disconnect check on that path only
    /// works through a downcast — a plain `matches!` silently never fires.
    #[cfg(target_os = "windows")]
    #[test]
    fn a_boxed_disconnect_is_recognised_as_permanent() {
        let disconnected: Box<dyn Error + Send + Sync> =
            Box::new(async_hid::HidError::Disconnected);
        assert!(is_permanent_disconnect(disconnected.as_ref()));
        // `NotConnected` is the *open* failure, not a dead live handle: it is
        // retryable, so it must keep flowing to the read loop as an error.
        let not_connected: Box<dyn Error + Send + Sync> =
            Box::new(async_hid::HidError::NotConnected);
        assert!(!is_permanent_disconnect(not_connected.as_ref()));
        let unrelated: Box<dyn Error + Send + Sync> =
            Box::new(io::Error::other("write failed for some other reason"));
        assert!(!is_permanent_disconnect(unrelated.as_ref()));
    }
}
