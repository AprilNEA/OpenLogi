//! The edit a G600 profile is under before it is saved to the mouse.
//!
//! A profile is edited as what it means to the person using it: up to
//! [`DPI_STAGE_COUNT`] distinct DPIs, lowest first, and which of them the
//! profile starts on. Which stage slot holds which DPI is the mouse's
//! business, so a save writes the DPIs into the first slots in ascending
//! order and turns the rest off; the DPI button then steps up through them.
//!
//! A save can give the stage slot the mouse is using another DPI. A draft
//! therefore remembers which DPI each stage was read with, so the stage the
//! mouse is using can be followed through the edit and named in the save
//! that keeps the mouse on it.

use openlogi_core::hid::g600::{
    DPI_STAGE_COUNT, DpiSettings, DpiSettingsError, DpiStage, DpiState, DpiValue, OnboardProfile,
    ProfileDpi, StageAfterStore,
};

use crate::ui::log_track::Nudge;

/// A G600 profile's stages and its default stage, as edited.
///
/// The stages are distinct and ascending, and there is at least one unless
/// the profile was read with none; the default is one of them.
#[derive(Debug, Clone)]
pub(crate) struct G600Draft {
    stages: Vec<DpiValue>,
    /// The DPI each stage was read with, beside it in [`Self::stages`];
    /// `None` for a stage added since.
    read_as: Vec<Option<DpiValue>>,
    default: usize,
}

impl G600Draft {
    /// The edit that starts from `profile` as the mouse reports it: its
    /// enabled stages in ascending order, a repeated DPI once.
    ///
    /// A stored DPI the mouse cannot be set to, such as one above 8200,
    /// starts at the nearest one it can. A profile that names no default
    /// stage, or an off one, starts on its first enabled stage.
    pub(crate) fn from_profile(profile: &ProfileDpi) -> Self {
        let mut stages: Vec<DpiValue> = profile
            .stages
            .iter()
            .flatten()
            .map(|dpi| DpiValue::nearest(*dpi))
            .collect();
        stages.sort_unstable();
        stages.dedup();
        let default_dpi = profile
            .default_stage
            .and_then(|stage| profile.dpi(stage))
            .or_else(|| profile.stages.iter().flatten().next().copied())
            .map(DpiValue::nearest);
        let default = default_dpi
            .and_then(|dpi| stages.iter().position(|stage| *stage == dpi))
            .unwrap_or(0);
        Self {
            read_as: stages.iter().copied().map(Some).collect(),
            stages,
            default,
        }
    }

    /// The stages, lowest first.
    pub(crate) fn stages(&self) -> &[DpiValue] {
        &self.stages
    }

    /// Where the default stage sits in [`Self::stages`].
    pub(crate) fn default_index(&self) -> usize {
        self.default
    }

    /// Whether the mouse has no room for another stage.
    pub(crate) fn is_full(&self) -> bool {
        self.stages.len() >= DPI_STAGE_COUNT
    }

    /// Add a stage at `dpi`, and where it landed. Refused when the profile
    /// is full or already has a stage at `dpi`.
    pub(crate) fn add(&mut self, dpi: DpiValue) -> Option<usize> {
        if self.is_full() {
            return None;
        }
        let Err(index) = self.stages.binary_search(&dpi) else {
            return None;
        };
        self.stages.insert(index, dpi);
        self.read_as.insert(index, None);
        if self.stages.len() > 1 && index <= self.default {
            self.default += 1;
        }
        Some(index)
    }

    /// Move stage `index` to `dpi`, and the DPI it landed on.
    ///
    /// A stage never reaches or passes its neighbours: it stops one
    /// [`DpiValue::STEP`] short of them, so the stages stay distinct and in
    /// order and the stage under the pointer stays the one being moved.
    /// There is no stage `index` when a drag outlives the draft it started
    /// on, for one: the move is refused.
    pub(crate) fn move_stage(&mut self, index: usize, dpi: DpiValue) -> Option<DpiValue> {
        self.stages.get(index)?;
        let floor = match index.checked_sub(1) {
            Some(lower) => self.stages[lower].get() + DpiValue::STEP,
            None => DpiValue::MIN,
        };
        let ceiling = match self.stages.get(index + 1) {
            Some(upper) => upper.get() - DpiValue::STEP,
            None => DpiValue::MAX,
        };
        let moved = DpiValue::nearest(dpi.get().clamp(floor, ceiling));
        self.stages[index] = moved;
        Some(moved)
    }

    /// Move stage `index` one [`DpiValue::STEP`] the way of `nudge`, within
    /// [`Self::move_stage`]'s bounds.
    pub(crate) fn nudge(&mut self, index: usize, nudge: Nudge) -> Option<DpiValue> {
        let current = self.stages.get(index)?.get();
        let target = match nudge {
            Nudge::Higher => current.saturating_add(DpiValue::STEP),
            Nudge::Lower => current.saturating_sub(DpiValue::STEP),
        };
        self.move_stage(index, DpiValue::nearest(target))
    }

    /// Remove stage `index`, and where the stage nearest it now sits.
    /// Refused for the last stage, which the mouse needs to start on.
    ///
    /// Removing the default stage makes that nearest stage the default:
    /// nearest by ratio, as the track shows DPI, and the lower one when the
    /// two neighbours are as near.
    pub(crate) fn remove(&mut self, index: usize) -> Option<usize> {
        if self.stages.len() <= 1 || index >= self.stages.len() {
            return None;
        }
        let removed = u32::from(self.stages.remove(index).get());
        self.read_as.remove(index);
        let lower = index.checked_sub(1);
        let upper = (index < self.stages.len()).then_some(index);
        let nearest = match (lower, upper) {
            (Some(lower), Some(upper)) => {
                let below = u32::from(self.stages[lower].get());
                let above = u32::from(self.stages[upper].get());
                // removed / below <= above / removed, without dividing.
                if removed * removed <= below * above {
                    lower
                } else {
                    upper
                }
            }
            (Some(only), None) | (None, Some(only)) => only,
            (None, None) => return None,
        };
        if self.default == index {
            self.default = nearest;
        } else if self.default > index {
            self.default -= 1;
        }
        Some(nearest)
    }

    /// Make stage `index` the one the profile starts on.
    pub(crate) fn set_default(&mut self, index: usize) {
        if index < self.stages.len() {
            self.default = index;
        }
    }

    /// What the mouse would store: the stages in ascending order from the
    /// first slot, the rest off, and the default stage's slot. A profile
    /// with no stage cannot be stored.
    pub(crate) fn settings(&self) -> Result<DpiSettings, DpiSettingsError> {
        let slots = std::array::from_fn(|slot| self.stages.get(slot).copied());
        let default = DpiStage::ALL
            .get(self.default)
            .copied()
            .unwrap_or(DpiStage::ALL[0]);
        DpiSettings::new(slots, default)
    }

    /// Whether this edit holds the stages and default `profile` opens with,
    /// so there is nothing to save. Where the stages came from is no part
    /// of what is stored.
    pub(crate) fn matches(&self, profile: &ProfileDpi) -> bool {
        let read = Self::from_profile(profile);
        self.stages == read.stages && self.default == read.default
    }

    /// Where the stage that was read at `dpi` sits now, moved or not; `None`
    /// once it is removed.
    fn read_at(&self, dpi: DpiValue) -> Option<usize> {
        self.read_as.iter().position(|read| *read == Some(dpi))
    }

    /// Where the stage `mouse` is using now sits on the track, while this
    /// draft of `profile` still holds it where it was read: never for a
    /// profile not in use, nor for a stage dragged away or removed, even
    /// when another stage has taken its DPI.
    pub(crate) fn in_use(&self, profile: OnboardProfile, mouse: &DpiState) -> Option<usize> {
        let dpi = in_use_dpi(profile, mouse)?;
        self.read_at(dpi).filter(|index| self.stages[*index] == dpi)
    }

    /// The stage the mouse uses once this draft of `profile` is saved.
    ///
    /// A save renumbers the stages lowest first, so the slot the mouse is
    /// using may then hold another DPI. The mouse follows the stage it is
    /// using instead: to wherever that stage lands, at the DPI it was
    /// dragged to, and to the default stage when it was removed. A profile
    /// not in use has no stage the mouse is on, so its save names none.
    pub(crate) fn stage_after_save(
        &self,
        profile: OnboardProfile,
        mouse: &DpiState,
    ) -> StageAfterStore {
        if mouse.active.profile != profile {
            return StageAfterStore::Keep;
        }
        let index = in_use_dpi(profile, mouse)
            .and_then(|dpi| self.read_at(dpi))
            .unwrap_or(self.default);
        // `index` is one of the draft's stages, which a save stores in the
        // first slots, so the stage is enabled in what is stored.
        DpiStage::ALL
            .get(index)
            .copied()
            .map_or(StageAfterStore::Keep, StageAfterStore::Use)
    }
}

/// The DPI of the stage `mouse` is using, when it is using `profile`.
fn in_use_dpi(profile: OnboardProfile, mouse: &DpiState) -> Option<DpiValue> {
    if mouse.active.profile != profile {
        return None;
    }
    mouse
        .profile(profile)
        .dpi(mouse.active.dpi_stage)
        .map(DpiValue::nearest)
}

/// Whether saving `profile`, even unedited, would store its stages in
/// other slots than it has them now: they are out of order, have a gap, or
/// repeat a DPI.
pub(crate) fn save_reorders(profile: &ProfileDpi) -> bool {
    let Ok(settings) = G600Draft::from_profile(profile).settings() else {
        return false;
    };
    DpiStage::ALL
        .into_iter()
        .any(|stage| profile.dpi(stage).map(DpiValue::nearest) != settings.dpi(stage))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::hid::g600::ActiveSelection;

    fn dpi(value: u16) -> DpiValue {
        DpiValue::new(value).expect("a G600 DPI")
    }

    fn profile(stages: [Option<u16>; 4], default: Option<u8>) -> ProfileDpi {
        ProfileDpi {
            stages,
            default_stage: default.and_then(DpiStage::from_number),
            shift: Some(400),
        }
    }

    fn values(draft: &G600Draft) -> Vec<u16> {
        draft.stages().iter().map(|stage| stage.get()).collect()
    }

    /// The stages and default stage a save would store.
    fn stored(draft: &G600Draft) -> ([Option<u16>; 4], u8) {
        let settings = draft.settings().expect("a storable draft");
        (
            DpiStage::ALL.map(|stage| settings.dpi(stage).map(DpiValue::get)),
            settings.default_stage().number(),
        )
    }

    #[test]
    fn an_unedited_draft_matches_its_profile_and_saves_it_as_read() {
        let read = profile([Some(800), Some(1600), Some(3200), None], Some(3));
        let draft = G600Draft::from_profile(&read);

        assert!(draft.matches(&read));
        let settings = draft.settings().expect("the profile is valid");
        let mut saved = read;
        saved.apply(&settings);
        assert_eq!(saved, read);
        assert!(!save_reorders(&read));
    }

    #[test]
    fn stages_out_of_order_or_with_gaps_open_ascending_and_save_from_the_first_slot() {
        // Off, off, off, 3500: one stage in the last slot.
        let gap = profile([None, None, None, Some(3500)], Some(4));
        let draft = G600Draft::from_profile(&gap);
        assert_eq!(values(&draft), [3500]);
        assert_eq!(stored(&draft), ([Some(3500), None, None, None], 1));
        assert!(save_reorders(&gap));

        // 3200 before 800, starting on 800: the default follows its DPI.
        let reversed = profile([Some(3200), Some(800), None, None], Some(2));
        let draft = G600Draft::from_profile(&reversed);
        assert_eq!(values(&draft), [800, 3200]);
        assert_eq!(draft.default_index(), 0);
        assert_eq!(stored(&draft), ([Some(800), Some(3200), None, None], 1));
        assert!(save_reorders(&reversed));
        assert!(
            draft.matches(&reversed),
            "reordering alone is no edit to save"
        );
    }

    #[test]
    fn a_repeated_dpi_opens_as_one_stage() {
        let read = profile([Some(800), Some(800), Some(1600), None], Some(2));
        let draft = G600Draft::from_profile(&read);

        assert_eq!(values(&draft), [800, 1600]);
        assert_eq!(draft.default_index(), 0);
        assert!(save_reorders(&read));
    }

    #[test]
    fn a_profile_without_a_usable_default_starts_on_its_first_enabled_stage() {
        // The mouse names no default, or an off one.
        for default in [None, Some(1)] {
            let read = profile([None, Some(3200), Some(1200), None], default);
            let draft = G600Draft::from_profile(&read);
            assert_eq!(draft.stages()[draft.default_index()].get(), 3200);
        }
    }

    #[test]
    fn a_profile_with_no_stage_opens_empty_and_cannot_be_saved_until_one_is_added() {
        let mut draft = G600Draft::from_profile(&profile([None; 4], None));

        assert!(draft.stages().is_empty());
        assert_eq!(draft.settings(), Err(DpiSettingsError::NoStageEnabled));
        assert_eq!(draft.add(dpi(1600)), Some(0));
        assert_eq!(stored(&draft), ([Some(1600), None, None, None], 1));
    }

    #[test]
    fn a_stored_dpi_the_mouse_cannot_be_set_to_starts_at_the_nearest_one() {
        let read = profile([Some(12750), None, None, None], Some(1));
        let draft = G600Draft::from_profile(&read);

        assert_eq!(values(&draft), [DpiValue::MAX]);
        assert_eq!(stored(&draft).0[0], Some(DpiValue::MAX));
    }

    #[test]
    fn an_added_stage_takes_its_place_in_order_and_the_default_follows_its_dot() {
        let mut draft =
            G600Draft::from_profile(&profile([Some(800), Some(3200), None, None], Some(2)));

        assert_eq!(draft.add(dpi(400)), Some(0));
        assert_eq!(draft.add(dpi(1600)), Some(2));
        assert_eq!(values(&draft), [400, 800, 1600, 3200]);
        assert_eq!(draft.stages()[draft.default_index()].get(), 3200);
        assert_eq!(
            stored(&draft),
            ([Some(400), Some(800), Some(1600), Some(3200)], 4)
        );
    }

    #[test]
    fn adding_is_refused_when_full_or_the_dpi_is_taken() {
        let mut draft =
            G600Draft::from_profile(&profile([Some(400), Some(800), Some(1600), None], Some(1)));

        assert_eq!(draft.add(dpi(800)), None, "a second stage at 800");
        assert!(!draft.is_full());
        assert_eq!(draft.add(dpi(6400)), Some(3));
        assert!(draft.is_full());
        assert_eq!(draft.add(dpi(3200)), None, "a fifth stage");
        assert_eq!(values(&draft), [400, 800, 1600, 6400]);
    }

    #[test]
    fn a_moved_stage_stops_one_step_short_of_its_neighbours() {
        let mut draft =
            G600Draft::from_profile(&profile([Some(400), Some(800), Some(1600), None], Some(1)));

        assert_eq!(draft.move_stage(1, dpi(1200)), Some(dpi(1200)));
        assert_eq!(draft.move_stage(1, dpi(1600)), Some(dpi(1550)));
        assert_eq!(draft.move_stage(1, dpi(5000)), Some(dpi(1550)));
        assert_eq!(draft.move_stage(1, dpi(200)), Some(dpi(450)));
        assert_eq!(draft.move_stage(0, dpi(200)), Some(dpi(200)));
        assert_eq!(draft.move_stage(2, dpi(8200)), Some(dpi(8200)));
        assert_eq!(draft.move_stage(3, dpi(800)), None, "no fourth stage");
        assert_eq!(values(&draft), [200, 450, 8200]);
    }

    #[test]
    fn a_move_for_a_stage_the_draft_no_longer_has_is_refused() {
        // A drag that started on a four-stage draft, still moving after the
        // draft was swapped for one with a single stage.
        let mut draft = G600Draft::from_profile(&profile([Some(800), None, None, None], Some(1)));

        for index in [1, 2, 3, usize::MAX] {
            assert_eq!(draft.move_stage(index, dpi(1600)), None, "stage {index}");
        }
        assert_eq!(values(&draft), [800]);
    }

    #[test]
    fn a_nudge_moves_one_step_and_stops_at_the_range_and_its_neighbours() {
        let mut draft =
            G600Draft::from_profile(&profile([Some(200), Some(250), Some(8200), None], Some(1)));

        assert_eq!(
            draft.nudge(0, Nudge::Lower),
            Some(dpi(200)),
            "already the lowest"
        );
        assert_eq!(
            draft.nudge(0, Nudge::Higher),
            Some(dpi(200)),
            "250 is next to it"
        );
        assert_eq!(draft.nudge(1, Nudge::Higher), Some(dpi(300)));
        assert_eq!(
            draft.nudge(2, Nudge::Higher),
            Some(dpi(8200)),
            "already the highest"
        );
        assert_eq!(draft.nudge(2, Nudge::Lower), Some(dpi(8150)));
    }

    #[test]
    fn removing_the_default_stage_moves_the_default_to_the_nearest_one() {
        // 1600 sits a ratio of 2 from 800 and of 2 from 3200: the lower wins.
        let read = profile([Some(800), Some(1600), Some(3200), None], Some(2));
        let mut draft = G600Draft::from_profile(&read);
        assert_eq!(draft.remove(1), Some(0));
        assert_eq!(draft.stages()[draft.default_index()].get(), 800);

        // 1000 is nearer 1600 (×1.6) than 400 (×2.5).
        let read = profile([Some(400), Some(1000), Some(1600), None], Some(2));
        let mut draft = G600Draft::from_profile(&read);
        assert_eq!(draft.remove(1), Some(1));
        assert_eq!(draft.stages()[draft.default_index()].get(), 1600);

        // The highest stage has only a lower neighbour.
        let read = profile([Some(400), Some(1000), None, None], Some(2));
        let mut draft = G600Draft::from_profile(&read);
        assert_eq!(draft.remove(1), Some(0));
        assert_eq!(stored(&draft), ([Some(400), None, None, None], 1));
    }

    #[test]
    fn removing_another_stage_keeps_the_default_on_its_dot() {
        let read = profile([Some(400), Some(800), Some(3200), None], Some(3));
        let mut draft = G600Draft::from_profile(&read);

        draft.remove(0);

        assert_eq!(draft.stages()[draft.default_index()].get(), 3200);
        assert_eq!(stored(&draft), ([Some(800), Some(3200), None, None], 2));
    }

    #[test]
    fn the_last_stage_cannot_be_removed() {
        let mut draft = G600Draft::from_profile(&profile([Some(800), None, None, None], Some(1)));

        assert_eq!(draft.remove(0), None);
        assert_eq!(draft.remove(3), None);
        assert_eq!(values(&draft), [800]);
    }

    /// `profile` holding `stages` is in use, on `stage`; the other profiles
    /// hold 400 alone.
    fn using(number: u8, stages: [Option<u16>; 4], stage: u8) -> (OnboardProfile, DpiState) {
        let in_use = OnboardProfile::from_number(number).expect("a profile");
        let mut profiles = [profile([Some(400), None, None, None], Some(1)); 3];
        profiles[usize::from(number - 1)] = profile(stages, Some(1));
        let mouse = DpiState {
            active: ActiveSelection {
                profile: in_use,
                dpi_stage: DpiStage::from_number(stage).expect("a stage"),
            },
            profiles,
        };
        (in_use, mouse)
    }

    fn draft_of(profile: OnboardProfile, mouse: &DpiState) -> G600Draft {
        G600Draft::from_profile(mouse.profile(profile))
    }

    #[test]
    fn the_stage_in_use_is_marked_only_on_its_profile_and_until_it_moves() {
        let (in_use, mouse) = using(1, [Some(800), Some(1600), Some(3200), None], 2);
        let other = OnboardProfile::ALL[1];
        let mut draft = draft_of(in_use, &mouse);

        assert_eq!(draft.in_use(in_use, &mouse), Some(1));
        assert_eq!(
            draft_of(other, &mouse).in_use(other, &mouse),
            None,
            "profile 2 is not in use"
        );
        // A stage added below moves the dot in use along the track.
        draft.add(dpi(400));
        assert_eq!(draft.in_use(in_use, &mouse), Some(2));

        // Dragged away, it is no longer the stage the mouse is using...
        draft.move_stage(2, dpi(2000));
        assert_eq!(draft.in_use(in_use, &mouse), None);
        // ...and another stage dragged onto 1600 does not become it.
        draft.move_stage(1, dpi(1600));
        assert_eq!(values(&draft), [400, 1600, 2000, 3200]);
        assert_eq!(draft.in_use(in_use, &mouse), None);
        // Back where it was read, it is again.
        draft.move_stage(1, dpi(800));
        draft.move_stage(2, dpi(1600));
        assert_eq!(draft.in_use(in_use, &mouse), Some(2));
        draft.remove(2);
        assert_eq!(draft.in_use(in_use, &mouse), None);
    }

    /// The stage a save of `draft` names, checked against what it stores:
    /// a named stage is always enabled there.
    fn named(draft: &G600Draft, profile: OnboardProfile, mouse: &DpiState) -> Option<u16> {
        let settings = draft.settings().expect("storable");
        match draft.stage_after_save(profile, mouse) {
            StageAfterStore::Keep => None,
            StageAfterStore::Use(stage) => Some(
                settings
                    .dpi(stage)
                    .expect("the named stage is enabled")
                    .get(),
            ),
        }
    }

    #[test]
    fn a_save_of_the_profile_in_use_names_where_its_stage_lands() {
        // On stage 2, 1600. Adding 400 below puts 1600 in slot 3.
        let (in_use, mouse) = using(1, [Some(800), Some(1600), Some(3200), None], 2);
        let mut draft = draft_of(in_use, &mouse);
        draft.add(dpi(400));
        assert_eq!(
            draft.stage_after_save(in_use, &mouse),
            StageAfterStore::Use(DpiStage::from_number(3).expect("a stage"))
        );

        // A stage added above leaves 1600 in slot 2, still named.
        let mut draft = draft_of(in_use, &mouse);
        draft.add(dpi(6400));
        assert_eq!(
            draft.stage_after_save(in_use, &mouse),
            StageAfterStore::Use(DpiStage::from_number(2).expect("a stage"))
        );

        // The stage in use, dragged, keeps the mouse with it.
        let mut draft = draft_of(in_use, &mouse);
        draft.add(dpi(400));
        draft.move_stage(2, dpi(2000));
        assert_eq!(named(&draft, in_use, &mouse), Some(2000));

        // A profile not in use names no stage.
        let other = OnboardProfile::ALL[1];
        let mut draft = draft_of(other, &mouse);
        draft.add(dpi(200));
        assert_eq!(draft.stage_after_save(other, &mouse), StageAfterStore::Keep);
    }

    #[test]
    fn a_stored_profile_out_of_order_keeps_the_mouse_on_its_dpi_once_saved() {
        // 3200 in slot 1, in use; saved, it moves to slot 2.
        let (in_use, mouse) = using(1, [Some(3200), Some(800), None, None], 1);
        let mut draft = draft_of(in_use, &mouse);
        draft.set_default(1);

        assert_eq!(
            draft.stage_after_save(in_use, &mouse),
            StageAfterStore::Use(DpiStage::from_number(2).expect("a stage"))
        );
        assert_eq!(named(&draft, in_use, &mouse), Some(3200));
    }

    #[test]
    fn removing_the_stage_in_use_names_the_default_stage() {
        let (in_use, mouse) = using(1, [Some(800), Some(1600), Some(3200), None], 2);
        let mut draft = draft_of(in_use, &mouse);
        draft.remove(1);
        draft.set_default(1);
        assert_eq!(named(&draft, in_use, &mouse), Some(3200), "the default");
        draft.set_default(0);
        assert_eq!(named(&draft, in_use, &mouse), Some(800), "the default");
    }

    #[test]
    fn edits_to_dpi_and_default_stage_show_as_changes() {
        let read = profile([Some(400), Some(800), None, None], Some(1));
        let mut draft = G600Draft::from_profile(&read);

        draft.move_stage(0, dpi(450));
        assert!(!draft.matches(&read));
        draft.move_stage(0, dpi(400));
        assert!(draft.matches(&read));
        draft.set_default(1);
        assert!(!draft.matches(&read));
        assert_eq!(stored(&draft), ([Some(400), Some(800), None, None], 2));
        draft.set_default(7);
        assert_eq!(draft.default_index(), 1, "no stage 8 to start on");
    }
}
