//! The device's own identity, as a probe of HID++ `0x0003`
//! DeviceInformation learned it, and the one change the cache may make to it.

use openlogi_core::device::DeviceModelInfo;
use serde::{Deserialize, Serialize};

/// What a probe learned of the device's identity from DeviceInformation.
///
/// One state per outcome, so a probe cannot claim an identity and a failed
/// read of it at once: a model without the serial number the device reports
/// is not a [`Self::Known`]. The probe judges an answer through
/// [`Self::answered`], and a probed state changes only through
/// [`Self::backfill`], the cache's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum ProbedIdentity {
    /// The device has no DeviceInformation feature, or the probe never got
    /// that far: there is nothing the probe failed to read.
    #[default]
    Unavailable,
    /// The DeviceInformation read failed: the device has an identity the
    /// probe learned nothing of.
    ReadFailed,
    /// The device answered DeviceInformation but its serial-number read
    /// failed: it has an identity the probe only partly got.
    SerialLost(AnsweringUnit),
    /// The device answered in full.
    Known(DeviceModelInfo),
}

/// Which unit answered a DeviceInformation read that lost its serial number.
///
/// Enough to tell that a cached identity is another device's, and not a
/// [`DeviceModelInfo`], so that it can never be published as an identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct AnsweringUnit {
    unit_id: [u8; 4],
    model_ids: [u16; 3],
    extended_model_id: u8,
}

impl AnsweringUnit {
    fn of(model: &DeviceModelInfo) -> Self {
        Self {
            unit_id: model.unit_id,
            model_ids: model.model_ids,
            extended_model_id: model.extended_model_id,
        }
    }
}

/// How the serial-number read of an answered DeviceInformation went.
pub(super) enum SerialRead {
    /// The device reports no serial number, or sent a blank one.
    Unreported,
    /// The device sent its serial number.
    Read(String),
    /// The device reports a serial number, but the read of it failed.
    Lost,
}

impl ProbedIdentity {
    /// The identity a DeviceInformation answer gives: `model` is the rest of
    /// the answer, and `serial` how the read of its serial number went. The
    /// serial number comes from `serial` alone, whatever `model` holds.
    pub(super) fn answered(mut model: DeviceModelInfo, serial: SerialRead) -> Self {
        model.serial_number = match serial {
            SerialRead::Unreported => None,
            SerialRead::Read(serial) => Some(serial),
            // With its serial lost, the model info would read as a unit or
            // model identity, a key the device does not answer to once its
            // serial is read (#2025). No identity until the serial read
            // succeeds or the cache backfills it.
            SerialRead::Lost => return Self::SerialLost(AnsweringUnit::of(&model)),
        };
        Self::Known(model)
    }

    /// Fill what a failed read missed from the identity an earlier probe of
    /// this cache entry read, so a transient DeviceInformation failure cannot
    /// flip the device's config key (#482).
    ///
    /// A cache key does not prove which device it read: a Unifying slot can be
    /// paired anew, and an OS node id handed to another device. So a lost
    /// serial read takes the cached identity only if the unit it did read is
    /// that identity's, or the replacement would be published under the
    /// previous device's key. A read that failed outright has nothing to
    /// refuse it with.
    ///
    /// Every other transition is refused: a fresh answer outranks the cache,
    /// an absent feature is the device's answer rather than a failed read, and
    /// a cache that read no more than this probe has nothing to give.
    pub(super) fn backfill(&mut self, cached: &Self) {
        let Self::Known(known) = cached else {
            return;
        };
        let may_take_it = match self {
            Self::ReadFailed => true,
            Self::SerialLost(unit) => *unit == AnsweringUnit::of(known),
            Self::Unavailable | Self::Known(_) => false,
        };
        if may_take_it {
            self.clone_from(cached);
        }
    }

    /// Whether a read failed, so the device has identity data this probe is
    /// missing. Such a probe is not memoized on first sight, or it would pin
    /// a weaker config key than the device has (#482).
    pub(super) fn is_incomplete(&self) -> bool {
        matches!(self, Self::ReadFailed | Self::SerialLost(_))
    }

    /// The model information a paired device publishes.
    pub(super) fn model(&self) -> Option<&DeviceModelInfo> {
        match self {
            Self::Known(model) => Some(model),
            Self::Unavailable | Self::ReadFailed | Self::SerialLost(_) => None,
        }
    }

    /// [`Self::model`], by value.
    pub(super) fn into_model(self) -> Option<DeviceModelInfo> {
        match self {
            Self::Known(model) => Some(model),
            Self::Unavailable | Self::ReadFailed | Self::SerialLost(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::device::{DeviceModelInfo, DeviceTransports};

    use super::{AnsweringUnit, ProbedIdentity, SerialRead};

    fn model(unit_id: [u8; 4], serial: Option<&str>) -> DeviceModelInfo {
        DeviceModelInfo {
            entity_count: 1,
            serial_number: serial.map(str::to_string),
            unit_id,
            transports: DeviceTransports::default(),
            model_ids: [0xc09d, 0, 0],
            extended_model_id: 1,
        }
    }

    fn lost(unit_id: [u8; 4]) -> ProbedIdentity {
        ProbedIdentity::SerialLost(AnsweringUnit::of(&model(unit_id, None)))
    }

    #[test]
    fn a_read_serial_completes_the_identity() {
        let identity =
            ProbedIdentity::answered(model([1, 2, 3, 4], None), SerialRead::Read("ABC".into()));

        assert_eq!(
            identity,
            ProbedIdentity::Known(model([1, 2, 3, 4], Some("ABC")))
        );
        assert!(!identity.is_incomplete());
    }

    /// One the caller left in the model is not a serial the device sent.
    #[test]
    fn only_the_serial_read_gives_a_serial_number() {
        let identity =
            ProbedIdentity::answered(model([1, 2, 3, 4], Some("stray")), SerialRead::Unreported);

        assert_eq!(identity, ProbedIdentity::Known(model([1, 2, 3, 4], None)));
    }

    #[test]
    fn a_device_that_reports_no_serial_has_a_complete_identity() {
        let identity = ProbedIdentity::answered(model([1, 2, 3, 4], None), SerialRead::Unreported);

        assert_eq!(identity, ProbedIdentity::Known(model([1, 2, 3, 4], None)));
        assert!(!identity.is_incomplete());
    }

    /// Its model info would read as a unit identity and key the device
    /// `unit:<hex>`, a key it stops answering to once its serial is read
    /// (#2025).
    #[test]
    fn a_lost_serial_read_beside_a_unit_id_is_a_failed_read() {
        let identity = ProbedIdentity::answered(model([1, 2, 3, 4], None), SerialRead::Lost);

        assert_eq!(identity, lost([1, 2, 3, 4]));
        assert_eq!(identity.model(), None);
        assert!(
            identity.is_incomplete(),
            "the device has a serial the probe failed to read"
        );
    }

    #[test]
    fn a_lost_serial_read_beside_a_zero_unit_id_is_a_failed_read() {
        let identity = ProbedIdentity::answered(model([0; 4], None), SerialRead::Lost);

        assert_eq!(identity, lost([0; 4]));
        assert_eq!(identity.model(), None);
    }

    #[test]
    fn only_a_failed_read_is_incomplete() {
        assert!(ProbedIdentity::ReadFailed.is_incomplete());
        assert!(lost([1, 2, 3, 4]).is_incomplete());
        assert!(
            !ProbedIdentity::Unavailable.is_incomplete(),
            "an absent feature is the device's whole answer"
        );
    }

    /// Every pair of states: the cache fills a failed read from an identity
    /// it read in full, unless the read said it came from another unit, and
    /// refuses the rest. A fresh answer outranks the cache, an absent feature
    /// is the device's answer rather than a failed read, and a cache that read
    /// no more than the probe has nothing to give.
    #[test]
    fn backfill_takes_the_cache_only_over_a_failed_read_of_that_unit() {
        use ProbedIdentity::{Known, ReadFailed, Unavailable};
        let fresh_known = Known(model([1, 2, 3, 4], None));
        let cached_known = Known(model([9, 9, 9, 9], Some("abc123")));
        let lost_here = lost([9, 9, 9, 9]);
        let lost_elsewhere = lost([1, 2, 3, 4]);
        // (fresh, cached, the fresh probe once backfilled)
        let table = [
            (Unavailable, Unavailable, Unavailable),
            (Unavailable, ReadFailed, Unavailable),
            (Unavailable, lost_here.clone(), Unavailable),
            (Unavailable, cached_known.clone(), Unavailable),
            (ReadFailed, Unavailable, ReadFailed),
            (ReadFailed, ReadFailed, ReadFailed),
            (ReadFailed, lost_here.clone(), ReadFailed),
            (ReadFailed, cached_known.clone(), cached_known.clone()),
            (lost_here.clone(), Unavailable, lost_here.clone()),
            (lost_here.clone(), ReadFailed, lost_here.clone()),
            (lost_here.clone(), lost_elsewhere.clone(), lost_here.clone()),
            (
                lost_here.clone(),
                cached_known.clone(),
                cached_known.clone(),
            ),
            (lost_elsewhere.clone(), cached_known.clone(), lost_elsewhere),
            (fresh_known.clone(), Unavailable, fresh_known.clone()),
            (fresh_known.clone(), ReadFailed, fresh_known.clone()),
            (fresh_known.clone(), lost_here, fresh_known.clone()),
            (fresh_known.clone(), cached_known, fresh_known),
        ];

        for (fresh, cached, expected) in table {
            let mut backfilled = fresh.clone();
            backfilled.backfill(&cached);
            assert_eq!(backfilled, expected, "{fresh:?} backfilled from {cached:?}");
        }
    }

    /// What a lost read did get must all agree with the cached identity. An
    /// all-zero unit id says nothing, so between two of them only the model
    /// can refuse, and a device that never reports a unit keeps its key.
    #[test]
    fn a_lost_serial_read_refuses_what_it_contradicts() {
        let cached = model([9, 9, 9, 9], Some("abc123"));
        let unit = AnsweringUnit::of(&cached);
        let unitless = model([0; 4], Some("abc123"));
        let table = [
            (unit, cached.clone(), true),
            (
                AnsweringUnit {
                    unit_id: [1, 2, 3, 4],
                    ..unit
                },
                cached.clone(),
                false,
            ),
            (
                AnsweringUnit {
                    model_ids: [0xc09e, 0, 0],
                    ..unit
                },
                cached.clone(),
                false,
            ),
            (
                AnsweringUnit {
                    extended_model_id: 2,
                    ..unit
                },
                cached.clone(),
                false,
            ),
            (
                AnsweringUnit {
                    unit_id: [0; 4],
                    ..unit
                },
                cached,
                false,
            ),
            (AnsweringUnit::of(&unitless), unitless.clone(), true),
            (
                AnsweringUnit {
                    model_ids: [0xc09e, 0, 0],
                    ..AnsweringUnit::of(&unitless)
                },
                unitless,
                false,
            ),
        ];

        for (read, cached, takes_it) in table {
            let fresh = ProbedIdentity::SerialLost(read);
            let mut backfilled = fresh.clone();
            let cached = ProbedIdentity::Known(cached);
            backfilled.backfill(&cached);
            let expected = if takes_it { cached.clone() } else { fresh };
            assert_eq!(backfilled, expected, "{read:?} backfilled from {cached:?}");
        }
    }

    #[test]
    fn only_a_complete_identity_publishes_a_model() {
        let model = model([1, 2, 3, 4], Some("ABC"));

        assert_eq!(ProbedIdentity::Known(model.clone()).model(), Some(&model));
        assert_eq!(ProbedIdentity::ReadFailed.model(), None);
        assert_eq!(lost([1, 2, 3, 4]).model(), None);
        assert_eq!(ProbedIdentity::Unavailable.model(), None);
    }
}
