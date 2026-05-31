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
/// HID++ long-report usage page / usage. Filtering on this pair gives us one
/// HID node per physical HID++ device on every supported OS.
const HIDPP_USAGE_PAGE: u16 = 0xff00;
const HIDPP_LONG_USAGE_ID: u16 = 0x0002;
pub(crate) const HID_GENERIC_DESKTOP: u16 = 0x0001;
pub(crate) const HID_MOUSE_USAGE_ID: u16 = 0x0002;

pub(crate) async fn enumerate_hidpp_devices() -> Result<Vec<async_hid::Device>, async_hid::HidError>
{
    let backend = HidBackend::default();
    Ok(backend
        .enumerate()
        .await?
        .filter(|d| d.vendor_id == LOGITECH_VID && is_hidpp_candidate(d))
        .collect()
        .await)
}

fn is_hidpp_candidate(d: &DeviceInfo) -> bool {
    (d.usage_page == HIDPP_USAGE_PAGE && d.usage_id == HIDPP_LONG_USAGE_ID) || is_logitech_mouse(d)
}

pub(crate) fn is_logitech_mouse(d: &DeviceInfo) -> bool {
    d.vendor_id == LOGITECH_VID
        && d.usage_page == HID_GENERIC_DESKTOP
        && d.usage_id == HID_MOUSE_USAGE_ID
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
