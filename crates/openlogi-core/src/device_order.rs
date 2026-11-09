//! Canonical device ordering shared by the GUI carousel and the agent's
//! no-selection fallback.
//!
//! HID enumeration order shifts as devices wake, sleep, or are reselected, so
//! both processes order devices by a stable, route-derived identity instead.
//! Sharing the key keeps device order deterministic. The agent additionally
//! excludes standalone raw-HID devices when choosing its input-capture target;
//! they remain ordered here for inventory, display, and settings re-apply.

use crate::hid::DeviceRoute;

/// A configuration key backed by enough information to identify one physical
/// device across inventory snapshots and process restarts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhysicalDeviceKey(String);

/// The identity fragment of a runtime key whose device identity was never
/// read: the all-zero unit id HID++ reports in that case, kept verbatim so
/// keys written before model identities existed stay recognizable.
const UNREAD_UNIT_FRAGMENT: &str = "unit:00000000";

/// A route-derived identity used to order devices deterministically.
///
/// Receiver UID + slot, serial and unit identities are stable and unique; a
/// direct device reporting neither is as stable as its model, which its route
/// already names. OS-node identities on raw HID routes are deliberately retained here
/// only so a transient inventory record can still be ordered; they cannot
/// become a [`PhysicalDeviceKey`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeviceStableId {
    /// Paired to a receiver — Bolt and Unifying share this variant so order
    /// agrees regardless of receiver family.
    Bolt {
        /// Case-folded receiver unique id.
        receiver_uid: String,
        /// Pairing slot on the receiver.
        slot: u8,
    },
    /// Attached straight to the host (USB or Bluetooth), disambiguated by its
    /// own [`DeviceIdentity`].
    Direct {
        /// USB vendor id.
        vendor_id: u16,
        /// USB product id.
        product_id: u16,
        /// What the device said about itself, or `None` while the probe has
        /// not read DeviceInformation yet (HID++ 1.0 hardware, or a feature
        /// walk that timed out). Only an answered probe can key settings.
        identity: Option<DeviceIdentity>,
    },
    /// A standalone raw-HID device, addressed by its USB/usage identifiers
    /// plus an OS-node- or serial-derived identity string.
    RawHid {
        /// USB vendor id.
        vendor_id: u16,
        /// USB product id.
        product_id: u16,
        /// HID usage page.
        usage_page: u16,
        /// HID usage id.
        usage_id: u16,
        /// Case-folded OS-node- or serial-derived identity.
        identity: String,
    },
    /// No route was reported; ordered by pairing slot plus its own
    /// [`DeviceIdentity`].
    Unknown {
        /// Pairing slot, when known.
        slot: u8,
        /// What the device said about itself, `None` while unread — see
        /// [`Self::Direct`].
        identity: Option<DeviceIdentity>,
    },
}

/// The most specific identity a device reports about itself, from a serial
/// number down to nothing beyond its model.
///
/// Ordered from most to least specific; [`Self::from_parts`] always picks the
/// most specific one the probe returned.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeviceIdentity {
    /// Case-folded serial number.
    Serial(String),
    /// HID++ unit id, used when no serial is reported.
    Unit(UnitId),
    /// The device reports neither: it is one instance of its model, and only
    /// the route it was reached by tells it apart from another one. Firmware
    /// answering DeviceInformation v0 (the G402, for instance) lands here.
    Model,
}

/// A HID++ unit id that the device actually filled in.
///
/// The all-zero unit id is the wire sentinel for "not reported", so it has no
/// value here; [`DeviceIdentity::from_parts`] turns it into
/// [`DeviceIdentity::Model`] instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnitId([u8; 4]);

impl UnitId {
    /// Wrap a reported unit id, or `None` for the all-zero sentinel.
    #[must_use]
    pub fn new(unit_id: [u8; 4]) -> Option<Self> {
        (unit_id != [0; 4]).then_some(Self(unit_id))
    }

    fn hex(self) -> String {
        let [a, b, c, d] = self.0;
        format!("{a:02x}{b:02x}{c:02x}{d:02x}")
    }
}

impl DeviceIdentity {
    /// Prefer the serial number (case-folded) when present, then the unit id,
    /// and fall back to the model when the device reports neither.
    #[must_use]
    pub fn from_parts(serial: Option<&str>, unit_id: [u8; 4]) -> Self {
        serial
            .filter(|serial| !serial.is_empty())
            .map(|serial| Self::Serial(serial.to_ascii_lowercase()))
            .or_else(|| UnitId::new(unit_id).map(Self::Unit))
            .unwrap_or(Self::Model)
    }
}

impl DeviceStableId {
    /// Build the stable id from a device's route plus the identity its probe
    /// read, `None` while DeviceInformation has not been answered. `slot` is
    /// only consulted for a routeless device (the Bolt/Direct cases carry
    /// their own slot / addressing inside the route).
    #[must_use]
    pub fn from_parts(
        route: Option<&DeviceRoute>,
        slot: u8,
        identity: Option<DeviceIdentity>,
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
                identity,
            },
            Some(DeviceRoute::RawHid {
                vendor_id,
                product_id,
                usage_page,
                usage_id,
                identity,
            }) => Self::RawHid {
                vendor_id: *vendor_id,
                product_id: *product_id,
                usage_page: *usage_page,
                usage_id: *usage_id,
                identity: identity.to_ascii_lowercase(),
            },
            None => Self::Unknown { slot, identity },
        }
    }

    /// Route-derived key used while a device is present in a runtime snapshot.
    ///
    /// Unlike [`Self::physical_key`], this also represents a device whose
    /// identity was never read, a routeless device known only by its model, or
    /// a raw-HID device known only by its OS node. Such a key is suitable for
    /// short-lived UI bookkeeping only and must not be written to
    /// configuration.
    ///
    /// This intentionally keys receiver-connected devices by receiver UID +
    /// pairing slot rather than model id, so two identical mice paired to the
    /// same receiver can carry different settings. A direct device is keyed by
    /// its route plus whatever identity it reports; for a model identity the
    /// key *is* the route. An identity the probe has not read yet is written
    /// as the all-zero unit id it stands for, so
    /// [`PhysicalDeviceKey::is_transient`] reads live and historical keys
    /// alike.
    #[must_use]
    pub fn runtime_key(&self) -> String {
        match self {
            Self::Bolt { receiver_uid, slot } => format!("receiver:{receiver_uid}:slot:{slot}"),
            Self::Direct {
                vendor_id,
                product_id,
                identity,
            } => with_identity(
                format!("direct:{vendor_id:04x}:{product_id:04x}"),
                identity.as_ref(),
            ),
            Self::RawHid {
                vendor_id,
                product_id,
                usage_page,
                usage_id,
                identity,
            } => format!(
                "raw:{vendor_id:04x}:{product_id:04x}:{usage_page:04x}:{usage_id:04x}:{identity}"
            ),
            Self::Unknown { slot, identity } => {
                with_identity(format!("unknown:slot:{slot}"), identity.as_ref())
            }
        }
    }

    /// Key naming the *path* a device was reached by, with the device's own
    /// identity removed.
    ///
    /// Used as the key of a config entry's `links` table, so one device
    /// reached two ways indexes one entry. Only [`Self::Direct`] carries a
    /// device identity that moves to the entry key; the others keep
    /// [`Self::runtime_key`] unchanged — a receiver key already names the
    /// receiver rather than the paired device, and raw/routeless identities
    /// are OS node ids that cannot become an entry key.
    #[must_use]
    pub fn route_key(&self) -> String {
        match self {
            Self::Direct {
                vendor_id,
                product_id,
                ..
            } => format!("direct:{vendor_id:04x}:{product_id:04x}"),
            Self::Bolt { .. } | Self::RawHid { .. } | Self::Unknown { .. } => self.runtime_key(),
        }
    }

    /// The direct route a saved [`Self::route_key`] names — a config entry's
    /// `links` key, `direct:<vid>:<pid>` — or `None` for any other key,
    /// including a key that carries more after the product ID.
    ///
    /// The inverse of [`Self::route_key`]'s direct arm, so an offline entry
    /// still says how it was reached: a route-keyed fact such as
    /// [`DeviceRoute::keypad`] holds while the device is unplugged.
    #[must_use]
    pub fn direct_route_of_key(key: &str) -> Option<DeviceRoute> {
        let (vendor_id, product_id) = key.strip_prefix("direct:")?.split_once(':')?;
        let vendor_id = u16::from_str_radix(vendor_id, 16).ok()?;
        let product_id = u16::from_str_radix(product_id, 16).ok()?;
        Some(DeviceRoute::Direct {
            vendor_id,
            product_id,
        })
    }

    /// Stable key for persisted per-physical-device configuration.
    ///
    /// Receiver-connected devices are identified by receiver UID + pairing
    /// slot. A direct device is identified by its route plus the identity it
    /// reports, down to the route alone for a model identity: the USB model is
    /// the most specific thing such hardware says about itself. A routeless
    /// device needs a serial or unit id, since nothing else about it survives
    /// a reconnect. Raw HID devices require a serial-backed route identity; OS
    /// node identities are transient probe results, not physical identities.
    #[must_use]
    pub fn physical_key(&self) -> Option<PhysicalDeviceKey> {
        match self {
            Self::Bolt { .. } => Some(PhysicalDeviceKey(self.runtime_key())),
            Self::Direct { identity, .. } => identity
                .as_ref()
                .map(|_| PhysicalDeviceKey(self.runtime_key())),
            Self::Unknown { identity, .. } => identity
                .as_ref()
                .and_then(DeviceIdentity::fragment)
                .map(|_| PhysicalDeviceKey(self.runtime_key())),
            Self::RawHid { identity, .. } => {
                raw_identity_is_physical(identity).then(|| PhysicalDeviceKey(self.runtime_key()))
            }
        }
    }
}

/// Append a device's identity fragment to the key of the route it sits on.
///
/// An identity the probe never read is spelled as the all-zero unit id; a
/// model identity adds nothing, the route already names the model.
fn with_identity(route: String, identity: Option<&DeviceIdentity>) -> String {
    let fragment = identity.map_or_else(
        || Some(UNREAD_UNIT_FRAGMENT.to_string()),
        DeviceIdentity::fragment,
    );
    match fragment {
        Some(fragment) => format!("{route}:{fragment}"),
        None => route,
    }
}

impl DeviceIdentity {
    /// This identity as a bare `serial:`/`unit:` fragment carrying no route
    /// information, or `None` for a model identity, which only means
    /// something next to the route it was reached by.
    pub(crate) fn fragment(&self) -> Option<String> {
        match self {
            Self::Serial(serial) => Some(format!("serial:{serial}")),
            Self::Unit(unit) => Some(format!("unit:{}", unit.hex())),
            Self::Model => None,
        }
    }
}

impl PhysicalDeviceKey {
    /// Parse a key emitted by [`DeviceStableId::physical_key`].
    ///
    /// A bare `direct:<vid>:<pid>` key is the identity of a direct device that
    /// reports nothing beyond its model. Keys whose identity fragment is the
    /// all-zero unit id, and model names that predate route keys, return
    /// `None`; callers use that distinction when applying compatibility
    /// behavior.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        if receiver_key_is_valid(value)
            || DeviceStableId::direct_route_of_key(value).is_some()
            || direct_identity_fragment(value).is_some_and(identity_fragment_is_physical)
            || raw_identity_fragment(value).is_some_and(raw_identity_is_physical)
            || unknown_identity_fragment(value).is_some_and(identity_fragment_is_physical)
            || identity_fragment_is_physical(value)
        {
            Some(Self(value.to_string()))
        } else {
            None
        }
    }

    /// This key as the inventory writes it when it names the device by its
    /// own identity: `serial:` or `unit:`, in lowercase like
    /// [`DeviceIdentity::config_key`]. `None` for a key that names a route
    /// that reaches the device.
    #[must_use]
    pub fn to_identity(&self) -> Option<Self> {
        identity_fragment_is_physical(&self.0).then(|| Self(self.0.to_ascii_lowercase()))
    }

    /// Whether `value` is a structurally valid runtime key whose only device
    /// identity is the all-zero unit id.
    #[must_use]
    pub fn is_transient(value: &str) -> bool {
        direct_identity_fragment(value)
            .or_else(|| unknown_identity_fragment(value))
            .is_some_and(|identity| identity == UNREAD_UNIT_FRAGMENT)
            || raw_identity_fragment(value)
                .is_some_and(|identity| identity.starts_with("id:") || identity.is_empty())
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

fn raw_identity_fragment(value: &str) -> Option<&str> {
    let mut parts = value.strip_prefix("raw:")?.splitn(5, ':');
    let vendor_id = parts.next()?;
    let product_id = parts.next()?;
    let usage_page = parts.next()?;
    let usage_id = parts.next()?;
    let identity = parts.next()?;
    (is_hex_word(vendor_id)
        && is_hex_word(product_id)
        && is_hex_word(usage_page)
        && is_hex_word(usage_id)
        && !identity.is_empty())
    .then_some(identity)
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

fn raw_identity_is_physical(value: &str) -> bool {
    value
        .strip_prefix("serial:")
        .is_some_and(|serial| !serial.is_empty())
        || value
            .strip_prefix("stable:")
            .is_some_and(|identity| !identity.is_empty())
}

fn is_hex_word(value: &str) -> bool {
    value.len() == 4 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use crate::hid::DeviceRoute;

    use super::{DeviceIdentity, DeviceStableId, PhysicalDeviceKey, UnitId};

    #[test]
    fn unifying_route_maps_to_bolt_stable_id() {
        let route = DeviceRoute::Unifying {
            receiver_uid: "DA2699E1".into(),
            slot: 2,
        };
        let id = DeviceStableId::from_parts(
            Some(&route),
            2,
            Some(DeviceIdentity::from_parts(None, [0; 4])),
        );
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
            DeviceStableId::from_parts(
                Some(&bolt),
                1,
                Some(DeviceIdentity::from_parts(None, [0; 4]))
            ),
            DeviceStableId::from_parts(
                Some(&unifying),
                1,
                Some(DeviceIdentity::from_parts(None, [0; 4]))
            ),
        );
    }

    #[test]
    fn config_key_is_physical_not_model_scoped() {
        let route = DeviceRoute::Bolt {
            receiver_uid: "AABB".into(),
            slot: 2,
        };

        assert_eq!(
            DeviceStableId::from_parts(
                Some(&route),
                2,
                Some(DeviceIdentity::from_parts(Some("SERIAL"), [1, 2, 3, 4]))
            )
            .physical_key()
            .map(PhysicalDeviceKey::into_string),
            Some("receiver:aabb:slot:2".to_string())
        );
    }

    #[test]
    fn a_device_reporting_neither_serial_nor_unit_is_identified_by_its_model() {
        assert_eq!(
            DeviceIdentity::from_parts(None, [0; 4]),
            DeviceIdentity::Model
        );
        assert_eq!(
            DeviceIdentity::from_parts(Some(""), [0; 4]),
            DeviceIdentity::Model
        );
        assert!(UnitId::new([0; 4]).is_none());
    }

    #[test]
    fn model_identified_direct_device_persists_under_its_route() {
        // A G402 answers DeviceInformation v0: every identity field is zero.
        // Its most specific identity is the USB model, so the route *is* the
        // key, exactly as for a device whose settings predate per-unit keys.
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc07e,
        };
        let id = DeviceStableId::from_parts(
            Some(&route),
            0xff,
            Some(DeviceIdentity::from_parts(None, [0; 4])),
        );

        assert_eq!(id.runtime_key(), "direct:046d:c07e");
        assert_eq!(id.runtime_key(), id.route_key());
        assert_eq!(
            id.physical_key().map(PhysicalDeviceKey::into_string),
            Some("direct:046d:c07e".to_string())
        );
        assert!(PhysicalDeviceKey::parse("direct:046d:c07e").is_some());
    }

    #[test]
    fn an_unread_identity_is_transient_and_never_persisted() {
        // HID++ 1.0 hardware, or a feature walk that timed out: nothing has
        // been read yet, so there is no identity to key on, not even the model.
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb023,
        };
        let id = DeviceStableId::from_parts(Some(&route), 0xff, None);

        assert_eq!(id.runtime_key(), "direct:046d:b023:unit:00000000");
        assert!(PhysicalDeviceKey::is_transient(&id.runtime_key()));
        assert!(id.physical_key().is_none());
        assert!(PhysicalDeviceKey::parse(&id.runtime_key()).is_none());
    }

    #[test]
    fn historical_zero_unit_keys_stay_transient() {
        // Before model identities existed, an identityless direct device was
        // written as `unit:00000000`. Such a key never resolves to a device.
        let legacy = "direct:046d:b023:unit:00000000";
        assert!(PhysicalDeviceKey::is_transient(legacy));
        assert!(PhysicalDeviceKey::parse(legacy).is_none());
        assert!(!PhysicalDeviceKey::is_transient("direct:046d:c07e"));
    }

    #[test]
    fn model_identity_without_a_route_cannot_be_persisted() {
        // A routeless device has nothing but its slot once its model is all
        // it reports, and a slot alone survives no reconnect.
        let id = DeviceStableId::from_parts(None, 1, Some(DeviceIdentity::Model));

        assert_eq!(id.runtime_key(), "unknown:slot:1");
        assert!(id.physical_key().is_none());
        assert!(PhysicalDeviceKey::parse(&id.runtime_key()).is_none());
    }

    #[test]
    fn serial_identity_is_physical_when_unit_is_zero() {
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb023,
        };
        let key = DeviceStableId::from_parts(
            Some(&route),
            0xff,
            Some(DeviceIdentity::from_parts(Some("ABCDEF"), [0; 4])),
        )
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
        let key = DeviceStableId::from_parts(
            Some(&route),
            0xff,
            Some(DeviceIdentity::from_parts(None, [0xa3, 0x93, 0xca, 0xe0])),
        )
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

    #[test]
    fn raw_os_identity_is_transient_across_reconnects() {
        let old = DeviceRoute::RawHid {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "id:old-node".into(),
        };
        let new = DeviceRoute::RawHid {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "id:new-node".into(),
        };
        let old_id = DeviceStableId::from_parts(
            Some(&old),
            0xff,
            Some(DeviceIdentity::from_parts(None, [0; 4])),
        );
        let new_id = DeviceStableId::from_parts(
            Some(&new),
            0xff,
            Some(DeviceIdentity::from_parts(None, [0; 4])),
        );
        assert_ne!(old_id.runtime_key(), new_id.runtime_key());
        assert!(old_id.physical_key().is_none());
        assert!(new_id.physical_key().is_none());
    }

    #[test]
    fn raw_serial_identity_survives_a_changed_os_node() {
        let route = DeviceRoute::RawHid {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "serial:glow-1".into(),
        };
        let key = DeviceStableId::from_parts(
            Some(&route),
            0xff,
            Some(DeviceIdentity::from_parts(Some("glow-1"), [0; 4])),
        )
        .physical_key()
        .map(PhysicalDeviceKey::into_string);
        assert_eq!(key, Some("raw:046d:c900:ff43:0202:serial:glow-1".into()));
    }

    #[test]
    fn a_direct_route_key_drops_the_device_identity() {
        // The identity moves to the entry key; the route names only the path, so
        // the same mouse cabled and on its receiver can index the same entry.
        let id = DeviceStableId::Direct {
            vendor_id: 0x046d,
            product_id: 0xc08d,
            identity: Some(DeviceIdentity::from_parts(None, [0x6b, 0xe9, 0xd3, 0x00])),
        };
        assert_eq!(id.runtime_key(), "direct:046d:c08d:unit:6be9d300");
        assert_eq!(id.route_key(), "direct:046d:c08d");
    }

    #[test]
    fn other_routes_keep_their_runtime_key() {
        // A receiver key names the receiver and slot, never the paired device, so
        // there is no identity to strip. Raw and routeless keys keep their
        // identity because it is an OS node id, not a device identity that can
        // move to the entry key.
        let bolt = DeviceStableId::Bolt {
            receiver_uid: "82839805".to_string(),
            slot: 1,
        };
        assert_eq!(bolt.route_key(), bolt.runtime_key());

        let unknown = DeviceStableId::Unknown {
            slot: 255,
            identity: Some(DeviceIdentity::from_parts(None, [1, 2, 3, 4])),
        };
        assert_eq!(unknown.route_key(), unknown.runtime_key());
    }

    #[test]
    fn a_saved_direct_key_names_its_route_and_nothing_else_does() {
        let wired = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc354,
        };
        let id = DeviceStableId::Direct {
            vendor_id: 0x046d,
            product_id: 0xc354,
            identity: Some(DeviceIdentity::from_parts(Some("2603WD7076V8"), [0; 4])),
        };
        assert_eq!(
            DeviceStableId::direct_route_of_key(&id.route_key()),
            Some(wired)
        );
        // A runtime key carries the device identity after the route: it is not
        // a `links` key, and neither is any other shape.
        for key in [
            id.runtime_key().as_str(),
            "direct:046d:c355:extra",
            "receiver:82839805:slot:1",
            "serial:2603wd7076v8",
            "unit:4f4c4403",
            "raw:046d:c900:ff43:0202:abc",
            "direct:046d",
            "direct:zz:c354",
        ] {
            assert_eq!(DeviceStableId::direct_route_of_key(key), None, "{key}");
        }
    }
}
