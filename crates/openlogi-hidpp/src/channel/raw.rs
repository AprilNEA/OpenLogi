//! The transport a HID++ channel is built on, and the report-descriptor
//! probe that decides whether the transport speaks HID++ at all.

use std::error::Error;

use async_trait::async_trait;
use hidreport::{Field, Report, ReportDescriptor, Usage, UsageId, UsagePage};

use super::{ChannelError, LONG_REPORT_ID, SHORT_REPORT_ID};

/// hidapi defines this as the maximum EXPECTED size of report descriptors.
/// We will trust this for now, but a workaround may be required if devices do
/// in fact return longer descriptors.
const MAX_REPORT_DESCRIPTOR_LENGTH: usize = 4096;

/// The HID usage page both HID++ report kinds are declared under.
const HIDPP_USAGE_PAGE: u16 = 0xff00;

/// The HID usage ID identifying short HID++ message reports.
const SHORT_REPORT_USAGE: u16 = 0x0001;

/// The HID usage ID identifying long HID++ message reports.
const LONG_REPORT_USAGE: u16 = 0x0002;

/// Represents an arbitrary HID communication channel that is both readable and
/// writable. It has to support async I/O.
///
/// Any type this trait is implemented for can be used for HID(++)
/// communication. If a specific channel supports HID++ is determined at a later
/// stage and is not directly related to potential implementations of this
/// trait.
#[async_trait]
pub trait RawHidChannel: Sync + Send + 'static {
    /// Provides the vendor ID of the connected HID device.
    fn vendor_id(&self) -> u16;

    /// Provides the product ID of the connected HID device.
    fn product_id(&self) -> u16;

    /// Writes a raw report to the channel.
    ///
    /// Returns the exact amount of written bytes on success.
    async fn write_report(&self, src: &[u8]) -> Result<usize, Box<dyn Error + Sync + Send>>;

    /// Reads a raw report from the channel.
    ///
    /// If the buffer is not large enough to fit the whole report, its remainder
    /// should be discarded and must not be returned by any succeeding call to
    /// [`Self::read_report`].
    ///
    /// Returns the exact amount or read bytes on success. An `Err` is treated
    /// as transient: the [`HidppChannel`](super::HidppChannel) read loop logs
    /// it and retries, so an implementation must not surface a condition that
    /// will never clear (it would busy-spin the loop). For a *permanent*
    /// failure — the device is gone and no report will ever arrive — the future
    /// may instead park forever. That is sound because the read loop always
    /// races this future against the channel's close signal in a `select!`; any
    /// other caller must do the same and must not await `read_report` bare.
    async fn read_report(&self, buf: &mut [u8]) -> Result<usize, Box<dyn Error + Sync + Send>>;

    /// Whether the underlying device connection is still usable.
    ///
    /// Implementations that can detect a permanent disconnect should override
    /// this. The default preserves the behavior of transports that cannot
    /// report connection state.
    fn is_connected(&self) -> bool {
        true
    }

    /// If the implementation already knows whether the underlying HID channel
    /// supports HID++ messages, it should return `Some((supports_short,
    /// supports_long))` from this method.
    ///
    /// In this case, the report descriptor will not be read and parsed.
    fn supports_short_long_hidpp(&self) -> Option<(bool, bool)>;

    /// Retrieves the raw HID report descriptor from the channel.
    ///
    /// This is used to determine whether the channel supports HID++.
    ///
    /// Returns the exact size of the report descriptor on success.
    async fn get_report_descriptor(
        &self,
        buf: &mut [u8],
    ) -> Result<usize, Box<dyn Error + Sync + Send>>;
}

/// Checks whether a raw channel supports short or long HID++ messages.
pub(super) async fn supports_short_long_hidpp(
    chan: &impl RawHidChannel,
) -> Result<(bool, bool), ChannelError> {
    if let Some((supports_short, supports_long)) = chan.supports_short_long_hidpp() {
        return Ok((supports_short, supports_long));
    }

    let mut raw_descriptor = vec![0u8; MAX_REPORT_DESCRIPTOR_LENGTH];
    let descriptor_size = chan.get_report_descriptor(&mut raw_descriptor).await?;

    let descriptor = ReportDescriptor::try_from(&raw_descriptor[..descriptor_size])
        .map_err(ChannelError::ReportDescriptor)?;

    Ok((
        declares_report(&descriptor, SHORT_REPORT_ID, SHORT_REPORT_USAGE),
        declares_report(&descriptor, LONG_REPORT_ID, LONG_REPORT_USAGE),
    ))
}

/// Whether `descriptor` declares an input report with `report_id` whose first
/// field is an array covering the HID++ `usage`.
///
/// A device that reports the ID without the matching usage is not speaking
/// HID++ on it, so both halves have to hold.
fn declares_report(descriptor: &ReportDescriptor, report_id: u8, usage: u16) -> bool {
    descriptor
        .find_input_report(&[report_id])
        .and_then(|report| report.fields().first())
        .and_then(|field| match field {
            Field::Array(arr) => Some(arr.usage_range()),
            _ => None,
        })
        .is_some_and(|range| {
            range
                .lookup_usage(&Usage::from_page_and_id(
                    UsagePage::from(HIDPP_USAGE_PAGE),
                    UsageId::from(usage),
                ))
                .is_some()
        })
}
