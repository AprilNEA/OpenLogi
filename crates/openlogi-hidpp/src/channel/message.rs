//! The two report widths a HID++ channel carries, and the raw message
//! framing shared by both protocol versions.

/// The ID of the HID report that is used to transmit short HID++ messages.
pub const SHORT_REPORT_ID: u8 = 0x10;

/// The length of short HID++ message reports (including report ID).
pub const SHORT_REPORT_LENGTH: usize = 7;

/// The ID of the HID report that is used to transmit long HID++ messages.
pub const LONG_REPORT_ID: u8 = 0x11;

/// The length of long HID++ message reports (including report ID).
pub const LONG_REPORT_LENGTH: usize = 20;

/// Represents an unversioned HID++ message.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum HidppMessage {
    /// Represents a short HID++ message.
    ///
    /// Please check
    /// [`HidppChannel::supports_short`](super::HidppChannel::supports_short)
    /// before sending this kind of message.
    Short([u8; SHORT_REPORT_LENGTH - 1]),

    /// Represents a long HID++ message.
    ///
    /// Please check
    /// [`HidppChannel::supports_long`](super::HidppChannel::supports_long)
    /// before sending this kind of message.
    Long([u8; LONG_REPORT_LENGTH - 1]),
}

impl HidppMessage {
    /// Tries to read a HID++ message from raw data.
    #[must_use]
    pub fn read_raw(data: &[u8]) -> Option<Self> {
        let (&report_id, rest) = data.split_first()?;

        // The empty-remainder patterns enforce the exact report lengths.
        if report_id == SHORT_REPORT_ID
            && let Some((&payload, [])) = rest.split_first_chunk()
        {
            Some(HidppMessage::Short(payload))
        } else if report_id == LONG_REPORT_ID
            && let Some((&payload, [])) = rest.split_first_chunk()
        {
            Some(HidppMessage::Long(payload))
        } else {
            None
        }
    }

    /// Writes a HID++ message in its raw byte form into a buffer.
    ///
    /// Returns the amount of written bytes.
    pub fn write_raw(&self, buf: &mut [u8]) -> usize {
        match self {
            Self::Short(payload) => {
                buf[0] = SHORT_REPORT_ID;
                buf[1..SHORT_REPORT_LENGTH].copy_from_slice(payload);
                SHORT_REPORT_LENGTH
            }
            Self::Long(payload) => {
                buf[0] = LONG_REPORT_ID;
                buf[1..LONG_REPORT_LENGTH].copy_from_slice(payload);
                LONG_REPORT_LENGTH
            }
        }
    }

    /// The HID++ addressing header `(device_index, feature_index, function)` —
    /// the first three payload bytes, present on both report kinds. Used only
    /// for wire tracing (OpenLogi-specific; not in upstream hidpp).
    pub(super) fn header(&self) -> (u8, u8, u8) {
        let payload: &[u8] = match self {
            Self::Short(payload) => payload,
            Self::Long(payload) => payload,
        };
        (payload[0], payload[1], payload[2])
    }

    /// Re-frames a short message as a long one, leaving a long one untouched.
    ///
    /// The HID++ header bytes (device / feature / function|sw) sit at the same
    /// offsets in both widths, so the only change is the report id plus
    /// zero-padding the trailing payload. Used to send on a channel that
    /// exposes only the long HID++ report — see
    /// [`HidppChannel::normalize_outgoing`](super::HidppChannel). (OpenLogi
    /// local addition.)
    #[must_use]
    pub(super) fn widened(self) -> Self {
        match self {
            Self::Short(payload) => {
                let mut long = [0u8; LONG_REPORT_LENGTH - 1];
                long[..payload.len()].copy_from_slice(&payload);
                Self::Long(long)
            }
            long @ Self::Long(_) => long,
        }
    }
}
