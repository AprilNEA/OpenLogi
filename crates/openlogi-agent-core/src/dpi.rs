//! DPI-cycle state shared with background action dispatch.

use std::collections::HashMap;

use openlogi_hid::{DeviceRoute, DpiCapabilities};

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
}

/// Shared state consumed by the OS hook thread and the DPI panel UI to
/// implement DPI preset cycling and direct preset selection actions.
///
/// `index` is the position of the *current* DPI (i.e. the one last set on the
/// device), not the next-to-fire. `cycle` advances and returns the new value.
#[derive(Debug, Clone, Default)]
pub struct DpiCycleState {
    pub presets: Vec<u32>,
    pub index: usize,
    pub target: Option<DeviceRoute>,
    pub capabilities: Option<DpiCapabilities>,
}

impl DpiCycleState {
    /// Advance to the next preset (wrapping last → first) and return the new
    /// DPI + the device target to write to. Returns `None` if `presets` is
    /// empty.
    pub fn cycle(&mut self) -> Option<(u32, Option<DeviceRoute>)> {
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
    pub fn set(&mut self, i: usize) -> Option<(u32, Option<DeviceRoute>)> {
        if self.presets.is_empty() {
            return None;
        }
        let clamped = i.min(self.presets.len() - 1);
        self.index = clamped;
        Some((self.normalize(self.presets[clamped]), self.target.clone()))
    }

    fn normalize(&self, dpi: u32) -> u32 {
        self.capabilities
            .as_ref()
            .map_or(dpi, |caps| caps.snap(dpi))
    }
}
