//! A device record's liveness: where it stands against the latest inventory
//! merge, and the decisions every consumer derives from that.

use openlogi_core::device::DeviceKind;
use openlogi_core::hid::DeviceRoute;

use super::DeviceRecord;

/// Inventory snapshots can briefly miss a real device while another HID++
/// request is in flight. Keep the previous record through this many
/// consecutive misses so a transient probe timeout does not make the device card
/// disappear mid-interaction.
pub(in crate::state) const INVENTORY_MISS_GRACE: u8 = 2;

/// Whether a device answered when the device list last listed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Link {
    /// It answered.
    Online,
    /// Listed but not answering: asleep, out of range, switched to another
    /// computer, or a placeholder restored from configuration.
    Offline,
}

/// Where a [`DeviceRecord`] stands against the latest inventory merge.
///
/// The only writers are the record's sources — each snapshot, camera scan
/// and configuration placeholder lists a record afresh — and
/// [`Self::missed`], which the merge applies to a record the snapshot left
/// out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Presence {
    /// The latest build of the device list listed the device.
    Listed(Link),
    /// The latest snapshots omitted the device. Its card stays as its last
    /// listing left it until the grace runs out.
    GraceKept(Grace),
}

/// A grace-kept record's last listed link and how many snapshots in a row
/// omitted it, from 1 to [`INVENTORY_MISS_GRACE`]. Only [`Presence::missed`]
/// makes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Grace {
    last: Link,
    misses: u8,
}

impl Presence {
    /// A source listing the device, with the source's own `online` flag.
    pub(super) fn listed(online: bool) -> Self {
        Self::Listed(if online { Link::Online } else { Link::Offline })
    }

    /// The presence after one more snapshot omitted the device, or `None`
    /// once that exceeds [`INVENTORY_MISS_GRACE`] and the card goes.
    #[must_use]
    fn missed(self) -> Option<Self> {
        let grace = match self {
            Self::Listed(last) => Grace { last, misses: 1 },
            Self::GraceKept(Grace { last, misses }) => Grace {
                last,
                misses: misses + 1,
            },
        };
        (grace.misses <= INVENTORY_MISS_GRACE).then_some(Self::GraceKept(grace))
    }

    /// The link the card shows. A grace-kept card shows its last listing, so
    /// a probe that missed a device in use leaves it as it was.
    fn link(self) -> Link {
        match self {
            Self::Listed(link) | Self::GraceKept(Grace { last: link, .. }) => link,
        }
    }

    /// Whether the card shows the device online. See [`Self::link`].
    fn is_online(self) -> bool {
        self.link() == Link::Online
    }

    /// Whether the latest snapshot itself read the device online. Never a
    /// grace-kept presence, whatever it shows: nothing was read of it.
    fn sighted(self) -> bool {
        self == Self::Listed(Link::Online)
    }
}

impl DeviceRecord {
    /// Whether the card shows the device online. See [`Presence::is_online`].
    pub(crate) fn is_online(&self) -> bool {
        self.presence.is_online()
    }

    /// The route a write reaches the device on now: its route while it is
    /// shown online. `None` for an offline device — a receiver keeps an
    /// asleep device's route, but nothing answers on it — and for an online
    /// device with no route of its own, a camera or the demo keyboard.
    pub(crate) fn addressable_route(&self) -> Option<&DeviceRoute> {
        self.route.as_ref().filter(|_| self.is_online())
    }

    /// The config key what the latest snapshot read of this device may be
    /// recorded under: its identity, and the route it was reached by.
    ///
    /// `Some` only for a persistent record the snapshot itself listed
    /// online. An offline route/identity pairing would be a guess, and a
    /// grace-kept record was not read at all — even one still shown online,
    /// and even one naming a key a route adoption has just retired.
    pub(in crate::state) fn sighting_key(&self) -> Option<&str> {
        self.presence
            .sighted()
            .then(|| self.persistent_config_key())
            .flatten()
    }

    /// Whether this record's card shows the same as `other`'s: equal in
    /// everything but how many snapshots in a row omitted the device, which
    /// nothing renders.
    pub(in crate::state) fn shows_as(&self, other: &Self) -> bool {
        self.presence.link() == other.presence.link()
            && *self
                == Self {
                    presence: self.presence,
                    ..other.clone()
                }
    }

    /// This record as the merge keeps it after a snapshot omitted it, or
    /// `None` once it goes.
    ///
    /// Two kinds never earn grace. An all-zero direct unit id is only a
    /// transient probe result: if the next snapshot resolves a physical
    /// serial/unit key, keeping this record through the miss grace would show
    /// both cards. And a camera reappears under a new capture id after a port
    /// change, so a stale `cam-live` record would sit beside the new one.
    pub(in crate::state) fn missed(&self) -> Option<Self> {
        if !self.is_persistent() || self.kind == DeviceKind::Camera {
            return None;
        }
        Some(Self {
            presence: self.presence.missed()?,
            ..self.clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::device::DeviceKind;
    use openlogi_core::hid::DeviceRoute;

    use super::{INVENTORY_MISS_GRACE, Link, Presence};
    use crate::state::devices::tests::online_record;

    fn bolt_route() -> DeviceRoute {
        DeviceRoute::Bolt {
            receiver_uid: "82839805".to_string(),
            slot: 1,
        }
    }

    #[test]
    fn a_source_lists_its_own_online_flag() {
        assert_eq!(Presence::listed(true), Presence::Listed(Link::Online));
        assert_eq!(Presence::listed(false), Presence::Listed(Link::Offline));
    }

    /// A missed device keeps showing its last link for exactly
    /// [`INVENTORY_MISS_GRACE`] misses, is never a sighting meanwhile, and
    /// goes on the next miss.
    #[test]
    fn a_missed_presence_keeps_its_last_link_through_the_grace() {
        for last in [Link::Online, Link::Offline] {
            let mut presence = Presence::Listed(last);
            for misses in 1..=INVENTORY_MISS_GRACE {
                presence = presence
                    .missed()
                    .unwrap_or_else(|| panic!("{last:?} evicted after {misses} misses"));
                assert_eq!(
                    presence.is_online(),
                    last == Link::Online,
                    "{last:?} after {misses} misses"
                );
                assert!(!presence.sighted(), "{last:?} after {misses} misses");
            }
            assert_eq!(presence.missed(), None, "{last:?} outlived the grace");
        }
    }

    /// Every decision the record derives from its presence, its route and
    /// its persistence, against each combination.
    #[test]
    fn record_decisions_follow_presence_route_and_persistence() {
        let grace_kept = |last| Presence::Listed(last).missed().expect("in grace");
        let presences = [
            (Presence::Listed(Link::Online), true, true),
            (Presence::Listed(Link::Offline), false, false),
            (grace_kept(Link::Online), true, false),
            (grace_kept(Link::Offline), false, false),
        ];
        for (presence, shows_online, sighted) in presences {
            for route in [Some(bolt_route()), None] {
                for persistent in [true, false] {
                    let mut record = online_record("unit:6be9d300");
                    record.presence = presence;
                    record.route.clone_from(&route);
                    record.persistent = persistent;
                    let case = format!("{presence:?} route={route:?} persistent={persistent}");

                    assert_eq!(record.is_online(), shows_online, "{case}");
                    assert_eq!(
                        record.addressable_route(),
                        route.as_ref().filter(|_| shows_online),
                        "{case}"
                    );
                    assert_eq!(
                        record.sighting_key(),
                        (sighted && persistent).then_some("unit:6be9d300"),
                        "{case}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_missed_record_keeps_everything_but_its_presence() {
        let mut record = online_record("unit:6be9d300");
        record.route = Some(bolt_route());

        let kept = record.missed().expect("a persistent mouse earns grace");

        assert_ne!(kept, record, "the miss is counted");
        assert!(kept.shows_as(&record), "but nothing on screen changed");
        assert_eq!(kept.addressable_route(), Some(&bolt_route()));
        assert_eq!(kept.sighting_key(), None);

        let mut offline = record.clone();
        offline.presence = Presence::Listed(Link::Offline);
        assert!(!offline.shows_as(&record));
        let mut renamed = kept.clone();
        renamed.display_name = "renamed".to_string();
        assert!(!renamed.shows_as(&record));
    }

    #[test]
    fn transient_records_and_cameras_never_earn_grace() {
        let mut transient = online_record("direct:046d:b023:unit:00000000");
        transient.persistent = false;
        assert_eq!(transient.missed(), None);

        let mut camera = online_record("cam:0893");
        camera.kind = DeviceKind::Camera;
        assert_eq!(camera.missed(), None);
    }
}
