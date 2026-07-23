//! Canonical device ordering shared by the GUI carousel and the agent's
//! no-selection fallback.
//!
//! HID enumeration order shifts as devices wake, sleep, or are reselected, so
//! both processes order devices by a stable, route-derived identity instead.
//! Sharing the key here is what keeps them agreeing on "the first device": when
//! no `selected_device` is persisted, the GUI shows index 0 of its sorted list
//! and the agent targets index 0 of its own — they must be the same device.

use openlogi_hid::DeviceRoute;

/// A configuration key backed by enough information to identify one physical
/// device across inventory snapshots and process restarts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhysicalDeviceKey(String);

/// A route-derived identity used to order devices deterministically.
///
/// Receiver UID + slot and direct serial/non-zero unit identities are stable
/// and unique. A direct all-zero unit identity is deliberately retained here
/// only so a transient inventory record can still be ordered; it cannot become
/// a [`PhysicalDeviceKey`].
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
        serial
            .filter(|serial| !serial.is_empty())
            .map_or(Self::Unit(unit_id), |serial| {
                Self::Serial(serial.to_ascii_lowercase())
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

    /// Route-derived key used while a device is present in a runtime snapshot.
    ///
    /// Unlike [`Self::physical_key`], this also represents a direct or
    /// routeless device whose only reported identity is an all-zero unit id.
    /// Such a key is suitable for short-lived UI bookkeeping only and must not
    /// be written to configuration.
    ///
    /// This intentionally keys receiver-connected devices by receiver UID +
    /// pairing slot rather than model id, so two identical mice paired to the
    /// same receiver can carry different settings.
    #[must_use]
    pub fn runtime_key(&self) -> String {
        match self {
            Self::Bolt { receiver_uid, slot } => format!("receiver:{receiver_uid}:slot:{slot}"),
            Self::Direct {
                vendor_id,
                product_id,
                identity,
            } => format!("direct:{vendor_id:04x}:{product_id:04x}:{}", identity.key()),
            Self::Unknown { slot, identity } => format!("unknown:slot:{slot}:{}", identity.key()),
        }
    }

    /// Stable key for persisted per-physical-device configuration.
    ///
    /// Receiver-connected devices are identified by receiver UID + pairing
    /// slot. Direct and routeless devices require either a non-empty serial
    /// number or a non-zero unit id; an all-zero unit id is a transient probe
    /// result, not a physical identity.
    #[must_use]
    pub fn physical_key(&self) -> Option<PhysicalDeviceKey> {
        match self {
            Self::Bolt { .. } => Some(PhysicalDeviceKey(self.runtime_key())),
            Self::Direct { identity, .. } | Self::Unknown { identity, .. } => identity
                .is_physical()
                .then(|| PhysicalDeviceKey(self.runtime_key())),
        }
    }
}

impl DeviceIdentity {
    fn is_physical(&self) -> bool {
        match self {
            Self::Serial(serial) => !serial.is_empty(),
            Self::Unit(unit) => *unit != [0; 4],
        }
    }

    fn key(&self) -> String {
        match self {
            Self::Serial(serial) => format!("serial:{serial}"),
            Self::Unit(unit) => format!("unit:{}", hex_unit(*unit)),
        }
    }
}

impl PhysicalDeviceKey {
    /// Parse a key emitted by [`DeviceStableId::physical_key`].
    ///
    /// Legacy model-scoped configuration keys intentionally return `None`;
    /// callers can use that distinction when applying compatibility behavior
    /// without treating a model identifier as a physical identity.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        if receiver_key_is_valid(value)
            || direct_identity_fragment(value).is_some_and(identity_fragment_is_physical)
            || unknown_identity_fragment(value).is_some_and(identity_fragment_is_physical)
        {
            Some(Self(value.to_string()))
        } else {
            None
        }
    }

    /// Whether `value` is a structurally valid runtime key whose only device
    /// identity is the all-zero unit id.
    #[must_use]
    pub fn is_transient(value: &str) -> bool {
        direct_identity_fragment(value)
            .or_else(|| unknown_identity_fragment(value))
            .is_some_and(|identity| identity == "unit:00000000")
    }

    /// Borrow the serialized configuration key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and return its serialized configuration key.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

fn receiver_key_is_valid(value: &str) -> bool {
    value
        .strip_prefix("receiver:")
        .and_then(|rest| rest.rsplit_once(":slot:"))
        .is_some_and(|(receiver_uid, slot)| !receiver_uid.is_empty() && slot.parse::<u8>().is_ok())
}

fn direct_identity_fragment(value: &str) -> Option<&str> {
    let mut parts = value.strip_prefix("direct:")?.splitn(3, ':');
    let vendor_id = parts.next()?;
    let product_id = parts.next()?;
    let identity = parts.next()?;
    (is_hex_word(vendor_id) && is_hex_word(product_id)).then_some(identity)
}

fn unknown_identity_fragment(value: &str) -> Option<&str> {
    let (slot, identity) = value.strip_prefix("unknown:slot:")?.split_once(':')?;
    slot.parse::<u8>().ok().map(|_| identity)
}

fn identity_fragment_is_physical(value: &str) -> bool {
    value
        .strip_prefix("serial:")
        .is_some_and(|serial| !serial.is_empty())
        || value.strip_prefix("unit:").is_some_and(|unit| {
            unit.len() == 8
                && unit.bytes().all(|byte| byte.is_ascii_hexdigit())
                && unit != "00000000"
        })
}

fn is_hex_word(value: &str) -> bool {
    value.len() == 4 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hex_unit(unit: [u8; 4]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        unit[0], unit[1], unit[2], unit[3]
    )
}

#[cfg(test)]
mod tests {
    use openlogi_hid::DeviceRoute;

    use super::{DeviceStableId, PhysicalDeviceKey};

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

    #[test]
    fn config_key_is_physical_not_model_scoped() {
        let route = DeviceRoute::Bolt {
            receiver_uid: "AABB".into(),
            slot: 2,
        };

        assert_eq!(
            DeviceStableId::from_parts(Some(&route), 2, Some("SERIAL"), [1, 2, 3, 4])
                .physical_key()
                .map(PhysicalDeviceKey::into_string),
            Some("receiver:aabb:slot:2".to_string())
        );
    }

    #[test]
    fn zero_unit_direct_identity_is_transient() {
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb023,
        };
        let id = DeviceStableId::from_parts(Some(&route), 0xff, None, [0; 4]);

        assert!(id.physical_key().is_none());
        assert_eq!(id.runtime_key(), "direct:046d:b023:unit:00000000");
        assert!(PhysicalDeviceKey::is_transient(&id.runtime_key()));
        assert!(PhysicalDeviceKey::parse(&id.runtime_key()).is_none());
    }

    #[test]
    fn serial_identity_is_physical_when_unit_is_zero() {
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb023,
        };
        let key = DeviceStableId::from_parts(Some(&route), 0xff, Some("ABCDEF"), [0; 4])
            .physical_key()
            .map(PhysicalDeviceKey::into_string);

        assert_eq!(key, Some("direct:046d:b023:serial:abcdef".to_string()));
    }

    #[test]
    fn nonzero_unit_identity_is_physical_without_serial() {
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb023,
        };
        let key = DeviceStableId::from_parts(Some(&route), 0xff, None, [0xa3, 0x93, 0xca, 0xe0])
            .physical_key()
            .map(PhysicalDeviceKey::into_string);

        assert_eq!(key, Some("direct:046d:b023:unit:a393cae0".to_string()));
    }

    #[test]
    fn parser_distinguishes_physical_keys_from_legacy_model_keys() {
        assert!(PhysicalDeviceKey::parse("receiver:d0289db2:slot:1").is_some());
        assert!(PhysicalDeviceKey::parse("direct:046d:b023:unit:a393cae0").is_some());
        assert!(PhysicalDeviceKey::parse("2b034").is_none());
    }
}
