//! DPI-cycle state shared with background action dispatch.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use openlogi_hid::{DeviceRoute, Dpi, DpiCapabilities};

/// Per-device DPI-cycle states plus the GUI's current selection.
///
/// HID++ capture dispatch resolves against the device an event arrived on; the
/// OS hook cannot attribute an event to a device, so it dispatches against the
/// selection — the same behavior the runtime had when this was a single state.
#[derive(Debug, Clone, Default)]
pub struct DpiCycles {
    /// Config key of the GUI-selected device (the OS hook's dispatch target).
    pub selected: Option<String>,
    /// One cycle state per online device, keyed by config key.
    pub by_key: HashMap<String, DpiCycleState>,
}

impl DpiCycles {
    /// The state for `key`, falling back to the selected device when `key` is
    /// `None` (the OS hook path).
    pub fn state_for(&mut self, key: Option<&str>) -> Option<&mut DpiCycleState> {
        let key = key.or(self.selected.as_deref())?;
        self.by_key.get_mut(key)
    }

    /// The config key, DPI and write target of a held DPI shift on `key`
    /// (same fallback as [`Self::state_for`]).
    #[must_use]
    pub fn target_for_shift(&self, key: Option<&str>) -> Option<(&str, Dpi, DeviceRoute)> {
        let key = key.or(self.selected.as_deref())?;
        let (key, state) = self.by_key.get_key_value(key)?;
        Some((key, state.lowest()?, state.target.clone()?))
    }

    /// The DPI config or an action last set on `key`'s sensor, when known.
    #[must_use]
    pub fn current(&self, key: &str) -> Option<Dpi> {
        self.by_key.get(key)?.current
    }

    /// `key`'s mouse was set back to its saved `dpi`; see
    /// [`DpiCycleState::reapplied`].
    pub fn reapplied(&mut self, key: &str, dpi: Dpi) {
        if let Some(state) = self.by_key.get_mut(key) {
            state.reapplied(dpi);
        }
    }

    /// `key`'s mouse reconnected at its onboard DPI, so a reading taken
    /// before is stale.
    pub(crate) fn reconnected(&mut self, key: &str) {
        if let Some(state) = self.by_key.get_mut(key) {
            state.before_shift = DpiBeforeShift::default();
        }
    }

    /// [`Self::reapplied`] for every mouse at `route`: the settings app
    /// writes by route, not by key.
    pub(crate) fn reapplied_at(&mut self, route: &DeviceRoute, dpi: Dpi) {
        for state in self.by_key.values_mut() {
            if state.target.as_ref() == Some(route) {
                state.reapplied(dpi);
            }
        }
    }

    /// The mouse at `route` rejected `dpi`. Each mouse there still holding
    /// it forgets it, so a DPI shift reads the mouse instead.
    pub(crate) fn rejected_at(&mut self, route: &DeviceRoute, dpi: Dpi) {
        for state in self.by_key.values_mut() {
            if state.target.as_ref() == Some(route) && state.current == Some(state.normalize(dpi)) {
                state.current = None;
            }
        }
    }

    /// The write target for `key` (same fallback as [`Self::state_for`])
    /// without a mutable borrow — for dispatch that only needs the route, like
    /// the SmartShift toggle.
    #[must_use]
    pub fn target_for(&self, key: Option<&str>) -> Option<DeviceRoute> {
        let key = key.or(self.selected.as_deref())?;
        self.by_key.get(key).and_then(|state| state.target.clone())
    }
}

/// The DPI a mouse had before a held DPI shift, read from its sensor when
/// config or an action never set one.
///
/// It is kept until a restore is written. A hold that supersedes a pending
/// restore then reuses it, where a fresh reading would be the shifted DPI.
#[derive(Debug, Clone, Default)]
pub(crate) struct DpiBeforeShift(Arc<Mutex<Option<Dpi>>>);

impl DpiBeforeShift {
    pub(crate) fn get(&self) -> Option<Dpi> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn set(&self, dpi: Dpi) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(dpi);
    }

    pub(crate) fn clear(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// Shared state consumed by the OS hook thread and the DPI panel UI to
/// implement DPI preset cycling and direct preset selection actions.
///
/// `index` is the position of the *current* DPI (i.e. the one last set on the
/// device), not the next-to-fire. `cycle` advances and returns the new value.
#[derive(Debug, Clone, Default)]
pub struct DpiCycleState {
    pub presets: Vec<Dpi>,
    pub index: usize,
    pub target: Option<DeviceRoute>,
    pub capabilities: Option<DpiCapabilities>,
    /// The sensor DPI last set through config or an action, when known.
    pub current: Option<Dpi>,
    /// The configured DPI this state was built from.
    pub committed: Option<Dpi>,
    /// The DPI the mouse had before a held DPI shift. It goes with this
    /// state when the mouse goes offline, and [`DpiCycles::reconnected`]
    /// replaces it.
    pub(crate) before_shift: DpiBeforeShift,
}

impl DpiCycleState {
    /// Advance to the next preset (wrapping last → first) and return the new
    /// DPI + the device target to write to. Returns `None` if `presets` is
    /// empty.
    pub fn cycle(&mut self) -> Option<(Dpi, Option<DeviceRoute>)> {
        if self.presets.is_empty() {
            return None;
        }
        self.index = (self.index + 1) % self.presets.len();
        Some(self.select())
    }

    /// Jump to preset `i`, clamping to the list length. Returns the DPI +
    /// target, or `None` if `presets` is empty.
    pub fn set(&mut self, i: usize) -> Option<(Dpi, Option<DeviceRoute>)> {
        if self.presets.is_empty() {
            return None;
        }
        self.index = i.min(self.presets.len() - 1);
        Some(self.select())
    }

    /// Steps one preset up, stopping at the last.
    pub fn step_up(&mut self) -> Option<(Dpi, Option<DeviceRoute>)> {
        let last = self.presets.len().checked_sub(1)?;
        self.index = (self.index + 1).min(last);
        Some(self.select())
    }

    /// Steps one preset down, stopping at the first.
    pub fn step_down(&mut self) -> Option<(Dpi, Option<DeviceRoute>)> {
        self.presets.len().checked_sub(1)?;
        self.index = self.index.saturating_sub(1);
        Some(self.select())
    }

    /// Steps to the lowest preset above `reading`, the DPI read from a
    /// mouse OpenLogi never set, stopping at the highest. The index says
    /// nothing about where such a mouse is.
    pub fn step_up_from(&mut self, reading: Dpi) -> Option<(Dpi, Option<DeviceRoute>)> {
        let reading = self.normalize(reading);
        let dpis = self.normalized_presets();
        let above = (0..dpis.len())
            .filter(|&i| dpis[i] > reading)
            .min_by_key(|&i| dpis[i]);
        self.index = above.or_else(|| (0..dpis.len()).max_by_key(|&i| dpis[i]))?;
        Some(self.select())
    }

    /// Steps to the highest preset below `reading`, stopping at the lowest;
    /// see [`Self::step_up_from`].
    pub fn step_down_from(&mut self, reading: Dpi) -> Option<(Dpi, Option<DeviceRoute>)> {
        let reading = self.normalize(reading);
        let dpis = self.normalized_presets();
        let below = (0..dpis.len())
            .filter(|&i| dpis[i] < reading)
            .max_by_key(|&i| dpis[i]);
        self.index = below.or_else(|| (0..dpis.len()).min_by_key(|&i| dpis[i]))?;
        Some(self.select())
    }

    fn normalized_presets(&self) -> Vec<Dpi> {
        self.presets
            .iter()
            .map(|&dpi| self.normalize(dpi))
            .collect()
    }

    /// The lowest preset, which a held DPI shift drops to.
    #[must_use]
    pub fn lowest(&self) -> Option<Dpi> {
        let low = self.presets.iter().copied().min()?;
        Some(self.normalize(low))
    }

    fn select(&mut self) -> (Dpi, Option<DeviceRoute>) {
        let dpi = self.normalize(self.presets[self.index]);
        self.current = Some(dpi);
        (dpi, self.target.clone())
    }

    /// The mouse was set back to its saved `dpi`, so a DPI shift returns
    /// there and the presets step on from it.
    pub fn reapplied(&mut self, dpi: Dpi) {
        let dpi = self.normalize(dpi);
        self.current = Some(dpi);
        if let Some(index) = nearest_preset(&self.presets, dpi) {
            self.index = index;
        }
    }

    fn normalize(&self, dpi: Dpi) -> Dpi {
        self.capabilities
            .as_ref()
            .map_or(dpi, |caps| caps.nearest(dpi))
    }
}

/// The position of the preset closest to `dpi`.
pub(crate) fn nearest_preset(presets: &[Dpi], dpi: Dpi) -> Option<usize> {
    (0..presets.len()).min_by_key(|&i| presets[i].into_inner().abs_diff(dpi.into_inner()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cycles_with(key: &str, slot: u8) -> DpiCycles {
        let mut cycles = DpiCycles::default();
        cycles.by_key.insert(
            key.to_string(),
            DpiCycleState {
                presets: vec![Dpi::new(800), Dpi::new(1600)],
                index: 0,
                target: Some(DeviceRoute::Bolt {
                    receiver_uid: "AA00".to_string(),
                    slot,
                }),
                capabilities: None,
                ..DpiCycleState::default()
            },
        );
        cycles
    }

    #[test]
    fn a_reapplied_dpi_is_what_a_shift_returns_to() {
        let mut cycles = cycles_with("a", 1);
        let state = cycles.state_for(Some("a")).unwrap();
        state.set(0);
        state.reapplied(Dpi::new(1600));
        assert_eq!(state.current, Some(Dpi::new(1600)));
        assert_eq!(state.index, 1);
    }

    #[test]
    fn step_stops_at_both_ends_and_shift_drops_to_the_lowest_preset() {
        let mut cycles = cycles_with("a", 1);
        let state = cycles.state_for(Some("a")).unwrap();
        assert_eq!(state.step_down().unwrap().0, Dpi::new(800));
        assert_eq!(state.step_up().unwrap().0, Dpi::new(1600));
        assert_eq!(state.step_up().unwrap().0, Dpi::new(1600));
        assert_eq!(state.lowest(), Some(Dpi::new(800)));
    }

    /// A preset is not a reading: until config or an action sets the DPI, a
    /// shift has to ask the mouse what to return to.
    #[test]
    fn current_dpi_is_unknown_until_config_or_an_action_sets_it() {
        let mut cycles = cycles_with("a", 1);
        assert_eq!(cycles.current("a"), None);
        let (key, low, _) = cycles.target_for_shift(Some("a")).unwrap();
        assert_eq!((key, low), ("a", Dpi::new(800)));
        cycles.state_for(Some("a")).unwrap().step_up();
        assert_eq!(cycles.current("a"), Some(Dpi::new(1600)));
    }

    /// A mouse OpenLogi never set steps from the DPI it reads: at 1600 with
    /// the G502's stock presets the next one is 3200, not the 800 after
    /// index 0. A reading between presets steps to the nearest one past it.
    #[test]
    fn a_step_from_a_reading_goes_past_the_mouse_dpi() {
        let mut state = DpiCycleState {
            presets: [400, 800, 1600, 3200, 6400].map(Dpi::new).to_vec(),
            ..DpiCycleState::default()
        };
        let up = |state: &mut DpiCycleState, reading| state.step_up_from(Dpi::new(reading));
        let down = |state: &mut DpiCycleState, reading| state.step_down_from(Dpi::new(reading));
        assert_eq!(up(&mut state, 1600).unwrap().0, Dpi::new(3200));
        assert_eq!(state.index, 3);
        assert_eq!(state.current, Some(Dpi::new(3200)));
        assert_eq!(down(&mut state, 1600).unwrap().0, Dpi::new(800));
        assert_eq!(up(&mut state, 1200).unwrap().0, Dpi::new(1600));
        assert_eq!(down(&mut state, 1200).unwrap().0, Dpi::new(800));
        assert_eq!(
            up(&mut state, 6400).unwrap().0,
            Dpi::new(6400),
            "stops at the top"
        );
        assert_eq!(
            down(&mut state, 100).unwrap().0,
            Dpi::new(400),
            "stops at the bottom"
        );
        assert!(
            DpiCycleState::default()
                .step_up_from(Dpi::new(800))
                .is_none()
        );
    }

    #[test]
    fn target_for_resolves_explicit_key_and_selection_fallback() {
        let mut cycles = cycles_with("a", 1);
        assert!(cycles.target_for(Some("a")).is_some());
        assert!(cycles.target_for(Some("missing")).is_none());
        // No key and no selection → nothing to target.
        assert!(cycles.target_for(None).is_none());
        // The OS-hook path (no key) follows the selection.
        cycles.selected = Some("a".to_string());
        assert!(cycles.target_for(None).is_some());
        assert_eq!(
            cycles.target_for(None),
            cycles.state_for(None).and_then(|s| s.target.clone())
        );
    }
}
