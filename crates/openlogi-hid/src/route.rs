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
use openlogi_core::device::DeviceInventory;
use serde::{Deserialize, Serialize};

use crate::transport::{enumerate_hidpp_devices, open_hidpp_channel};

/// HID++ device index that addresses a directly-attached device's own
/// features (USB-cable or Bluetooth, no receiver indirection).
pub const DIRECT_DEVICE_INDEX: u8 = 0xff;

/// How to reach a controllable HID++ device.
///
/// Crosses the agent↔GUI IPC (every per-device RPC takes one), so variant and
/// field order are wire format — changes require a `PROTOCOL_VERSION` bump
/// (guarded by `openlogi-agent-core/tests/wire_format.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceRoute {
    /// Paired to a Logi Bolt receiver. `receiver_uid` disambiguates multiple
    /// plugged-in receivers; `slot` is the device's pairing slot (1..=6).
    Bolt { receiver_uid: String, slot: u8 },
    /// Paired to a Logi Unifying receiver. Same addressing structure as Bolt
    /// (receiver channel + pairing slot) but the receiver speaks HID++ 1.0.
    Unifying { receiver_uid: String, slot: u8 },
    /// Attached straight to the host over USB cable or Bluetooth, addressed at
    /// the HID++ self-index. Re-found by matching the HID node's vendor/product
    /// id — two identical mice on one host are indistinguishable here, so the
    /// first match wins (acceptable for v0).
    Direct { vendor_id: u16, product_id: u16 },
}

/// USB product IDs that identify Logi Bolt receivers.
pub const BOLT_PIDS: &[u16] = &[0xc548];

/// USB product IDs that identify Logi Unifying receivers. Used by callers that
/// need to construct the correct [`DeviceRoute`] variant from a raw inventory.
pub const UNIFYING_PIDS: &[u16] = &[0xc52b, 0xc532];

impl DeviceRoute {
    /// The HID++ device index features are addressed at for this route: the
    /// pairing slot for a Bolt device, the self-index for a direct one.
    #[must_use]
    pub fn device_index(&self) -> u8 {
        match self {
            Self::Bolt { slot, .. } | Self::Unifying { slot, .. } => *slot,
            Self::Direct { .. } => DIRECT_DEVICE_INDEX,
        }
    }

    /// Build the route that reaches a paired device from a receiver inventory.
    ///
    /// Picks [`DeviceRoute::Unifying`] or [`DeviceRoute::Bolt`] based on the
    /// receiver's product ID using the canonical `UNIFYING_PIDS` / `BOLT_PIDS`
    /// lists. Any receiver PID not in `UNIFYING_PIDS` — including future Bolt
    /// variants whose PID isn't yet in `BOLT_PIDS` — defaults to
    /// [`DeviceRoute::Bolt`] so writes keep working rather than silently
    /// dropping. [`DeviceRoute::Direct`] is used for directly-attached devices
    /// (slot == [`DIRECT_DEVICE_INDEX`] with no receiver UID). Returns `None`
    /// when the receiver UID is unknown (writes are skipped, not mis-routed).
    #[must_use]
    pub fn device_route_for(inv: &DeviceInventory, slot: u8) -> Option<Self> {
        match &inv.receiver.unique_id {
            Some(uid) if UNIFYING_PIDS.contains(&inv.receiver.product_id) => Some(Self::Unifying {
                receiver_uid: uid.clone(),
                slot,
            }),
            Some(uid) => {
                // Default to Bolt for any receiver whose PID is not in
                // UNIFYING_PIDS. This covers both known Bolt PIDs (BOLT_PIDS)
                // and any future Bolt-compatible receiver with a new PID —
                // returning None would silently drop writes for such receivers.
                if !BOLT_PIDS.contains(&inv.receiver.product_id) {
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
            DeviceRoute::Unifying { receiver_uid, .. } => {
                let Some(Receiver::Unifying(unifying)) = receiver::detect(Arc::clone(&channel))
                else {
                    continue;
                };
                if let Ok(uid) = unifying.get_unique_id().await
                    && uid.eq_ignore_ascii_case(receiver_uid)
                {
                    return Ok(Some(channel));
                }
            }
            DeviceRoute::Direct { .. } => return Ok(Some(channel)),
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use openlogi_core::device::{DeviceInventory, ReceiverInfo};

    use super::{DIRECT_DEVICE_INDEX, DeviceRoute, UNIFYING_PIDS};

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
    fn device_route_for_unifying_pids_create_unifying_route() {
        for &pid in UNIFYING_PIDS {
            let route = DeviceRoute::device_route_for(&inv(pid, Some("A1B2")), 2);
            assert!(
                matches!(route, Some(DeviceRoute::Unifying { ref receiver_uid, slot: 2 }) if receiver_uid == "A1B2"),
                "pid {pid:#06x} should produce Unifying route"
            );
        }
    }

    #[test]
    fn device_route_for_bolt_pid_creates_bolt_route() {
        // 0xC548 is Bolt; anything not in UNIFYING_PIDS defaults to Bolt so
        // future Bolt variants with unknown PIDs still work.
        let route = DeviceRoute::device_route_for(&inv(0xc548, Some("UID")), 1);
        assert!(matches!(
            route,
            Some(DeviceRoute::Bolt { ref receiver_uid, slot: 1 }) if receiver_uid == "UID"
        ));
    }

    #[test]
    fn device_route_for_direct_when_no_uid_and_direct_slot() {
        let route = DeviceRoute::device_route_for(&inv(0xb025, None), DIRECT_DEVICE_INDEX);
        assert!(matches!(
            route,
            Some(DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb025
            })
        ));
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
