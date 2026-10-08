//! DPI-cycle state shared with background action dispatch.

use std::collections::HashMap;

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

    #[must_use]
    pub fn target_for_shift(&self, key: Option<&str>) -> Option<(Dpi, Dpi, DeviceRoute)> {
        let key = key.or(self.selected.as_deref())?;
        let (low, restore, target) = self.by_key.get(key)?.shift()?;
        Some((low, restore, target?))
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
        Some((
            self.normalize(self.presets[self.index]),
            self.target.clone(),
        ))
    }

    /// Jump to preset `i`, clamping to the list length. Returns the DPI +
    /// target, or `None` if `presets` is empty.
    pub fn set(&mut self, i: usize) -> Option<(Dpi, Option<DeviceRoute>)> {
        if self.presets.is_empty() {
            return None;
        }
        let clamped = i.min(self.presets.len() - 1);
        self.index = clamped;
        Some((self.normalize(self.presets[clamped]), self.target.clone()))
    }

    /// Steps one preset without wrapping.
    pub fn step(&mut self, up: bool) -> Option<(Dpi, Option<DeviceRoute>)> {
        let last = self.presets.len().checked_sub(1)?;
        self.index = if up {
            (self.index + 1).min(last)
        } else {
            self.index.saturating_sub(1)
        };
        Some((
            self.normalize(self.presets[self.index]),
            self.target.clone(),
        ))
    }

    /// The lowest preset, and the current one to return to.
    #[must_use]
    pub fn shift(&self) -> Option<(Dpi, Dpi, Option<DeviceRoute>)> {
        let low = self.presets.iter().copied().min()?;
        let current = *self.presets.get(self.index)?;
        Some((
            self.normalize(low),
            self.normalize(current),
            self.target.clone(),
        ))
    }

    fn normalize(&self, dpi: Dpi) -> Dpi {
        self.capabilities
            .as_ref()
            .map_or(dpi, |caps| caps.nearest(dpi))
    }
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
            },
        );
        cycles
    }

    #[test]
    fn step_stops_at_both_ends_and_shift_returns_to_current() {
        let mut cycles = cycles_with("a", 1);
        let state = cycles.state_for(Some("a")).unwrap();
        assert_eq!(state.step(false).unwrap().0, Dpi::new(800));
        assert_eq!(state.step(true).unwrap().0, Dpi::new(1600));
        assert_eq!(state.step(true).unwrap().0, Dpi::new(1600));
        let (low, back, _) = state.shift().unwrap();
        assert_eq!((low, back), (Dpi::new(800), Dpi::new(1600)));
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
