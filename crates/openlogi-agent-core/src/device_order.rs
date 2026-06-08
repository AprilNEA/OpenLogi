//! Canonical device ordering shared by the GUI carousel and the agent's
//! no-selection fallback.
//!
//! HID enumeration order shifts as devices wake, sleep, or are reselected, so
//! both processes order devices by a stable, route-derived identity instead.
//! Sharing the key here is what keeps them agreeing on "the first device": when
//! no `selected_device` is persisted, the GUI shows index 0 of its sorted list
//! and the agent targets index 0 of its own — they must be the same device.

use openlogi_hid::DeviceRoute;

/// A stable, route-derived identity used to order devices deterministically.
/// Distinct devices never share one (a Bolt receiver UID + slot, a direct
/// vendor/product + serial/unit, or a slot + serial/unit are each unique), so
/// it alone fixes the sort order regardless of secondary tiebreakers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeviceStableId {
    Bolt {
        receiver_uid: String,
        slot: u8,
    },
    Direct {
        vendor_id: u16,
        product_id: u16,
        identity: DeviceIdentity,
    },
    Unknown {
        slot: u8,
        identity: DeviceIdentity,
    },
}

/// A device's own identity, used to disambiguate two same-model direct devices.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeviceIdentity {
    Serial(String),
    Unit([u8; 4]),
}

impl DeviceIdentity {
    /// Prefer the serial number (case-folded) when present, else the unit id.
    #[must_use]
    pub fn from_parts(serial: Option<&str>, unit_id: [u8; 4]) -> Self {
        serial.map_or(Self::Unit(unit_id), |s| {
            Self::Serial(s.to_ascii_lowercase())
        })
    }
}

impl DeviceStableId {
    /// Build the stable id from a device's route plus its identity fields.
    /// `slot` is only consulted for a routeless device (the Bolt/Direct cases
    /// carry their own slot / addressing inside the route).
    #[must_use]
    pub fn from_parts(
        route: Option<&DeviceRoute>,
        slot: u8,
        serial: Option<&str>,
        unit_id: [u8; 4],
    ) -> Self {
        match route {
            Some(
                DeviceRoute::Bolt { receiver_uid, slot }
                | DeviceRoute::Unifying { receiver_uid, slot },
            ) => Self::Bolt {
                receiver_uid: receiver_uid.to_ascii_lowercase(),
                slot: *slot,
            },
            Some(DeviceRoute::Direct {
                vendor_id,
                product_id,
            }) => Self::Direct {
                vendor_id: *vendor_id,
                product_id: *product_id,
                identity: DeviceIdentity::from_parts(serial, unit_id),
            },
            None => Self::Unknown {
                slot,
                identity: DeviceIdentity::from_parts(serial, unit_id),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use openlogi_hid::DeviceRoute;

    use super::DeviceStableId;

    #[test]
    fn unifying_route_maps_to_bolt_stable_id() {
        let route = DeviceRoute::Unifying {
            receiver_uid: "DA2699E1".into(),
            slot: 2,
        };
        let id = DeviceStableId::from_parts(Some(&route), 2, None, [0; 4]);
        // Unifying and Bolt share the same stable-id variant so the GUI and
        // agent agree on carousel order regardless of receiver family.
        assert!(
            matches!(id, DeviceStableId::Bolt { ref receiver_uid, slot: 2 }
                if receiver_uid == "da2699e1"),
            "Unifying route should map to DeviceStableId::Bolt with case-folded uid"
        );
    }

    #[test]
    fn bolt_and_unifying_same_uid_slot_produce_identical_stable_id() {
        let bolt = DeviceRoute::Bolt {
            receiver_uid: "AABB".into(),
            slot: 1,
        };
        let unifying = DeviceRoute::Unifying {
            receiver_uid: "AABB".into(),
            slot: 1,
        };
        assert_eq!(
            DeviceStableId::from_parts(Some(&bolt), 1, None, [0; 4]),
            DeviceStableId::from_parts(Some(&unifying), 1, None, [0; 4]),
        );
    }
}
