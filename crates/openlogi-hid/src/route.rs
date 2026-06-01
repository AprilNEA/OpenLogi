//! How to reach a controllable HID++ device, and the logic to (re-)open its
//! channel.
//!
//! Two addressing modes:
//!
//! - [`DeviceRoute::Bolt`] — a device paired to a Logi Bolt receiver, reached
//!   through the receiver channel at a pairing slot.
//! - [`DeviceRoute::Direct`] — a device attached straight to the host over a
//!   USB cable or Bluetooth, reached on its own channel at the HID++
//!   self-index [`DIRECT_DEVICE_INDEX`].
//!
//! Both the write path ([`crate::write`]) and the capture session
//! ([`crate::gesture`]) resolve a route to an open channel through
//! [`open_route_channel`], so the Bolt-vs-direct branch lives in exactly one
//! place.

use std::fmt;
use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    receiver::{self, Receiver},
};

use crate::transport::{enumerate_hidpp_devices, open_hidpp_channel};

/// HID++ device index that addresses a directly-attached device's own
/// features (USB-cable or Bluetooth, no receiver indirection).
pub const DIRECT_DEVICE_INDEX: u8 = 0xff;

/// How to reach a controllable HID++ device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceRoute {
    /// Paired to a Logi Bolt receiver. `receiver_uid` disambiguates multiple
    /// plugged-in receivers; `slot` is the device's pairing slot (1..=6).
    Bolt { receiver_uid: String, slot: u8 },
    /// Attached straight to the host over USB cable or Bluetooth, addressed at
    /// the HID++ self-index. Re-found by matching the HID node's vendor/product
    /// id — two identical mice on one host are indistinguishable here, so the
    /// first match wins (acceptable for v0).
    Direct { vendor_id: u16, product_id: u16 },
}

impl DeviceRoute {
    /// The HID++ device index features are addressed at for this route: the
    /// pairing slot for a Bolt device, the self-index for a direct one.
    #[must_use]
    pub fn device_index(&self) -> u8 {
        match self {
            Self::Bolt { slot, .. } => *slot,
            Self::Direct { .. } => DIRECT_DEVICE_INDEX,
        }
    }
}

impl fmt::Display for DeviceRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bolt { receiver_uid, slot } => {
                write!(f, "slot {slot} on receiver {receiver_uid}")
            }
            Self::Direct {
                vendor_id,
                product_id,
            } => write!(f, "direct {vendor_id:04x}:{product_id:04x}"),
        }
    }
}

/// Enumerate HID++ candidates and open the channel that reaches `route`.
///
/// For a Bolt route this is the receiver channel (the caller addresses the
/// device through its slot via [`DeviceRoute::device_index`]); for a direct
/// route it is the device's own channel. Returns `None` when nothing matching
/// is currently connected.
///
/// The returned [`async_hid::DeviceInfo`] reflects the matched HID node (e.g.
/// its `usage_page` distinguishes BLE-direct `0xff43` from USB/receiver
/// `0xff00`). Callers that only need the channel can discard it with `let Some((_, ch)) = …`.
pub(crate) async fn open_route_channel(
    route: &DeviceRoute,
) -> Result<Option<(async_hid::DeviceInfo, Arc<HidppChannel>)>, async_hid::HidError> {
    let candidates = enumerate_hidpp_devices().await?;
    for dev in candidates {
        // A direct route's vendor/product id is on the unopened `DeviceInfo`
        // (`async_hid::Device` derefs to it), so skip non-matching nodes before
        // paying the ~100ms channel-open cost — otherwise every direct write on
        // a host that also has a Bolt receiver opens the receiver's channel
        // first. The Bolt branch still needs an open channel for `detect`.
        if let DeviceRoute::Direct {
            vendor_id,
            product_id,
        } = route
            && (dev.vendor_id != *vendor_id || dev.product_id != *product_id)
        {
            continue;
        }
        let Some((info, channel)) = open_hidpp_channel(dev).await? else {
            continue;
        };
        match route {
            DeviceRoute::Bolt { receiver_uid, .. } => {
                let Some(Receiver::Bolt(bolt)) = receiver::detect(Arc::clone(&channel)) else {
                    continue;
                };
                if let Ok(uid) = bolt.get_unique_id().await
                    && uid.eq_ignore_ascii_case(receiver_uid)
                {
                    return Ok(Some((info, channel)));
                }
            }
            DeviceRoute::Direct { .. } => return Ok(Some((info, channel))),
        }
    }
    Ok(None)
}
