//! The G600's feature reports as bytes.
//!
//! The layout is libratbag's (`src/driver-logitech-g600.c`, MIT), confirmed
//! byte for byte against a G600 on Windows 11. Every report is a feature
//! report on the `0xff80/0x0080` collection, whose declared feature length
//! is [`FEATURE_REPORT_LEN`] for every report id — a host that pads to it
//! (Windows) hands back [`ACTIVE_REPORT_LEN`] meaningful bytes of report
//! `0xf0` followed by zeros.
//!
//! Report `0xf0` is the selection: `[0xf0, selection, 0, 0]`. Read, the
//! selection byte packs the active DPI slot and profile ([`ActiveState`]).
//! Written, `0x40 | slot << 1` selects a DPI slot and `0x80 | profile << 4`
//! a profile; the mouse applies either at once.
//!
//! Reports `0xf3`, `0xf4` and `0xf5` are the three onboard profiles, each
//! [`FEATURE_REPORT_LEN`] bytes: the LED colour and effect, the report rate,
//! a DPI-shift value, the default slot, four DPI slots, and twenty
//! `(code, modifier, key)` button triples followed by twenty more for the
//! G-shift layer. This driver reads the DPI fields; the profile is otherwise
//! carried whole so that nothing undocumented is lost.

use openlogi_core::hid::Dpi;
use thiserror::Error;

/// The collection's declared feature-report length, which every profile
/// report fills.
pub const FEATURE_REPORT_LEN: usize = 154;
/// Report `0xf0`'s own length: the id, the selection byte and two zero bytes.
pub const ACTIVE_REPORT_LEN: usize = 4;
pub(super) const ACTIVE_REPORT_ID: u8 = 0xf0;
const PROFILE_REPORT_IDS: [u8; 3] = [0xf3, 0xf4, 0xf5];
/// DPI is stored in units of 50: `0x04` is 200, `0xa4` is 8200, and 0 is a
/// disabled slot.
const DPI_STEP: u16 = 50;
/// Selection-byte flag: bits 1–2 carry the DPI slot to select.
const SELECT_DPI_SLOT: u8 = 0x40;

const LED_OFFSET: usize = 1;
const REPORT_RATE_OFFSET: usize = 11;
const DPI_SHIFT_OFFSET: usize = 12;
const DPI_DEFAULT_OFFSET: usize = 13;
const DPI_SLOTS_OFFSET: usize = 14;

/// One of the three onboard profiles, 0-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ProfileSlot(u8);

impl ProfileSlot {
    /// Every profile, in report order.
    pub(super) const ALL: [Self; 3] = [Self(0), Self(1), Self(2)];

    fn new(index: u8) -> Option<Self> {
        (usize::from(index) < PROFILE_REPORT_IDS.len()).then_some(Self(index))
    }

    pub(super) fn index(self) -> u8 {
        self.0
    }

    pub(super) fn report_id(self) -> u8 {
        PROFILE_REPORT_IDS[usize::from(self.0)]
    }
}

/// One of a profile's four DPI slots, 0-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DpiSlot(u8);

impl DpiSlot {
    /// Every slot, in profile order.
    pub(super) const ALL: [Self; 4] = [Self(0), Self(1), Self(2), Self(3)];

    pub(super) fn index(self) -> u8 {
        self.0
    }

    fn offset(self) -> usize {
        DPI_SLOTS_OFFSET + usize::from(self.0)
    }
}

/// Report `0xf0` as the mouse answers it: which profile and DPI slot are
/// active.
///
/// The selection byte packs, least significant bit first, one unknown bit,
/// the DPI slot in two bits, one unknown bit and the profile in four. Neither
/// unknown bit is interpreted: the low one is set on the mouse's first answer
/// after power-on and clear after any select, the other is always set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ActiveState {
    pub(super) profile: ProfileSlot,
    pub(super) dpi_slot: DpiSlot,
}

impl ActiveState {
    pub(super) fn decode(report: &[u8]) -> Result<Self, G600ReportError> {
        let &[id, selection, ..] = report else {
            return Err(G600ReportError::Length {
                report_id: report.first().copied(),
                len: report.len(),
            });
        };
        if id != ACTIVE_REPORT_ID {
            return Err(G600ReportError::ReportId(id));
        }
        let profile = (selection >> 4) & 0x0f;
        let profile = ProfileSlot::new(profile).ok_or(G600ReportError::Profile(profile))?;
        Ok(Self {
            profile,
            dpi_slot: DpiSlot((selection >> 1) & 0x03),
        })
    }
}

/// The `0xf0` write that makes `slot` the active DPI slot of the active
/// profile.
pub(super) fn encode_select_dpi_slot(slot: DpiSlot) -> [u8; ACTIVE_REPORT_LEN] {
    [ACTIVE_REPORT_ID, SELECT_DPI_SLOT | (slot.0 << 1), 0, 0]
}

/// One onboard profile, kept as the mouse holds it so that every byte,
/// documented or not, survives a round trip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProfileReport([u8; FEATURE_REPORT_LEN]);

impl ProfileReport {
    pub(super) fn decode(report: &[u8]) -> Result<Self, G600ReportError> {
        let bytes: [u8; FEATURE_REPORT_LEN] =
            report.try_into().map_err(|_| G600ReportError::Length {
                report_id: report.first().copied(),
                len: report.len(),
            })?;
        if !PROFILE_REPORT_IDS.contains(&bytes[0]) {
            return Err(G600ReportError::ReportId(bytes[0]));
        }
        Ok(Self(bytes))
    }

    pub(super) fn into_bytes(self) -> [u8; FEATURE_REPORT_LEN] {
        self.0
    }

    /// The LED colour as red, green, blue.
    pub(super) fn led(&self) -> [u8; 3] {
        [
            self.0[LED_OFFSET],
            self.0[LED_OFFSET + 1],
            self.0[LED_OFFSET + 2],
        ]
    }

    /// The USB report rate in hertz; the byte is `1000 / rate - 1`.
    pub(super) fn report_rate_hz(&self) -> u16 {
        1000 / (u16::from(self.0[REPORT_RATE_OFFSET]) + 1)
    }

    /// The DPI the DPI-shift button drops to while held, or `None` when the
    /// profile disables it.
    pub(super) fn dpi_shift(&self) -> Option<Dpi> {
        dpi_from_raw(self.0[DPI_SHIFT_OFFSET])
    }

    /// The slot the profile starts in. Stored 1-based; 0 names no default.
    pub(super) fn default_dpi_slot(&self) -> Option<DpiSlot> {
        let raw = self.0[DPI_DEFAULT_OFFSET];
        DpiSlot::ALL.into_iter().find(|slot| slot.0 + 1 == raw)
    }

    /// The DPI in `slot`, or `None` when the profile disables that slot.
    pub(super) fn dpi_slot(&self, slot: DpiSlot) -> Option<Dpi> {
        dpi_from_raw(self.0[slot.offset()])
    }

    /// Every slot's DPI, in slot order.
    pub(super) fn dpi_slots(&self) -> [Option<Dpi>; 4] {
        DpiSlot::ALL.map(|slot| self.dpi_slot(slot))
    }
}

fn dpi_from_raw(raw: u8) -> Option<Dpi> {
    (raw != 0).then(|| Dpi::from(u16::from(raw) * DPI_STEP))
}

/// Why bytes are not the G600 report they were read as.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub(super) enum G600ReportError {
    /// The report is not the length its id calls for.
    #[error("report {report_id:02x?} is {len} bytes long")]
    Length {
        /// The report id, when there was a first byte.
        report_id: Option<u8>,
        /// The length handed back.
        len: usize,
    },
    /// The first byte is not the id that was asked for.
    #[error("report id {0:#04x} is not the G600 report asked for")]
    ReportId(u8),
    /// The selection byte names a profile the mouse does not have.
    #[error("profile {0} is not one of the three onboard profiles")]
    Profile(u8),
}

#[cfg(test)]
mod tests {
    use openlogi_core::hid::Dpi;

    use super::super::tests::{profile_1_captured, profile_2_stock};
    use super::{
        ACTIVE_REPORT_ID, ActiveState, DpiSlot, G600ReportError, ProfileReport, ProfileSlot,
        encode_select_dpi_slot,
    };

    #[test]
    fn the_selection_byte_carries_the_profile_and_slot() {
        // As captured: profile 1, slot 0, the power-on bit still set.
        let state = ActiveState::decode(&[ACTIVE_REPORT_ID, 0x19, 0, 0]).unwrap();
        assert_eq!(state.profile, ProfileSlot::ALL[1]);
        assert_eq!(state.dpi_slot, DpiSlot::ALL[0]);
        // After selecting slot 3 and then profile 2.
        let state = ActiveState::decode(&[ACTIVE_REPORT_ID, 0x1e, 0, 0]).unwrap();
        assert_eq!(state.dpi_slot, DpiSlot::ALL[3]);
        let state = ActiveState::decode(&[ACTIVE_REPORT_ID, 0x28, 0, 0]).unwrap();
        assert_eq!(state.profile, ProfileSlot::ALL[2]);
        assert_eq!(state.dpi_slot, DpiSlot::ALL[0]);
    }

    #[test]
    fn a_padded_selection_report_decodes_like_a_short_one() {
        let mut padded = [0; 154];
        padded[0] = ACTIVE_REPORT_ID;
        padded[1] = 0x28;
        assert_eq!(
            ActiveState::decode(&padded).unwrap(),
            ActiveState::decode(&[ACTIVE_REPORT_ID, 0x28, 0, 0]).unwrap()
        );
    }

    #[test]
    fn a_selection_report_that_is_not_one_is_refused() {
        assert_eq!(
            ActiveState::decode(&[ACTIVE_REPORT_ID]),
            Err(G600ReportError::Length {
                report_id: Some(ACTIVE_REPORT_ID),
                len: 1
            })
        );
        assert_eq!(
            ActiveState::decode(&[0xf3, 0x19, 0, 0]),
            Err(G600ReportError::ReportId(0xf3))
        );
        assert_eq!(
            ActiveState::decode(&[ACTIVE_REPORT_ID, 0x38, 0, 0]),
            Err(G600ReportError::Profile(3))
        );
    }

    #[test]
    fn selecting_a_slot_sets_the_flag_and_shifts_the_slot_in() {
        assert_eq!(encode_select_dpi_slot(DpiSlot::ALL[0]), [0xf0, 0x40, 0, 0]);
        assert_eq!(encode_select_dpi_slot(DpiSlot::ALL[2]), [0xf0, 0x44, 0, 0]);
        assert_eq!(encode_select_dpi_slot(DpiSlot::ALL[3]), [0xf0, 0x46, 0, 0]);
    }

    #[test]
    fn the_stock_gaming_profile_decodes() {
        let profile = ProfileReport::decode(&profile_2_stock()).unwrap();
        assert_eq!(profile.led(), [0x00, 0xff, 0x00]);
        assert_eq!(profile.report_rate_hz(), 1000);
        assert_eq!(profile.dpi_shift(), Some(Dpi::from(400)));
        assert_eq!(profile.default_dpi_slot(), Some(DpiSlot::ALL[1]));
        assert_eq!(
            profile.dpi_slots(),
            [400, 1200, 2000, 3200].map(|dpi| Some(Dpi::from(dpi)))
        );
    }

    #[test]
    fn a_profile_with_one_slot_leaves_the_rest_disabled() {
        let profile = ProfileReport::decode(&profile_1_captured()).unwrap();
        assert_eq!(profile.led(), [0xff, 0xff, 0xff]);
        assert_eq!(profile.dpi_shift(), None);
        assert_eq!(profile.default_dpi_slot(), Some(DpiSlot::ALL[0]));
        assert_eq!(
            profile.dpi_slots(),
            [Some(Dpi::from(1200)), None, None, None]
        );
    }

    #[test]
    fn a_profile_with_no_default_slot_says_so() {
        let mut bytes = profile_2_stock();
        bytes[13] = 0;
        let profile = ProfileReport::decode(&bytes).unwrap();
        assert_eq!(profile.default_dpi_slot(), None);
    }

    #[test]
    fn a_profile_round_trips_every_byte() {
        let bytes = profile_2_stock();
        assert_eq!(ProfileReport::decode(&bytes).unwrap().into_bytes(), bytes);
    }

    #[test]
    fn a_profile_report_of_the_wrong_shape_is_refused() {
        let bytes = profile_2_stock();
        assert_eq!(
            ProfileReport::decode(&bytes[..153]),
            Err(G600ReportError::Length {
                report_id: Some(0xf5),
                len: 153
            })
        );
        let mut long = bytes.to_vec();
        long.push(0);
        assert_eq!(
            ProfileReport::decode(&long),
            Err(G600ReportError::Length {
                report_id: Some(0xf5),
                len: 155
            })
        );
        let mut other_id = bytes;
        other_id[0] = 0xf2;
        assert_eq!(
            ProfileReport::decode(&other_id),
            Err(G600ReportError::ReportId(0xf2))
        );
    }
}
