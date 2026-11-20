//! Logitech G600 MMO gaming mouse raw-HID identity.
//!
//! The G600 predates HID++ 2.0 on gaming mice and answers no HID++ request.
//! Its configuration lives behind vendor feature reports on the second USB
//! interface's `0xff80/0x0080` collection (the first collection there is the
//! G-key keyboard): report `0xf0` carries the active profile and DPI slot,
//! `0xf3`–`0xf5` the three 154-byte onboard profiles (libratbag,
//! `src/driver-logitech-g600.c`, MIT). OpenLogi surfaces it as a standalone
//! raw-HID device. This module decodes those reports into typed values and
//! encodes the DPI writes; `openlogi-device` reads and writes them.

use core::time::Duration;

use crate::LOGITECH_VENDOR_ID;

/// Stable driver-family identifier carried by standalone G600 inventory.
pub const G600_DRIVER_ID: &str = "logitech-g600";
/// G600 USB product ID.
pub const G600_PRODUCT_ID: u16 = 0xc24a;
/// Usage page of the G600's vendor feature-report collection.
pub const G600_USAGE_PAGE: u16 = 0xff80;
/// Usage ID of the G600's vendor feature-report collection.
pub const G600_USAGE_ID: u16 = 0x0080;
/// Marketed product name.
pub const G600_NAME: &str = "G600 Gaming Mouse";

/// Whether an HID node is the G600's vendor feature-report collection.
#[must_use]
pub fn matches_g600(vendor_id: u16, product_id: u16, usage_page: u16, usage_id: u16) -> bool {
    (vendor_id, product_id, usage_page, usage_id)
        == (
            LOGITECH_VENDOR_ID,
            G600_PRODUCT_ID,
            G600_USAGE_PAGE,
            G600_USAGE_ID,
        )
}

/// Feature-report ID that carries the active profile and DPI stage.
pub const ACTIVE_REPORT_ID: u8 = 0xf0;
/// Length of the active-profile report, including its ID byte.
pub const ACTIVE_REPORT_LEN: usize = 4;
/// Feature-report IDs of the three onboard profiles, in profile order.
const PROFILE_REPORT_IDS: [u8; PROFILE_COUNT] = [0xf3, 0xf4, 0xf5];
/// Length of an onboard-profile report, including its ID byte.
pub const PROFILE_REPORT_LEN: usize = 154;
/// Number of onboard profiles.
pub const PROFILE_COUNT: usize = 3;
/// Number of DPI stages in a profile.
pub const DPI_STAGE_COUNT: usize = 4;

/// Byte offset of the DPI-shift value in a profile report.
const SHIFT_DPI_OFFSET: usize = 12;
/// Byte offset of the 1-based default DPI stage in a profile report.
const DEFAULT_STAGE_OFFSET: usize = 13;
/// Byte offset of the first DPI stage in a profile report.
const STAGES_OFFSET: usize = 14;
/// DPI represented by one unit of a DPI byte (`200` DPI is `0x04`).
const DPI_PER_UNIT: u16 = 50;
/// Bit that turns an active-profile report into a set-DPI-stage command.
const SET_STAGE_FLAG: u8 = 0x40;

/// How long one feature-report transfer to the G600 may take before the
/// driver gives up on it. Only some hosts bound the transfer themselves.
pub const REPORT_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the mouse needs to store a profile report it has accepted. A
/// request that reaches it sooner corrupts the stored profile: on a G600 any
/// request within 300 ms of the write left bytes shifted or zeroed, while one
/// after 500 ms never did.
pub const PROFILE_COMMIT_TIME: Duration = Duration::from_secs(1);

/// The most feature reports one profile write moves: the active selection
/// and the profile read, the profile written and read back, then a stage
/// selected and read back.
const PROFILE_WRITE_REPORTS: u32 = 6;

/// The longest one profile write takes while every report it moves stays
/// within [`REPORT_TIMEOUT`]: those reports plus the commit wait.
pub const MAX_PROFILE_WRITE_TIME: Duration = REPORT_TIMEOUT
    .saturating_mul(PROFILE_WRITE_REPORTS)
    .saturating_add(PROFILE_COMMIT_TIME);

/// How long a client waits for the answer to a profile write it sent. The
/// agent serves one G600 request per mouse at a time, so the write may first
/// wait out another as long as itself.
pub const PROFILE_WRITE_DEADLINE: Duration = MAX_PROFILE_WRITE_TIME.saturating_mul(2);

/// One of the three onboard profiles, numbered from 1 like the mouse's own
/// software.
///
/// Serialized as its number, so a number outside `1..=`[`PROFILE_COUNT`]
/// fails to deserialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(try_from = "u8", into = "u8")
)]
pub struct OnboardProfile(u8);

impl OnboardProfile {
    /// Every onboard profile, in profile order.
    pub const ALL: [Self; PROFILE_COUNT] = [Self(0), Self(1), Self(2)];

    /// Profile `number`, counting from 1, or `None` outside
    /// `1..=`[`PROFILE_COUNT`].
    #[must_use]
    pub fn from_number(number: u8) -> Option<Self> {
        let index = number.checked_sub(1)?;
        (usize::from(index) < PROFILE_COUNT).then_some(Self(index))
    }

    /// The profile's number, counting from 1.
    #[must_use]
    pub fn number(self) -> u8 {
        self.0 + 1
    }

    /// Feature-report ID that carries this profile.
    #[must_use]
    pub fn report_id(self) -> u8 {
        PROFILE_REPORT_IDS[usize::from(self.0)]
    }

    fn index(self) -> usize {
        usize::from(self.0)
    }
}

impl TryFrom<u8> for OnboardProfile {
    type Error = InvalidValue;

    fn try_from(number: u8) -> Result<Self, Self::Error> {
        Self::from_number(number).ok_or(InvalidValue::Profile(number))
    }
}

impl From<OnboardProfile> for u8 {
    fn from(profile: OnboardProfile) -> Self {
        profile.number()
    }
}

/// One of the four DPI stages of a profile, numbered from 1 like the mouse's
/// own software.
///
/// Serialized as its number, so a number outside `1..=`[`DPI_STAGE_COUNT`]
/// fails to deserialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(try_from = "u8", into = "u8")
)]
pub struct DpiStage(u8);

impl DpiStage {
    /// Every DPI stage, in stage order.
    pub const ALL: [Self; DPI_STAGE_COUNT] = [Self(0), Self(1), Self(2), Self(3)];

    /// Stage `number`, counting from 1, or `None` outside
    /// `1..=`[`DPI_STAGE_COUNT`].
    #[must_use]
    pub fn from_number(number: u8) -> Option<Self> {
        let index = number.checked_sub(1)?;
        (usize::from(index) < DPI_STAGE_COUNT).then_some(Self(index))
    }

    /// The stage's number, counting from 1.
    #[must_use]
    pub fn number(self) -> u8 {
        self.0 + 1
    }

    /// This stage's entry in `per_stage`, which holds one entry per stage in
    /// [`Self::ALL`] order.
    #[must_use]
    pub fn of<T>(self, per_stage: &[T; DPI_STAGE_COUNT]) -> &T {
        &per_stage[self.index()]
    }

    /// This stage's entry in `per_stage`, to change. See [`Self::of`].
    pub fn of_mut<T>(self, per_stage: &mut [T; DPI_STAGE_COUNT]) -> &mut T {
        &mut per_stage[self.index()]
    }

    fn index(self) -> usize {
        usize::from(self.0)
    }
}

impl TryFrom<u8> for DpiStage {
    type Error = InvalidValue;

    fn try_from(number: u8) -> Result<Self, Self::Error> {
        Self::from_number(number).ok_or(InvalidValue::Stage(number))
    }
}

impl From<DpiStage> for u8 {
    fn from(stage: DpiStage) -> Self {
        stage.number()
    }
}

/// A DPI the G600 can store in a stage: a multiple of [`Self::STEP`] from
/// [`Self::MIN`] to [`Self::MAX`].
///
/// Serialized as the DPI, so a value [`Self::new`] refuses fails to
/// deserialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(try_from = "u16", into = "u16")
)]
pub struct DpiValue(u8);

impl DpiValue {
    /// Lowest settable DPI.
    pub const MIN: u16 = 200;
    /// Highest settable DPI.
    pub const MAX: u16 = 8200;
    /// Granularity of a DPI setting.
    pub const STEP: u16 = DPI_PER_UNIT;

    /// `dpi`, or `None` when it is out of range or not a multiple of
    /// [`Self::STEP`].
    #[must_use]
    pub fn new(dpi: u16) -> Option<Self> {
        if !(Self::MIN..=Self::MAX).contains(&dpi) || !dpi.is_multiple_of(Self::STEP) {
            return None;
        }
        u8::try_from(dpi / Self::STEP).ok().map(Self)
    }

    /// The DPI.
    #[must_use]
    pub fn get(self) -> u16 {
        u16::from(self.0) * DPI_PER_UNIT
    }

    /// The settable DPI nearest to `dpi`: clamped to [`Self::MIN`] and
    /// [`Self::MAX`], and rounded to a multiple of [`Self::STEP`], halves up.
    #[must_use]
    pub fn nearest(dpi: u16) -> Self {
        let clamped = dpi.clamp(Self::MIN, Self::MAX);
        let units = (clamped + Self::STEP / 2) / Self::STEP;
        // A clamped DPI is at most `MAX_UNITS` steps, so it fits a byte.
        Self(u8::try_from(units).unwrap_or(Self::MAX_UNITS))
    }

    /// [`Self::MAX`] in units of [`Self::STEP`].
    const MAX_UNITS: u8 = 164;
}

impl TryFrom<u16> for DpiValue {
    type Error = InvalidValue;

    fn try_from(dpi: u16) -> Result<Self, Self::Error> {
        Self::new(dpi).ok_or(InvalidValue::Dpi(dpi))
    }
}

impl From<DpiValue> for u16 {
    fn from(dpi: DpiValue) -> Self {
        dpi.get()
    }
}

/// A number that names no onboard profile, DPI stage, or settable DPI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidValue {
    /// Not a profile number from 1 to [`PROFILE_COUNT`].
    Profile(u8),
    /// Not a stage number from 1 to [`DPI_STAGE_COUNT`].
    Stage(u8),
    /// Not a DPI [`DpiValue::new`] accepts.
    Dpi(u16),
}

impl core::fmt::Display for InvalidValue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Profile(number) => write!(f, "{number} is not a G600 onboard profile"),
            Self::Stage(number) => write!(f, "{number} is not a G600 DPI stage"),
            Self::Dpi(dpi) => write!(
                f,
                "{dpi} is not a G600 DPI: {} to {} in steps of {}",
                DpiValue::MIN,
                DpiValue::MAX,
                DpiValue::STEP
            ),
        }
    }
}

impl core::error::Error for InvalidValue {}

/// The profile and DPI stage the mouse is currently using.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ActiveSelection {
    /// The onboard profile in use.
    pub profile: OnboardProfile,
    /// The DPI stage in use within that profile.
    pub dpi_stage: DpiStage,
}

impl ActiveSelection {
    /// The stage to select once `settings` are stored in `profile`, so the
    /// mouse applies the new values: `Ok(None)` for a profile not in use
    /// when `after` is [`StageAfterStore::Keep`], otherwise the stage
    /// `after` names.
    ///
    /// [`StageAfterStore::Keep`] keeps the stage in use while it stays
    /// enabled and takes the new default stage when it does not, as
    /// libratbag does.
    ///
    /// # Errors
    ///
    /// [`StageAfterStoreError::NotInUse`] when `after` names a stage but
    /// `profile` is not the one in use, and
    /// [`StageAfterStoreError::StageDisabled`] when the named stage is
    /// disabled in `settings`. Either way nothing should be written.
    pub fn stage_after_store(
        self,
        profile: OnboardProfile,
        settings: &DpiSettings,
        after: StageAfterStore,
    ) -> Result<Option<DpiStage>, StageAfterStoreError> {
        match after {
            StageAfterStore::Keep if self.profile != profile => Ok(None),
            StageAfterStore::Keep => Ok(Some(if settings.is_enabled(self.dpi_stage) {
                self.dpi_stage
            } else {
                settings.default_stage
            })),
            StageAfterStore::Use(stage) if self.profile != profile => {
                Err(StageAfterStoreError::NotInUse {
                    profile,
                    active: self.profile,
                    stage,
                })
            }
            StageAfterStore::Use(stage) if !settings.is_enabled(stage) => {
                Err(StageAfterStoreError::StageDisabled { profile, stage })
            }
            StageAfterStore::Use(stage) => Ok(Some(stage)),
        }
    }
}

/// Which DPI stage the mouse uses once a profile is stored.
///
/// Only the profile in use has a stage the mouse is on, so only a write to
/// it may name one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum StageAfterStore {
    /// Stay on the stage slot in use while it stays enabled, and take the
    /// new default stage when it does not. A profile not in use is only
    /// stored.
    Keep,
    /// Switch to this stage of the profile in use once it is stored. A
    /// save may renumber the stages, so the slot in use can come to hold
    /// another DPI; the writer names where the stage it means landed.
    Use(DpiStage),
}

/// Why the stage [`StageAfterStore`] names cannot follow a profile write.
/// Either way nothing should reach the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageAfterStoreError {
    /// A stage was named for a profile the mouse is not using.
    NotInUse {
        /// The profile being stored.
        profile: OnboardProfile,
        /// The profile the mouse is using.
        active: OnboardProfile,
        /// The stage that was named.
        stage: DpiStage,
    },
    /// The named stage is disabled in the settings being stored.
    StageDisabled {
        /// The profile being stored, which is the one in use.
        profile: OnboardProfile,
        /// The stage that was named.
        stage: DpiStage,
    },
}

impl core::fmt::Display for StageAfterStoreError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotInUse {
                profile,
                active,
                stage,
            } => write!(
                f,
                "DPI stage {} was named for profile {}, but the mouse is using profile {}",
                stage.number(),
                profile.number(),
                active.number()
            ),
            Self::StageDisabled { profile, stage } => write!(
                f,
                "DPI stage {} is disabled in the settings for profile {}",
                stage.number(),
                profile.number()
            ),
        }
    }
}

impl core::error::Error for StageAfterStoreError {}

/// Decodes an active-profile report (`0xf0`), ID byte included.
///
/// Returns `None` for a wrong ID, a wrong length, or a profile index past the
/// three onboard profiles.
#[must_use]
pub fn decode_active(report: &[u8]) -> Option<ActiveSelection> {
    let Ok::<[u8; ACTIVE_REPORT_LEN], _>([ACTIVE_REPORT_ID, selection, ..]) = report.try_into()
    else {
        return None;
    };
    Some(ActiveSelection {
        profile: OnboardProfile::from_number((selection >> 4) + 1)?,
        dpi_stage: DpiStage((selection >> 1) & 0b11),
    })
}

/// Encodes the active-profile report (`0xf0`) that switches the active
/// profile's DPI stage to `stage`, ID byte included.
#[must_use]
pub fn encode_set_active_stage(stage: DpiStage) -> [u8; ACTIVE_REPORT_LEN] {
    [ACTIVE_REPORT_ID, SET_STAGE_FLAG | (stage.0 << 1), 0, 0]
}

/// The DPI settings stored in one onboard profile, as the mouse reports
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ProfileDpi {
    /// DPI of each stage, `None` for a disabled stage.
    pub stages: [Option<u16>; DPI_STAGE_COUNT],
    /// Stage the profile starts on, `None` when the report names none.
    pub default_stage: Option<DpiStage>,
    /// DPI while the DPI-shift button is held, `None` when disabled.
    pub shift: Option<u16>,
}

impl ProfileDpi {
    /// Whether `stage` holds a DPI, so the mouse can switch to it.
    #[must_use]
    pub fn is_enabled(&self, stage: DpiStage) -> bool {
        self.stages[stage.index()].is_some()
    }

    /// The DPI of `stage`, `None` when it is disabled.
    #[must_use]
    pub fn dpi(&self, stage: DpiStage) -> Option<u16> {
        self.stages[stage.index()]
    }

    /// Set the stages and the default stage to `settings`, keeping the DPI
    /// shift: what the profile reads back as once [`patch_profile_dpi`] has
    /// been stored.
    pub fn apply(&mut self, settings: &DpiSettings) {
        self.stages = settings.stages.map(|stage| stage.map(DpiValue::get));
        self.default_stage = Some(settings.default_stage);
    }
}

/// The G600's active selection and the DPI settings of its onboard
/// profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DpiState {
    /// The profile and DPI stage the mouse is using.
    pub active: ActiveSelection,
    /// Each onboard profile's DPI settings, in [`OnboardProfile::ALL`] order.
    pub profiles: [ProfileDpi; PROFILE_COUNT],
}

impl DpiState {
    /// The DPI settings of `profile`.
    #[must_use]
    pub fn profile(&self, profile: OnboardProfile) -> &ProfileDpi {
        &self.profiles[profile.index()]
    }

    /// The DPI settings of `profile`, to change.
    pub fn profile_mut(&mut self, profile: OnboardProfile) -> &mut ProfileDpi {
        &mut self.profiles[profile.index()]
    }

    /// What the mouse reads back as once `settings` are stored in `profile`
    /// and the stage [`ActiveSelection::stage_after_store`] names for
    /// `after` is selected.
    ///
    /// # Errors
    ///
    /// What [`ActiveSelection::stage_after_store`] refuses, leaving `self`
    /// unchanged.
    pub fn store(
        &mut self,
        profile: OnboardProfile,
        settings: &DpiSettings,
        after: StageAfterStore,
    ) -> Result<(), StageAfterStoreError> {
        let stage = self.active.stage_after_store(profile, settings, after)?;
        self.profile_mut(profile).apply(settings);
        if let Some(stage) = stage {
            self.active.dpi_stage = stage;
        }
        Ok(())
    }
}

/// Decodes the DPI fields of an onboard-profile report (`0xf3`–`0xf5`), ID byte
/// included.
///
/// Returns `None` when the length is not [`PROFILE_REPORT_LEN`] or the ID is not
/// an onboard profile's.
#[must_use]
pub fn decode_profile_dpi(report: &[u8]) -> Option<ProfileDpi> {
    if report.len() != PROFILE_REPORT_LEN || !PROFILE_REPORT_IDS.contains(&report[0]) {
        return None;
    }
    let dpi = |byte: u8| (byte != 0).then(|| u16::from(byte) * DPI_PER_UNIT);
    Some(ProfileDpi {
        stages: core::array::from_fn(|stage| dpi(report[STAGES_OFFSET + stage])),
        default_stage: DpiStage::from_number(report[DEFAULT_STAGE_OFFSET]),
        shift: dpi(report[SHIFT_DPI_OFFSET]),
    })
}

/// The DPI stages of an onboard profile and the stage it starts on: at least
/// one stage enabled, and the default stage among them.
///
/// Deserializing re-validates through [`Self::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(try_from = "UncheckedDpiSettings", into = "UncheckedDpiSettings")
)]
pub struct DpiSettings {
    stages: [Option<DpiValue>; DPI_STAGE_COUNT],
    default_stage: DpiStage,
}

/// The fields of [`DpiSettings`] before validation: its serialized form.
#[cfg(feature = "serde")]
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct UncheckedDpiSettings {
    stages: [Option<DpiValue>; DPI_STAGE_COUNT],
    default_stage: DpiStage,
}

#[cfg(feature = "serde")]
impl TryFrom<UncheckedDpiSettings> for DpiSettings {
    type Error = DpiSettingsError;

    fn try_from(unchecked: UncheckedDpiSettings) -> Result<Self, Self::Error> {
        Self::new(unchecked.stages, unchecked.default_stage)
    }
}

#[cfg(feature = "serde")]
impl From<DpiSettings> for UncheckedDpiSettings {
    fn from(settings: DpiSettings) -> Self {
        Self {
            stages: settings.stages,
            default_stage: settings.default_stage,
        }
    }
}

impl DpiSettings {
    /// Settings with each stage at its DPI, or disabled where `None`, starting
    /// on `default_stage`.
    ///
    /// # Errors
    ///
    /// [`DpiSettingsError::NoStageEnabled`] when every stage is disabled, and
    /// [`DpiSettingsError::DefaultStageDisabled`] when `default_stage` is.
    pub fn new(
        stages: [Option<DpiValue>; DPI_STAGE_COUNT],
        default_stage: DpiStage,
    ) -> Result<Self, DpiSettingsError> {
        if stages.iter().all(Option::is_none) {
            return Err(DpiSettingsError::NoStageEnabled);
        }
        if stages[default_stage.index()].is_none() {
            return Err(DpiSettingsError::DefaultStageDisabled(default_stage));
        }
        Ok(Self {
            stages,
            default_stage,
        })
    }

    /// The stage the profile starts on.
    #[must_use]
    pub fn default_stage(&self) -> DpiStage {
        self.default_stage
    }

    /// Whether `stage` holds a DPI.
    #[must_use]
    pub fn is_enabled(&self, stage: DpiStage) -> bool {
        self.stages[stage.index()].is_some()
    }

    /// The DPI of `stage`, `None` when it is disabled.
    #[must_use]
    pub fn dpi(&self, stage: DpiStage) -> Option<DpiValue> {
        self.stages[stage.index()]
    }
}

/// Why a set of DPI stages cannot be stored in a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpiSettingsError {
    /// Every stage is disabled, which leaves the mouse no DPI.
    NoStageEnabled,
    /// The stage the profile would start on is disabled.
    DefaultStageDisabled(DpiStage),
}

impl core::fmt::Display for DpiSettingsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoStageEnabled => f.write_str("at least one DPI stage must be enabled"),
            Self::DefaultStageDisabled(stage) => {
                write!(f, "default stage {} is disabled", stage.number())
            }
        }
    }
}

impl core::error::Error for DpiSettingsError {}

/// Returns the onboard-profile `report`, ID byte included, with its four
/// stage bytes and its default-stage byte set from `settings` and every other
/// byte, the DPI shift included, unchanged.
///
/// The report holds the whole profile in onboard flash and carries no
/// checksum, so writing it back sets every field; patch the report the
/// device just returned, never one assembled from decoded values.
#[must_use]
pub fn patch_profile_dpi(
    report: &[u8; PROFILE_REPORT_LEN],
    settings: &DpiSettings,
) -> [u8; PROFILE_REPORT_LEN] {
    let mut patched = *report;
    for (byte, stage) in patched[STAGES_OFFSET..STAGES_OFFSET + DPI_STAGE_COUNT]
        .iter_mut()
        .zip(settings.stages)
    {
        // A zero byte disables the stage.
        *byte = stage.map_or(0, |dpi| dpi.0);
    }
    patched[DEFAULT_STAGE_OFFSET] = settings.default_stage.number();
    patched
}

#[cfg(test)]
mod tests {
    use super::{
        ActiveSelection, DPI_STAGE_COUNT, DpiSettings, DpiSettingsError, DpiStage, DpiState,
        DpiValue, InvalidValue, OnboardProfile, PROFILE_REPORT_LEN, ProfileDpi, StageAfterStore,
        StageAfterStoreError, decode_active, decode_profile_dpi, encode_set_active_stage,
        matches_g600, patch_profile_dpi,
    };
    use crate::LOGITECH_VENDOR_ID;

    fn profile_report(
        id: u8,
        shift: u8,
        default: u8,
        stages: [u8; DPI_STAGE_COUNT],
    ) -> [u8; PROFILE_REPORT_LEN] {
        let mut report = [0; PROFILE_REPORT_LEN];
        report[0] = id;
        report[12] = shift;
        report[13] = default;
        report[14..18].copy_from_slice(&stages);
        report
    }

    fn stage(number: u8) -> DpiStage {
        DpiStage::from_number(number).expect("valid stage")
    }

    fn dpi(value: u16) -> DpiValue {
        DpiValue::new(value).expect("valid DPI")
    }

    #[test]
    fn active_report_splits_profile_and_stage() {
        // Third profile in the high nibble, fourth stage in bits 1-2.
        assert_eq!(
            decode_active(&[0xf0, 0x26, 0, 0]),
            Some(ActiveSelection {
                profile: OnboardProfile::from_number(3).expect("third profile"),
                dpi_stage: stage(4),
            })
        );
    }

    #[test]
    fn active_report_rejects_bad_id_length_and_profile() {
        assert_eq!(decode_active(&[0xf1, 0x00, 0, 0]), None);
        assert_eq!(decode_active(&[0xf0, 0x00, 0]), None);
        assert_eq!(decode_active(&[0xf0, 0x30, 0, 0]), None);
    }

    #[test]
    fn profile_report_scales_stages_and_flags_disabled_ones() {
        let report = profile_report(0xf4, 0x04, 2, [0x04, 0x10, 0x00, 0xa4]);
        let decoded = decode_profile_dpi(&report);
        assert_eq!(
            decoded,
            Some(ProfileDpi {
                stages: [Some(200), Some(800), None, Some(8200)],
                default_stage: Some(stage(2)),
                shift: Some(200),
            })
        );
        let decoded = decoded.expect("valid report");
        assert!(decoded.is_enabled(stage(4)));
        assert!(!decoded.is_enabled(stage(3)));
    }

    #[test]
    fn profile_report_without_default_or_shift_has_none() {
        let report = profile_report(0xf3, 0, 0, [0x10; DPI_STAGE_COUNT]);
        let decoded = decode_profile_dpi(&report).expect("valid report");
        assert_eq!(decoded.default_stage, None);
        assert_eq!(decoded.shift, None);
        // A default stage past the fourth is not a stage.
        let report = profile_report(0xf3, 0, 5, [0x10; DPI_STAGE_COUNT]);
        assert_eq!(
            decode_profile_dpi(&report).expect("valid").default_stage,
            None
        );
    }

    #[test]
    fn profile_report_rejects_bad_id_and_length() {
        assert_eq!(
            decode_profile_dpi(&profile_report(0xf6, 0, 1, [0; DPI_STAGE_COUNT])),
            None
        );
        let report = profile_report(0xf3, 0, 1, [0; DPI_STAGE_COUNT]);
        assert_eq!(decode_profile_dpi(&report[..PROFILE_REPORT_LEN - 1]), None);
    }

    #[test]
    fn dpi_values_are_bounded_multiples_of_fifty() {
        assert_eq!(DpiValue::new(200).map(DpiValue::get), Some(200));
        assert_eq!(DpiValue::new(8200).map(DpiValue::get), Some(8200));
        assert_eq!(DpiValue::new(150), None);
        assert_eq!(DpiValue::new(8250), None);
        assert_eq!(DpiValue::new(825), None);
        assert_eq!(DpiValue::new(0), None);
    }

    #[test]
    fn nearest_dpi_clamps_and_rounds_to_the_step() {
        assert_eq!(DpiValue::nearest(0).get(), DpiValue::MIN);
        assert_eq!(DpiValue::nearest(u16::MAX).get(), DpiValue::MAX);
        assert_eq!(DpiValue::nearest(1624).get(), 1600);
        assert_eq!(DpiValue::nearest(1625).get(), 1650);
        assert_eq!(DpiValue::nearest(8199).get(), 8200);
        assert_eq!(DpiValue::nearest(800), dpi(800));
        assert_eq!(
            DpiValue::MAX_UNITS,
            u8::try_from(DpiValue::MAX / DpiValue::STEP).expect("fits")
        );
    }

    #[test]
    fn stages_and_profiles_number_from_one() {
        assert_eq!(DpiStage::from_number(4).map(DpiStage::number), Some(4));
        assert_eq!(DpiStage::from_number(0), None);
        assert_eq!(DpiStage::from_number(5), None);
        assert_eq!(
            OnboardProfile::ALL.map(OnboardProfile::report_id),
            [0xf3, 0xf4, 0xf5]
        );
        assert_eq!(OnboardProfile::ALL.map(OnboardProfile::number), [1, 2, 3]);
        assert_eq!(OnboardProfile::from_number(3), Some(OnboardProfile::ALL[2]));
        assert_eq!(OnboardProfile::from_number(0), None);
        assert_eq!(OnboardProfile::from_number(4), None);
    }

    #[test]
    fn set_active_stage_report_flags_the_stage_in_bits_one_and_two() {
        assert_eq!(encode_set_active_stage(stage(1)), [0xf0, 0x40, 0, 0]);
        assert_eq!(encode_set_active_stage(stage(4)), [0xf0, 0x46, 0, 0]);
    }

    #[test]
    fn settings_need_an_enabled_default_stage() {
        assert_eq!(
            DpiSettings::new([None; DPI_STAGE_COUNT], stage(1)),
            Err(DpiSettingsError::NoStageEnabled)
        );
        assert_eq!(
            DpiSettings::new([Some(dpi(800)), None, Some(dpi(1600)), None], stage(2)),
            Err(DpiSettingsError::DefaultStageDisabled(stage(2)))
        );
        let settings = DpiSettings::new([Some(dpi(800)), None, Some(dpi(1600)), None], stage(3))
            .expect("valid");
        assert_eq!(settings.default_stage(), stage(3));
        assert!(settings.is_enabled(stage(1)));
        assert!(!settings.is_enabled(stage(4)));
    }

    /// A profile report with no zero byte, so a byte cleared by mistake shows.
    fn busy_report() -> [u8; PROFILE_REPORT_LEN] {
        let mut report: [u8; PROFILE_REPORT_LEN] =
            core::array::from_fn(|index| u8::try_from(index % 251 + 1).expect("below 256"));
        report[0] = 0xf4;
        report
    }

    #[test]
    fn patching_sets_every_stage_and_the_default_and_nothing_else() {
        let report = busy_report();
        let settings = DpiSettings::new([None, Some(dpi(3200)), None, Some(dpi(200))], stage(4))
            .expect("valid settings");

        let patched = patch_profile_dpi(&report, &settings);

        // Disabled stages are zero; the default byte is 1-based.
        assert_eq!(patched[13..18], [4, 0x00, 0x40, 0x00, 0x04]);
        for (index, (before, after)) in report.iter().zip(&patched).enumerate() {
            if !(13..18).contains(&index) {
                assert_eq!(before, after, "byte {index} changed");
            }
        }
        let decoded = decode_profile_dpi(&patched).expect("still a profile report");
        assert_eq!(decoded.stages, [None, Some(3200), None, Some(200)]);
        assert_eq!(decoded.default_stage, Some(stage(4)));
    }

    #[test]
    fn applying_settings_reads_like_the_stored_patch() {
        let report = busy_report();
        let settings = DpiSettings::new([Some(dpi(400)), None, Some(dpi(8200)), None], stage(3))
            .expect("valid settings");
        let mut applied = decode_profile_dpi(&report).expect("a profile report");

        applied.apply(&settings);

        assert_eq!(
            Some(applied),
            decode_profile_dpi(&patch_profile_dpi(&report, &settings))
        );
        assert_eq!(applied.dpi(stage(3)), Some(8200));
        assert_eq!(settings.dpi(stage(2)), None);
    }

    #[test]
    fn the_stage_after_a_store_follows_the_profile_in_use_and_the_named_stage() {
        use StageAfterStore::{Keep, Use};

        let profile = |number| OnboardProfile::from_number(number).expect("a profile");
        let active = ActiveSelection {
            profile: profile(2),
            dpi_stage: stage(3),
        };
        let keeps_stage_3 =
            DpiSettings::new([Some(dpi(400)), None, Some(dpi(800)), None], stage(1))
                .expect("valid settings");
        let drops_stage_3 =
            DpiSettings::new([Some(dpi(400)), Some(dpi(800)), None, None], stage(2))
                .expect("valid settings");

        // (profile stored, settings, request) -> verdict
        let table = [
            (profile(1), &drops_stage_3, Keep, Ok(None)),
            (profile(2), &keeps_stage_3, Keep, Ok(Some(stage(3)))),
            (profile(2), &drops_stage_3, Keep, Ok(Some(stage(2)))),
            // A named stage wins over the slot in use and the default.
            (
                profile(2),
                &keeps_stage_3,
                Use(stage(1)),
                Ok(Some(stage(1))),
            ),
            (
                profile(2),
                &drops_stage_3,
                Use(stage(1)),
                Ok(Some(stage(1))),
            ),
            (
                profile(2),
                &drops_stage_3,
                Use(stage(3)),
                Err(StageAfterStoreError::StageDisabled {
                    profile: profile(2),
                    stage: stage(3),
                }),
            ),
            (
                profile(1),
                &drops_stage_3,
                Use(stage(1)),
                Err(StageAfterStoreError::NotInUse {
                    profile: profile(1),
                    active: profile(2),
                    stage: stage(1),
                }),
            ),
        ];
        for (stored, settings, after, verdict) in table {
            assert_eq!(
                active.stage_after_store(stored, settings, after),
                verdict,
                "storing profile {} with {after:?}",
                stored.number()
            );
        }
    }

    #[test]
    fn storing_applies_the_settings_and_the_stage_after_or_refuses_whole() {
        let profile = |number| OnboardProfile::from_number(number).expect("a profile");
        let drops_stage_3 =
            DpiSettings::new([Some(dpi(400)), Some(dpi(800)), None, None], stage(2))
                .expect("valid settings");
        let read = ProfileDpi {
            stages: [Some(1600), Some(3200), Some(6400), None],
            default_stage: Some(stage(1)),
            shift: Some(400),
        };
        let mut state = DpiState {
            active: ActiveSelection {
                profile: profile(2),
                dpi_stage: stage(3),
            },
            profiles: [read; 3],
        };

        state
            .store(profile(2), &drops_stage_3, StageAfterStore::Keep)
            .expect("stored");
        assert_eq!(state.active.dpi_stage, stage(2));
        assert_eq!(
            state.profile(profile(2)).stages,
            [Some(400), Some(800), None, None]
        );
        assert_eq!(state.profile(profile(2)).shift, Some(400));

        state
            .store(profile(2), &drops_stage_3, StageAfterStore::Use(stage(1)))
            .expect("stored");
        assert_eq!(state.active.dpi_stage, stage(1), "the named stage");

        let before = state;
        state
            .store(profile(3), &drops_stage_3, StageAfterStore::Use(stage(1)))
            .expect_err("profile 3 is not in use");
        state
            .store(profile(2), &drops_stage_3, StageAfterStore::Use(stage(4)))
            .expect_err("stage 4 is disabled");
        assert_eq!(state, before, "a refused store changes nothing");

        state
            .store(profile(3), &drops_stage_3, StageAfterStore::Keep)
            .expect("stored");
        assert_eq!(state.active.dpi_stage, stage(1), "profile 3 is not in use");
        assert_eq!(*state.profile(profile(1)), read);
    }

    #[test]
    fn stages_index_per_stage_arrays_in_stage_order() {
        let mut per_stage = [10, 20, 30, 40];
        assert_eq!(*stage(3).of(&per_stage), 30);
        *stage(1).of_mut(&mut per_stage) = 11;
        assert_eq!(per_stage, [11, 20, 30, 40]);
    }

    #[test]
    fn conversions_take_numbers_from_one_and_dpi_in_steps() {
        assert_eq!(OnboardProfile::try_from(2).map(u8::from), Ok(2));
        assert_eq!(OnboardProfile::try_from(4), Err(InvalidValue::Profile(4)));
        assert_eq!(DpiStage::try_from(4).map(u8::from), Ok(4));
        assert_eq!(DpiStage::try_from(0), Err(InvalidValue::Stage(0)));
        assert_eq!(DpiValue::try_from(1650).map(u16::from), Ok(1650));
        assert_eq!(DpiValue::try_from(1625), Err(InvalidValue::Dpi(1625)));
        assert_eq!(DpiStage::ALL.map(DpiStage::number), [1, 2, 3, 4]);
    }

    #[test]
    fn vendor_collection_matches() {
        assert!(matches_g600(LOGITECH_VENDOR_ID, 0xc24a, 0xff80, 0x0080));
    }

    #[test]
    fn sibling_collections_and_other_vendors_do_not_match() {
        // Mouse interface and the G-key keyboard collection of the same device.
        assert!(!matches_g600(LOGITECH_VENDOR_ID, 0xc24a, 0x0001, 0x0002));
        assert!(!matches_g600(LOGITECH_VENDOR_ID, 0xc24a, 0x0001, 0x0006));
        assert!(!matches_g600(0xffff, 0xc24a, 0xff80, 0x0080));
    }
}
