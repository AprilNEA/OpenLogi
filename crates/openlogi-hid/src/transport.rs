//! `RawHidChannel` implementation over `async-hid`.
//!
//! `hidpp` derives short/long-report support by reading the HID report
//! descriptor, but `async-hid 0.4` only exposes descriptors on Linux. We avoid
//! that path by pre-filtering to the Logitech HID++ vendor collections at
//! enumeration time (see [`HIDPP_LONG_COLLECTIONS`]) and reporting support
//! straight from [`hidpp::channel::RawHidChannel::supports_short_long_hidpp`]: USB / receiver
//! collections carry both reports; BLE-direct collections are long-only, and the
//! `hidpp` channel up-converts outgoing short messages to long for them.

use std::error::Error;
#[cfg(not(target_os = "windows"))]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, LazyLock};

#[cfg(not(target_os = "windows"))]
use async_hid::{AsyncHidRead, AsyncHidWrite, DeviceReader, DeviceWriter};
use async_hid::{DeviceInfo, HidBackend};
use futures_lite::{Stream, StreamExt as _};
use hidpp::channel::{HidppChannel, RequestSwId, SwIdPolicy};
use hidpp::nibble::U4;
#[cfg(not(target_os = "windows"))]
use hidpp::{async_trait, channel::RawHidChannel};
#[cfg(not(target_os = "windows"))]
use tokio::sync::Mutex;
use tracing::debug;

use crate::LOGITECH_VENDOR_ID;
use openlogi_device::backend::{BackendError, HotplugEvent, NodeId, NodeInfo};
use openlogi_device::write::matches_litra;
use openlogi_device::{DeviceIoGate, DeviceIoSignal, device_io_channel};

/// Collapses `async-hid`'s error taxonomy into the backend-agnostic
/// [`BackendError`] every caller above this module sees.
///
/// A named function, not a `From` impl: both types are foreign to this crate
/// now that the contract lives in `openlogi-device`, which is the orphan rule
/// saying out loud what the layering already did — an adapter belongs to the
/// backend it adapts. `Disconnected` and `NotConnected` fold together; nothing
/// above the transport acts on the distinction.
fn backend_error(error: async_hid::HidError) -> BackendError {
    match error {
        async_hid::HidError::Disconnected | async_hid::HidError::NotConnected => {
            BackendError::Disconnected
        }
        other => BackendError::Backend(other.to_string()),
    }
}

fn device_io_suspended() -> BackendError {
    BackendError::Backend("host device I/O is suspended".into())
}

fn device_io_error() -> Box<dyn Error + Send + Sync> {
    std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        "host device I/O is suspended",
    )
    .into()
}

/// Classify a failed device open. On macOS `IOHIDDeviceOpen` denies silently —
/// the error is indistinguishable from exclusive access — so fold the Input
/// Monitoring state into the message: it is the difference between "grant the
/// permission" and "close the other app, or log out and back in".
#[cfg(not(target_os = "windows"))]
fn open_error(error: async_hid::HidError) -> BackendError {
    match backend_error(error) {
        #[cfg(target_os = "macos")]
        BackendError::Backend(message) => {
            let hint = if crate::permissions::has_access() {
                "Input Monitoring is granted to this process — another app may \
                 hold the device exclusively, or macOS is serving a stale \
                 permission session (log out and back in)"
            } else {
                "Input Monitoring is NOT granted to this process; grant it to \
                 OpenLogi Agent under System Settings → Privacy & Security → \
                 Input Monitoring"
            };
            BackendError::Backend(format!("{message}: {hint}"))
        }
        other => other,
    }
}

/// `DeviceId` is an opaque OS handle (a hidraw path, a Windows device path, an
/// IOKit entry), so [`NodeId`] carries its `Debug` rendering verbatim.
///
/// Verbatim matters: that string is what [`NodeInfo::identity`] has always
/// embedded for serial-less nodes, so keeping it byte-identical keeps existing
/// raw-HID routes resolving across this refactor.
fn node_id(info: &DeviceInfo) -> NodeId {
    NodeId::from(format!("{:?}", info.id))
}

/// Restates an `async-hid` node as the backend-agnostic [`NodeInfo`] every
/// layer above this module stores, filters and routes on.
fn node_info(info: &DeviceInfo) -> NodeInfo {
    {
        NodeInfo {
            id: node_id(info),
            vendor_id: info.vendor_id,
            product_id: info.product_id,
            usage_page: info.usage_page,
            usage_id: info.usage_id,
            name: info.name.clone(),
            manufacturer: info.manufacturer.clone(),
            serial_number: info.serial_number.clone(),
        }
    }
}

/// Bitmask of leased HID++ software ids (`1..=15`; bit `N` means id `N` is taken).
///
/// HID++ correlates request/response by `(device, feature, function, software_id)`.
/// Concurrent opens of the same physical HID node each get a private pending
/// queue but share the OS input report stream, so a shared software id lets a
/// response satisfy the wrong open. Each channel leases one **fixed** id for its
/// lifetime (no rotation — offset rotating sequences still collide across
/// channels) and frees it on drop via [`SwIdPolicy::Leased`].
static SW_ID_LEASES: AtomicU16 = AtomicU16::new(0);

mod native;
pub(crate) use native::native_backend;

#[cfg(any(target_os = "windows", test))]
mod windows;
// Native Win32 HID report-write fallback, used by the Windows composite
// channel in `windows` when async-hid's async write path fails.
#[cfg(target_os = "windows")]
mod windows_hid;
#[cfg(target_os = "windows")]
use windows::WindowsHidppChannel;
#[cfg(test)]
use windows::normalize_collection_path;

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
// Not `expect`: the lint fires in the `--lib` build and not in the `--test`
// one, so an expectation is always unfulfilled for one of them.
#[cfg_attr(
    target_os = "windows",
    expect(clippy::allow_attributes, reason = "see above"),
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
/// builds, schedules, and (on drop) cancels one. Building a fresh backend per
/// reconciliation spun up and tore down an `IOHIDManager` repeatedly — needless
/// churn (issue #99). Reusing one long-lived backend is the usage async-hid
/// intends, and keeps the device set warm between event/recovery passes.
/// `HidBackend` is `Arc`-backed, so this is shared, not copied.
///
/// Inventory and route-addressed standalone operations may enumerate through
/// this one backend concurrently. That is sound: async-hid declares the backend
/// `Send + Sync`, `enumerate` only reads a snapshot
/// (`IOHIDManagerCopyDevices`), and sharing a single long-lived `IOHIDManager`
/// across threads is the model hidapi uses too.
static HID_BACKEND: LazyLock<HidBackend> = LazyLock::new(HidBackend::default);

/// One process-wide lifecycle authority for this host HID stack. Every native
/// backend handle and open channel receives the corresponding read gate.
static DEVICE_IO: LazyLock<(DeviceIoSignal, DeviceIoGate)> = LazyLock::new(device_io_channel);

pub(crate) fn device_io_signal() -> DeviceIoSignal {
    DEVICE_IO.0.clone()
}

pub(crate) fn device_io_gate() -> DeviceIoGate {
    DEVICE_IO.1.clone()
}

/// Subscribe to the backend's node connect/disconnect events, restated as the
/// backend-agnostic [`HotplugEvent`].
///
/// The node identity `async-hid` attaches is dropped: every consumer reacts by
/// re-enumerating, so carrying it would only invite someone to trust it.
pub(crate) fn watch_nodes() -> Result<impl Stream<Item = HotplugEvent> + Send + Unpin, BackendError>
{
    let stream = HID_BACKEND.watch().map_err(backend_error)?;
    Ok(stream.map(|event| match event {
        async_hid::DeviceEvent::Connected(_) => HotplugEvent::Connected,
        async_hid::DeviceEvent::Disconnected(_) => HotplugEvent::Disconnected,
    }))
}

pub(crate) async fn enumerate_devices() -> Result<Vec<async_hid::Device>, BackendError> {
    let all: Vec<async_hid::Device> = HID_BACKEND
        .enumerate()
        .await
        .map_err(backend_error)?
        .collect()
        .await;

    // One-time visibility into what the OS actually reports for Logitech nodes,
    // so a transport that uses an unexpected vendor page (e.g. a new BLE mouse)
    // can be diagnosed from `OPENLOGI_LOG=debug` without a rebuild.
    for d in all.iter().filter(|d| d.vendor_id == LOGITECH_VENDOR_ID) {
        debug!(
            name = %d.name,
            pid = format_args!("{:04x}", d.product_id),
            usage_page = format_args!("{:#06x}", d.usage_page),
            usage_id = format_args!("{:#06x}", d.usage_id),
            matched = is_hidpp_long_collection(d.usage_page, d.usage_id),
            "logitech HID node"
        );
    }

    Ok(all)
}

/// Whether an enumerated node belongs to the HID++ channel path.
///
/// Wraps [`is_hidpp_candidate`] with the parts only this backend can answer:
/// the platform node id needed to recognise a `hid-logitech-dj` child node.
pub(crate) fn is_hidpp_node(device: &async_hid::Device) -> bool {
    is_hidpp_candidate(
        device.vendor_id,
        device.product_id,
        device.usage_page,
        device.usage_id,
        is_receiver_child_node(&device.id),
    )
}

/// Whether an enumerated node belongs to the HID++ channel path.
///
/// Standalone drivers have precedence over the generic collection matcher. A
/// Litra Glow intentionally uses the same BLE usage collection as Logitech
/// HID++ peripherals, so the full product/usage tuple must be excluded here;
/// the collection itself remains a valid HID++ candidate for other products.
fn is_hidpp_candidate(
    vendor_id: u16,
    product_id: u16,
    usage_page: u16,
    usage_id: u16,
    receiver_child: bool,
) -> bool {
    vendor_id == LOGITECH_VENDOR_ID
        && is_hidpp_long_collection(usage_page, usage_id)
        && !matches_litra(vendor_id, product_id, usage_page, usage_id)
        && !receiver_child
}

/// Returns `true` when a HID++ node is a virtual per-device interface created by
/// the `hid-logitech-dj` kernel driver as a child of a Unifying or Bolt receiver.
///
/// On Linux, each device paired to a Unifying receiver gets its own hidraw node
/// whose sysfs path is a subdirectory of the receiver's HID device path. These
/// nodes expose the same HID++ long-report collection as the receiver, but HID++
/// communication must go through the receiver node, not these child nodes.
/// Probing them directly causes long timeouts and produces no useful inventory.
///
/// Detection: the sysfs path of a child node looks like
/// `.../0003:046D:C52B.0009/0003:046D:4076.000A`
/// while the receiver itself ends at `…/0003:046D:C52B.0009`. We check whether
/// any known receiver PID appears as a *parent directory* component in the path.
#[cfg(target_os = "linux")]
fn is_receiver_child_node(id: &async_hid::DeviceId) -> bool {
    use async_hid::DeviceId;
    let DeviceId::DevPath(dev_path) = id else {
        return false;
    };
    let Some(node_name) = dev_path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let sysfs_link = format!("/sys/class/hidraw/{node_name}/device");
    let Ok(real_path) = std::fs::canonicalize(&sysfs_link) else {
        return false;
    };
    is_receiver_child_sysfs_path(&real_path.to_string_lossy())
}

/// Determines whether a resolved sysfs path belongs to a device that is a
/// child of a known receiver. Separated from `is_receiver_child_node` so it
/// can be unit-tested without filesystem access.
#[cfg(any(target_os = "linux", test))]
fn is_receiver_child_sysfs_path(path: &str) -> bool {
    // Build parent-component markers from the canonical receiver registry so
    // adding a new receiver identity only needs to be done in one place.
    // The kernel HID device name format is "BUS:VID:PID.IFACE" with uppercase hex.
    crate::RECEIVERS.iter().any(|receiver| {
        let marker = format!(":{:04X}:{:04X}.", receiver.vendor_id, receiver.product_id);
        // A parent component contains the marker followed by at least one
        // more "/" — it is not the terminal component of the path.
        path.find(&marker)
            .is_some_and(|idx| path[idx + marker.len()..].contains('/'))
    })
}

#[cfg(not(target_os = "linux"))]
fn is_receiver_child_node(_id: &async_hid::DeviceId) -> bool {
    false
}

/// Lease one free software id in `1..=15`, or `None` if all 15 are held.
fn try_lease_sw_id() -> Option<RequestSwId> {
    loop {
        let bits = SW_ID_LEASES.load(Ordering::Acquire);
        let free = (1u8..=15).find(|&id| bits & (1u16 << id) == 0)?;
        let next = bits | (1u16 << free);
        if SW_ID_LEASES
            .compare_exchange(bits, next, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            // `free` is `1..=15`, so the id-0 rejection can never fire here.
            return RequestSwId::new(U4::from_lo(free));
        }
    }
}

fn free_sw_id(id: u8) {
    if (1..=15).contains(&id) {
        SW_ID_LEASES.fetch_and(!(1u16 << id), Ordering::Release);
    }
}

/// Give `channel` a process-unique fixed software id for its lifetime, or
/// refuse when all 15 ids are leased.
///
/// The `Leased` policy is fixed-id by construction — rotation cannot be
/// combined with it, which is the point: concurrent channels that share a
/// rotating `1..=15` sequence eventually reuse the same id and cross-match.
/// Refusing on exhaustion is equally deliberate: with the pool empty, the
/// default id 1 is by construction held by a live channel, so falling back to
/// it would silently recreate the response cross-matching this allocator
/// exists to prevent. A refused open surfaces as a failed probe, which the
/// ledger replays and retries next tick.
fn configure_channel_sw_ids(channel: &mut HidppChannel) -> Result<(), BackendError> {
    let id = try_lease_sw_id().ok_or_else(|| {
        BackendError::Backend(
            "all 15 HID++ software ids are leased — refusing an open that would share one".into(),
        )
    })?;
    channel.set_sw_id_policy(SwIdPolicy::Leased {
        id,
        free: free_sw_id,
    });
    Ok(())
}

pub(crate) async fn open_hidpp_channel(
    dev: &async_hid::Device,
    device_io: DeviceIoGate,
) -> Result<Option<Arc<HidppChannel>>, BackendError> {
    if !device_io.allows_io() {
        return Err(device_io_suspended());
    }
    // `Device: Deref<Target = DeviceInfo>` — clone the deref'd value because
    // the channel keeps it for the lifetime of the open.
    let info: DeviceInfo = (**dev).clone();
    // On Windows the short (0x10) and long (0x11) HID++ report collections are
    // exposed as separate device interfaces, so the channel must open both and
    // route by report id (see WindowsHidppChannel). Elsewhere one node carries
    // both reports (or is long-only), handled by AsyncHidChannel.
    #[cfg(target_os = "windows")]
    {
        let raw = WindowsHidppChannel::open(dev, info.clone(), device_io)
            .await
            .map_err(backend_error)?;
        let channel = match HidppChannel::from_raw_channel(raw).await {
            Ok(mut c) => {
                configure_channel_sw_ids(&mut c)?;
                Arc::new(c)
            }
            Err(e) => {
                debug!(name = %info.name, error = ?e, "not a HID++ channel");
                return Ok(None);
            }
        };
        Ok(Some(channel))
    }

    #[cfg(not(target_os = "windows"))]
    {
        let (reader, writer) = dev.open().await.map_err(open_error)?;
        if !device_io.allows_io() {
            return Err(device_io_suspended());
        }
        // BLE-direct devices expose only the long HID++ report; flag the channel so
        // it advertises short-unsupported and the `hidpp` channel up-converts shorts.
        let long_only = is_long_only_collection(info.usage_page, info.usage_id);
        let raw = AsyncHidChannel::new(reader, writer, info.clone(), long_only, device_io);
        let channel = match HidppChannel::from_raw_channel(raw).await {
            Ok(mut c) => {
                configure_channel_sw_ids(&mut c)?;
                Arc::new(c)
            }
            Err(e) => {
                debug!(name = %info.name, error = ?e, "not a HID++ channel");
                return Ok(None);
            }
        };
        // Logged once per actual open. The inventory watcher reuses channels across
        // reconciliations, so a steadily-connected device should log this on first
        // sight (and on reconnect) only — not every pass.
        debug!(name = %info.name, vid = format_args!("{:04x}", info.vendor_id), "opened HID++ channel");
        Ok(Some(channel))
    }
}

#[cfg(test)]
mod sw_id_lease_tests {
    use super::{free_sw_id, try_lease_sw_id};

    #[test]
    fn leases_are_unique_until_freed() {
        // Leave any ids held by concurrent tests alone: lease two free slots,
        // check they differ, free them, and confirm the first id is reusable.
        let free = |id: super::RequestSwId| free_sw_id(id.get().to_lo());
        let Some(a) = try_lease_sw_id() else {
            return;
        };
        let Some(b) = try_lease_sw_id() else {
            free(a);
            return;
        };
        assert_ne!(a, b);
        free(a);
        let Some(c) = try_lease_sw_id() else {
            free(b);
            return;
        };
        assert_eq!(c, a);
        free(b);
        free(c);
    }
}

#[cfg(not(target_os = "windows"))]
pub(crate) struct AsyncHidChannel {
    reader: Mutex<DeviceReader>,
    writer: Mutex<DeviceWriter>,
    info: DeviceInfo,
    connected: AtomicBool,
    device_io: DeviceIoGate,
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
        device_io: DeviceIoGate,
    ) -> Self {
        Self {
            reader: Mutex::new(reader),
            writer: Mutex::new(writer),
            info,
            connected: AtomicBool::new(true),
            device_io,
            long_only,
        }
    }

    fn mark_disconnected(&self) {
        if self.connected.swap(false, Ordering::AcqRel) {
            debug!(name = %self.info.name, "HID channel disconnected");
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
        if !self.device_io.allows_io() {
            return Err(device_io_error());
        }
        let mut w = self.writer.lock().await;
        if !self.device_io.allows_io() {
            return Err(device_io_error());
        }
        match w.write_output_report(src).await {
            Ok(()) => Ok(src.len()),
            Err(e) => {
                if matches!(e, async_hid::HidError::Disconnected) {
                    self.mark_disconnected();
                }
                Err(e.into())
            }
        }
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
            Err(async_hid::HidError::Disconnected) => {
                self.mark_disconnected();
                std::future::pending().await
            }
            Err(e) => Err(e.into()),
        }
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
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
mod tests;
