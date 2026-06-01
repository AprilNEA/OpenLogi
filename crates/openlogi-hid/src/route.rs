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
    device::Device,
    feature::device_information::DeviceInformationFeature,
    receiver::{self, Receiver},
};
use openlogi_core::device::DeviceModelInfo;
use tracing::debug;

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
    /// the HID++ self-index. Re-found by matching both the HID node's
    /// vendor/product id and the stronger HID++ DeviceInformation identity.
    Direct {
        vendor_id: u16,
        product_id: u16,
        identity: DirectDeviceIdentity,
    },
}

/// Stable identity for a directly-attached HID++ device.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DirectDeviceIdentity {
    pub serial_number: Option<String>,
    pub unit_id: [u8; 4],
}

impl DirectDeviceIdentity {
    #[must_use]
    pub fn from_model(model: &DeviceModelInfo) -> Self {
        Self {
            serial_number: model.serial_number.clone(),
            unit_id: model.unit_id,
        }
    }

    fn matches(&self, serial_number: Option<&str>, unit_id: [u8; 4]) -> bool {
        if unit_id != self.unit_id {
            return false;
        }
        match (&self.serial_number, serial_number) {
            (Some(expected), Some(actual)) => actual.eq_ignore_ascii_case(expected),
            (Some(_), None) => false,
            (None, _) => true,
        }
    }
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
                identity,
            } => match &identity.serial_number {
                Some(serial) => {
                    write!(f, "direct {vendor_id:04x}:{product_id:04x} serial {serial}")
                }
                None => write!(
                    f,
                    "direct {vendor_id:04x}:{product_id:04x} unit {:02x?}",
                    identity.unit_id
                ),
            },
        }
    }
}

/// Enumerate HID++ candidates and open the channel that reaches `route`.
///
/// For a Bolt route this is the receiver channel (the caller addresses the
/// device through its slot via [`DeviceRoute::device_index`]); for a direct
/// route it is the device's own channel. Returns `None` when nothing matching
/// is currently connected.
pub(crate) async fn open_route_channel(
    route: &DeviceRoute,
) -> Result<Option<Arc<HidppChannel>>, async_hid::HidError> {
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
            ..
        } = route
            && (dev.vendor_id != *vendor_id || dev.product_id != *product_id)
        {
            continue;
        }
        let Some((_, channel)) = open_hidpp_channel(dev).await? else {
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
                    return Ok(Some(channel));
                }
            }
            DeviceRoute::Direct { identity, .. } => {
                if direct_identity_matches(&channel, identity).await {
                    return Ok(Some(channel));
                }
            }
        }
    }
    Ok(None)
}

async fn direct_identity_matches(
    channel: &Arc<HidppChannel>,
    expected: &DirectDeviceIdentity,
) -> bool {
    let device = match Device::new(Arc::clone(channel), DIRECT_DEVICE_INDEX).await {
        Ok(device) => device,
        Err(e) => {
            debug!(error = ?e, "direct route DeviceInformation probe failed");
            return false;
        }
    };
    let Some(feature) = device.get_feature::<DeviceInformationFeature>() else {
        return false;
    };
    let info = match feature.get_device_info().await {
        Ok(info) => info,
        Err(e) => {
            debug!(error = ?e, "direct route device info read failed");
            return false;
        }
    };
    let serial_number = if expected.serial_number.is_some() && info.capabilities.serial_number {
        feature
            .get_serial_number()
            .await
            .ok()
            .and_then(|serial| normalize_serial_number(&serial))
    } else {
        None
    };
    expected.matches(serial_number.as_deref(), info.unit_id)
}

fn normalize_serial_number(serial: &str) -> Option<String> {
    let serial = serial.trim_matches('\0').trim().to_string();
    (!serial.is_empty()).then_some(serial)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> DirectDeviceIdentity {
        DirectDeviceIdentity {
            serial_number: Some("ABC123".to_string()),
            unit_id: [1, 2, 3, 4],
        }
    }

    #[test]
    fn direct_identity_requires_matching_unit_id() {
        assert!(!identity().matches(Some("ABC123"), [4, 3, 2, 1]));
    }

    #[test]
    fn direct_identity_requires_serial_when_expected() {
        assert!(identity().matches(Some("abc123"), [1, 2, 3, 4]));
        assert!(!identity().matches(None, [1, 2, 3, 4]));
        assert!(!identity().matches(Some("OTHER"), [1, 2, 3, 4]));
    }

    #[test]
    fn direct_identity_without_serial_matches_unit_id_only() {
        let identity = DirectDeviceIdentity {
            serial_number: None,
            unit_id: [1, 2, 3, 4],
        };

        assert!(identity.matches(None, [1, 2, 3, 4]));
        assert!(identity.matches(Some("anything"), [1, 2, 3, 4]));
    }
}
