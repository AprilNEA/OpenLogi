#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "windows")]
use std::{error::Error, io};

#[cfg(target_os = "windows")]
use async_hid::{AsyncHidRead, AsyncHidWrite, DeviceInfo, DeviceReader, DeviceWriter};
#[cfg(target_os = "windows")]
use futures_lite::StreamExt as _;
use hidpp::channel::{LONG_REPORT_ID, SHORT_REPORT_ID};
#[cfg(target_os = "windows")]
use hidpp::{
    async_trait,
    channel::{LONG_REPORT_LENGTH, RawHidChannel, SHORT_REPORT_LENGTH},
};
#[cfg(target_os = "windows")]
use tokio::sync::Mutex;
#[cfg(target_os = "windows")]
use tracing::debug;

#[cfg(target_os = "windows")]
use openlogi_device::DeviceIoGate;

#[cfg(target_os = "windows")]
use super::windows_hid::NativeHidWriter;

#[cfg(target_os = "windows")]
use super::HID_BACKEND;

const VERY_LONG_REPORT_ID: u8 = 0x12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReportEndpoint {
    Short,
    Long,
}

fn endpoint_for_report_id(report_id: u8) -> Option<ReportEndpoint> {
    match report_id {
        SHORT_REPORT_ID => Some(ReportEndpoint::Short),
        LONG_REPORT_ID | VERY_LONG_REPORT_ID => Some(ReportEndpoint::Long),
        _ => None,
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
            // The native fallback works around async-hid write quirks on a
            // *live* device; reopening a device that is gone can only fail, and
            // its `NativeWriteError` would replace the typed `Disconnected`
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

#[cfg(target_os = "windows")]
pub(super) struct WindowsHidppChannel {
    info: DeviceInfo,
    short: Option<HidEndpoint>,
    long: HidEndpoint,
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
        let short_dev = find_windows_short_collection(&long_info).await?;
        let (long_reader, long_writer) = long_dev.open().await?;
        let long = HidEndpoint::new(long_reader, long_writer, &long_info);

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
            supports_long = true,
            "opened Windows HID++ composite channel"
        );

        Ok(Self {
            info: long_info,
            short,
            long,
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
}

/// Whether an already-boxed transport error means the handle is permanently
/// dead. [`HidEndpoint::write_report`] boxes the `async_hid` error before the
/// channel sees it, so that path can only recognise the variant by downcast.
#[cfg(target_os = "windows")]
fn is_permanent_disconnect(error: &(dyn Error + Send + Sync + 'static)) -> bool {
    error
        .downcast_ref::<async_hid::HidError>()
        .is_some_and(|e| matches!(e, async_hid::HidError::Disconnected))
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
        if !self.device_io.allows_io() {
            return Err(super::device_io_error());
        }
        let endpoint = match src.first().copied().and_then(endpoint_for_report_id) {
            Some(ReportEndpoint::Short) => self.short.as_ref(),
            Some(ReportEndpoint::Long) => Some(&self.long),
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

        if !self.device_io.allows_io() {
            return Err(super::device_io_error());
        }
        let result = endpoint.write_report(src).await;
        if let Err(e) = &result
            && is_permanent_disconnect(e.as_ref())
        {
            self.mark_disconnected();
        }
        result
    }

    async fn read_report(&self, buf: &mut [u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        if let Some(short) = &self.short {
            let mut short_buf = [0u8; SHORT_REPORT_LENGTH];
            let mut long_buf = [0u8; LONG_REPORT_LENGTH];
            let mut short_reader = short.reader.lock().await;
            let mut long_reader = self.long.reader.lock().await;
            // `select!` drops the losing read future, but no report is lost:
            // async-hid's win32 `IoBuffer` owns the in-flight OVERLAPPED read and
            // its buffer (not the future), so the pending operation survives the
            // drop, and the next `read_report` — re-locking this same endpoint —
            // resumes it and retrieves the report. This relies on reusing the
            // per-endpoint reader across calls; do not reopen readers per read.
            return tokio::select! {
                res = short_reader.read_input_report(&mut short_buf) => match res {
                    Ok(len) => copy_report(&short_buf, len, buf),
                    Err(async_hid::HidError::Disconnected) => self.park_disconnected().await,
                    Err(e) => Err(e.into()),
                },
                res = long_reader.read_input_report(&mut long_buf) => match res {
                    Ok(len) => copy_report(&long_buf, len, buf),
                    Err(async_hid::HidError::Disconnected) => self.park_disconnected().await,
                    Err(e) => Err(e.into()),
                },
            };
        }

        let mut reader = self.long.reader.lock().await;
        match reader.read_input_report(buf).await {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_ids_select_their_windows_endpoint() {
        assert_eq!(
            endpoint_for_report_id(SHORT_REPORT_ID),
            Some(ReportEndpoint::Short)
        );
        assert_eq!(
            endpoint_for_report_id(LONG_REPORT_ID),
            Some(ReportEndpoint::Long)
        );
        assert_eq!(
            endpoint_for_report_id(VERY_LONG_REPORT_ID),
            Some(ReportEndpoint::Long)
        );
        assert_eq!(endpoint_for_report_id(0x13), None);
    }

    /// `HidEndpoint::write_report` boxes the `async_hid` error before the
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
