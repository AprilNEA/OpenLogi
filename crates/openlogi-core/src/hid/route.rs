//! How to reach a controllable HID++ device — addressing data only, no I/O.
//!
//! Two addressing modes:
//!
//! - [`DeviceRoute::Bolt`] — a device paired to a Logi Bolt receiver, reached
//!   through the receiver channel at a pairing slot.
//! - [`DeviceRoute::Direct`] — a device attached straight to the host over a
//!   USB cable or Bluetooth, reached on its own channel at the HID++
//!   self-index [`DIRECT_DEVICE_INDEX`].
//!
//! Opening the channel a route names is `openlogi_hid::channel::route::open_route_channel`
//! — the one place both the write path and the capture session resolve a
//! route to an open channel, so the Bolt-vs-direct branch lives in exactly
//! one place.

use std::fmt;

pub use openlogi_device_registry::LOGITECH_VENDOR_ID;
pub use openlogi_device_registry::receiver::{
    RECEIVERS, ReceiverBrand, ReceiverDescriptor, ReceiverProtocol, find_receiver,
};
use serde::{Deserialize, Serialize};

use crate::device::DeviceInventory;

/// HID++ device index that addresses a directly-attached device's own
/// features (USB-cable or Bluetooth, no receiver indirection).
pub const DIRECT_DEVICE_INDEX: u8 = 0xff;

/// How to reach a controllable HID++ device.
///
/// Crosses the agent↔GUI IPC (every per-device RPC takes one), so variant and
/// field order are wire format — changes require a `PROTOCOL_VERSION` bump
/// (guarded by `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceRoute {
    /// Paired to a Logi Bolt receiver. `receiver_uid` disambiguates multiple
    /// plugged-in receivers; `slot` is the device's pairing slot (1..=6).
    Bolt {
        /// Receiver unique ID used to select the physical Bolt receiver.
        receiver_uid: String,
        /// Pairing slot of the target device on that receiver.
        slot: u8,
    },
    /// Paired to a Logi Unifying receiver. Same addressing structure as Bolt
    /// (receiver channel + pairing slot) but the receiver speaks HID++ 1.0.
    Unifying {
        /// Receiver unique ID used to select the physical Unifying receiver.
        receiver_uid: String,
        /// Pairing slot of the target device on that receiver.
        slot: u8,
    },
    /// Attached straight to the host over USB cable or Bluetooth, addressed at
    /// the HID++ self-index. Re-found by matching the HID node's vendor/product
    /// id — two identical mice on one host are indistinguishable here, so the
    /// first match wins (acceptable for v0).
    Direct {
        /// USB/HID vendor ID of the direct device.
        vendor_id: u16,
        /// USB/HID product ID of the direct device.
        product_id: u16,
    },
    /// Standalone raw-HID device, such as a Litra light. The identity is an
    /// opaque transport-generated value used to disambiguate duplicate HID
    /// nodes; this route must never be passed to HID++ channel code.
    RawHid {
        /// HID vendor ID.
        vendor_id: u16,
        /// HID product ID.
        product_id: u16,
        /// HID usage page.
        usage_page: u16,
        /// HID usage ID.
        usage_id: u16,
        /// Stable/opaque device identity selected during enumeration.
        identity: String,
    },
}

/// Whether `product_id` is a receiver that speaks the Unifying HID++ 1.0
/// register protocol — a Unifying receiver proper, or a protocol-compatible
/// Nano or Lightspeed receiver. Such receivers are addressed with
/// [`DeviceRoute::Unifying`].
#[must_use]
pub fn speaks_unifying_protocol(product_id: u16) -> bool {
    find_receiver(LOGITECH_VENDOR_ID, product_id)
        .is_some_and(|receiver| receiver.protocol == ReceiverProtocol::Unifying)
}

/// Whether `product_id` is a known Logitech receiver dongle of any family
/// (Bolt, Unifying, or Lightspeed).
#[must_use]
pub fn is_receiver_pid(product_id: u16) -> bool {
    find_receiver(LOGITECH_VENDOR_ID, product_id).is_some()
}

/// Human-readable name for a receiver identified by `product_id`, used to label
/// it in the inventory. Lightspeed receivers share the Unifying protocol path
/// but are surfaced under their own name.
#[must_use]
pub fn receiver_display_name(product_id: u16) -> &'static str {
    if find_receiver(LOGITECH_VENDOR_ID, product_id)
        .is_some_and(|receiver| receiver.brand == ReceiverBrand::Lightspeed)
    {
        "Lightspeed Receiver"
    } else {
        "Unifying Receiver"
    }
}

impl DeviceRoute {
    /// Whether two receiver routes use the same physical HID transport.
    /// Direct routes cannot prove identity because they carry only VID/PID.
    #[must_use]
    pub fn shares_transport(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Bolt {
                    receiver_uid: left, ..
                },
                Self::Bolt {
                    receiver_uid: right,
                    ..
                },
            )
            | (
                Self::Unifying {
                    receiver_uid: left, ..
                },
                Self::Unifying {
                    receiver_uid: right,
                    ..
                },
            ) => left.eq_ignore_ascii_case(right),
            _ => false,
        }
    }

    /// The HID++ device index features are addressed at for this route: the
    /// pairing slot for a Bolt device, the self-index for a direct one.
    #[must_use]
    pub fn device_index(&self) -> u8 {
        match self {
            Self::Bolt { slot, .. } | Self::Unifying { slot, .. } => *slot,
            Self::Direct { .. } | Self::RawHid { .. } => DIRECT_DEVICE_INDEX,
        }
    }

    /// Build the route that reaches a paired device from a receiver inventory.
    ///
    /// Picks [`DeviceRoute::Unifying`] or [`DeviceRoute::Bolt`] based on the
    /// receiver's identity in [`RECEIVERS`] (Unifying proper plus
    /// protocol-compatible Nano and Lightspeed receivers). Any receiver that
    /// does not speak the Unifying protocol — including future Bolt variants
    /// whose PID is not yet registered — defaults to [`DeviceRoute::Bolt`] so
    /// writes keep working rather than silently dropping.
    /// [`DeviceRoute::Direct`] is used for directly-attached devices
    /// (slot == [`DIRECT_DEVICE_INDEX`] with no receiver UID). Returns `None`
    /// when the receiver UID is unknown (writes are skipped, not mis-routed).
    #[must_use]
    pub fn device_route_for(inv: &DeviceInventory, slot: u8) -> Option<Self> {
        match &inv.receiver.unique_id {
            Some(uid) if speaks_unifying_protocol(inv.receiver.product_id) => {
                Some(Self::Unifying {
                    receiver_uid: uid.clone(),
                    slot,
                })
            }
            Some(uid) => {
                // Default to Bolt for any receiver that does not speak the
                // Unifying protocol. This covers both known Bolt receivers and
                // any future Bolt-compatible receiver with a new PID — returning
                // None would silently drop writes for such receivers.
                if find_receiver(LOGITECH_VENDOR_ID, inv.receiver.product_id)
                    .is_none_or(|receiver| receiver.protocol != ReceiverProtocol::Bolt)
                {
                    tracing::debug!(
                        pid = format_args!("{:04x}", inv.receiver.product_id),
                        "unknown receiver PID — routing as Bolt"
                    );
                }
                Some(Self::Bolt {
                    receiver_uid: uid.clone(),
                    slot,
                })
            }
            None if slot == DIRECT_DEVICE_INDEX => Some(Self::Direct {
                vendor_id: inv.receiver.vendor_id,
                product_id: inv.receiver.product_id,
            }),
            None => None,
        }
    }
}

impl fmt::Display for DeviceRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bolt { receiver_uid, slot } | Self::Unifying { receiver_uid, slot } => {
                write!(f, "slot {slot} on receiver {receiver_uid}")
            }
            Self::Direct {
                vendor_id,
                product_id,
            } => write!(f, "direct {vendor_id:04x}:{product_id:04x}"),
            Self::RawHid {
                vendor_id,
                product_id,
                usage_page,
                usage_id,
                identity,
            } => write!(
                f,
                "raw {vendor_id:04x}:{product_id:04x} usage {usage_page:04x}:{usage_id:04x} ({identity})"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use crate::device::{DeviceInventory, ReceiverInfo};

    use super::{
        DIRECT_DEVICE_INDEX, DeviceRoute, RECEIVERS, ReceiverProtocol, receiver_display_name,
    };

    fn inv(product_id: u16, unique_id: Option<&str>) -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "test".into(),
                vendor_id: 0x046d,
                product_id,
                unique_id: unique_id.map(str::to_string),
            },
            paired: vec![],
        }
    }

    #[test]
    fn device_route_for_known_receiver_follows_its_protocol() {
        for receiver in RECEIVERS {
            let route = DeviceRoute::device_route_for(&inv(receiver.product_id, Some("A1B2")), 2);
            match receiver.protocol {
                ReceiverProtocol::Bolt => assert_matches!(
                    route,
                    Some(DeviceRoute::Bolt { ref receiver_uid, slot: 2 }) if receiver_uid == "A1B2"
                ),
                ReceiverProtocol::Unifying => assert_matches!(
                    route,
                    Some(DeviceRoute::Unifying { ref receiver_uid, slot: 2 }) if receiver_uid == "A1B2"
                ),
            }
        }
    }

    #[test]
    fn lightspeed_receiver_has_its_own_display_name() {
        // 0xc539 is the receiver bundled with the G502 LIGHTSPEED and the
        // G Pro Wireless. It routes through the Unifying code path, but it is
        // Lightspeed hardware and says so in its own USB product string, so it
        // must not be surfaced as a Unifying receiver.
        assert_eq!(receiver_display_name(0xc539), "Lightspeed Receiver");
        assert_eq!(receiver_display_name(0xc53f), "Lightspeed Receiver");
        assert_eq!(receiver_display_name(0xc547), "Lightspeed Receiver");
        assert_eq!(receiver_display_name(0xc54d), "Lightspeed Receiver");
        assert_eq!(receiver_display_name(0xc52b), "Unifying Receiver");
        assert_eq!(receiver_display_name(0xc532), "Unifying Receiver");
    }

    #[test]
    fn device_route_for_unknown_receiver_defaults_to_bolt() {
        let route = DeviceRoute::device_route_for(&inv(0xc5ff, Some("UID")), 1);
        assert_matches!(
            route,
            Some(DeviceRoute::Bolt { ref receiver_uid, slot: 1 }) if receiver_uid == "UID"
        );
    }

    #[test]
    fn device_route_for_direct_when_no_uid_and_direct_slot() {
        let route = DeviceRoute::device_route_for(&inv(0xb025, None), DIRECT_DEVICE_INDEX);
        assert_matches!(
            route,
            Some(DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb025
            })
        );
    }

    #[test]
    fn device_route_for_none_when_no_uid_and_non_direct_slot() {
        let route = DeviceRoute::device_route_for(&inv(0xc52b, None), 1);
        assert!(route.is_none());
    }

    #[test]
    fn unifying_device_index_is_the_slot() {
        let route = DeviceRoute::Unifying {
            receiver_uid: "X".into(),
            slot: 4,
        };
        assert_eq!(route.device_index(), 4);
    }

    #[test]
    fn unifying_display_matches_bolt_format() {
        let r = DeviceRoute::Unifying {
            receiver_uid: "AABBCC".into(),
            slot: 3,
        };
        assert_eq!(r.to_string(), "slot 3 on receiver AABBCC");
    }
}
