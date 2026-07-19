#[cfg(target_os = "windows")]
use std::{error::Error, io};

#[cfg(target_os = "windows")]
use async_hid::{AsyncHidRead, AsyncHidWrite, DeviceInfo, DeviceReader, DeviceWriter};
#[cfg(target_os = "windows")]
use futures_lite::StreamExt as _;
#[cfg(target_os = "windows")]
use hidpp::{
    async_trait,
    channel::{
        LONG_REPORT_ID, LONG_REPORT_LENGTH, RawHidChannel, SHORT_REPORT_ID, SHORT_REPORT_LENGTH,
    },
};
#[cfg(target_os = "windows")]
use tokio::sync::Mutex;
#[cfg(target_os = "windows")]
use tracing::debug;

#[cfg(target_os = "windows")]
use crate::windows_hid::NativeHidWriter;

#[cfg(target_os = "windows")]
use super::HID_BACKEND;

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
pub(super) struct WindowsHidppChannel {
    info: DeviceInfo,
    short: Option<HidEndpoint>,
    long: Option<HidEndpoint>,
}

#[cfg(target_os = "windows")]
impl WindowsHidppChannel {
    pub(super) async fn open(
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
